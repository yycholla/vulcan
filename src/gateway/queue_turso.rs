//! Turso backend for the gateway inbound/outbound queues (GH #704).
//! Same async surface as the rusqlite impls in `queue.rs` — methods
//! were already async there (via `db_blocking`), so callers are
//! untouched; only the constructors' handle type changes. The whole
//! `db_blocking`/r2d2/`spawn_blocking` apparatus disappears: turso is
//! natively async.
//!
//! All three gateway stores share one `gateway.db`, so the boot path
//! opens a single `turso::Database` and hands each store its own
//! `Connection` (see `crate::db::open_database`).

use anyhow::{Result, anyhow};

use super::queue::{
    DEFAULT_INBOUND_HEARTBEAT_STALE_SECS, DEFAULT_INBOUND_MAX_ATTEMPTS, InboundQueue, InboundRow,
    OutboundQueue, OutboundRow, outbound_backoff_secs,
};
use crate::platform::{InboundMessage, OutboundAttachment, OutboundMessage};

/// Initialize the gateway schema on a fresh `gateway.db` (GH #704).
/// Mirrors `memory::schema::GATEWAY_SCHEMA` minus the PRAGMAs and the
/// rusqlite-era additive-column migrations — Turso DBs start fresh.
pub(super) async fn initialize_gateway_db(conn: &turso::Connection) -> Result<()> {
    for stmt in [
        "CREATE TABLE IF NOT EXISTS inbound_queue (
            id INTEGER PRIMARY KEY,
            platform TEXT NOT NULL,
            chat_id  TEXT NOT NULL,
            user_id  TEXT NOT NULL,
            text     TEXT NOT NULL,
            received_at INTEGER NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            state    TEXT NOT NULL,
            last_error TEXT,
            last_heartbeat_at INTEGER,
            attachments_json TEXT NOT NULL DEFAULT '[]',
            message_id TEXT,
            reply_to TEXT
        )",
        "CREATE INDEX IF NOT EXISTS idx_inbound_lane  ON inbound_queue(platform, chat_id, state)",
        "CREATE INDEX IF NOT EXISTS idx_inbound_state ON inbound_queue(state, received_at)",
        "CREATE TABLE IF NOT EXISTS outbound_queue (
            id INTEGER PRIMARY KEY,
            platform TEXT NOT NULL,
            chat_id  TEXT NOT NULL,
            text     TEXT NOT NULL,
            attachments_json TEXT NOT NULL DEFAULT '[]',
            enqueued_at INTEGER NOT NULL,
            next_attempt_at INTEGER NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            state    TEXT NOT NULL,
            last_error TEXT,
            edit_target TEXT,
            reply_to TEXT,
            turn_id TEXT
        )",
        "CREATE INDEX IF NOT EXISTS idx_outbound_due ON outbound_queue(state, next_attempt_at)",
        "CREATE TABLE IF NOT EXISTS scheduler_runs (
            job_id            TEXT PRIMARY KEY,
            last_fired_at     INTEGER,
            last_finished_at  INTEGER,
            last_status       TEXT,
            last_error        TEXT,
            last_inbound_id   INTEGER,
            total_fires       INTEGER NOT NULL DEFAULT 0,
            skipped_fires     INTEGER NOT NULL DEFAULT 0,
            failed_fires      INTEGER NOT NULL DEFAULT 0,
            completed_fires   INTEGER NOT NULL DEFAULT 0,
            active_fires      INTEGER NOT NULL DEFAULT 0
        )",
    ] {
        crate::db::execute_ddl(conn, stmt).await?;
    }
    Ok(())
}

impl InboundQueue {
    pub fn new(conn: turso::Connection) -> Self {
        Self::with_policy(
            conn,
            DEFAULT_INBOUND_MAX_ATTEMPTS,
            DEFAULT_INBOUND_HEARTBEAT_STALE_SECS,
        )
    }

