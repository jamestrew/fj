//! Provider abstraction for `fj agent` (review / fix / explain).
//!
//! There is no required backend yet. Configuration is env-var based:
//! `FJ_AGENT_PROVIDER`, `FJ_AGENT_API_KEY`, `FJ_AGENT_MODEL`. With nothing
//! set, the Null provider is used and any request reports a clear
//! "agent backend not configured" error. A built-in `mock` provider exists
//! for development and tests. The Fjord token is NEVER reused for AI.

use anyhow::{bail, Result};

/// What an agent command asks the provider to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTask {
    Review,
    Fix,
    Explain,
}

impl AgentTask {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentTask::Review => "review",
            AgentTask::Fix => "fix",
            AgentTask::Explain => "explain",
        }
    }
}

/// A request to the provider: the task plus the free-form context to act on
/// (a diff, a file, a question) and the configured model.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub task: AgentTask,
    pub input: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentResponse {
    pub provider: String,
    pub model: Option<String>,
    pub output: String,
}

pub trait AgentProvider {
    fn complete(&self, req: &AgentRequest) -> Result<AgentResponse>;
}

/// Agent configuration read from the environment. Empty vars are treated as
/// unset. Note: this intentionally does NOT read any Fjord/Forgejo token.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentConfig {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

impl AgentConfig {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            provider: var("FJ_AGENT_PROVIDER"),
            api_key: var("FJ_AGENT_API_KEY"),
            model: var("FJ_AGENT_MODEL"),
        }
    }
}

/// Default provider when nothing is configured: every request fails with an
/// actionable message rather than silently doing nothing.
struct NullProvider;

impl AgentProvider for NullProvider {
    fn complete(&self, _req: &AgentRequest) -> Result<AgentResponse> {
        bail!(
            "agent backend not configured. Set FJ_AGENT_PROVIDER (and \
             FJ_AGENT_API_KEY / FJ_AGENT_MODEL) to enable AI features. \
             The Fjord token is never reused for this."
        );
    }
}

/// Deterministic provider for development and tests: echoes the request back.
struct MockProvider;

impl AgentProvider for MockProvider {
    fn complete(&self, req: &AgentRequest) -> Result<AgentResponse> {
        Ok(AgentResponse {
            provider: "mock".to_string(),
            model: req.model.clone(),
            output: format!("[mock {} output]\n{}", req.task.as_str(), req.input),
        })
    }
}

/// Resolve a provider from config. Unset → Null (requests report
/// "not configured"). `mock` → the built-in mock. Any other value is rejected
/// with a clear message — real providers are TBD.
pub fn resolve(cfg: &AgentConfig) -> Result<Box<dyn AgentProvider>> {
    match cfg.provider.as_deref() {
        None => Ok(Box::new(NullProvider)),
        Some("mock") => Ok(Box::new(MockProvider)),
        Some(other) => bail!(
            "unknown agent provider '{other}'. Supported: mock (real providers are TBD). \
             Unset FJ_AGENT_PROVIDER to disable agent features."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(task: AgentTask, model: Option<&str>) -> AgentRequest {
        AgentRequest {
            task,
            input: "some context".into(),
            model: model.map(str::to_string),
        }
    }

    #[test]
    fn task_as_str() {
        assert_eq!(AgentTask::Review.as_str(), "review");
        assert_eq!(AgentTask::Fix.as_str(), "fix");
        assert_eq!(AgentTask::Explain.as_str(), "explain");
    }

    #[test]
    fn unset_resolves_to_null_which_reports_not_configured() {
        let p = resolve(&AgentConfig::default()).unwrap();
        let err = p.complete(&req(AgentTask::Explain, None)).unwrap_err();
        assert!(err.to_string().contains("not configured"));
        // And it never points at the Fjord token.
        assert!(err.to_string().contains("never reused"));
    }

    #[test]
    fn mock_provider_echoes_the_request() {
        let cfg = AgentConfig {
            provider: Some("mock".into()),
            ..Default::default()
        };
        let p = resolve(&cfg).unwrap();
        let resp = p.complete(&req(AgentTask::Review, Some("m1"))).unwrap();
        assert_eq!(resp.provider, "mock");
        assert_eq!(resp.model.as_deref(), Some("m1"));
        assert!(resp.output.contains("[mock review output]"));
        assert!(resp.output.contains("some context"));
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let cfg = AgentConfig {
            provider: Some("openai".into()),
            ..Default::default()
        };
        match resolve(&cfg) {
            Ok(_) => panic!("unknown provider should be rejected"),
            Err(e) => assert!(e.to_string().contains("unknown agent provider 'openai'")),
        }
    }
}
