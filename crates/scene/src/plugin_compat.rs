//! Plugin lifecycle compatibility shim (v2 → v3).
//!
//! The v2 supervisor imported `PluginDecl`, `Lifecycle`, and
//! `lower_plugin()` from `ark-scene-v2-archive::plugin`. V3 replaces
//! plugins with [`ExtensionBinding`] (T-099) which describes protocol
//! mode + render mode wiring.
//!
//! This module maps the v3 types back to a v2-shaped `PluginDecl` so the
//! supervisor can migrate incrementally without rewriting its plugin
//! lifecycle management.

use crate::ext::binding::{ExtensionBinding, ProtocolMode, RenderMode};

/// V2-compatible plugin declaration, backed by a v3 [`ExtensionBinding`].
///
/// The v2 `PluginDecl` carried a name + optional command (for subprocess
/// plugins) + optional plugin_path (for wasm/zellij-native plugins).
/// V3 splits this into orthogonal `ProtocolMode` + `RenderMode` axes;
/// the shim collapses them back for callers that haven't migrated yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDecl {
    /// Extension / plugin name.
    pub name: String,
    /// Subprocess command (from `ProtocolMode::Subprocess` or
    /// `RenderMode::Command`). `None` for in-process or wasm-only
    /// extensions.
    pub command: Option<String>,
    /// Plugin artifact path (from `ProtocolMode::Wasm` or
    /// `RenderMode::Plugin`). `None` for subprocess or in-process
    /// extensions.
    pub plugin_path: Option<String>,
}

/// Convert a v3 [`ExtensionBinding`] into a v2-style [`PluginDecl`].
///
/// Priority when both protocol and render modes contribute values:
///
/// - `command`: prefers `ProtocolMode::Subprocess.command`, falls back
///   to `RenderMode::Command.command`.
/// - `plugin_path`: prefers `ProtocolMode::Wasm.plugin_path`, falls
///   back to `RenderMode::Plugin.plugin_path`.
pub fn binding_to_plugin_decl(binding: &ExtensionBinding) -> PluginDecl {
    let command = match &binding.protocol_mode {
        ProtocolMode::Subprocess { command, .. } => Some(command.clone()),
        _ => match &binding.render_mode {
            RenderMode::Command { command, .. } => Some(command.clone()),
            _ => None,
        },
    };

    let plugin_path = match &binding.protocol_mode {
        ProtocolMode::Wasm { plugin_path } => Some(plugin_path.clone()),
        _ => match &binding.render_mode {
            RenderMode::Plugin { plugin_path } => Some(plugin_path.clone()),
            _ => None,
        },
    };

    PluginDecl {
        name: binding.name.clone(),
        command,
        plugin_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subprocess_command_view_binding() {
        let binding = ExtensionBinding {
            name: "git-diff".into(),
            protocol_mode: ProtocolMode::Subprocess {
                command: "git-diff".into(),
                args: vec![],
            },
            render_mode: RenderMode::Command {
                command: "git-diff-viewer".into(),
                args: vec![],
            },
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "git-diff");
        // Protocol-mode command takes priority.
        assert_eq!(decl.command, Some("git-diff".into()));
        assert_eq!(decl.plugin_path, None);
    }

    #[test]
    fn wasm_plugin_view_binding() {
        let binding = ExtensionBinding {
            name: "ai-chat.wasm".into(),
            protocol_mode: ProtocolMode::Wasm {
                plugin_path: "ai-chat.wasm".into(),
            },
            render_mode: RenderMode::Plugin {
                plugin_path: "AiChatPlugin".into(),
            },
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "ai-chat.wasm");
        // Wasm protocol_path takes priority over render-mode plugin_path.
        assert_eq!(decl.plugin_path, Some("ai-chat.wasm".into()));
        assert_eq!(decl.command, None);
    }

    #[test]
    fn in_process_headless_binding() {
        let binding = ExtensionBinding {
            name: "metrics".into(),
            protocol_mode: ProtocolMode::InProcess,
            render_mode: RenderMode::Headless,
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "metrics");
        assert_eq!(decl.command, None);
        assert_eq!(decl.plugin_path, None);
    }

    #[test]
    fn in_process_command_view_binding() {
        let binding = ExtensionBinding {
            name: "editor".into(),
            protocol_mode: ProtocolMode::InProcess,
            render_mode: RenderMode::Command {
                command: "EditView".into(),
                args: vec![],
            },
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "editor");
        // Falls back to render-mode command.
        assert_eq!(decl.command, Some("EditView".into()));
        assert_eq!(decl.plugin_path, None);
    }

    #[test]
    fn in_process_plugin_view_binding() {
        let binding = ExtensionBinding {
            name: "demo".into(),
            protocol_mode: ProtocolMode::InProcess,
            render_mode: RenderMode::Plugin {
                plugin_path: "DemoPlugin".into(),
            },
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "demo");
        assert_eq!(decl.command, None);
        // Falls back to render-mode plugin_path.
        assert_eq!(decl.plugin_path, Some("DemoPlugin".into()));
    }

    #[test]
    fn subprocess_headless_binding() {
        let binding = ExtensionBinding {
            name: "bg-sync".into(),
            protocol_mode: ProtocolMode::Subprocess {
                command: "bg-sync".into(),
                args: vec!["--daemon".into()],
            },
            render_mode: RenderMode::Headless,
        };
        let decl = binding_to_plugin_decl(&binding);
        assert_eq!(decl.name, "bg-sync");
        assert_eq!(decl.command, Some("bg-sync".into()));
        assert_eq!(decl.plugin_path, None);
    }
}
