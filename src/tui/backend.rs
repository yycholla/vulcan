use crate::agent::Agent;
use crate::agent::ModelSelection as Selection;
use crate::client::Client;
use crate::config::Config;
use crate::hooks::InputDecision;
use crate::memory::{SearchHit, SessionSummary};
use crate::provider::catalog::{ModelFeatures, ModelInfo, Pricing};
use crate::provider::{Message, StreamEvent};
use crate::tools::EditDiffSink;
use crate::trust::TrustLevel;
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub enum TuiBackend {
    Direct(Mutex<Agent>),
    #[cfg(feature = "daemon")]
    Daemon {
        client: Arc<Client>,
        session_id: Mutex<String>,
        active_model: Mutex<String>,
        active_profile: Mutex<Option<String>>,
        max_context: Mutex<usize>,
    },
}

impl TuiBackend {
    pub async fn diff_sink(&self) -> Option<EditDiffSink> {
        match self {
            Self::Direct(agent) => Some(agent.lock().await.diff_sink().clone()),
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => None,
        }
    }

    pub async fn pricing(&self) -> Option<Pricing> {
        match self {
            Self::Direct(agent) => agent.lock().await.pricing().cloned(),
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => None,
        }
    }

    pub async fn active_model(&self) -> String {
        match self {
            Self::Direct(agent) => agent.lock().await.active_model().to_string(),
            #[cfg(feature = "daemon")]
            Self::Daemon { active_model, .. } => active_model.lock().await.clone(),
        }
    }

    pub async fn max_context(&self) -> usize {
        match self {
            Self::Direct(agent) => agent.lock().await.max_context(),
            #[cfg(feature = "daemon")]
            Self::Daemon { max_context, .. } => *max_context.lock().await,
        }
    }

    pub async fn active_profile(&self) -> Option<String> {
        match self {
            Self::Direct(agent) => agent.lock().await.active_profile().map(str::to_string),
            #[cfg(feature = "daemon")]
            Self::Daemon { active_profile, .. } => active_profile.lock().await.clone(),
        }
    }

    pub async fn workspace_trust_level(&self) -> TrustLevel {
        match self {
            Self::Direct(agent) => agent.lock().await.trust_profile().level,
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => TrustLevel::Trusted, // out of scope
        }
    }

    pub async fn session_id(&self) -> String {
        match self {
            Self::Direct(agent) => agent.lock().await.session_id().to_string(),
            #[cfg(feature = "daemon")]
            Self::Daemon { session_id, .. } => session_id.lock().await.clone(),
        }
    }

