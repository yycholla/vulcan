use super::*;
use crate::provider::ToolCall;
use tempfile::TempDir;

fn store_in(dir: &TempDir) -> SessionStore {
    // Bypass `new()` so we don't write into ~/.vulcan during tests.
    let path = dir.path().join("sessions.db");
    let conn = Connection::open(&path).unwrap();
    initialize_conn(&conn).unwrap();
    SessionStore {
        conn: Mutex::new(conn),
    }
}

#[test]
fn round_trip_messages() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let session_id = uuid::Uuid::new_v4().to_string();

    let messages = vec![
        Message::System {
            content: "you are a helpful agent".into(),
        },
        Message::User {
            content: "what is rust?".into(),
        },
        Message::Assistant {
            content: Some("a systems language with strong types".into()),
            tool_calls: None,
            reasoning_content: None,
        },
    ];

    store.save_messages(&session_id, &messages).unwrap();
    let loaded = store.load_history(&session_id).unwrap().unwrap();
    assert_eq!(loaded.len(), 3);
    match &loaded[1] {
        Message::User { content } => assert_eq!(content, "what is rust?"),
        other => panic!("expected User, got {other:?}"),
    }
}

#[test]
fn provider_profile_round_trips() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();

    // No row yet → None.
    assert_eq!(store.load_provider_profile(&id).unwrap(), None);

    // Set a profile (creates the row).
    store.save_provider_profile(&id, Some("local")).unwrap();
    assert_eq!(
        store.load_provider_profile(&id).unwrap().as_deref(),
        Some("local")
    );

    // Clearing collapses back to None.
    store.save_provider_profile(&id, None).unwrap();
    assert_eq!(store.load_provider_profile(&id).unwrap(), None);
}

#[test]
fn provider_profile_survives_save_messages() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();

    store.save_provider_profile(&id, Some("local")).unwrap();
    store
        .save_messages(
            &id,
            &[Message::User {
                content: "hi".into(),
            }],
        )
        .unwrap();

    // save_messages must not clobber the profile column.
    assert_eq!(
        store.load_provider_profile(&id).unwrap().as_deref(),
        Some("local")
    );
}

#[test]
fn list_sessions_includes_provider_profile() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();
    store
        .save_messages(
            &id,
            &[Message::User {
                content: "hi".into(),
            }],
        )
        .unwrap();
    store.save_provider_profile(&id, Some("local")).unwrap();

    let summaries = store.list_sessions(10).unwrap();
    let summary = summaries.iter().find(|s| s.id == id).expect("summary");
    assert_eq!(summary.provider_profile.as_deref(), Some("local"));
}

#[test]
fn last_session_id_returns_most_recent() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();

    store
        .save_messages(
            &id,
            &[Message::User {
                content: "first".into(),
            }],
        )
        .unwrap();
    assert_eq!(store.last_session_id(), Some(id));
}

#[test]
fn list_sessions_returns_summaries_in_recency_order() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let s1 = uuid::Uuid::new_v4().to_string();
    let s2 = uuid::Uuid::new_v4().to_string();

    store
        .save_messages(
            &s1,
            &[Message::User {
                content: "a".into(),
            }],
        )
        .unwrap();
    // Sleep 1s would make this deterministic, but the second save bumps
    // last_active beyond the first's wall-clock-second granularity in
    // practice. Make it explicit by saving twice with different content.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store
        .save_messages(
            &s2,
            &[Message::User {
                content: "b".into(),
            }],
        )
        .unwrap();

    let summaries = store.list_sessions(10).unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, s2, "most recent should come first");
    assert_eq!(summaries[1].id, s1);
    assert_eq!(summaries[0].message_count, 1);
}

#[test]
fn fts_search_finds_content() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let session_id = uuid::Uuid::new_v4().to_string();

    store
        .save_messages(
            &session_id,
            &[
                Message::User {
                    content: "the quick brown fox jumps over the lazy dog".into(),
                },
                Message::User {
                    content: "lorem ipsum dolor sit amet".into(),
                },
            ],
        )
        .unwrap();

    let hits = store.search_messages("brown fox", 10).unwrap();
    assert!(
        hits.iter().any(|h| h.content.contains("brown fox")),
        "expected fox hit, got {hits:?}"
    );
}

#[test]
fn session_lineage_survives_metadata_and_message_saves() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let parent_id = uuid::Uuid::new_v4().to_string();
    let child_id = uuid::Uuid::new_v4().to_string();

    store
        .save_messages(
            &parent_id,
            &[Message::User {
                content: "root".into(),
            }],
        )
        .unwrap();
    store
        .save_session_metadata(
            &child_id,
            Some(&parent_id),
            Some("branched from root session"),
        )
        .unwrap();
    store
        .save_messages(
            &child_id,
            &[Message::User {
                content: "child".into(),
            }],
        )
        .unwrap();

    let summaries = store.list_sessions(10).unwrap();
    let child = summaries
        .iter()
        .find(|s| s.id == child_id)
        .expect("child summary should exist");
    assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
    assert_eq!(
        child.lineage_label.as_deref(),
        Some("branched from root session")
    );
    assert_eq!(child.message_count, 1);
}

