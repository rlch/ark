//! Extension-pipe-proto binding (T-099).
//!
//! When a pane mounts a view from a subprocess extension, ark starts
//! the protocol handler; zellij runs the view command in the pane; the
//! protocol handler connects to the view process via app-native RPC.
//!
//! [`ExtensionBinding`] describes *how* an extension's protocol handler
//! and view renderer connect. [`resolve_binding`] examines the
//! extension's [`ExtensionMetadata`] (delivery mode, capabilities,
//! views) to determine the wiring.
//!
//! This module is pure data — no I/O, no async.

use ark_ext_metadata_types::ExtensionMetadata;

/// Describes how an extension's protocol handler and view renderer connect.
///
/// Protocol handler = subprocess / compiled-in / wasm running the extension
/// protocol. View renderer = the command/plugin that zellij runs in a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionBinding {
    /// Extension name.
    pub name: String,
    /// How the protocol handler is delivered.
    pub protocol_mode: ProtocolMode,
    /// How the view is rendered.
    pub render_mode: RenderMode,
}

/// How the extension's protocol handler is delivered to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolMode {
    /// In-process trait dispatch (compiled-in extension).
    InProcess,
    /// Subprocess with NDJSON JSON-RPC 2.0 transport.
    Subprocess {
        /// Command to spawn.
        command: String,
        /// Arguments passed to the subprocess.
        args: Vec<String>,
    },
    /// Wasm plugin via ark-bus pipe bridge.
    Wasm {
        /// Path to the `.wasm` plugin artifact.
        plugin_path: String,
    },
}

/// How the extension's view is rendered in a zellij pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderMode {
    /// Command run in a pane (CommandView).
    Command {
        /// Command to execute in the pane.
        command: String,
        /// Arguments passed to the command.
        args: Vec<String>,
    },
    /// Zellij plugin loaded in a pane (PluginView).
    Plugin {
        /// Path to the zellij plugin artifact.
        plugin_path: String,
    },
    /// No visible view (headless extension).
    Headless,
}

