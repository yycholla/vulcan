//! Built-in BeforePrompt hook that injects the workspace's *accepted*
//! playbook entries as a system block (YYC-223 wiring).
//!
//! Transient injection at AfterSystem — same shape as `SkillsHook` and
//! `RecallHook` — so the persistent history never carries the block.
//! Only entries the user has explicitly accepted are rendered;
//! `render_accepted_entries` filters proposed ones, which is the
//! security contract: an unreviewed agent suggestion must never
//! silently flow back into a prompt.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::playbook::{PlaybookStore, render_accepted_entries};
use crate::provider::Message;

use super::{HookHandler, HookOutcome, InjectPosition};

pub struct PlaybookHook {
    store: Arc<dyn PlaybookStore>,
    workspace: String,
}

impl PlaybookHook {
    pub fn new(store: Arc<dyn PlaybookStore>, workspace: String) -> Self {
        Self { store, workspace }
    }
}

#[async_trait]
impl HookHandler for PlaybookHook {
    fn name(&self) -> &str {
        "playbook"
    }

    /// After SkillsHook (10), before RecallHook (15): playbook entries
    /// are user-curated instructions, weightier than recalled background
    /// but subordinate to the skills catalog.
    fn priority(&self) -> i32 {
        12
    }

    async fn before_prompt(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        let body = match render_accepted_entries(self.store.as_ref(), &self.workspace) {
            Ok(Some(body)) => body,
            Ok(None) => return Ok(HookOutcome::Continue),
            Err(e) => {
                tracing::debug!("playbook: skipping injection: {e}");
                return Ok(HookOutcome::Continue);
            }
        };
        Ok(HookOutcome::InjectMessages {
            messages: vec![Message::System { content: body }],
            position: InjectPosition::AfterSystem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::{EntryStatus, PlaybookEntry, PlaybookSection, SqlitePlaybookStore};

    fn entry(status: EntryStatus) -> PlaybookEntry {
        PlaybookEntry {
            id: uuid::Uuid::new_v4(),
            section: PlaybookSection::Setup,
            body: "run direnv exec . cargo build".into(),
            source: "test".into(),
            status,
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn injects_accepted_entries_and_skips_proposed_only_workspaces() {
        let store = Arc::new(SqlitePlaybookStore::try_open_in_memory().unwrap());
        let ws = "/tmp/ws";

        // Proposed-only workspace → no injection.
        store
            .upsert_entry(ws, &entry(EntryStatus::Proposed))
            .unwrap();
        let hook = PlaybookHook::new(store.clone(), ws.to_string());
        let outcome = hook
            .before_prompt(&[], CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));

        // Accepted entry → injected as an AfterSystem system message.
        store
            .upsert_entry(ws, &entry(EntryStatus::Accepted))
            .unwrap();
        let outcome = hook
            .before_prompt(&[], CancellationToken::new())
            .await
            .unwrap();
        match outcome {
            HookOutcome::InjectMessages { messages, position } => {
                assert_eq!(position, InjectPosition::AfterSystem);
                assert_eq!(messages.len(), 1);
                let Message::System { content } = &messages[0] else {
                    panic!("expected system message");
                };
                assert!(content.contains("Project playbook"));
                assert!(content.contains("direnv exec"));
            }
            other => panic!("expected injection, got {other:?}"),
        }
    }
}