    pub fn with_policy(
        conn: turso::Connection,
        max_attempts: u32,
        heartbeat_stale_secs: i64,
    ) -> Self {
        Self {
            conn,
            max_attempts,
            heartbeat_stale_secs,
        }
    }

    pub async fn enqueue(&self, msg: InboundMessage) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        let mut rows = self
            .conn
            .query(
                "INSERT INTO inbound_queue (platform, chat_id, user_id, text, received_at, state) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending') \
                 RETURNING id",
                turso::params_from_iter([
                    turso::Value::from(msg.platform),
                    turso::Value::from(msg.chat_id),
                    turso::Value::from(msg.user_id),
                    turso::Value::from(msg.text),
                    turso::Value::from(now),
                ]),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| anyhow!("inbound insert returned no row"))?;
        Ok(row.get(0)?)
    }

    pub async fn claim_next(&self) -> Result<Option<InboundRow>> {
        let now = chrono::Utc::now().timestamp();
        let mut rows = self
            .conn
            .query(
                "UPDATE inbound_queue \
                 SET state='processing', attempts = attempts + 1, last_heartbeat_at = ?1 \
                 WHERE id = (SELECT id FROM inbound_queue WHERE state='pending' \
                             ORDER BY received_at ASC LIMIT 1) \
                 RETURNING id, platform, chat_id, user_id, text, received_at, attempts",
                (now,),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(InboundRow {
                id: row.get(0)?,
                platform: row.get(1)?,
                chat_id: row.get(2)?,
                user_id: row.get(3)?,
                text: row.get(4)?,
                received_at: row.get(5)?,
                attempts: row.get(6)?,
            })),
            None => Ok(None),
        }
    }

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM inbound_queue WHERE id = ?1", (id,))
            .await?;
        Ok(())
    }

    pub async fn complete_with_outbound(&self, id: i64, msg: OutboundMessage) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        let mut conn = self.conn.clone();
        let tx = conn.transaction().await?;
        tx.execute(
            "INSERT INTO outbound_queue \
             (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state, \
              edit_target, reply_to, turn_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending', ?6, ?7, ?8)",
            turso::params_from_iter([
                turso::Value::from(msg.platform),
                turso::Value::from(msg.chat_id),
                turso::Value::from(msg.text),
                turso::Value::from(attachments_json),
                turso::Value::from(now),
                msg.edit_target.into(),
                msg.reply_to.into(),
                msg.turn_id.into(),
            ]),
        )
        .await?;
        let outbound_id = tx.last_insert_rowid();
        tx.execute("DELETE FROM inbound_queue WHERE id = ?1", (id,))
            .await?;
        tx.commit().await?;
        Ok(outbound_id)
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let attempts: i64 = {
            let mut rows = self
                .conn
                .query("SELECT attempts FROM inbound_queue WHERE id = ?1", (id,))
                .await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => return Err(anyhow!("inbound row {id} not found")),
            }
        };
        let next_state = if attempts as u32 >= self.max_attempts {
            tracing::error!(
                target: "gateway::queue",
                id, attempts, error = %error, max_attempts = self.max_attempts,
                "inbound DLQ transition: row exhausted retries, routing to dead-letter queue",
            );
            "dead"
        } else {
            tracing::warn!(
                target: "gateway::queue",
                id, attempts, error = %error, max_attempts = self.max_attempts,
                "inbound message marked failed; will retry",
            );
            "pending"
        };
        self.conn
            .execute(
                "UPDATE inbound_queue SET state = ?1, last_error = ?2 WHERE id = ?3",
                turso::params_from_iter([
                    turso::Value::from(next_state.to_string()),
                    turso::Value::from(error.to_string()),
                    turso::Value::from(id),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn recover_processing(&self) -> Result<usize> {
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - self.heartbeat_stale_secs;
        let n = self
            .conn
            .execute(
                "UPDATE inbound_queue SET state='pending' \
                 WHERE state='processing' \
                   AND (last_heartbeat_at IS NULL OR last_heartbeat_at < ?1)",
                (cutoff,),
            )
            .await?;
        Ok(n as usize)
    }

    #[cfg(test)]
    pub async fn count_dead(&self) -> Result<usize> {
        let mut rows = self
            .conn
            .query("SELECT count(*) FROM inbound_queue WHERE state='dead'", ())
            .await?;
        let n: i64 = match rows.next().await? {
            Some(row) => row.get(0)?,
            None => 0,
        };
        Ok(n.max(0) as usize)
    }
}

impl OutboundQueue {
    pub fn new(conn: turso::Connection, max_attempts: u32) -> Self {
        Self { conn, max_attempts }
    }

    pub async fn enqueue(&self, msg: OutboundMessage) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        let attachments_json = serde_json::to_string(&msg.attachments)?;
        let mut rows = self
            .conn
            .query(
                "INSERT INTO outbound_queue \
                 (platform, chat_id, text, attachments_json, enqueued_at, next_attempt_at, state, \
                  edit_target, reply_to, turn_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'pending', ?6, ?7, ?8) \
                 RETURNING id",
                turso::params_from_iter([
                    turso::Value::from(msg.platform),
                    turso::Value::from(msg.chat_id),
                    turso::Value::from(msg.text),
                    turso::Value::from(attachments_json),
                    turso::Value::from(now),
                    msg.edit_target.into(),
                    msg.reply_to.into(),
                    msg.turn_id.into(),
                ]),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| anyhow!("outbound insert returned no row"))?;
        Ok(row.get(0)?)
    }

    pub async fn claim_due(&self, now: i64) -> Result<Option<OutboundRow>> {
        let mut rows = self
            .conn
            .query(
                "UPDATE outbound_queue \
                 SET state='sending', attempts = attempts + 1 \
                 WHERE id = (SELECT id FROM outbound_queue \
                             WHERE state='pending' AND next_attempt_at <= ?1 \
                             ORDER BY next_attempt_at ASC, id ASC LIMIT 1) \
                 RETURNING id, platform, chat_id, text, attachments_json, enqueued_at, \
                           next_attempt_at, attempts, state, last_error, edit_target, reply_to, \
                           turn_id",
                (now,),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(outbound_row_from_turso(&row)?)),
            None => Ok(None),
        }
    }

    pub async fn mark_done(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM outbound_queue WHERE id = ?1", (id,))
            .await?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let row = self.peek(id).await?;
        let now = chrono::Utc::now().timestamp();
        let dlq = row.attempts as u32 >= self.max_attempts;
        let next_state = if dlq { "failed" } else { "pending" };
        let next_at = now + outbound_backoff_secs(row.attempts);
        self.conn
            .execute(
                "UPDATE outbound_queue \
                 SET state = ?1, next_attempt_at = ?2, last_error = ?3 \
                 WHERE id = ?4",
                turso::params_from_iter([
                    turso::Value::from(next_state.to_string()),
                    turso::Value::from(next_at),
                    turso::Value::from(error.to_string()),
                    turso::Value::from(id),
                ]),
            )
            .await?;
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
        let n = self
            .conn
            .execute(
                "UPDATE outbound_queue SET state='pending' WHERE state='sending'",
                (),
            )
            .await?;
        Ok(n as usize)
    }

    pub async fn peek(&self, id: i64) -> Result<OutboundRow> {
        let mut rows = self
            .conn
            .query(
                "SELECT id, platform, chat_id, text, attachments_json, enqueued_at, \
                        next_attempt_at, attempts, state, last_error, edit_target, reply_to, \
                        turn_id \
                 FROM outbound_queue WHERE id = ?1",
                (id,),
            )
            .await?;
        match rows.next().await? {
            Some(row) => outbound_row_from_turso(&row),
            None => Err(anyhow!("row not found")),
        }
    }
}

