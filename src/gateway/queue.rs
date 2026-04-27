use anyhow::{Result, anyhow};
use rusqlite::params;

use crate::memory::DbPool;
use crate::platform::{InboundMessage, OutboundAttachment, OutboundMessage};

/// Default per-row attempt cap before mark_failed routes a row to the
/// dead-letter queue (YYC-137). Operators can override via
/// `InboundQueue::with_policy`.
pub const DEFAULT_INBOUND_MAX_ATTEMPTS: u32 = 3;

/// Default staleness threshold for `recover_processing`. Rows whose
/// `last_heartbeat_at` is older than `now - this` are considered
/// crashed and reset to `pending`. Anything fresher is left running
/// (YYC-137 dedup against duplicate work after a quick worker
/// restart). 30 min picked to comfortably exceed the longest healthy
/// run_prompt turn; tunable via `with_policy`.
pub const DEFAULT_INBOUND_HEARTBEAT_STALE_SECS: i64 = 1800;

pub struct InboundQueue {
    conn: DbPool,
    max_attempts: u32,
    heartbeat_stale_secs: i64,
}

#[derive(Debug, Clone)]
pub struct InboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
    pub received_at: i64,
    pub attempts: i64,
}

/// Snapshot of a dead-letter row for the DLQ admin endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeadInboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
    pub received_at: i64,
    pub attempts: i64,
    pub last_error: Option<String>,
}

impl InboundQueue {
    pub fn new(conn: DbPool) -> Self {
        Self::with_policy(
            conn,
            DEFAULT_INBOUND_MAX_ATTEMPTS,
            DEFAULT_INBOUND_HEARTBEAT_STALE_SECS,
        )
    }

    /// Construct with explicit retry + heartbeat-staleness policy
    /// (YYC-137). `max_attempts` caps how many times a single row gets
    /// retried before it's routed to the dead-letter queue;
    /// `heartbeat_stale_secs` controls how old a `processing` row's
    /// heartbeat must be before `recover_processing` recycles it.
    pub fn with_policy(conn: DbPool, max_attempts: u32, heartbeat_stale_secs: i64) -> Self {
        Self {
            conn,
            max_attempts,
            heartbeat_stale_secs,
        }
    }

