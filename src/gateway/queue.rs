use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use rusqlite::{Connection, params};

use crate::platform::{InboundMessage, OutboundMessage};

pub struct InboundQueue {
    conn: Arc<Mutex<Connection>>,
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

impl InboundQueue {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub async fn enqueue(&self, msg: InboundMessage) -> Result<i64> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO inbound_queue (platform, chat_id, user_id, text, received_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![msg.platform, msg.chat_id, msg.user_id, msg.text, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn claim_next(&self) -> Result<Option<InboundRow>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        // RETURNING (SQLite >= 3.35) makes the claim a single atomic statement.
        let mut stmt = conn.prepare(
            "UPDATE inbound_queue \
             SET state='processing', attempts = attempts + 1 \
             WHERE id = (SELECT id FROM inbound_queue WHERE state='pending' \
                         ORDER BY received_at ASC LIMIT 1) \
             RETURNING id, platform, chat_id, user_id, text, received_at, attempts",
        )?;
        let mut rows = stmt.query([])?;
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

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        conn.execute("DELETE FROM inbound_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub async fn complete_with_outbound(&self, id: i64, msg: OutboundMessage) -> Result<i64> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        tx.execute(
            "INSERT INTO outbound_queue \
             (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending')",
            params![msg.platform, msg.chat_id, msg.text, attachments_json, now],
        )?;
        let outbound_id = tx.last_insert_rowid();
        tx.execute("DELETE FROM inbound_queue WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(outbound_id)
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        // inbound_queue has no last_error column; narrate via tracing instead.
        tracing::warn!(target: "gateway::queue", id, error, "inbound message marked failed");
        conn.execute(
            "UPDATE inbound_queue SET state='failed' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub async fn recover_processing(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let n = conn.execute(
            "UPDATE inbound_queue SET state='pending' WHERE state='processing'",
            [],
        )?;
        Ok(n)
    }
}

pub struct OutboundQueue {
    conn: Arc<Mutex<Connection>>,
    max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct OutboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub text: String,
    pub attachments: Vec<String>,
    pub enqueued_at: i64,
    pub next_attempt_at: i64,
    pub attempts: i64,
    pub state: String,
    pub last_error: Option<String>,
}

// Retry waits indexed by failures-so-far: 1st → 5s, 2nd → 30s, ... clamps at 7200s.
fn outbound_backoff_secs(attempts: i64) -> i64 {
    const SCHEDULE: &[i64] = &[5, 30, 300, 1800, 7200];
    let idx = (attempts - 1).clamp(0, (SCHEDULE.len() - 1) as i64) as usize;
    SCHEDULE[idx]
}

impl OutboundQueue {
    pub fn new(conn: Arc<Mutex<Connection>>, max_attempts: u32) -> Self {
        Self { conn, max_attempts }
    }

    pub async fn enqueue(&self, msg: OutboundMessage) -> Result<i64> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        conn.execute(
            "INSERT INTO outbound_queue \
             (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending')",
            params![msg.platform, msg.chat_id, msg.text, attachments_json, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn claim_due(&self, now: i64) -> Result<Option<OutboundRow>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let mut stmt = conn.prepare(
            "UPDATE outbound_queue \
             SET state='sending', attempts = attempts + 1 \
             WHERE id = (SELECT id FROM outbound_queue \
                         WHERE state='pending' AND next_attempt_at <= ?1 \
                         ORDER BY next_attempt_at ASC LIMIT 1) \
             RETURNING id, platform, chat_id, text, attachments_json, enqueued_at, \
                       next_attempt_at, attempts, state, last_error",
        )?;
        let mut rows = stmt.query(params![now])?;
        if let Some(row) = rows.next()? {
            let attachments_json: String = row.get(4)?;
            let attachments: Vec<String> = serde_json::from_str(&attachments_json)?;
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
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        conn.execute("DELETE FROM outbound_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let row = self.peek(id).await?;
        let now = chrono::Utc::now().timestamp();
        let next_state = if row.attempts as u32 >= self.max_attempts {
            "failed"
        } else {
            "pending"
        };
        let next_at = now + outbound_backoff_secs(row.attempts);
        {
            let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
            conn.execute(
                "UPDATE outbound_queue \
                 SET state = ?1, next_attempt_at = ?2, last_error = ?3 \
                 WHERE id = ?4",
                params![next_state, next_at, error, id],
            )?;
        }
        tracing::warn!(target: "gateway::queue", id, error, "outbound delivery failed");
        Ok(())
    }

    pub async fn recover_sending(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let n = conn.execute(
            "UPDATE outbound_queue SET state='pending' WHERE state='sending'",
            [],
        )?;
        Ok(n)
    }

    pub async fn peek(&self, id: i64) -> Result<OutboundRow> {
        let conn = self.conn.lock().map_err(|_| anyhow!("queue DB poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT id, platform, chat_id, text, attachments_json, enqueued_at, \
                    next_attempt_at, attempts, state, last_error \
             FROM outbound_queue WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let attachments_json: String = row.get(4)?;
            let attachments: Vec<String> = serde_json::from_str(&attachments_json)?;
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
            })
        } else {
            Err(anyhow!("row not found"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().expect("open mem db");
        crate::memory::initialize_test_conn(&conn).expect("schema");
        Arc::new(Mutex::new(conn))
    }

    fn sample_msg() -> InboundMessage {
        InboundMessage {
            platform: "loopback".into(),
            chat_id: "c1".into(),
            user_id: "u1".into(),
            text: "hi".into(),
        }
    }

    fn sample_out_msg() -> OutboundMessage {
        OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c1".into(),
            text: "hello".into(),
            attachments: vec![],
        }
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

    #[tokio::test]
    async fn outbound_mark_done_deletes() {
        let conn = test_conn();
        let q = OutboundQueue::new(Arc::clone(&conn), 5);
        let id = q.enqueue(sample_out_msg()).await.unwrap();
        let _ = q.claim_due(chrono::Utc::now().timestamp()).await.unwrap();
        q.mark_done(id).await.unwrap();
        assert!(q.peek(id).await.is_err(), "row should be gone");
    }

    #[tokio::test]
    async fn outbound_recover_sending_resets_to_pending() {
        let conn = test_conn();
        let q = OutboundQueue::new(Arc::clone(&conn), 5);
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
        let q = OutboundQueue::new(Arc::clone(&conn), 5);
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
        let q = InboundQueue::new(Arc::clone(&conn));
        let id = q.enqueue(sample_msg()).await.unwrap();
        let _ = q.claim_next().await.unwrap();
        q.mark_done(id).await.unwrap();
        let conn = conn.lock().expect("lock test conn");
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
    async fn mark_failed_sets_state() {
        let q = InboundQueue::new(test_conn());
        let id = q.enqueue(sample_msg()).await.unwrap();
        let _ = q.claim_next().await.unwrap();
        q.mark_failed(id, "boom").await.unwrap();
        assert!(q.claim_next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recover_processing_resets_to_pending() {
        let conn = test_conn();
        let q = InboundQueue::new(Arc::clone(&conn));
        q.enqueue(sample_msg()).await.unwrap();
        let _ = q.claim_next().await.unwrap();
        drop(q);
        let q2 = InboundQueue::new(conn);
        let recovered = q2.recover_processing().await.unwrap();
        assert_eq!(recovered, 1);
        assert!(q2.claim_next().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn complete_with_outbound_enqueues_reply_and_deletes_inbound_atomically() {
        let conn = test_conn();
        let inbound = InboundQueue::new(Arc::clone(&conn));
        let outbound = OutboundQueue::new(Arc::clone(&conn), 5);
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
                },
            )
            .await
            .unwrap();

        assert!(inbound.claim_next().await.unwrap().is_none());
        let outbound_row = outbound.peek(outbound_id).await.unwrap();
        assert_eq!(outbound_row.text, "reply");
    }
}
