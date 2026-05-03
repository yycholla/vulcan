use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::config::Config;
use crate::pause::{self, AgentResume};

use super::KeyEv;
use super::diff_scrubber::DiffScrubberAction;
use super::input::{TuiInputEvent, TuiKeyCode};
use super::model_picker::ModelPickerOutcome;
use super::provider_picker::ProviderPickerOutcome;
use super::state::{AppState, ChatMessage, ChatRole};

pub(super) async fn drive_diff_scrubber(
    app: &mut AppState,
    key_rx: &mut mpsc::UnboundedReceiver<KeyEv>,
) {
    match key_rx.recv().await {
        Some(KeyEv::Press(TuiInputEvent::Key(key))) => {
            let outcome = app.handle_diff_scrubber_key(key);
            match outcome.action {
                DiffScrubberAction::Accept(indices) => {
                    if let Some(p) = outcome.pause {
                        let _ = p.reply.send(AgentResume::AcceptHunks(indices.clone()));
                    }
                    let label = if indices.is_empty() {
                        "no hunks accepted - file unchanged"
                    } else if indices.len() == outcome.total {
                        "all hunks accepted"
                    } else {
                        "subset of hunks accepted"
                    };
                    app.messages.push(ChatMessage {
                        role: ChatRole::System,
                        content: format!(
                            "▶  edit_file resumed — {} ({}/{})",
                            label,
                            indices.len(),
                            outcome.total
                        ),
                        ..Default::default()
                    });
                }
                DiffScrubberAction::Cancel => {
                    if let Some(p) = outcome.pause {
                        let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
                    }
                    app.messages.push(ChatMessage {
                        role: ChatRole::System,
                        content: "▶  edit_file cancelled — file unchanged".into(),
                        ..Default::default()
                    });
                }
                DiffScrubberAction::Continue => {}
            }
        }
        Some(KeyEv::Press(_)) => {}
        Some(KeyEv::Error(e)) => {
            tracing::error!("Terminal input error (diff scrubber): {e}");
            if let Some(p) = app.close_diff_scrubber() {
                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
            }
        }
        None => {
            if let Some(p) = app.close_diff_scrubber() {
                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
            }
        }
    }
}