    pub async fn enqueue(&self, msg: InboundMessage) -> Result<i64> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO inbound_queue (platform, chat_id, user_id, text, received_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![msg.platform, msg.chat_id, msg.user_id, msg.text, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn claim_next(&self) -> Result<Option<InboundRow>> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let now = chrono::Utc::now().timestamp();
        // RETURNING (SQLite >= 3.35) makes the claim a single atomic statement.
        // YYC-137: stamp last_heartbeat_at on claim so recover_processing can
        // tell live-but-slow rows from crash-orphaned ones.
        let mut stmt = conn.prepare(
            "UPDATE inbound_queue \
             SET state='processing', attempts = attempts + 1, last_heartbeat_at = ?1 \
             WHERE id = (SELECT id FROM inbound_queue WHERE state='pending' \
                         ORDER BY received_at ASC LIMIT 1) \
             RETURNING id, platform, chat_id, user_id, text, received_at, attempts",
        )?;
        let mut rows = stmt.query(params![now])?;
        if let Some(row) = rows.next()? {
            Ok(Some(InboundRow {
                id: row.get(0)?,
                platform: row.get(1)?,
                chat_id: row.get(2)?,
                user_id: row.get(3)?,
                text: row.get(4)?,
                received_at: row.get(5)?,
                attempts: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Refresh `last_heartbeat_at` for a row in-flight (YYC-137). Long
    /// runs that exceed the stale threshold can call this periodically
    /// to keep `recover_processing` from racing them on next startup.
    pub async fn heartbeat(&self, id: i64) -> Result<()> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE inbound_queue SET last_heartbeat_at = ?1 \
             WHERE id = ?2 AND state = 'processing'",
            params![now, id],
        )?;
        Ok(())
    }

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        conn.execute("DELETE FROM inbound_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub async fn complete_with_outbound(&self, id: i64, msg: OutboundMessage) -> Result<i64> {
        let mut conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        tx.execute(
            "INSERT INTO outbound_queue \
             (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state, \
              edit_target, reply_to, turn_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending', ?6, ?7, ?8)",
            params![
                msg.platform,
                msg.chat_id,
                msg.text,
                attachments_json,
                now,
                msg.edit_target,
                msg.reply_to,
                msg.turn_id,
            ],
        )?;
        let outbound_id = tx.last_insert_rowid();
        tx.execute("DELETE FROM inbound_queue WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(outbound_id)
    }

    /// Record a processing failure. If the row's `attempts` count has
    /// reached `max_attempts` it routes to the dead-letter queue
    /// (`state='dead'`) so it stops being claimed. Otherwise the row
    /// goes back to `state='pending'` for another try (YYC-137).
    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let attempts: i64 = conn.query_row(
            "SELECT attempts FROM inbound_queue WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let next_state = if attempts as u32 >= self.max_attempts {
            // YYC-146: DLQ transitions log at error so operators don't
            // need to query state to notice exhausted rows.
            tracing::error!(
                target: "gateway::queue",
                id, attempts, error, max_attempts = self.max_attempts,
                "inbound DLQ transition: row exhausted retries, routing to dead-letter queue",
            );
            "dead"
        } else {
            tracing::warn!(
                target: "gateway::queue",
                id, attempts, error, max_attempts = self.max_attempts,
                "inbound message marked failed; will retry",
            );
            "pending"
        };
        conn.execute(
            "UPDATE inbound_queue SET state = ?1, last_error = ?2 WHERE id = ?3",
            params![next_state, error, id],
        )?;
        Ok(())
    }

    /// Reset crash-orphaned `processing` rows back to `pending`.
    ///
    /// YYC-137: only rows whose `last_heartbeat_at` is older than
    /// `now - heartbeat_stale_secs` (or null — pre-heartbeat-column
    /// rows from older DB versions) are reset. Recently-touched rows
    /// are assumed to belong to a live worker and are left alone, so
    /// a fast worker restart doesn't duplicate in-flight work.
    pub async fn recover_processing(&self) -> Result<usize> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - self.heartbeat_stale_secs;
        let n = conn.execute(
            "UPDATE inbound_queue SET state='pending' \
             WHERE state='processing' \
               AND (last_heartbeat_at IS NULL OR last_heartbeat_at < ?1)",
            params![cutoff],
        )?;
        Ok(n)
    }

    /// Count of rows currently in the dead-letter queue (YYC-137).
    pub async fn count_dead(&self) -> Result<usize> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM inbound_queue WHERE state='dead'",
            [],
            |row| row.get(0),
        )?;
        Ok(n.max(0) as usize)
    }

    /// Snapshot the most recent `limit` dead-letter rows so an admin
    /// endpoint can surface them for replay (YYC-137).
    pub async fn list_dead(&self, limit: usize) -> Result<Vec<DeadInboundRow>> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, platform, chat_id, user_id, text, received_at, attempts, last_error \
             FROM inbound_queue WHERE state='dead' \
             ORDER BY received_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(DeadInboundRow {
                    id: row.get(0)?,
                    platform: row.get(1)?,
                    chat_id: row.get(2)?,
                    user_id: row.get(3)?,
                    text: row.get(4)?,
                    received_at: row.get(5)?,
                    attempts: row.get(6)?,
                    last_error: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Move a dead row back to `pending` so it gets re-claimed on the
    /// next worker poll. Resets `attempts` to 0 so the retry budget
    /// starts fresh; preserves `last_error` for diagnostics. Returns
    /// whether the row was found in `dead` state (YYC-137).
    pub async fn replay_dead(&self, id: i64) -> Result<bool> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let n = conn.execute(
            "UPDATE inbound_queue SET state='pending', attempts=0 \
             WHERE id = ?1 AND state='dead'",
            params![id],
        )?;
        Ok(n > 0)
    }
}

pub struct OutboundQueue {
    conn: DbPool,
    max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct OutboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub text: String,
    pub attachments: Vec<OutboundAttachment>,
    pub enqueued_at: i64,
    pub next_attempt_at: i64,
    pub attempts: i64,
    pub state: String,
    pub last_error: Option<String>,
    /// YYC-18 PR-2a: anchor for edit-in-place streaming. When `Some`,
    /// the OutboundDispatcher routes to `Platform::edit` instead of
    /// `Platform::send`.
    pub edit_target: Option<String>,
    /// Reply / thread target on the platform side.
    pub reply_to: Option<String>,
    /// YYC-18 PR-2b: per-turn id used by the dispatcher to build the
    /// RenderKey for anchor capture. `None` for non-streaming rows.
    pub turn_id: Option<String>,
}

// Retry waits indexed by failures-so-far: 1st → 5s, 2nd → 30s, ... clamps at 7200s.
fn outbound_backoff_secs(attempts: i64) -> i64 {
    const SCHEDULE: &[i64] = &[5, 30, 300, 1800, 7200];
    let idx = (attempts - 1).clamp(0, (SCHEDULE.len() - 1) as i64) as usize;
    SCHEDULE[idx]
}

impl OutboundQueue {
    pub fn new(conn: DbPool, max_attempts: u32) -> Self {
        Self { conn, max_attempts }
    }

    pub async fn enqueue(&self, msg: OutboundMessage) -> Result<i64> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        conn.execute(
            "INSERT INTO outbound_queue \
             (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state, \
              edit_target, reply_to, turn_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending', ?6, ?7, ?8)",
            params![
                msg.platform,
                msg.chat_id,
                msg.text,
                attachments_json,
                now,
                msg.edit_target,
                msg.reply_to,
                msg.turn_id,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn claim_due(&self, now: i64) -> Result<Option<OutboundRow>> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let mut stmt = conn.prepare(
            "UPDATE outbound_queue \
             SET state='sending', attempts = attempts + 1 \
             WHERE id = (SELECT id FROM outbound_queue \
                         WHERE state='pending' AND next_attempt_at <= ?1 \
                         ORDER BY next_attempt_at ASC, id ASC LIMIT 1) \
             RETURNING id, platform, chat_id, text, attachments_json, enqueued_at, \
                       next_attempt_at, attempts, state, last_error, edit_target, reply_to, \
                       turn_id",
        )?;
        let mut rows = stmt.query(params![now])?;
        if let Some(row) = rows.next()? {
            let attachments_json: String = row.get(4)?;
            let attachments: Vec<OutboundAttachment> = serde_json::from_str(&attachments_json)?;
            Ok(Some(OutboundRow {
                id: row.get(0)?,
                platform: row.get(1)?,
                chat_id: row.get(2)?,
                text: row.get(3)?,
                attachments,
                enqueued_at: row.get(5)?,
                next_attempt_at: row.get(6)?,
                attempts: row.get(7)?,
                state: row.get(8)?,
                last_error: row.get(9)?,
                edit_target: row.get(10)?,
                reply_to: row.get(11)?,
                turn_id: row.get(12)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        conn.execute("DELETE FROM outbound_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let row = self.peek(id).await?;
        let now = chrono::Utc::now().timestamp();
        let dlq = row.attempts as u32 >= self.max_attempts;
        let next_state = if dlq { "failed" } else { "pending" };
        let next_at = now + outbound_backoff_secs(row.attempts);
        {
            let conn = self
                .conn
                .get()
                .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
            conn.execute(
                "UPDATE outbound_queue \
                 SET state = ?1, next_attempt_at = ?2, last_error = ?3 \
                 WHERE id = ?4",
                params![next_state, next_at, error, id],
            )?;
        }
        // YYC-146: DLQ transitions log at error with lane metadata.
        // Retries stay at warn so operators can filter on level.
        if dlq {
            tracing::error!(
                target: "gateway::queue",
                id,
                platform = %row.platform,
                chat_id = %row.chat_id,
                attempts = row.attempts,
                max_attempts = self.max_attempts,
                error,
                "outbound DLQ transition: row exhausted retries",
            );
        } else {
            tracing::warn!(
                target: "gateway::queue",
                id,
                platform = %row.platform,
                chat_id = %row.chat_id,
                attempts = row.attempts,
                error,
                "outbound delivery failed; will retry",
            );
        }
        Ok(())
    }

    pub async fn recover_sending(&self) -> Result<usize> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let n = conn.execute(
            "UPDATE outbound_queue SET state='pending' WHERE state='sending'",
            [],
        )?;
        Ok(n)
    }

    pub async fn peek(&self, id: i64) -> Result<OutboundRow> {
        let conn = self
            .conn
            .get()
            .map_err(|e| anyhow!("queue DB pool checkout: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, platform, chat_id, text, attachments_json, enqueued_at, \
                    next_attempt_at, attempts, state, last_error, edit_target, reply_to, \
                    turn_id \
             FROM outbound_queue WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let attachments_json: String = row.get(4)?;
            let attachments: Vec<OutboundAttachment> = serde_json::from_str(&attachments_json)?;
            Ok(OutboundRow {
                id: row.get(0)?,
                platform: row.get(1)?,
                chat_id: row.get(2)?,
                text: row.get(3)?,
                attachments,
                enqueued_at: row.get(5)?,
                next_attempt_at: row.get(6)?,
                attempts: row.get(7)?,
                state: row.get(8)?,
                last_error: row.get(9)?,
                edit_target: row.get(10)?,
                reply_to: row.get(11)?,
                turn_id: row.get(12)?,
            })
        } else {
            Err(anyhow!("row not found"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn sample_msg() -> InboundMessage {
        InboundMessage {
            platform: "loopback".into(),
            chat_id: "c1".into(),
            user_id: "u1".into(),
            text: "hi".into(),
            message_id: None,
            reply_to: None,
            attachments: vec![],
        }
    }

    fn sample_out_msg() -> OutboundMessage {
        OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c1".into(),
            text: "hello".into(),
            attachments: vec![],
            reply_to: None,
            edit_target: None,
            turn_id: None,
        }
    }

    #[tokio::test]
    async fn outbound_queue_round_trips_edit_target_and_reply_to() {
        let q = OutboundQueue::new(test_conn(), 5);
        let msg = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "edit me".into(),
            attachments: vec![],
            reply_to: Some("parent-msg-1".into()),
            edit_target: Some("anchor-msg-7".into()),
            turn_id: None,
        };
        q.enqueue(msg).await.unwrap();
        let row = q
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.edit_target, Some("anchor-msg-7".into()));
        assert_eq!(row.reply_to, Some("parent-msg-1".into()));
    }

    #[tokio::test]
    async fn outbound_queue_round_trips_turn_id() {
        let q = OutboundQueue::new(test_conn(), 5);
        let msg = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "x".into(),
            attachments: vec![],
            reply_to: None,
            edit_target: None,
            turn_id: Some("turn-abc".into()),
        };
        q.enqueue(msg).await.unwrap();
        let row = q
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.turn_id, Some("turn-abc".into()));
    }

    #[tokio::test]
    async fn outbound_queue_round_trips_typed_attachments() {
        use crate::platform::{AttachmentKind, OutboundAttachment};
        let q = OutboundQueue::new(test_conn(), 5);
        let msg = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "hi".into(),
            attachments: vec![OutboundAttachment {
                path: std::path::PathBuf::from("/tmp/x.png"),
                kind: AttachmentKind::Image,
                caption: Some("cap".into()),
            }],
            reply_to: None,
            edit_target: None,
            turn_id: None,
        };
        q.enqueue(msg).await.unwrap();
        let row = q
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.attachments.len(), 1);
        assert_eq!(row.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(row.attachments[0].caption.as_deref(), Some("cap"));
    }

    #[tokio::test]
    async fn outbound_enqueue_due_immediately() {
        let q = OutboundQueue::new(test_conn(), 5);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        let now = chrono::Utc::now().timestamp();
        let row = q.claim_due(now).await.unwrap().expect("due now");
        assert_eq!(row.id, id);
        assert_eq!(row.state, "sending");
        assert_eq!(row.attempts, 1);
    }

    #[tokio::test]
    async fn outbound_failure_schedules_next_attempt_with_backoff() {
        let q = OutboundQueue::new(test_conn(), 5);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        let claim_at = chrono::Utc::now().timestamp();
        let _ = q.claim_due(claim_at).await.unwrap().expect("claim");
        q.mark_failed(id, "boom").await.unwrap();
        let row = q.peek(id).await.unwrap();
        assert_eq!(row.attempts, 1);
        assert_eq!(row.state, "pending");
        assert_eq!(row.last_error.as_deref(), Some("boom"));
        let elapsed = row.next_attempt_at - claim_at;
        assert!(
            elapsed >= 5,
            "next_attempt_at should be >= claim+5, got {elapsed}"
        );
        assert!(
            elapsed <= 10,
            "next_attempt_at should be < claim+10, got {elapsed}"
        );
    }

    #[tokio::test]
    async fn outbound_drops_after_max_attempts() {
        let q = OutboundQueue::new(test_conn(), 3);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        for _ in 0..3 {
            let now = chrono::Utc::now().timestamp() + 1_000_000;
            let _ = q.claim_due(now).await.unwrap().expect("claim");
            q.mark_failed(id, "boom").await.unwrap();
        }
        let row = q.peek(id).await.unwrap();
        assert_eq!(row.state, "failed");
        assert_eq!(row.attempts, 3);
    }

    // YYC-146: a DLQ transition must surface as an `error` event with
    // lane metadata so operators see exhausted rows without polling
    // queue state. Captures tracing output to a buffer and asserts the
    // structured fields appear.
    #[tokio::test]
    async fn outbound_dlq_transition_logs_at_error_with_lane() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct VecMakeWriter(Arc<Mutex<Vec<u8>>>);
        struct SharedVec(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for SharedVec {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecMakeWriter {
            type Writer = SharedVec;
            fn make_writer(&'a self) -> SharedVec {
                SharedVec(self.0.clone())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(VecMakeWriter(buf.clone()))
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let q = OutboundQueue::new(test_conn(), 2);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        for _ in 0..2 {
            let now = chrono::Utc::now().timestamp() + 1_000_000;
            let _ = q.claim_due(now).await.unwrap().expect("claim");
            q.mark_failed(id, "boom").await.unwrap();
        }

        let captured = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("ERROR") && captured.contains("DLQ"),
            "expected ERROR-level DLQ event, got: {captured}",
        );
        assert!(
            captured.contains("platform=loopback") && captured.contains("chat_id=c1"),
            "expected lane metadata in event, got: {captured}",
        );
    }

    #[tokio::test]
    async fn outbound_mark_done_deletes() {
        let conn = test_conn();
        let q = OutboundQueue::new(conn.clone(), 5);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        let _ = q.claim_due(chrono::Utc::now().timestamp()).await.unwrap();
        q.mark_done(id).await.unwrap();
        assert!(q.peek(id).await.is_err(), "row should be gone");
    }

    #[tokio::test]
    async fn outbound_recover_sending_resets_to_pending() {
        let conn = test_conn();
        let q = OutboundQueue::new(conn.clone(), 5);
        q.enqueue(sample_out_msg()).await.unwrap();
        let _ = q.claim_due(chrono::Utc::now().timestamp()).await.unwrap();
        drop(q);
        let q2 = OutboundQueue::new(conn, 5);
        let recovered = q2.recover_sending().await.unwrap();
        assert_eq!(recovered, 1);
        assert!(
            q2.claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn backoff_schedule_values() {
        assert_eq!(outbound_backoff_secs(1), 5);
        assert_eq!(outbound_backoff_secs(2), 30);
        assert_eq!(outbound_backoff_secs(3), 300);
        assert_eq!(outbound_backoff_secs(4), 1800);
        assert_eq!(outbound_backoff_secs(5), 7200);
        assert_eq!(outbound_backoff_secs(99), 7200);
        assert_eq!(outbound_backoff_secs(0), 5);
    }

    #[tokio::test]
    async fn outbound_claim_due_skips_future_rows() {
        let conn = test_conn();
        let q = OutboundQueue::new(conn.clone(), 5);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        let _ = q.claim_due(chrono::Utc::now().timestamp()).await.unwrap();
        q.mark_failed(id, "nope").await.unwrap();
        assert!(
            q.claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            q.claim_due(chrono::Utc::now().timestamp() + 100)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn enqueue_then_claim_yields_pending_row() {
        let q = InboundQueue::new(test_conn());
        let id = q.enqueue(sample_msg()).await.unwrap();
        let claimed = q.claim_next().await.unwrap().expect("claim returns row");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.text, "hi");
        assert_eq!(claimed.platform, "loopback");
        assert_eq!(claimed.chat_id, "c1");
        assert_eq!(claimed.user_id, "u1");
    }

    #[tokio::test]
    async fn claim_marks_processing_so_second_claim_returns_none() {
        let q = InboundQueue::new(test_conn());
        let id = q.enqueue(sample_msg()).await.unwrap();
        let row = q.claim_next().await.unwrap().expect("first claim");
        assert_eq!(row.id, id);
        assert!(
            q.claim_next().await.unwrap().is_none(),
            "row in processing should not re-claim"
        );
    }

    #[tokio::test]
    async fn mark_done_deletes_row() {
        let conn = test_conn();
        let q = InboundQueue::new(conn.clone());
        let id = q.enqueue(sample_msg()).await.unwrap();
        let _ = q.claim_next().await.unwrap();
        q.mark_done(id).await.unwrap();
        let conn = conn.get().expect("get test conn");
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM inbound_queue WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0);
    }

    #[tokio::test]
    async fn mark_failed_retries_until_max_attempts_then_routes_to_dead() {
        // YYC-137: single retry budget = max_attempts. After
        // max_attempts mark_failed cycles, the row lands in
        // state='dead' and is no longer claimable.
        let q = InboundQueue::with_policy(test_conn(), 2, 60);
        let id = q.enqueue(sample_msg()).await.unwrap();

        // attempt 1 → fail → back to pending (still under cap).
        let _ = q.claim_next().await.unwrap().expect("attempt 1");
        q.mark_failed(id, "boom-1").await.unwrap();
        assert!(
            q.claim_next().await.unwrap().is_some(),
            "row should be re-claimable after first failure",
        );

        // attempt 2 → fail → exhausts budget → state='dead'.
        q.mark_failed(id, "boom-2").await.unwrap();
        assert!(
            q.claim_next().await.unwrap().is_none(),
            "row should not re-claim after reaching max_attempts",
        );
        assert_eq!(q.count_dead().await.unwrap(), 1);

        // last_error preserved + replay_dead resets attempts and
        // re-arms the row.
        let dead = q.list_dead(10).await.unwrap();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].last_error.as_deref(), Some("boom-2"));
        assert_eq!(dead[0].attempts, 2);

        let replayed = q.replay_dead(id).await.unwrap();
        assert!(replayed);
        let row = q.claim_next().await.unwrap().expect("row replayed");
        assert_eq!(
            row.attempts, 1,
            "replay_dead should reset attempts to 0 then claim bumps to 1"
        );
    }

    #[tokio::test]
    async fn recover_processing_resets_only_stale_heartbeats() {
        // YYC-137 acceptance: a freshly-claimed row (recent heartbeat)
        // is NOT recovered — that's the in-flight worker. Only rows
        // older than heartbeat_stale_secs get reset.
        let conn = test_conn();

        // heartbeat_stale_secs = 60: a just-claimed row's heartbeat
        // is too fresh to reset.
        let live = InboundQueue::with_policy(conn.clone(), 5, 60);
        let live_id = live.enqueue(sample_msg()).await.unwrap();
        let _ = live.claim_next().await.unwrap().expect("claim");

        let recovered_live = live.recover_processing().await.unwrap();
        assert_eq!(
            recovered_live, 0,
            "live worker's fresh heartbeat should not be recovered",
        );
        assert!(
            live.claim_next().await.unwrap().is_none(),
            "row should remain in 'processing'",
        );

        // Force the heartbeat into the past + run recovery with
        // heartbeat_stale_secs=10. Now the row IS stale.
        {
            let c = conn.get().unwrap();
            let now = chrono::Utc::now().timestamp();
            c.execute(
                "UPDATE inbound_queue SET last_heartbeat_at = ?1 WHERE id = ?2",
                params![now - 3600, live_id],
            )
            .unwrap();
        }
        let stale = InboundQueue::with_policy(conn.clone(), 5, 10);
        let recovered_stale = stale.recover_processing().await.unwrap();
        assert_eq!(recovered_stale, 1);
        assert!(
            stale.claim_next().await.unwrap().is_some(),
            "stale row should be re-claimable after recovery",
        );
    }

    #[tokio::test]
    async fn heartbeat_refreshes_last_heartbeat_at() {
        let conn = test_conn();
        let q = InboundQueue::with_policy(conn.clone(), 5, 60);
        let id = q.enqueue(sample_msg()).await.unwrap();
        let _ = q.claim_next().await.unwrap();

        // Stamp heartbeat into the past, then refresh; recovery with
        // a 10s window now leaves it alone.
        {
            let c = conn.get().unwrap();
            let now = chrono::Utc::now().timestamp();
            c.execute(
                "UPDATE inbound_queue SET last_heartbeat_at = ?1 WHERE id = ?2",
                params![now - 3600, id],
            )
            .unwrap();
        }
        q.heartbeat(id).await.unwrap();
        let q_strict = InboundQueue::with_policy(conn, 5, 10);
        assert_eq!(q_strict.recover_processing().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn complete_with_outbound_enqueues_reply_and_deletes_inbound_atomically() {
        let conn = test_conn();
        let inbound = InboundQueue::new(conn.clone());
        let outbound = OutboundQueue::new(conn.clone(), 5);
        let id = inbound.enqueue(sample_msg()).await.unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");
        assert_eq!(row.id, id);

        let outbound_id = inbound
            .complete_with_outbound(
                id,
                OutboundMessage {
                    platform: "loopback".into(),
                    chat_id: "c1".into(),
                    text: "reply".into(),
                    attachments: vec![],
                    reply_to: None,
                    edit_target: None,
                    turn_id: None,
                },
            )
            .await
            .unwrap();

        assert!(inbound.claim_next().await.unwrap().is_none());
        let outbound_row = outbound.peek(outbound_id).await.unwrap();
        assert_eq!(outbound_row.text, "reply");
    }
}
