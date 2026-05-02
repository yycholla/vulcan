//! Picker / session list state extracted from `tui/state.rs` (YYC-110).
//! Picker / session list state extracted from `tui/state.rs` (YYC-110).
//! Render implementations live in `tui/widgets/` and overlay composition
//! code — only the data shape lives here.

/// One entry in the session-list picker overlay (YYC-15).
#[derive(Clone, Debug)]
pub struct SessionState {
    pub id: String,
    pub label: String,
    pub message_count: usize,
    pub created_at: i64,
    pub last_active: i64,
    pub parent_session_id: Option<String>,
    pub lineage_label: Option<String>,
    pub preview: Option<String>,
    pub status: SessionStatus,
    pub is_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Live,
    Saved,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Saved => "saved",
        }
    }
}

/// One row in the provider picker overlay (YYC-97). `None` name = the
/// legacy unnamed `[provider]` block; `Some(name)` = a `[providers.<name>]`
/// profile.
#[derive(Clone, Debug)]
pub struct ProviderPickerEntry {
    pub name: Option<String>,
    pub model: String,
    pub base_url: String,
}
