//! Engine resolution compatibility shim (v2 → v3).
//!
//! The v2 supervisor imported `EngineLaunch`, `lower_engine()`, and
//! `ensure_no_engine_conflict()` from `ark-scene-v2-archive::engine`.
//! V3 replaces those with [`ext::acp::AcpConfig`] + [`ext::acp::find_acp_extension`].
//!
//! This module re-exposes the v2-shaped API backed by the v3 implementation
//! so the supervisor can migrate incrementally.

use crate::error::SceneError;
use crate::ext::acp::{find_acp_extension, AcpConfig};
use crate::ext::registry::ExtensionRegistry;

/// V2-compatible engine launch descriptor.
///
/// Maps 1:1 from the v3 [`AcpConfig`] + its inner [`LaunchSpec`]:
/// `command` = `launch.command.value`, `args` = `launch.args[*].value`.
#[derive(Debug, Clone)]
pub struct EngineLaunch {
    /// Executable name or path (`argv[0]`).
    pub command: String,
    /// Arguments passed to the subprocess (`argv[1..]`).
    pub args: Vec<String>,
    /// Name of the extension that provides the engine.
    pub extension_name: String,
}

impl From<AcpConfig> for EngineLaunch {
    fn from(cfg: AcpConfig) -> Self {
        Self {
            command: cfg.launch.command.value.clone(),
            args: cfg
                .launch
                .args
                .iter()
                .map(|a| a.value.clone())
                .collect(),
            extension_name: cfg.extension_name,
        }
    }
}

/// Extract the engine launch configuration from a v3 extension registry.
///
/// Returns `Ok(Some(…))` when exactly one ACP-capable extension is
/// activated, `Ok(None)` when none are, and `Err` when more than one
/// declares ACP capability.
pub fn resolve_engine(registry: &ExtensionRegistry) -> Result<Option<EngineLaunch>, SceneError> {
    find_acp_extension(registry).map(|opt| opt.map(EngineLaunch::from))
}

/// Assert that at most one engine is declared in the registry.
///
/// Equivalent to calling [`resolve_engine`] and discarding the result —
/// the v2 supervisor used this as a standalone validation gate.
pub fn ensure_no_engine_conflict(registry: &ExtensionRegistry) -> Result<(), SceneError> {
    find_acp_extension(registry).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        AgentCapability, CapabilitySet, ConfigSchema, ExtensionMetadata, LaunchSpec, StringNode,
    };

    /// Build minimal metadata with an optional ACP agent capability.
    fn meta_with_agent(name: &str, agent: Option<AgentCapability>) -> ExtensionMetadata {
        let mut entries = Vec::new();
        if agent.is_some() {
            entries.push(StringNode::new("agent"));
        }
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet {
                entries,
                agent,
            },
        }
    }

    fn acp_agent(command: &str, args: &[&str]) -> AgentCapability {
        AgentCapability {
            speaks: StringNode::new("acp"),
            launch: LaunchSpec {
                command: StringNode::new(command),
                args: args.iter().map(|a| StringNode::new(*a)).collect(),
            },
        }
    }

    #[test]
    fn resolve_engine_returns_none_for_empty_registry() {
        let reg = ExtensionRegistry::new();
        let result = resolve_engine(&reg).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_engine_returns_launch_for_single_acp() {
        let mut reg = ExtensionRegistry::new();
        let m = meta_with_agent("claude-code", Some(acp_agent("claude", &["--acp"])));
        reg.activate("claude-code", &m).unwrap();

        let launch = resolve_engine(&reg).unwrap().unwrap();
        assert_eq!(launch.command, "claude");
        assert_eq!(launch.args, vec!["--acp"]);
        assert_eq!(launch.extension_name, "claude-code");
    }

    #[test]
    fn resolve_engine_errors_on_multiple_acp() {
        let mut reg = ExtensionRegistry::new();
        let m1 = meta_with_agent("claude-code", Some(acp_agent("claude", &["--acp"])));
        let m2 = meta_with_agent("other-agent", Some(acp_agent("other", &[])));
        reg.activate("claude-code", &m1).unwrap();
        reg.activate("other-agent", &m2).unwrap();

        let err = resolve_engine(&reg).unwrap_err();
        assert!(matches!(err, SceneError::AcpMultipleAgents { .. }));
    }

    #[test]
    fn ensure_no_engine_conflict_passes_with_zero_or_one() {
        let reg = ExtensionRegistry::new();
        ensure_no_engine_conflict(&reg).unwrap();

        let mut reg = ExtensionRegistry::new();
        let m = meta_with_agent("claude-code", Some(acp_agent("claude", &[])));
        reg.activate("claude-code", &m).unwrap();
        ensure_no_engine_conflict(&reg).unwrap();
    }

    #[test]
    fn ensure_no_engine_conflict_fails_on_multiple() {
        let mut reg = ExtensionRegistry::new();
        let m1 = meta_with_agent("a", Some(acp_agent("a", &[])));
        let m2 = meta_with_agent("b", Some(acp_agent("b", &[])));
        reg.activate("a", &m1).unwrap();
        reg.activate("b", &m2).unwrap();

        let err = ensure_no_engine_conflict(&reg).unwrap_err();
        assert!(matches!(err, SceneError::AcpMultipleAgents { .. }));
    }

    #[test]
    fn engine_launch_from_acp_config() {
        let cfg = AcpConfig {
            extension_name: "test-ext".to_string(),
            launch: LaunchSpec {
                command: StringNode::new("my-cmd"),
                args: vec![StringNode::new("--flag"), StringNode::new("value")],
            },
        };
        let launch = EngineLaunch::from(cfg);
        assert_eq!(launch.command, "my-cmd");
        assert_eq!(launch.args, vec!["--flag", "value"]);
        assert_eq!(launch.extension_name, "test-ext");
    }
}
