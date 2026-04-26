use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::platform::InboundMessage;

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
        let conn = self.conn.lock().expect("queue mutex poisoned");
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO inbound_queue (platform, chat_id, user_id, text, received_at, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![msg.platform, msg.chat_id, msg.user_id, msg.text, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn claim_next(&self) -> Result<Option<InboundRow>> {
        let conn = self.conn.lock().expect("queue mutex poisoned");
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
        let conn = self.conn.lock().expect("queue mutex poisoned");
        conn.execute("DELETE FROM inbound_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().expect("queue mutex poisoned");
        // inbound_queue has no last_error column; narrate via tracing instead.
        tracing::warn!(target: "gateway::queue", id, error, "inbound message marked failed");
        conn.execute(
            "UPDATE inbound_queue SET state='failed' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub async fn recover_processing(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("queue mutex poisoned");
        let n = conn.execute(
            "UPDATE inbound_queue SET state='pending' WHERE state='processing'",
            [],
        )?;
        Ok(n)
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
}
