use crate::provider::Message;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent session storage using JSONL files
pub struct SessionStore {
    sessions_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
    pub title: Option<String>,
}

impl SessionStore {
    pub fn new() -> Self {
        let dir = crate::config::ferris_home().join("sessions");
        std::fs::create_dir_all(&dir).ok();
        Self { sessions_dir: dir }
    }

    /// Get the most recent session ID, if any
    pub fn last_session_id(&self) -> Option<String> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.sessions_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .filter_map(|e| {
                let meta = e.path().with_extension("json");
                let created = std::fs::metadata(&meta)
                    .and_then(|m| m.created())
                    .ok()?;
                Some((e.path(), created))
            })
            .collect();

        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries
            .first()
            .map(|(path, _)| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            })
    }

    /// Load message history for a session
    pub fn load_history(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
        let path = self.sessions_dir.join(format!("{session_id}.jsonl"));
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session {session_id}"))?;

        let messages: Vec<Message> = content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        Ok(Some(messages))
    }

    /// Save messages to session history
    pub fn save_messages(&self, messages: &[Message]) -> Result<String> {
        let session_id = self.last_session_id().unwrap_or_else(|| {
            uuid::Uuid::new_v4().to_string()
        });

        let path = self.sessions_dir.join(format!("{session_id}.jsonl"));

        let saved_count = if path.exists() {
            let existing = std::fs::read_to_string(&path)?;
            existing.lines().count()
        } else {
            0
        };

        // Write out the full history
        let mut file = std::fs::File::create(&path)?;
        for msg in messages {
            let line = serde_json::to_string(msg)?;
            writeln!(file, "{line}")?;
        }

        // Write meta
        let meta = SessionMeta {
            id: session_id.clone(),
            created_at: if saved_count == 0 {
                Utc::now().to_rfc3339()
            } else {
                // Read existing meta
                Self::read_meta(&self.sessions_dir, &session_id)
                    .unwrap_or_else(|| Utc::now().to_rfc3339())
            },
            updated_at: Utc::now().to_rfc3339(),
            message_count: messages.len(),
            title: None,
        };

        let meta_path = self.sessions_dir.join(format!("{session_id}.json"));
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;

        Ok(session_id)
    }

    fn read_meta(dir: &PathBuf, id: &str) -> Option<String> {
        let path = dir.join(format!("{id}.json"));
        let meta: SessionMeta =
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        Some(meta.created_at)
    }
}

// Helper for the write! macro in save_messages
use std::io::Write;