/// Resolve the [`ExtensionBinding`] for an extension from its metadata.
///
/// Heuristic:
///
/// 1. **Protocol mode** — if the extension's name ends with `.wasm` the
///    protocol handler is delivered as a wasm plugin. Otherwise, if the
///    `pipe` capability is declared, the handler is a subprocess
///    (command = extension name, no extra args). All other extensions
///    are treated as compiled-in (in-process trait dispatch).
///
/// 2. **Render mode** — determined by the first declared view (if any).
///    A view whose component identifier ends with `Plugin` is a zellij
///    plugin view (plugin_path = component value). Otherwise the view
///    is a command view (command = component value). Extensions with no
///    declared views are headless.
pub fn resolve_binding(metadata: &ExtensionMetadata) -> ExtensionBinding {
    let name = metadata.name.value.clone();

    // ── Protocol mode ──────────────────────────────────────────────
    let protocol_mode = if name.ends_with(".wasm") {
        ProtocolMode::Wasm {
            plugin_path: name.clone(),
        }
    } else if metadata
        .capability_names()
        .any(|c| c == "pipe" || c == "exec")
    {
        ProtocolMode::Subprocess {
            command: name.clone(),
            args: Vec::new(),
        }
    } else {
        ProtocolMode::InProcess
    };

    // ── Render mode ────────────────────────────────────────────────
    let render_mode = match metadata.views.first() {
        Some(view) => {
            let component = &view.component.value;
            if component.ends_with("Plugin") {
                RenderMode::Plugin {
                    plugin_path: component.clone(),
                }
            } else {
                RenderMode::Command {
                    command: component.clone(),
                    args: Vec::new(),
                }
            }
        }
        None => RenderMode::Headless,
    };

    ExtensionBinding {
        name,
        protocol_mode,
        render_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode, ViewDecl,
    };

    /// Build minimal extension metadata for binding tests.
    fn meta(name: &str, caps: &[&str], views: &[(&str, &str)]) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: views
                .iter()
                .map(|(n, component)| ViewDecl {
                    name: n.to_string(),
                    component: StringNode::new(*component),
                })
                .collect(),
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::from_strs(caps),
        }
    }

    // ── Protocol mode resolution ─────────────────────────────────

    #[test]
    fn compiled_in_extension_resolves_to_in_process() {
        let m = meta("editor", &[], &[("editor.main", "EditView")]);
        let b = resolve_binding(&m);
        assert_eq!(b.protocol_mode, ProtocolMode::InProcess);
    }

    #[test]
    fn pipe_capability_resolves_to_subprocess() {
        let m = meta("git-status", &["pipe"], &[("git-status.panel", "GitPanel")]);
        let b = resolve_binding(&m);
        assert_eq!(
            b.protocol_mode,
            ProtocolMode::Subprocess {
                command: "git-status".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn exec_capability_resolves_to_subprocess() {
        let m = meta("linter", &["exec"], &[]);
        let b = resolve_binding(&m);
        assert_eq!(
            b.protocol_mode,
            ProtocolMode::Subprocess {
                command: "linter".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn wasm_name_resolves_to_wasm_protocol() {
        let m = meta("ai-chat.wasm", &[], &[("ai-chat.view", "ChatPlugin")]);
        let b = resolve_binding(&m);
        assert_eq!(
            b.protocol_mode,
            ProtocolMode::Wasm {
                plugin_path: "ai-chat.wasm".into(),
            }
        );
    }

    // ── Render mode resolution ───────────────────────────────────

    #[test]
    fn view_with_plugin_suffix_resolves_to_plugin_render() {
        let m = meta("demo", &[], &[("demo.panel", "DemoPlugin")]);
        let b = resolve_binding(&m);
        assert_eq!(
            b.render_mode,
            RenderMode::Plugin {
                plugin_path: "DemoPlugin".into(),
            }
        );
    }

    #[test]
    fn view_without_plugin_suffix_resolves_to_command_render() {
        let m = meta("demo", &[], &[("demo.panel", "DemoPanel")]);
        let b = resolve_binding(&m);
        assert_eq!(
            b.render_mode,
            RenderMode::Command {
                command: "DemoPanel".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn no_views_resolves_to_headless() {
        let m = meta("background-sync", &["exec"], &[]);
        let b = resolve_binding(&m);
        assert_eq!(b.render_mode, RenderMode::Headless);
    }

    // ── Full binding resolution ──────────────────────────────────

    #[test]
    fn subprocess_extension_with_command_view() {
        let m = meta(
            "git-diff",
            &["pipe"],
            &[("git-diff.viewer", "git-diff-viewer")],
        );
        let b = resolve_binding(&m);
        assert_eq!(b.name, "git-diff");
        assert_eq!(
            b.protocol_mode,
            ProtocolMode::Subprocess {
                command: "git-diff".into(),
                args: vec![],
            }
        );
        assert_eq!(
            b.render_mode,
            RenderMode::Command {
                command: "git-diff-viewer".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn wasm_extension_with_plugin_view() {
        let m = meta(
            "ai-chat.wasm",
            &["network"],
            &[("ai-chat.panel", "AiChatPlugin")],
        );
        let b = resolve_binding(&m);
        assert_eq!(b.name, "ai-chat.wasm");
        assert_eq!(
            b.protocol_mode,
            ProtocolMode::Wasm {
                plugin_path: "ai-chat.wasm".into(),
            }
        );
        assert_eq!(
            b.render_mode,
            RenderMode::Plugin {
                plugin_path: "AiChatPlugin".into(),
            }
        );
    }

    #[test]
    fn in_process_headless_extension() {
        let m = meta("metrics", &[], &[]);
        let b = resolve_binding(&m);
        assert_eq!(b.name, "metrics");
        assert_eq!(b.protocol_mode, ProtocolMode::InProcess);
        assert_eq!(b.render_mode, RenderMode::Headless);
    }

    // ── Name passthrough ─────────────────────────────────────────

    #[test]
    fn binding_name_matches_metadata_name() {
        let m = meta("my-ext", &[], &[]);
        let b = resolve_binding(&m);
        assert_eq!(b.name, "my-ext");
    }
}