pub(super) async fn drive_model_picker(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<KeyEv>,
) {
    match key_rx.recv().await {
        Some(KeyEv::Press(TuiInputEvent::Key(key))) => match app.handle_model_picker_key(key) {
            ModelPickerOutcome::Commit {
                profile,
                model_id: id,
            } => {
                let result = {
                    let mut a = agent.lock().await;
                    let active = a.active_profile().map(str::to_string);
                    if active != profile {
                        a.switch_provider_model(profile.as_deref(), config, &id)
                            .await
                    } else {
                        a.switch_model(&id).await
                    }
                };
                match result {
                    Ok(selection) => {
                        app.model_label = selection.model.id.clone();
                        app.token_max = selection.max_context as u32;
                        app.pricing = selection.pricing;
                        app.provider_label = profile.clone();
                        let label = profile.as_deref().unwrap_or("default");
                        app.messages.push(ChatMessage {
                            role: ChatRole::System,
                            content: format!(
                                "Switched to {label} · {} · context {}",
                                app.model_label,
                                crate::tui::state::format_thousands(app.token_max),
                            ),
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        app.messages.push(ChatMessage {
                            role: ChatRole::System,
                            content: format!("Model switch failed: {e}"),
                            ..Default::default()
                        });
                    }
                }
            }
            ModelPickerOutcome::Close | ModelPickerOutcome::Continue => {}
        },
        Some(KeyEv::Press(_)) => {}
        Some(KeyEv::Error(e)) => {
            tracing::error!("Terminal input error (model picker): {e}");
            app.close_model_picker();
        }
        None => {
            app.close_model_picker();
        }
    }
}

pub(super) async fn drive_provider_picker(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<KeyEv>,
) {
    match key_rx.recv().await {
        Some(KeyEv::Press(TuiInputEvent::Key(key))) => match app.handle_provider_picker_key(key) {
            ProviderPickerOutcome::Commit(picked) => {
                let target: Option<&str> = picked.name.as_deref();
                let result = {
                    let mut a = agent.lock().await;
                    a.switch_provider(target, config).await
                };
                match result {
                    Ok(selection) => {
                        app.model_label = selection.model.id.clone();
                        app.token_max = selection.max_context as u32;
                        app.pricing = selection.pricing;
                        app.provider_label = target.map(str::to_string);
                        let label = target.unwrap_or("default");
                        app.messages.push(ChatMessage {
                            role: ChatRole::System,
                            content: format!(
                                "Provider switched to {label} · {} · context {}",
                                app.model_label,
                                crate::tui::state::format_thousands(app.token_max),
                            ),
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        app.messages.push(ChatMessage {
                            role: ChatRole::System,
                            content: format!("Provider switch failed: {e}"),
                            ..Default::default()
                        });
                    }
                }
            }
            ProviderPickerOutcome::Close | ProviderPickerOutcome::Continue => {}
        },
        Some(KeyEv::Press(_)) => {}
        Some(KeyEv::Error(e)) => {
            tracing::error!("Terminal input error (provider picker): {e}");
            app.close_provider_picker();
        }
        None => {
            app.close_provider_picker();
        }
    }
}

pub(super) async fn drive_session_picker(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<KeyEv>,
    pause_rx: &mut pause::PauseReceiver,
    pause_tx: &pause::PauseSender,
) {
    tokio::select! {
        ev = key_rx.recv() => {
            match ev {
                Some(KeyEv::Press(TuiInputEvent::Key(key))) => {
                    handle_session_picker_key(app, agent, config, key).await;
                }
                Some(KeyEv::Press(_)) => {}
                Some(KeyEv::Error(e)) => {
                    tracing::error!("Terminal input error (picker): {e}");
                    app.close_session_picker();
                }
                None => {
                    app.close_session_picker();
                }
            }
        }
        pause = pause_rx.recv() => {
            if let Some(p) = pause {
                app.close_session_picker();
                if let Err(e) = pause_tx.send(p).await {
                    tracing::warn!("failed to re-route pause: {e}");
                }
            }
        }
    }
}

async fn handle_session_picker_key(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    config: &Config,
    key: super::input::TuiKeyEvent,
) {
    match key.code {
        TuiKeyCode::Up | TuiKeyCode::Char('k') => {
            app.session_picker_up();
        }
        TuiKeyCode::Down | TuiKeyCode::Char('j') => {
            app.session_picker_down();
        }
        TuiKeyCode::Enter => {
            resume_selected_session(app, agent, config).await;
        }
        TuiKeyCode::Esc => {
            app.close_session_picker();
            app.messages.push(ChatMessage {
                role: ChatRole::System,
                content: "Starting a new session — use /search to find past conversations.".into(),
                ..Default::default()
            });
        }
        _ => {}
    }
}

async fn resume_selected_session(app: &mut AppState, agent: &Arc<Mutex<Agent>>, config: &Config) {
    let idx = app
        .session_picker_selection()
        .min(app.sessions.len().saturating_sub(1));
    let Some(picked) = app.sessions.get(idx).map(|session| session.id.clone()) else {
        app.close_session_picker();
        return;
    };
    let current = app.active_session_id.clone().unwrap_or_default();

    if picked == current {
        app.close_session_picker();
        return;
    }

    let (note, should_hydrate) = {
        let mut a = agent.lock().await;
        match a.resume_session(&picked) {
            Ok(()) => {
                if let Err(e) = a.restore_persisted_provider(config).await {
                    tracing::warn!("provider restore failed during picker resume: {e}");
                }
                (Some(format!("Resumed session {}", short_id(&picked))), true)
            }
            Err(e) => (Some(format!("Could not resume session: {e}")), false),
        }
    };
    app.close_session_picker();
    if should_hydrate {
        let a = agent.lock().await;
        app.model_label = a.active_model().to_string();
        app.token_max = a.max_context() as u32;
        app.pricing = a.pricing().cloned();
        app.provider_label = a.active_profile().map(str::to_string);
    }
    if let Some(n) = note {
        app.messages.push(ChatMessage {
            role: ChatRole::System,
            content: n,
            ..Default::default()
        });
    }
    if should_hydrate {
        let history = {
            let a = agent.lock().await;
            a.memory().load_history(&picked).ok().flatten()
        };
        if let Some(msgs) = history {
            for msg in msgs {
                use crate::provider::Message;
                match msg {
                    Message::User { content } => {
                        app.messages.push(ChatMessage::new(ChatRole::User, content));
                    }
                    Message::System { content } => {
                        app.messages
                            .push(ChatMessage::new(ChatRole::System, content));
                    }
                    Message::Assistant {
                        content,
                        reasoning_content,
                        ..
                    } => {
                        app.messages.push(ChatMessage {
                            role: ChatRole::Agent,
                            content: content.unwrap_or_default(),
                            reasoning: reasoning_content.unwrap_or_default(),
                            segments: Vec::new(),
                            render_version: 0,
                        });
                    }
                    Message::Tool { .. } => {}
                }
            }
        }
    }
    super::events::refresh_sessions(agent, app).await;
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