fn outbound_row_from_turso(row: &turso::Row) -> Result<OutboundRow> {
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
}

// GH #704 parity tests: the Turso queue impls must satisfy the same
// lifecycle contract the rusqlite tests pin down.
#[cfg(test)]
mod tests {
    use super::*;

    async fn queues() -> (InboundQueue, OutboundQueue) {
        let db = crate::db::open_database_in_memory().await.unwrap();
        let conn = crate::db::connect_database(&db).await.unwrap();
        initialize_gateway_db(&conn).await.unwrap();
        (
            InboundQueue::with_policy(crate::db::connect_database(&db).await.unwrap(), 2, 60),
            OutboundQueue::new(crate::db::connect_database(&db).await.unwrap(), 2),
        )
    }

    fn msg(text: &str) -> InboundMessage {
        InboundMessage {
            platform: "loopback".into(),
            chat_id: "c1".into(),
            user_id: "u1".into(),
            text: text.into(),
            attachments: Vec::new(),
            message_id: None,
            reply_to: None,
        }
    }

    #[tokio::test]
    async fn inbound_enqueue_claim_complete_round_trip() {
        let (inbound, outbound) = queues().await;
        let id = inbound.enqueue(msg("hello")).await.unwrap();
        assert!(id > 0);

        let row = inbound.claim_next().await.unwrap().expect("claimed");
        assert_eq!(row.text, "hello");
        assert_eq!(row.attempts, 1);
        // Nothing else pending.
        assert!(inbound.claim_next().await.unwrap().is_none());

        // Atomic complete: outbound row appears, inbound row gone.
        let out_id = inbound
            .complete_with_outbound(
                row.id,
                OutboundMessage {
                    platform: "loopback".into(),
                    chat_id: "c1".into(),
                    text: "reply".into(),
                    attachments: Vec::new(),
                    edit_target: None,
                    reply_to: None,
                    turn_id: None,
                },
            )
            .await
            .unwrap();
        let due = outbound
            .claim_due(chrono::Utc::now().timestamp() + 1)
            .await
            .unwrap()
            .expect("outbound due");
        assert_eq!(due.id, out_id);
        assert_eq!(due.text, "reply");
        outbound.mark_done(due.id).await.unwrap();
    }

