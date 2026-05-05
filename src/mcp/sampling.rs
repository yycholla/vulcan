use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct McpSamplingPolicy {
    pub enabled: bool,
    pub max_depth: u32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSamplingRequest {
    pub server_id: String,
    pub depth: u32,
    pub requested_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum McpSamplingDecision {
    Allow { max_tokens: u32 },
    Deny { reason: String },
}

impl McpSamplingDecision {
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPolicyAuditRecord {
    pub server_id: String,
    pub request_kind: String,
    pub decision: McpSamplingDecision,
}

impl McpSamplingPolicy {
    pub fn decide(
        &self,
        request: &McpSamplingRequest,
    ) -> (McpSamplingDecision, McpPolicyAuditRecord) {
        let decision = if !self.enabled {
            McpSamplingDecision::Deny {
                reason: "MCP sampling is disabled by default".into(),
            }
        } else if request.depth > self.max_depth {
            McpSamplingDecision::Deny {
                reason: format!(
                    "MCP sampling depth {} exceeds configured max {}",
                    request.depth, self.max_depth
                ),
            }
        } else if request.requested_tokens > self.max_tokens {
            McpSamplingDecision::Deny {
                reason: format!(
                    "MCP sampling token request {} exceeds configured max {}",
                    request.requested_tokens, self.max_tokens
                ),
            }
        } else {
            McpSamplingDecision::Allow {
                max_tokens: request.requested_tokens,
            }
        };
        let audit = McpPolicyAuditRecord {
            server_id: request.server_id.clone(),
            request_kind: "sampling/createMessage".into(),
            decision: decision.clone(),
        };
        (decision, audit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_denied_by_default() {
        let request = McpSamplingRequest {
            server_id: "fake".into(),
            depth: 0,
            requested_tokens: 1,
        };
        let (decision, audit) = McpSamplingPolicy::default().decide(&request);
        assert!(!decision.allowed());
        assert_eq!(audit.server_id, "fake");
        assert_eq!(audit.request_kind, "sampling/createMessage");
    }

    #[test]
    fn enabled_sampling_enforces_depth_and_token_budget() {
        let policy = McpSamplingPolicy {
            enabled: true,
            max_depth: 1,
            max_tokens: 128,
        };
        let too_deep = McpSamplingRequest {
            server_id: "fake".into(),
            depth: 2,
            requested_tokens: 64,
        };
        assert!(matches!(
            policy.decide(&too_deep).0,
            McpSamplingDecision::Deny { .. }
        ));

        let too_many_tokens = McpSamplingRequest {
            server_id: "fake".into(),
            depth: 1,
            requested_tokens: 256,
        };
        assert!(matches!(
            policy.decide(&too_many_tokens).0,
            McpSamplingDecision::Deny { .. }
        ));

        let allowed = McpSamplingRequest {
            server_id: "fake".into(),
            depth: 1,
            requested_tokens: 64,
        };
        assert_eq!(
            policy.decide(&allowed).0,
            McpSamplingDecision::Allow { max_tokens: 64 }
        );
    }
}