    pub async fn start_session(&self) {
        match self {
            Self::Direct(agent) => agent.lock().await.start_session().await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                if let Ok(resp) = client.call("session.create", serde_json::json!({})).await {
                    if let Some(id) = resp.get("session_id").and_then(|v| v.as_str()) {
                        *session_id.lock().await = id.to_string();
                    }
                }
            }
        }
    }

    pub async fn continue_last_session(&self) -> Result<()> {
        match self {
            Self::Direct(agent) => agent.lock().await.continue_last_session().await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let resp = client
                    .call("session.list_saved", serde_json::json!({ "limit": 1 }))
                    .await?;
                let sessions = parse_session_summaries(&resp);
                let Some(summary) = sessions.first() else {
                    anyhow::bail!("no sessions found");
                };
                client
                    .call_at_session(&summary.id, "session.resume", serde_json::json!({}))
                    .await?;
                *session_id.lock().await = summary.id.clone();
                if let Err(e) = self.sync_status().await {
                    tracing::debug!("daemon status sync after continue failed: {e}");
                }
                Ok(())
            }
        }
    }

    pub async fn resume_session(&self, id: &str) -> Result<()> {
        match self {
            Self::Direct(agent) => agent.lock().await.resume_session(id).await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                client
                    .call_at_session(id, "session.resume", serde_json::json!({}))
                    .await?;
                *session_id.lock().await = id.to_string();
                if let Err(e) = self.sync_status().await {
                    tracing::debug!("daemon status sync after resume failed: {e}");
                }
                Ok(())
            }
        }
    }

    pub async fn list_sessions(&self, limit: usize) -> Vec<SessionSummary> {
        match self {
            Self::Direct(agent) => agent
                .lock()
                .await
                .memory()
                .list_sessions(limit)
                .await
                .unwrap_or_default(),
            #[cfg(feature = "daemon")]
            Self::Daemon { client, .. } => {
                if let Ok(resp) = client
                    .call("session.list_saved", serde_json::json!({ "limit": limit }))
                    .await
                {
                    return parse_session_summaries(&resp);
                }
                vec![]
            }
        }
    }

    pub async fn get_messages(&self) -> Vec<Message> {
        match self {
            Self::Direct(agent) => agent.lock().await.get_messages().to_vec(),
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let sid = session_id.lock().await.clone();
                if let Ok(resp) = client
                    .call_at_session(&sid, "session.history", serde_json::json!({}))
                    .await
                {
                    if let Some(history) = resp.get("messages") {
                        if let Ok(m) = serde_json::from_value(history.clone()) {
                            return m;
                        }
                    }
                }
                vec![]
            }
        }
    }

    pub async fn apply_on_input(&self, input: &str) -> InputDecision {
        match self {
            Self::Direct(agent) => agent.lock().await.apply_on_input(input).await,
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => InputDecision::Continue,
        }
    }

    pub async fn cancel(&self) {
        match self {
            Self::Direct(agent) => agent.lock().await.cancel_current_turn(),
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let sid = session_id.lock().await.clone();
                let _ = client
                    .call_at_session(&sid, "prompt.cancel", serde_json::json!({}))
                    .await;
            }
        }
    }

    pub async fn switch_model(&self, model: &str) -> Result<Selection> {
        match self {
            Self::Direct(agent) => agent.lock().await.switch_model(model).await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client,
                session_id,
                active_model,
                max_context,
                ..
            } => {
                let sid = session_id.lock().await.clone();
                let resp = client
                    .call_at_session(
                        &sid,
                        "agent.switch_model",
                        serde_json::json!({ "model": model }),
                    )
                    .await?;
                let sel = parse_model_selection(&resp)?;
                *active_model.lock().await = sel.model.id.clone();
                *max_context.lock().await = sel.max_context;
                Ok(sel)
            }
        }
    }

    pub async fn switch_provider(
        &self,
        provider: Option<&str>,
        config: &Config,
    ) -> Result<Selection> {
        match self {
            Self::Direct(agent) => agent.lock().await.switch_provider(provider, config).await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client,
                session_id,
                active_model,
                active_profile,
                max_context,
            } => {
                let sid = session_id.lock().await.clone();
                let resp = client
                    .call_at_session(
                        &sid,
                        "agent.switch_provider",
                        serde_json::json!({ "profile": provider }),
                    )
                    .await?;
                let sel = parse_model_selection(&resp)?;
                *active_model.lock().await = sel.model.id.clone();
                *active_profile.lock().await = provider.map(str::to_string);
                *max_context.lock().await = sel.max_context;
                Ok(sel)
            }
        }
    }

    pub async fn switch_provider_model(
        &self,
        provider: Option<&str>,
        config: &Config,
        model: &str,
    ) -> Result<Selection> {
        match self {
            Self::Direct(agent) => {
                agent
                    .lock()
                    .await
                    .switch_provider_model(provider, config, model)
                    .await
            }
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client,
                session_id,
                active_model,
                active_profile,
                max_context,
            } => {
                let sid = session_id.lock().await.clone();
                let resp = client
                    .call_at_session(
                        &sid,
                        "agent.switch_provider_model",
                        serde_json::json!({ "profile": provider, "model": model }),
                    )
                    .await?;
                let sel = parse_model_selection(&resp)?;
                *active_model.lock().await = sel.model.id.clone();
                *active_profile.lock().await = provider.map(str::to_string);
                *max_context.lock().await = sel.max_context;
                Ok(sel)
            }
        }
    }

    pub async fn sync_status(&self) -> Result<()> {
        match self {
            Self::Direct(_) => Ok(()),
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client,
                session_id,
                active_model,
                active_profile,
                max_context,
            } => {
                let sid = session_id.lock().await.clone();
                let resp = client
                    .call_at_session(&sid, "agent.status", serde_json::json!({}))
                    .await?;
                if let Some(m) = resp.get("model").and_then(|v| v.as_str()) {
                    *active_model.lock().await = m.to_string();
                }
                if let Some(p) = resp.get("provider").and_then(|v| v.as_str()) {
                    *active_profile.lock().await = Some(p.to_string());
                }
                if let Some(ctx) = resp.get("max_context").and_then(|v| v.as_u64()) {
                    *max_context.lock().await = ctx as usize;
                }
                Ok(())
            }
        }
    }

    pub async fn available_models(&self) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
        match self {
            Self::Direct(agent) => agent.lock().await.available_models().await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let sid = session_id.lock().await.clone();
                let resp = client
                    .call_at_session(&sid, "agent.list_models", serde_json::json!({}))
                    .await?;
                if let Some(models_val) = resp.get("models") {
                    if let Some(arr) = models_val.as_array() {
                        let mut models = Vec::new();
                        for v in arr {
                            let id = v
                                .get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let display_name = v
                                .get("display_name")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let context_length =
                                v.get("context_length")
                                    .and_then(|x| x.as_u64())
                                    .unwrap_or_default() as usize;
                            models.push(crate::provider::catalog::ModelInfo {
                                id,
                                display_name,
                                context_length,
                                pricing: None,
                                features: crate::provider::catalog::ModelFeatures::default(),
                                top_provider: None,
                            });
                        }
                        return Ok(models);
                    }
                }
                Ok(vec![])
            }
        }
    }

    pub async fn load_history(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
        match self {
            Self::Direct(agent) => agent.lock().await.memory().load_history(session_id).await,
            #[cfg(feature = "daemon")]
            Self::Daemon { client, .. } => {
                let resp = client
                    .call_at_session(session_id, "session.history", serde_json::json!({}))
                    .await?;
                if let Some(history) = resp.get("messages") {
                    let msgs: Vec<Message> = serde_json::from_value(history.clone())?;
                    Ok(Some(msgs))
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub async fn search_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::memory::SearchHit>> {
        match self {
            Self::Direct(agent) => {
                agent
                    .lock()
                    .await
                    .memory()
                    .search_messages(query, limit)
                    .await
            }
            #[cfg(feature = "daemon")]
            Self::Daemon { client, .. } => {
                let resp = client
                    .call(
                        "session.search",
                        serde_json::json!({ "query": query, "limit": limit }),
                    )
                    .await?;
                Ok(parse_search_hits(&resp))
            }
        }
    }

    pub async fn skills(&self) -> Vec<crate::skills::Skill> {
        match self {
            Self::Direct(agent) => agent.lock().await.skills().to_vec(),
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => vec![], // Stubbed gracefully
        }
    }

    pub async fn orchestration(&self) -> Arc<crate::orchestration::OrchestrationStore> {
        match self {
            Self::Direct(agent) => agent.lock().await.orchestration().clone(),
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => Arc::new(crate::orchestration::OrchestrationStore::new()),
        }
    }

    pub async fn trust_profile(&self) -> crate::trust::TrustProfile {
        match self {
            Self::Direct(agent) => agent.lock().await.trust_profile().clone(),
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => crate::trust::TrustProfile::for_level_with_reason(
                crate::trust::TrustLevel::Trusted,
                "Daemon default",
            ),
        }
    }

    pub async fn restore_persisted_provider(&self, config: &Config) -> Result<()> {
        match self {
            Self::Direct(agent) => agent.lock().await.restore_persisted_provider(config).await,
            #[cfg(feature = "daemon")]
            Self::Daemon { .. } => Ok(()), // Managed on daemon side
        }
    }

    pub async fn run_prompt_stream_with_cancel(
        &self,
        input: &str,
        tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<String> {
        match self {
            Self::Direct(agent) => {
                agent
                    .lock()
                    .await
                    .run_prompt_stream_with_cancel(input, tx, cancel)
                    .await
            }
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let sid = session_id.lock().await.clone();
                let stream_frames = client
                    .call_stream_at_session(&sid, "prompt.stream", prompt_stream_params(input))
                    .await?;
                let mut rx = stream_frames.frames;
                let rx_done = stream_frames.done;

                let cancel_clone = cancel.clone();
                let client_clone = client.clone();
                let sid_clone = session_id.lock().await.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancel_clone.cancelled() => {
                            let _ = client_clone.call_at_session(&sid_clone, "prompt.cancel", serde_json::json!({})).await;
                        }
                    }
                });

                while let Some(frame) = rx.recv().await {
                    let ev = if frame.stream == "text" {
                        StreamEvent::Text(
                            frame
                                .data
                                .get("chunk")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        )
                    } else if frame.stream == "reasoning" {
                        StreamEvent::Reasoning(
                            frame
                                .data
                                .get("chunk")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        )
                    } else if frame.stream == "tool_call_start" {
                        StreamEvent::ToolCallStart {
                            id: frame
                                .data
                                .get("tool_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: frame
                                .data
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            args_summary: frame
                                .data
                                .get("args_summary")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        }
                    } else if frame.stream == "tool_call_end" {
                        StreamEvent::ToolCallEnd {
                            id: frame
                                .data
                                .get("tool_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: frame
                                .data
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            ok: frame
                                .data
                                .get("ok")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true),
                            output_preview: frame
                                .data
                                .get("output_preview")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            details: frame.data.get("details").cloned(),
                            result_meta: frame
                                .data
                                .get("result_meta")
                                .and_then(|v| serde_json::from_value(v.clone()).ok()),
                            elided_lines: frame
                                .data
                                .get("elided_lines")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize,
                            elapsed_ms: frame
                                .data
                                .get("elapsed_ms")
                                .and_then(|v| v.as_u64())
                                .unwrap_or_default(),
                        }
                    } else if frame.stream == "error" {
                        StreamEvent::Error(
                            frame
                                .data
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        )
                    } else {
                        continue;
                    };
                    let _ = tx.send(ev).await;
                }

                match rx_done.await {
                    Ok(Ok(resp)) => {
                        if let Some(err) = resp.error {
                            Err(anyhow::anyhow!("Daemon error: {}", err.message))
                        } else {
                            let text = resp
                                .result
                                .as_ref()
                                .and_then(|v| v.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            Ok(text)
                        }
                    }
                    Ok(Err(e)) => Err(anyhow::anyhow!("Daemon response error: {}", e)),
                    Err(e) => Err(anyhow::anyhow!("Daemon stream failed: {}", e)),
                }
            }
        }
    }

    pub async fn end_session(&self) {
        match self {
            Self::Direct(agent) => agent.lock().await.end_session().await,
            #[cfg(feature = "daemon")]
            Self::Daemon {
                client, session_id, ..
            } => {
                let sid = session_id.lock().await.clone();
                let _ = client
                    .call_at_session(&sid, "session.end", serde_json::json!({}))
                    .await;
            }
        }
    }
}

#[cfg(feature = "daemon")]
fn prompt_stream_params(input: &str) -> Value {
    serde_json::json!({ "text": input })
}

#[cfg(feature = "daemon")]
fn parse_session_summaries(resp: &Value) -> Vec<SessionSummary> {
    resp.get("sessions")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

#[cfg(feature = "daemon")]
fn parse_search_hits(resp: &Value) -> Vec<SearchHit> {
    let Some(hits) = resp.get("hits").and_then(|v| v.as_array()) else {
        return vec![];
    };
    hits.iter()
        .map(|h| SearchHit {
            session_id: h
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            position: h
                .get("position")
                .and_then(|v| v.as_i64())
                .unwrap_or_default(),
            role: h
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            content: h
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            created_at: h
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or_default(),
            score: h.get("score").and_then(|v| v.as_f64()).unwrap_or_default(),
        })
        .collect()
}

#[cfg(feature = "daemon")]
fn parse_model_selection(resp: &Value) -> Result<Selection> {
    let Some(id) = resp.get("id").and_then(|v| v.as_str()) else {
        anyhow::bail!("daemon switch_model response missing id");
    };
    let pricing: Option<Pricing> = resp
        .get("pricing")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Ok(Selection {
        model: ModelInfo {
            id: id.to_string(),
            display_name: resp
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string(),
            context_length: resp
                .get("context_length")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as usize,
            pricing: pricing.clone(),
            features: ModelFeatures::default(),
            top_provider: None,
        },
        max_context: resp
            .get("max_context")
            .and_then(|v| v.as_u64())
            .unwrap_or_default() as usize,
        pricing,
    })
}

#[cfg(all(test, feature = "daemon"))]
mod tests {
    use super::*;

    #[test]
    fn prompt_stream_params_use_daemon_text_key() {
        assert_eq!(
            prompt_stream_params("hello"),
            serde_json::json!({ "text": "hello" })
        );
    }

    #[test]
    fn parse_search_hits_reads_daemon_hits_key() {
        let hits = parse_search_hits(&serde_json::json!({
            "hits": [{
                "session_id": "s1",
                "position": 2,
                "role": "user",
                "content": "needle",
                "created_at": 3,
                "score": 0.5
            }]
        }));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].content, "needle");
    }

    #[test]
    fn parse_model_selection_reads_daemon_switch_shape() {
        let selection = parse_model_selection(&serde_json::json!({
            "id": "model-a",
            "display_name": "Model A",
            "context_length": 32000,
            "max_context": 16000
        }))
        .unwrap();
        assert_eq!(selection.model.id, "model-a");
        assert_eq!(selection.max_context, 16000);
    }
}