    #[tokio::test]
    async fn inbound_retries_then_dead_letters() {
        let (inbound, _outbound) = queues().await;
        inbound.enqueue(msg("flaky")).await.unwrap();

        // Attempt 1 fails -> back to pending.
        let row = inbound.claim_next().await.unwrap().expect("claim 1");
        inbound.mark_failed(row.id, "boom-1").await.unwrap();
        // Attempt 2 fails -> attempts == max_attempts -> dead.
        let row = inbound.claim_next().await.unwrap().expect("claim 2");
        assert_eq!(row.attempts, 2);
        inbound.mark_failed(row.id, "boom-2").await.unwrap();

        assert!(inbound.claim_next().await.unwrap().is_none());
        assert_eq!(inbound.count_dead().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn outbound_backoff_and_recover() {
        let (_inbound, outbound) = queues().await;
        let now = chrono::Utc::now().timestamp();
        let id = outbound
            .enqueue(OutboundMessage {
                platform: "loopback".into(),
                chat_id: "c1".into(),
                text: "out".into(),
                attachments: Vec::new(),
                edit_target: None,
                reply_to: None,
                turn_id: None,
            })
            .await
            .unwrap();
        let row = outbound.claim_due(now + 1).await.unwrap().expect("due");
        assert_eq!(row.id, id);
        // Failure schedules a retry in the future; not due now.
        outbound.mark_failed(id, "send failed").await.unwrap();
        assert!(outbound.claim_due(now + 1).await.unwrap().is_none());
        // recover_sending is a no-op here (row is pending), but must not error.
        assert_eq!(outbound.recover_sending().await.unwrap(), 0);
        let peeked = outbound.peek(id).await.unwrap();
        assert_eq!(peeked.state, "pending");
        assert_eq!(peeked.last_error.as_deref(), Some("send failed"));
    }
}
