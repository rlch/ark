//! Op AST node types — T-005 / R7.
//!
//! One facet-derived struct per canonical op verb, plus a top-level [`OpNode`]
//! enum that names them all. Structs carry the reaction/bind payload exactly as
//! it appears in the scene file; validation (direction enum, anchor enum, etc.)
//! is deferred to the compile pass (T-052 / T-053).

use ::kdl::KdlDocument;
use facet::Facet;
use facet_kdl as kdl;

// Handle fields are stored as String so facet-kdl can deserialize them
// directly from KDL arguments. Post-parse validation via Handle::new
// happens in a later pass (T-014).
type Handle = String;
// OverlayAttrs and ViewRef hold foreign `kdl::KdlDocument` and are marked
// `#[facet(opaque)]` on the fields that use them.
type OverlayAttrs = KdlDocument;
type ViewRef = KdlDocument;

/// `focus @handle` — transfer focus to a tab or pane.
#[derive(Facet, Debug, Clone)]
pub struct FocusOp {
    /// Target handle; compiler resolves tab-vs-pane from declaration (R7).
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Optional per-op Rhai guard (R4.5 — `when=` legal on every op node).
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `close @handle` — close the referenced tab or pane.
#[derive(Facet, Debug, Clone)]
pub struct CloseOp {
    /// Target handle; tab or pane (compiler-resolved).
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `rename @handle to="name"` — rename a tab (tab-only at compile time).
#[derive(Facet, Debug, Clone)]
pub struct RenameOp {
    /// Target tab handle.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// New display name (may contain `{Rhai}` interpolation holes).
    #[facet(kdl::property)]
    pub to: String,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `resize @handle direction=<dir> by=<inc|dec>` — pane-only resize.
#[derive(Facet, Debug, Clone)]
pub struct ResizeOp {
    /// Target pane handle.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Raw direction string (`"up"` / `"down"` / `"left"` / `"right"`);
    /// validated against the R7 set at T-052.
    #[facet(kdl::property)]
    pub direction: String,
    /// Raw magnitude string (`"inc"` / `"dec"`); validated at T-052.
    #[facet(kdl::property)]
    pub by: String,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `move @handle to=<anchor>` — reposition a pane to a named anchor.
#[derive(Facet, Debug, Clone)]
pub struct MoveOp {
    /// Target pane handle.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Raw anchor string (`"top-right"`, `"center"`, …); validated at T-052.
    #[facet(kdl::property)]
    pub to: String,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `pin @handle` — pin an overlay pane (survives tab switch).
#[derive(Facet, Debug, Clone)]
pub struct PinOp {
    /// Target overlay pane handle.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `unpin @handle` — unpin a previously pinned overlay pane.
#[derive(Facet, Debug, Clone)]
pub struct UnpinOp {
    /// Target overlay pane handle.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `spawn @handle { <view> }` — create a tiled or overlay pane. Overlay
/// vs tiled is distinguished by presence of [`overlay`](Self::overlay).
#[derive(Facet, Debug, Clone)]
pub struct SpawnOp {
    /// Handle of the newly spawned pane.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// `Some` when the caller wrote `spawn @h overlay pos=… size=… { … }`;
    /// `None` for tiled spawns.
    #[facet(opaque, default)]
    pub overlay: Option<OverlayAttrs>,
    /// Single view child node (the pane content, required per R3).
    #[facet(opaque, default)]
    pub view: ViewRef,
    /// Optional per-op Rhai guard (note: `SpawnOp` carries its own
    /// `when` because R7 already calls out overlay vs tiled modes —
    /// the guard still applies uniformly).
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `new_tab @handle name="name" cwd="path"` — create a tab.
#[derive(Facet, Debug, Clone)]
pub struct NewTabOp {
    /// Handle of the new tab.
    #[facet(kdl::argument)]
    pub handle: Handle,
    /// Optional display name (defaults to handle in the reconciler).
    #[facet(kdl::property, default)]
    pub name: Option<String>,
    /// Optional working directory for child panes (Rhai-interpolated).
    #[facet(kdl::property, default)]
    pub cwd: Option<String>,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `use_mode "name"` — switch active tab to the named mode layout.
#[derive(Facet, Debug, Clone)]
pub struct UseModeOp {
    /// Mode name (e.g. `"review"`; `"default"` reverts to the base layout).
    #[facet(kdl::argument)]
    pub mode: String,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `pipe from=@handle to=@handle payload="…"` — send payload between two panes.
#[derive(Facet, Debug, Clone)]
pub struct PipeOp {
    /// Source pane handle.
    #[facet(kdl::property)]
    pub from: Handle,
    /// Destination pane handle.
    #[facet(kdl::property)]
    pub to: Handle,
    /// Payload string (may contain `{Rhai}` interpolation holes).
    #[facet(kdl::property)]
    pub payload: String,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `emit "<event-name>" { <kv payload> }` — emit a UserEvent on the bus.
#[derive(Facet, Debug, Clone)]
pub struct EmitOp {
    /// Fully-qualified event name (e.g. `"user.my_event"`).
    #[facet(kdl::argument)]
    pub event_name: String,
    /// Opaque KDL payload block preserved verbatim; Rhai interpolation
    /// happens at dispatch time, not at parse time.
    #[facet(opaque, default)]
    pub payload: Option<KdlDocument>,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `set_status text="…" severity=<level> ttl_ms=<int>` — status-bar push.
#[derive(Facet, Debug, Clone)]
pub struct SetStatusOp {
    /// Status text (Rhai-interpolated).
    #[facet(kdl::property)]
    pub text: String,
    /// Optional severity level (`"info"` / `"success"` / `"warn"` / `"error"`).
    #[facet(kdl::property, default)]
    pub severity: Option<String>,
    /// Optional time-to-live in milliseconds; `None` = persistent.
    #[facet(kdl::property, default)]
    pub ttl_ms: Option<u64>,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `exec script="…" shell="…" timeout_ms=<int>` — run a shell script.
#[derive(Facet, Debug, Clone)]
pub struct ExecOp {
    /// Script body (Rhai-interpolated).
    #[facet(kdl::property)]
    pub script: String,
    /// Optional shell binary override (defaults to `$SHELL`).
    #[facet(kdl::property, default)]
    pub shell: Option<String>,
    /// Optional timeout in milliseconds.
    #[facet(kdl::property, default)]
    pub timeout_ms: Option<u64>,
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
    pub when: Option<String>,
}

/// `reload_scene` — re-parse the scene file and reconcile.
#[derive(Facet, Debug, Clone)]
pub struct ReloadSceneOp {
    /// Optional per-op Rhai guard.
    #[facet(kdl::property, default)]
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
    #[facet(rename = "focus")]
    Focus(FocusOp),
    /// `close @handle` op.
    #[facet(rename = "close")]
    Close(CloseOp),
    /// `rename @handle to="name"` op.
    #[facet(rename = "rename")]
    Rename(RenameOp),
    /// `resize @handle direction=<dir> by=<inc|dec>` op.
    #[facet(rename = "resize")]
    Resize(ResizeOp),
    /// `move @handle to=<anchor>` op.
    #[facet(rename = "move")]
    Move(MoveOp),
    /// `pin @handle` op.
    #[facet(rename = "pin")]
    Pin(PinOp),
    /// `unpin @handle` op.
    #[facet(rename = "unpin")]
    Unpin(UnpinOp),
    /// `spawn @handle [overlay …] { <view> }` op.
    #[facet(rename = "spawn")]
    Spawn(SpawnOp),
    /// `new_tab @handle …` op.
    #[facet(rename = "new_tab")]
    NewTab(NewTabOp),
    /// `use_mode "name"` op.
    #[facet(rename = "use_mode")]
    UseMode(UseModeOp),
    /// `pipe from=@h to=@h payload=…` op.
    #[facet(rename = "pipe")]
    Pipe(PipeOp),
    /// `emit "<event>" { … }` op.
    #[facet(rename = "emit")]
    Emit(EmitOp),
    /// `set_status …` op.
    #[facet(rename = "set_status")]
    SetStatus(SetStatusOp),
    /// `exec script=…` op.
    #[facet(rename = "exec")]
    Exec(ExecOp),
    /// `reload_scene` op.
    #[facet(rename = "reload_scene")]
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