#[test]
fn save_messages_preserves_existing_lineage_metadata() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let parent_id = uuid::Uuid::new_v4().to_string();
    let child_id = uuid::Uuid::new_v4().to_string();

    store
        .save_session_metadata(&child_id, Some(&parent_id), Some("forked"))
        .unwrap();
    store
        .save_messages(
            &child_id,
            &[Message::User {
                content: "first".into(),
            }],
        )
        .unwrap();
    store
        .save_messages(
            &child_id,
            &[Message::User {
                content: "second".into(),
            }],
        )
        .unwrap();

    let summaries = store.list_sessions(10).unwrap();
    let child = summaries
        .iter()
        .find(|s| s.id == child_id)
        .expect("child summary should exist");
    assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
    assert_eq!(child.lineage_label.as_deref(), Some("forked"));
    assert_eq!(child.message_count, 1);
}

#[test]
fn assistant_with_tool_calls_round_trips() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();

    let messages = vec![Message::Assistant {
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "bash".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
        }]),
        reasoning_content: None,
    }];

    store.save_messages(&id, &messages).unwrap();
    let loaded = store.load_history(&id).unwrap().unwrap();

    match &loaded[0] {
        Message::Assistant { tool_calls, .. } => {
            let tcs = tool_calls.as_ref().expect("tool calls present");
            assert_eq!(tcs.len(), 1);
            assert_eq!(tcs[0].function.name, "bash");
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[test]
fn queue_tables_created() {
    let store = SessionStore::in_memory();
    let conn = store.conn.lock();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master \
         WHERE type='table' AND name IN ('inbound_queue','outbound_queue')",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(count, 2);
}

#[test]
fn queue_indexes_created() {
    let store = SessionStore::in_memory();
    let conn = store.conn.lock();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master \
         WHERE type='index' AND name IN ('idx_inbound_lane','idx_inbound_state','idx_outbound_due')",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(count, 3);
}

#[test]
fn reasoning_content_round_trips() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let id = uuid::Uuid::new_v4().to_string();

    let messages = vec![Message::Assistant {
        content: Some("the answer is 42".into()),
        tool_calls: None,
        reasoning_content: Some("First I considered…then I weighed…".into()),
    }];

    store.save_messages(&id, &messages).unwrap();
    let loaded = store.load_history(&id).unwrap().unwrap();
    match &loaded[0] {
        Message::Assistant {
            reasoning_content, ..
        } => assert_eq!(
            reasoning_content.as_deref(),
            Some("First I considered…then I weighed…")
        ),
        other => panic!("expected Assistant, got {other:?}"),
    }
}

// YYC-149: every connection used by SessionStore should have the
// 5-second busy_timeout applied so a contended writer doesn't fail
// immediately with SQLITE_BUSY and pin a tokio worker thread.
#[test]
fn session_store_connection_has_busy_timeout() {
    let dir = TempDir::new().unwrap();
    let store = store_in(&dir);
    let conn = store.conn.lock();
    let timeout: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .expect("query busy_timeout");
    assert_eq!(timeout, 5_000, "expected 5000ms busy_timeout");
}

// YYC-149: a writer holding a transaction must not lock out a
// second writer immediately. The busy_timeout absorbs short
// contention. Spawns a thread that holds a write transaction for a
// brief moment, kicks off a competing write on the main thread,
// and asserts it succeeds — without the timeout this would return
// SQLITE_BUSY synchronously.
#[test]
fn busy_timeout_absorbs_short_writer_contention() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("contend.db");
    let primary = Connection::open(&path).unwrap();
    initialize_conn(&primary).unwrap();
    drop(primary);

    let path_thread = path.clone();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_thread = Arc::clone(&barrier);
    let holder = thread::spawn(move || {
        let mut conn = Connection::open(&path_thread).unwrap();
        initialize_conn(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO sessions (id, created_at, last_active) VALUES ('hold', 0, 0)",
            [],
        )
        .unwrap();
        // Signal the contender to attempt its write, then hold the
        // transaction briefly so the other thread has to wait inside
        // busy_timeout.
        barrier_thread.wait();
        thread::sleep(Duration::from_millis(150));
        tx.commit().unwrap();
    });

    barrier.wait();
    let conn = Connection::open(&path).unwrap();
    initialize_conn(&conn).unwrap();
    let result = conn.execute(
        "INSERT INTO sessions (id, created_at, last_active) VALUES ('contender', 0, 0)",
        [],
    );
    holder.join().unwrap();
    assert!(
        result.is_ok(),
        "busy_timeout should absorb short writer contention; got {result:?}",
    );
}
