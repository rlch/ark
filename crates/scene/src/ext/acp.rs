//! ACP activation detection (T-104).
//!
//! Scans the [`ExtensionRegistry`] for extensions that declare a
//! structured `capabilities.agent` block with `speaks = "acp"` and
//! returns the corresponding [`AcpConfig`] launch specification.
//!
//! # Error policy
//!
//! - Zero ACP-capable extensions: returns `Ok(None)`.
//! - Exactly one: returns `Ok(Some(AcpConfig))`.
//! - More than one: returns `Err(SceneError::AcpMultipleAgents)`.

use ark_ext_metadata_types::LaunchSpec;

use super::registry::ExtensionRegistry;
use crate::error::SceneError;

/// Launch configuration for an ACP-capable extension.
///
/// Returned by [`find_acp_extension`] when exactly one activated
/// extension declares `capabilities { agent { speaks "acp" } }`.
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// Name of the extension that provides ACP capability.
    pub extension_name: String,

    /// How to launch the agent subprocess. Cloned from the extension's
    /// [`LaunchSpec`] so the caller can spawn without borrowing the
    /// registry.
    pub launch: LaunchSpec,
}

/// Scan the registry for extensions declaring ACP capability.
///
/// Returns `Ok(Some(config))` when exactly one activated extension has
/// `capabilities.agent.speaks = "acp"`, `Ok(None)` when none do, and
/// `Err(SceneError::AcpMultipleAgents)` when more than one does.
pub fn find_acp_extension(registry: &ExtensionRegistry) -> Result<Option<AcpConfig>, SceneError> {
    let mut found: Vec<AcpConfig> = Vec::new();

    for ext_name in registry.active_extensions() {
        let Some(meta) = registry.metadata(ext_name) else {
            continue;
        };
        let Some(agent) = meta.agent_capability() else {
            continue;
        };
        if agent.speaks.value == "acp" {
            found.push(AcpConfig {
                extension_name: ext_name.to_string(),
                launch: agent.launch.clone(),
            });
        }
    }

    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.into_iter().next().unwrap())),
        _ => {
            let names: Vec<String> = found.iter().map(|c| c.extension_name.clone()).collect();
            Err(SceneError::AcpMultipleAgents { extensions: names })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        AgentCapability, CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode,
    };

    /// Helper: build minimal metadata with optional ACP agent capability.
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

    /// Helper: build an ACP agent capability.
    fn acp_agent(command: &str, args: &[&str]) -> AgentCapability {
        AgentCapability {
            speaks: StringNode::new("acp"),
            launch: LaunchSpec {
                command: StringNode::new(command),
                args: args.iter().map(|a| StringNode::new(*a)).collect(),
            },
        }
    }

    // ── Extension with ACP capability detected ───────────────────────

    #[test]
    fn finds_single_acp_extension() {
        let mut reg = ExtensionRegistry::new();
        let m = meta_with_agent("claude-code", Some(acp_agent("claude", &["--acp"])));
        reg.activate("claude-code", &m).unwrap();

        let config = find_acp_extension(&reg).unwrap();
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.extension_name, "claude-code");
        assert_eq!(config.launch.command.value, "claude");
        assert_eq!(config.launch.args.len(), 1);
        assert_eq!(config.launch.args[0].value, "--acp");
    }

    // ── Extension without ACP returns None ───────────────────────────

    #[test]
    fn no_acp_extension_returns_none() {
        let mut reg = ExtensionRegistry::new();
        let m = meta_with_agent("git-ext", None);
        reg.activate("git-ext", &m).unwrap();

        let config = find_acp_extension(&reg).unwrap();
        assert!(config.is_none());
    }

    #[test]
    fn empty_registry_returns_none() {
        let reg = ExtensionRegistry::new();
        let config = find_acp_extension(&reg).unwrap();
        assert!(config.is_none());
    }

    // ── Multiple ACP extensions errors ───────────────────────────────

    #[test]
    fn multiple_acp_extensions_errors() {
        let mut reg = ExtensionRegistry::new();
        let m1 = meta_with_agent("claude-code", Some(acp_agent("claude", &["--acp"])));
        let m2 = meta_with_agent("other-agent", Some(acp_agent("other", &["--acp"])));
        reg.activate("claude-code", &m1).unwrap();
        reg.activate("other-agent", &m2).unwrap();

        let err = find_acp_extension(&reg).unwrap_err();
        match err {
            SceneError::AcpMultipleAgents { extensions } => {
                assert_eq!(extensions.len(), 2);
                assert!(extensions.contains(&"claude-code".to_string()));
                assert!(extensions.contains(&"other-agent".to_string()));
            }
            other => panic!("expected AcpMultipleAgents, got: {other:?}"),
        }
    }

    // ── Non-ACP agent protocol is ignored ────────────────────────────

    #[test]
    fn non_acp_agent_ignored() {
        let mut reg = ExtensionRegistry::new();
        let agent = AgentCapability {
            speaks: StringNode::new("mcp"),
            launch: LaunchSpec {
                command: StringNode::new("mcp-server"),
                args: vec![],
            },
        };
        let m = meta_with_agent("mcp-ext", Some(agent));
        reg.activate("mcp-ext", &m).unwrap();

        let config = find_acp_extension(&reg).unwrap();
        assert!(config.is_none());
    }

    // ── Mixed extensions: only ACP one returned ──────────────────────

    #[test]
    fn mixed_extensions_finds_acp_only() {
        let mut reg = ExtensionRegistry::new();
        let m1 = meta_with_agent("plain-ext", None);
        let m2 = meta_with_agent("claude-code", Some(acp_agent("claude", &["--acp"])));
        reg.activate("plain-ext", &m1).unwrap();
        reg.activate("claude-code", &m2).unwrap();

        let config = find_acp_extension(&reg).unwrap().unwrap();
        assert_eq!(config.extension_name, "claude-code");
    }
}
