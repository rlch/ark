//! Op AST node types — T-005 / R7.
//!
//! One facet-derived struct per canonical op verb, plus a top-level [`OpNode`]
//! enum that names them all. Structs carry the reaction/bind payload exactly as
//! it appears in the scene file; validation (direction enum, anchor enum, etc.)
//! is deferred to the compile pass (T-052 / T-053).

use facet::Facet;
use ::kdl::KdlDocument;

// FIXME T-004: `Handle`, `OverlayAttrs`, and `ViewRef` should be imported from
// `crate::ast::layout` (defined by T-004). T-004 has not landed yet, so the
// types below are forward placeholders that will be swapped to the real
// definitions in a Tier 1 integration task.
type Handle = String;
type OverlayAttrs = KdlDocument;
type ViewRef = KdlDocument;

/// `focus @handle` — transfer focus to a tab or pane.
#[derive(Facet, Debug, Clone)]
pub struct FocusOp {
    /// Target handle; compiler resolves tab-vs-pane from declaration (R7).
    pub handle: Handle,
    /// Optional per-op Rhai guard (R4.5 — `when=` legal on every op node).
    pub when: Option<String>,
}

/// `close @handle` — close the referenced tab or pane.
#[derive(Facet, Debug, Clone)]
pub struct CloseOp {
    /// Target handle; tab or pane (compiler-resolved).
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `rename @handle to="name"` — rename a tab (tab-only at compile time).
#[derive(Facet, Debug, Clone)]
pub struct RenameOp {
    /// Target tab handle.
    pub handle: Handle,
    /// New display name (may contain `{Rhai}` interpolation holes).
    pub to: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `resize @handle direction=<dir> by=<inc|dec>` — pane-only resize.
#[derive(Facet, Debug, Clone)]
pub struct ResizeOp {
    /// Target pane handle.
    pub handle: Handle,
    /// Raw direction string (`"up"` / `"down"` / `"left"` / `"right"`);
    /// validated against the R7 set at T-052.
    pub direction: String,
    /// Raw magnitude string (`"inc"` / `"dec"`); validated at T-052.
    pub by: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `move @handle to=<anchor>` — reposition a pane to a named anchor.
#[derive(Facet, Debug, Clone)]
pub struct MoveOp {
    /// Target pane handle.
    pub handle: Handle,
    /// Raw anchor string (`"top-right"`, `"center"`, …); validated at T-052.
    pub to: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `pin @handle` — pin an overlay pane (survives tab switch).
#[derive(Facet, Debug, Clone)]
pub struct PinOp {
    /// Target overlay pane handle.
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `unpin @handle` — unpin a previously pinned overlay pane.
#[derive(Facet, Debug, Clone)]
pub struct UnpinOp {
    /// Target overlay pane handle.
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `spawn @handle { <view> }` — create a tiled or overlay pane. Overlay
/// vs tiled is distinguished by presence of [`overlay`](Self::overlay).
#[derive(Facet, Debug, Clone)]
pub struct SpawnOp {
    /// Handle of the newly spawned pane.
    pub handle: Handle,
    /// `Some` when the caller wrote `spawn @h overlay pos=… size=… { … }`;
    /// `None` for tiled spawns.
    #[facet(opaque)]
    pub overlay: Option<OverlayAttrs>,
    /// Single view child node (the pane content, required per R3).
    #[facet(opaque)]
    pub view: ViewRef,
    /// Optional per-op Rhai guard (note: `SpawnOp` carries its own
    /// `when` because R7 already calls out overlay vs tiled modes —
    /// the guard still applies uniformly).
    pub when: Option<String>,
}

/// `new_tab @handle name="name" cwd="path"` — create a tab.
#[derive(Facet, Debug, Clone)]
pub struct NewTabOp {
    /// Handle of the new tab.
    pub handle: Handle,
    /// Optional display name (defaults to handle in the reconciler).
    pub name: Option<String>,
    /// Optional working directory for child panes (Rhai-interpolated).
    pub cwd: Option<String>,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `use_mode "name"` — switch active tab to the named mode layout.
#[derive(Facet, Debug, Clone)]
pub struct UseModeOp {
    /// Mode name (e.g. `"review"`; `"default"` reverts to the base layout).
    pub mode: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `pipe from=@handle to=@handle payload="…"` — send payload between two panes.
#[derive(Facet, Debug, Clone)]
pub struct PipeOp {
    /// Source pane handle.
    pub from: Handle,
    /// Destination pane handle.
    pub to: Handle,
    /// Payload string (may contain `{Rhai}` interpolation holes).
    pub payload: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `emit "<event-name>" { <kv payload> }` — emit a UserEvent on the bus.
#[derive(Facet, Debug, Clone)]
pub struct EmitOp {
    /// Fully-qualified event name (e.g. `"user.my_event"`).
    pub event_name: String,
    /// Opaque KDL payload block preserved verbatim; Rhai interpolation
    /// happens at dispatch time, not at parse time.
    #[facet(opaque)]
    pub payload: Option<KdlDocument>,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `set_status text="…" severity=<level> ttl_ms=<int>` — status-bar push.
#[derive(Facet, Debug, Clone)]
pub struct SetStatusOp {
    /// Status text (Rhai-interpolated).
    pub text: String,
    /// Optional severity level (`"info"` / `"success"` / `"warn"` / `"error"`).
    pub severity: Option<String>,
    /// Optional time-to-live in milliseconds; `None` = persistent.
    pub ttl_ms: Option<u64>,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `acp.prompt text="…"` — send a user message into the ACP session.
#[derive(Facet, Debug, Clone)]
pub struct AcpPromptOp {
    /// Prompt body (Rhai-interpolated).
    pub text: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `acp.cancel` — cancel the in-flight ACP turn.
#[derive(Facet, Debug, Clone)]
pub struct AcpCancelOp {
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `acp.permit request_id="…" outcome=<allow|reject_once|reject_always>`.
#[derive(Facet, Debug, Clone)]
pub struct AcpPermitOp {
    /// ACP request id to respond to.
    pub request_id: String,
    /// Raw outcome string (`"allow"` / `"reject_once"` / `"reject_always"`);
    /// validated against the R7 set at T-052.
    pub outcome: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `acp.set_mode mode="…"` — set the ACP agent mode (plan / edit / …).
#[derive(Facet, Debug, Clone)]
pub struct AcpSetModeOp {
    /// Mode name (protocol-defined; ark passes through).
    pub mode: String,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `exec script="…" shell="…" timeout_ms=<int>` — run a shell script.
#[derive(Facet, Debug, Clone)]
pub struct ExecOp {
    /// Script body (Rhai-interpolated).
    pub script: String,
    /// Optional shell binary override (defaults to `$SHELL`).
    pub shell: Option<String>,
    /// Optional timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// `reload_scene` — re-parse the scene file and reconcile.
#[derive(Facet, Debug, Clone)]
pub struct ReloadSceneOp {
    /// Optional per-op Rhai guard.
    pub when: Option<String>,
}

/// Top-level op enumeration — one variant per canonical op struct, plus a
/// catch-all [`OpNode::Unknown`] that preserves the raw verb + args for
/// forward-compat. T-053 surfaces `error[scene/unknown-op]` diagnostics for
/// the `Unknown` branch.
#[derive(Facet, Debug, Clone)]
#[repr(u8)]
pub enum OpNode {
    /// `focus @handle` op.
    Focus(FocusOp),
    /// `close @handle` op.
    Close(CloseOp),
    /// `rename @handle to="name"` op.
    Rename(RenameOp),
    /// `resize @handle direction=<dir> by=<inc|dec>` op.
    Resize(ResizeOp),
    /// `move @handle to=<anchor>` op.
    Move(MoveOp),
    /// `pin @handle` op.
    Pin(PinOp),
    /// `unpin @handle` op.
    Unpin(UnpinOp),
    /// `spawn @handle [overlay …] { <view> }` op.
    Spawn(SpawnOp),
    /// `new_tab @handle …` op.
    NewTab(NewTabOp),
    /// `use_mode "name"` op.
    UseMode(UseModeOp),
    /// `pipe from=@h to=@h payload=…` op.
    Pipe(PipeOp),
    /// `emit "<event>" { … }` op.
    Emit(EmitOp),
    /// `set_status …` op.
    SetStatus(SetStatusOp),
    /// `acp.prompt text=…` op.
    AcpPrompt(AcpPromptOp),
    /// `acp.cancel` op.
    AcpCancel(AcpCancelOp),
    /// `acp.permit …` op.
    AcpPermit(AcpPermitOp),
    /// `acp.set_mode mode=…` op.
    AcpSetMode(AcpSetModeOp),
    /// `exec script=…` op.
    Exec(ExecOp),
    /// `reload_scene` op.
    ReloadScene(ReloadSceneOp),
    /// Catch-all for unknown verbs — preserves the raw verb + args so
    /// T-053 can surface `error[scene/unknown-op]` with suggestions.
    Unknown {
        /// Raw op verb as written in the scene file.
        verb: String,
        /// Raw KDL body captured verbatim at parse time.
        #[facet(opaque)]
        args: KdlDocument,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_op_constructs() {
        let node = OpNode::Focus(FocusOp {
            handle: "@x".to_string(),
            when: None,
        });
        match node {
            OpNode::Focus(op) => {
                assert_eq!(op.handle, "@x");
                assert!(op.when.is_none());
            }
            _ => panic!("expected OpNode::Focus"),
        }
    }
}
