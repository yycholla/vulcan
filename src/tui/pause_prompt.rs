use crate::pause::{AgentPause, AgentResume, PauseKind};

use super::input::{TuiKeyCode, TuiKeyEvent};

pub struct PausePromptState {
    summary: String,
    pause: Option<AgentPause>,
}

pub struct PausePromptOutcome {
    pub pause: Option<AgentPause>,
    pub resume: Option<AgentResume>,
    pub label: Option<&'static str>,
}

impl PausePromptState {
    pub fn new(summary: String, pause: AgentPause) -> Self {
        Self {
            summary,
            pause: Some(pause),
        }
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn pause(&self) -> Option<&AgentPause> {
        self.pause.as_ref()
    }

    pub fn take_pause(&mut self) -> Option<AgentPause> {
        self.pause.take()
    }

    pub fn handle_key(&mut self, key: TuiKeyEvent) -> PausePromptOutcome {
        let resume = self
            .pause
            .as_ref()
            .and_then(|pause| resume_for_key(pause, key));
        let Some(resume) = resume else {
            return PausePromptOutcome {
                pause: None,
                resume: None,
                label: None,
            };
        };

        let label = resume_label(&resume);
        PausePromptOutcome {
            pause: self.take_pause(),
            resume: Some(resume),
            label: Some(label),
        }
    }
}

pub fn pause_summary(pause: &AgentPause) -> String {
    match (&pause.kind, pause.options.is_empty()) {
        (
            PauseKind::SafetyApproval {
                command, reason, ..
            },
            false,
        ) => {
            format!("Safety: {reason}\n  $ {command}")
        }
        (PauseKind::ToolArgConfirm { tool, summary, .. }, false) => {
            format!("Confirm tool '{tool}': {summary}")
        }
        (PauseKind::SkillSave { suggested_name, .. }, false) => {
            format!("Save this as a skill named '{suggested_name}'?")
        }
        (PauseKind::UserChoice { question }, _) => question.clone(),
        (
            PauseKind::SafetyApproval {
                command, reason, ..
            },
            true,
        ) => {
            format!("Safety: {reason}\n  $ {command}\n  [a]llow once, [r]emember & allow, [d]eny")
        }
        (PauseKind::ToolArgConfirm { tool, summary, .. }, true) => {
            format!("Confirm tool '{tool}': {summary}\n  [a]llow once, [r]emember & allow, [d]eny")
        }
        (PauseKind::SkillSave { suggested_name, .. }, true) => {
            format!("Save this as a skill named '{suggested_name}'?\n  [a]llow once, [d]eny")
        }
        (PauseKind::DiffScrub { .. }, _) => "Review file edits before writing them".to_string(),
        (
            PauseKind::InputRewriteApproval {
                extension_id,
                before,
                after,
            },
            false,
        ) => {
            format!(
                "Extension '{extension_id}' proposes input rewrite:\n  before: {before}\n  after:  {after}"
            )
        }
        (
            PauseKind::InputRewriteApproval {
                extension_id,
                before,
                after,
            },
            true,
        ) => {
            format!(
                "Extension '{extension_id}' proposes input rewrite:\n  before: {before}\n  after:  {after}\n  [a]llow once, [d]eny"
            )
        }
    }
}

pub fn resume_label(resume: &AgentResume) -> &'static str {
    match resume {
        AgentResume::Allow => "allowed (once)",
        AgentResume::AllowAndRemember => "allowed (remembered)",
        AgentResume::Deny => "denied",
        AgentResume::DenyWithReason(_) => "denied",
        AgentResume::Custom(_) => "responded",
        AgentResume::AcceptHunks(_) => "applied",
    }
}

fn resume_for_key(pause: &AgentPause, key: TuiKeyEvent) -> Option<AgentResume> {
    if !pause.options.is_empty() {
        return match key.code {
            TuiKeyCode::Esc => Some(AgentResume::Deny),
            TuiKeyCode::Char(c) => pause
                .options
                .iter()
                .find(|option| option.key.eq_ignore_ascii_case(&c))
                .map(|option| option.resume.clone()),
            _ => None,
        };
    }

    match key.code {
        TuiKeyCode::Char('a') | TuiKeyCode::Char('A') => Some(AgentResume::Allow),
        TuiKeyCode::Char('r') | TuiKeyCode::Char('R') => Some(AgentResume::AllowAndRemember),
        TuiKeyCode::Char('d') | TuiKeyCode::Char('D') | TuiKeyCode::Esc => Some(AgentResume::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::pause::{OptionKind, PauseOption};
    use tokio::sync::oneshot;

    use super::*;
    use crate::pause::PauseKind;
    use crate::tui::input::TuiKeyModifiers;

    fn test_pause(options: Vec<PauseOption>) -> AgentPause {
        let (reply, _rx) = oneshot::channel();
        AgentPause {
            kind: PauseKind::SafetyApproval {
                tool: "shell".to_string(),
                command: "cargo test".to_string(),
                reason: "test".to_string(),
            },
            reply,
            options,
        }
    }

    fn key(code: TuiKeyCode) -> TuiKeyEvent {
        TuiKeyEvent::new(code, TuiKeyModifiers::NONE)
    }

    #[test]
    fn legacy_prompt_maps_allow_remember_and_deny_keys() {
        let mut state = PausePromptState::new("summary".to_string(), test_pause(Vec::new()));

        let outcome = state.handle_key(key(TuiKeyCode::Char('r')));

        assert!(outcome.pause.is_some());
        assert!(matches!(
            outcome.resume,
            Some(AgentResume::AllowAndRemember)
        ));
        assert_eq!(outcome.label, Some("allowed (remembered)"));
    }

    #[test]
    fn option_prompt_uses_declared_case_insensitive_key_mapping() {
        let mut state = PausePromptState::new(
            "summary".to_string(),
            test_pause(vec![PauseOption {
                key: 'y',
                label: "yes".to_string(),
                kind: OptionKind::Primary,
                resume: AgentResume::Custom("yes".to_string()),
            }]),
        );

        let outcome = state.handle_key(key(TuiKeyCode::Char('Y')));

        assert!(outcome.pause.is_some());
        assert!(matches!(
            outcome.resume,
            Some(AgentResume::Custom(value)) if value == "yes"
        ));
        assert_eq!(outcome.label, Some("responded"));
    }

    #[test]
    fn option_prompt_esc_denies() {
        let mut state = PausePromptState::new(
            "summary".to_string(),
            test_pause(vec![PauseOption {
                key: 'y',
                label: "yes".to_string(),
                kind: OptionKind::Primary,
                resume: AgentResume::Allow,
            }]),
        );

        let outcome = state.handle_key(key(TuiKeyCode::Esc));

        assert!(outcome.pause.is_some());
        assert!(matches!(outcome.resume, Some(AgentResume::Deny)));
        assert_eq!(outcome.label, Some("denied"));
    }

    #[test]
    fn unrelated_key_keeps_pause_mounted() {
        let mut state = PausePromptState::new("summary".to_string(), test_pause(Vec::new()));

        let outcome = state.handle_key(key(TuiKeyCode::Char('x')));

        assert!(outcome.pause.is_none());
        assert!(outcome.resume.is_none());
        assert!(state.pause().is_some());
    }
}
