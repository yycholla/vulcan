//! Session lifecycle + history accessors extracted from `agent/mod.rs`
//! (YYC-109 redo). Owns start/end/resume/continue/fork hooks and the
//! borrow accessor for the underlying `SessionStore`.

use anyhow::Result;
use uuid::Uuid;

use super::Agent;

impl Agent {
    /// Fires `session_start` on all hook handlers. Call once after construction
    /// (`AgentBuilder::build` doesn't call it itself because hooks aren't
    /// always async-available at construction time).
    pub async fn start_session(&self) {
        if let Err(e) = self
            .memory
            .save_session_metadata(&self.session_id, None, None)
        {
            tracing::warn!("failed to initialize session metadata: {e}");
        }
        self.hooks.session_start(&self.session_id).await;
    }

    /// Fires `session_end` and records the total turn count. Also
    /// reaps any LSP servers spawned during the session (YYC-46).
    pub async fn end_session(&self) {
        self.hooks.session_end(&self.session_id, self.turns).await;
        self.hooks
            .on_session_shutdown(self.turn_cancel.clone())
            .await;
        self.lsp_manager.shutdown_all().await;
    }
    pub fn resume_session(&mut self, session_id: &str) -> Result<()> {
        let history = self
            .memory
            .load_history(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        self.session_id = session_id.to_string();
        tracing::info!("resumed session {session_id} ({} messages)", history.len());
        Ok(())
    }

    /// Resume the most recently active session. Errors if there are no
    /// sessions on disk.
    pub fn continue_last_session(&mut self) -> Result<()> {
        match self.memory.last_session_id() {
            Some(id) => self.resume_session(&id),
            None => anyhow::bail!("No previous session to resume"),
        }
    }

    /// Create a new child session rooted at the current one, persist its
    /// lineage, and switch the agent to that child session immediately.
    pub fn fork_session(&mut self, lineage_label: Option<&str>) -> Result<String> {
        let parent_session_id = self.session_id.clone();
        let child_session_id = Uuid::new_v4().to_string();
        self.memory.save_session_metadata(
            &child_session_id,
            Some(&parent_session_id),
            lineage_label,
        )?;
        self.session_id = child_session_id.clone();
        tracing::info!(
            "forked session {} -> {}",
            parent_session_id,
            child_session_id
        );
        Ok(child_session_id)
    }

    /// Emit `on_session_before_fork`, then create a child session.
    pub async fn fork_session_with_hooks(&mut self, lineage_label: Option<&str>) -> Result<String> {
        self.hooks
            .on_session_before_fork(self.turn_cancel.clone())
            .await;
        self.fork_session(lineage_label)
    }

    /// Borrow the underlying `SessionStore`. Used by the TUI's `/search`
    /// command and the `vulcan search` CLI subcommand to run FTS queries.
    pub fn memory(&self) -> &crate::memory::SessionStore {
        &self.memory
    }
}
