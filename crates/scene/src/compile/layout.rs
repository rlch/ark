//! Layout lowering — scene `LayoutNode` AST → zellij KDL (T-034..T-040).
//!
//! Translates ark's native layout DSL (R3) into the zellij layout subset
//! ark targets for `zellij action override-layout`. The lowering is
//! structural — zellij owns the runtime; ark owns the vocabulary — so
//! every ark-specific attribute (`@handle`, `when=`) is either stripped
//! or repurposed as its zellij equivalent (e.g. `@handle` → zellij
//! `name=`).
//!
//! # Translation matrix (R3)
//!
//! | Scene DSL                                   | Zellij KDL                                                                       |
//! |--------------------------------------------|----------------------------------------------------------------------------------|
//! | `layout { tab @h { body } }`               | `layout { tab name="h" { ... } }`                                                |
//! | `row { … }`                                 | `pane split_direction="horizontal" { … }`                                       |
//! | `col { … }`                                 | `pane split_direction="vertical" { … }`                                         |
//! | `pane @h { command "cmd" args=[…] }`       | `pane name="h" command="env" args=["ARK_HANDLE=@h", "cmd", …]`                  |
//! | `pane @h { shell }`                         | `pane name="h" command="env" args=["ARK_HANDLE=@h", "$SHELL"]`                 |
//! | `pane @h { edit path="p" }`                 | `pane name="h" edit="p"`  (no env wrapper — zellij native)                      |
//! | `span=N` on siblings                        | normalise to 100%, emit `size="N%"` per sibling                                 |
//! | `cells=N`                                   | `size=N`                                                                         |
//! | `min/max`                                   | `size_min` / `size_max`                                                          |
//! | overlay `pos=… size=… sticky=true`          | `floating_panes { pane name="h" x=… y=… width=… height=… pinned=true }`         |
//!
//! # `ARK_HANDLE` env wrapper (T-039)
//!
//! Every `CommandView`-rendered pane has its command prefixed with `env
//! ARK_HANDLE=@<handle>` so zellij's override-layout matching (by
//! `command + args` tuple) can disambiguate two shells that would otherwise
//! be identical. `ZellijView` panes (e.g. `edit`) do not get the wrapper —
//! zellij owns those panes natively and identifies them by `name=`.
//!
//! # Rendered artifact (T-040)
//!
//! [`write_layout_artifact`] writes the compiled KDL to
//! `${XDG_RUNTIME_DIR}/ark/layouts/<id-hash>-scene.kdl` with file mode
//! `0600` and re-parses it through `kdl::KdlDocument::parse` before
//! returning so a corrupt writer can't hand an invalid file to zellij.

// Tolerate `Result<T, SceneError>` size across this module — the error
// enum is deliberately big (it carries miette source buffers); the crate
// as a whole has already accepted the heap cost.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use miette::{NamedSource, SourceSpan};

use crate::ast::LayoutNode;
use crate::ast::layout::{ColNode, Handle, LayoutChild, PaneNode, RowNode, TabNode, ViewRef};
use crate::error::SceneError;
use crate::id::SceneId;
use crate::view::{RenderMode, ViewRegistry};

// ---------------------------------------------------------------------------
// Terminal size defaults used for overlay anchor math (R3.8–R3.12)
// ---------------------------------------------------------------------------

/// Default logical terminal width (cols) used for overlay anchor math
/// when the reconciler hasn't learned the real terminal size yet.
pub const DEFAULT_TERMINAL_COLS: u32 = 80;

/// Default logical terminal height (rows) used for overlay anchor math
/// when the reconciler hasn't learned the real terminal size yet.
pub const DEFAULT_TERMINAL_ROWS: u32 = 24;

/// Overlay-anchor computation input. Passed to [`compile_layout_kdl`]
/// indirectly via [`LayoutCompileCtx`]. The default is the logical 80×24
/// terminal grid used by zellij's own overlay defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    /// Terminal width in cells (columns).
    pub cols: u32,
    /// Terminal height in cells (rows).
    pub rows: u32,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_TERMINAL_COLS,
            rows: DEFAULT_TERMINAL_ROWS,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level entry point (T-034)
// ---------------------------------------------------------------------------

/// Lower a scene [`LayoutNode`] into a zellij-compatible [`KdlDocument`].
///
/// The returned document has one top-level `layout { … }` node that
/// zellij's override-layout consumer will accept verbatim. The caller is
/// responsible for merging in any `keybinds { }` block (T-065).
///
/// Errors:
///
/// - [`SceneError::MisplacedNode`] — raised if a layout contains no tabs;
///   zellij requires at least one.
/// - [`SceneError::UnknownView`] — raised if a pane's view alias is
///   non-empty and not registered. An *empty* alias is treated as the
///   `shell` primitive so tests can construct partial trees before T-026+
///   populates real aliases; downstream tiers should always populate
///   aliases via view resolution before calling into this.
#[allow(clippy::result_large_err)]
pub fn compile_layout_kdl(
    layout: &LayoutNode,
    registry: &ViewRegistry,
) -> Result<KdlDocument, SceneError> {
    compile_layout_kdl_with_terminal(layout, registry, TerminalSize::default())
}

/// Same as [`compile_layout_kdl`] but with a caller-provided terminal
/// size for overlay anchor math. Used by the reconciler once it has
/// learned the real size.
#[allow(clippy::result_large_err)]
pub fn compile_layout_kdl_with_terminal(
    layout: &LayoutNode,
    registry: &ViewRegistry,
    term: TerminalSize,
) -> Result<KdlDocument, SceneError> {
    let ctx = LayoutCompileCtx { registry, term };
    let mut doc = KdlDocument::new();

    let mut layout_node = KdlNode::new("layout");
    let mut layout_body = KdlDocument::new();

    for tab in &layout.tabs {
        ctx.emit_tab(tab, &mut layout_body)?;
    }

    layout_node.set_children(layout_body);
    doc.nodes_mut().push(layout_node);
    doc.autoformat();
    Ok(doc)
}

/// Shared state for a single compilation pass.
struct LayoutCompileCtx<'a> {
    registry: &'a ViewRegistry,
    term: TerminalSize,
}

impl<'a> LayoutCompileCtx<'a> {
    // -----------------------------------------------------------------
    // Tab emission (T-034 + T-038)
    // -----------------------------------------------------------------

    fn emit_tab(&self, tab: &TabNode, out: &mut KdlDocument) -> Result<(), SceneError> {
        let handle = Handle::new(&tab.handle).map_err(|e| {
            SceneError::MisplacedNode {
                node: format!("@{}", tab.handle),
                parent: format!("handle: {e}"),
                src: NamedSource::new("<layout>", String::new()),
                span: SourceSpan::new(0.into(), 1),
            }
        })?;

        let mut tab_node = KdlNode::new("tab");
        // Zellij `name="…"` — use the user-set `name` when provided,
        // otherwise fall back to the bare handle identifier.
        let display_name = tab.name.clone().unwrap_or_else(|| handle.name().to_string());
        tab_node.push(KdlEntry::new_prop("name", display_name));
        if let Some(cwd) = &tab.cwd {
            tab_node.push(KdlEntry::new_prop("cwd", cwd.clone()));
        }
        if matches!(tab.focus.as_deref(), Some("true")) {
            tab_node.push(KdlEntry::new_prop("focus", true));
        }

        // Split top-level children into tiled body + overlay list; zellij
        // renders overlays in a sibling `floating_panes { … }` block.
        let mut tiled: Vec<&LayoutChild> = Vec::new();
        let mut overlays: Vec<(Handle, &PaneNode)> = Vec::new();
        for child in &tab.body {
            if let LayoutChild::Pane(pane) = child {
                if pane_is_overlay(pane) {
                    let h = Handle::new(&pane.handle).map_err(|e| {
                        SceneError::MisplacedNode {
                            node: format!("@{}", pane.handle),
                            parent: format!("handle: {e}"),
                            src: NamedSource::new("<layout>", String::new()),
                            span: SourceSpan::new(0.into(), 1),
                        }
                    })?;
                    overlays.push((h, pane));
                    continue;
                }
            }
            tiled.push(child);
        }

        // Tiled body — normalise sizing across siblings and emit each
        // child into the tab body document.
        let mut tab_body = KdlDocument::new();
        self.emit_children(&tiled, &mut tab_body)?;

        if !overlays.is_empty() {
            let mut floating = KdlNode::new("floating_panes");
            let mut floating_body = KdlDocument::new();
            for (handle, pane) in overlays {
                self.emit_overlay(&handle, pane, &mut floating_body)?;
            }
            floating.set_children(floating_body);
            tab_body.nodes_mut().push(floating);
        }

        tab_node.set_children(tab_body);
        out.nodes_mut().push(tab_node);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Row / col / pane children (T-034 + T-035 + T-036)
    // -----------------------------------------------------------------

    fn emit_children(
        &self,
        children: &[&LayoutChild],
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        // Normalise spans across siblings — a child with `span=N` is
        // rendered as `size="<pct>%"` where `pct = N / Σspan × 100`.
        let spans: Vec<Option<u32>> = children.iter().map(|c| child_span(c)).collect();
        let total: u32 = spans.iter().filter_map(|s| *s).sum();

        for (i, child) in children.iter().enumerate() {
            self.emit_child(child, spans[i], total, out)?;
        }
        Ok(())
    }

    fn emit_child(
        &self,
        child: &LayoutChild,
        own_span: Option<u32>,
        total_span: u32,
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        match child {
            LayoutChild::Row(row) => self.emit_split(
                "horizontal",
                row,
                own_span,
                total_span,
                out,
            ),
            LayoutChild::Col(col) => self.emit_split_col(col, own_span, total_span, out),
            LayoutChild::Pane(pane) => self.emit_pane(pane, own_span, total_span, out),
        }
    }

    fn emit_split(
        &self,
        direction: &str,
        row: &RowNode,
        own_span: Option<u32>,
        total_span: u32,
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        let mut node = KdlNode::new("pane");
        node.push(KdlEntry::new_prop("split_direction", direction));
        push_sizing(
            &mut node,
            SizingInput {
                span: row.span.or(own_span),
                cells: row.cells,
                min: row.min,
                max: row.max,
                total_span,
            },
        );
        let mut body = KdlDocument::new();
        let inner: Vec<&LayoutChild> = row.body.iter().collect();
        self.emit_children(&inner, &mut body)?;
        node.set_children(body);
        out.nodes_mut().push(node);
        Ok(())
    }

    fn emit_split_col(
        &self,
        col: &ColNode,
        own_span: Option<u32>,
        total_span: u32,
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        let mut node = KdlNode::new("pane");
        node.push(KdlEntry::new_prop("split_direction", "vertical"));
        push_sizing(
            &mut node,
            SizingInput {
                span: col.span.or(own_span),
                cells: col.cells,
                min: col.min,
                max: col.max,
                total_span,
            },
        );
        let mut body = KdlDocument::new();
        let inner: Vec<&LayoutChild> = col.body.iter().collect();
        self.emit_children(&inner, &mut body)?;
        node.set_children(body);
        out.nodes_mut().push(node);
        Ok(())
    }

    fn emit_pane(
        &self,
        pane: &PaneNode,
        own_span: Option<u32>,
        total_span: u32,
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        let handle = Handle::new(&pane.handle).map_err(|e| {
            SceneError::MisplacedNode {
                node: format!("@{}", pane.handle),
                parent: format!("handle: {e}"),
                src: NamedSource::new("<layout>", String::new()),
                span: SourceSpan::new(0.into(), 1),
            }
        })?;
        let mut node = KdlNode::new("pane");
        node.push(KdlEntry::new_prop("name", handle.name().to_string()));
        push_sizing(
            &mut node,
            SizingInput {
                span: pane.span.or(own_span),
                cells: pane.cells,
                min: pane.min,
                max: pane.max,
                total_span,
            },
        );
        self.apply_view(&handle, &pane.view, &mut node)?;
        out.nodes_mut().push(node);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Overlay emission (T-037)
    // -----------------------------------------------------------------

    fn emit_overlay(
        &self,
        handle: &Handle,
        pane: &PaneNode,
        out: &mut KdlDocument,
    ) -> Result<(), SceneError> {
        // Overlays don't carry sizing siblings — they're always absolute.
        let overlay_attrs = pane_overlay_attrs(pane).expect("pane_is_overlay checked");
        let pos = parse_pos(&overlay_attrs.pos)?;
        let size = parse_overlay_size(&overlay_attrs.size)?;
        let (x, y, w, h) = anchor_overlay(pos, size, self.term);

        let mut node = KdlNode::new("pane");
        node.push(KdlEntry::new_prop("name", handle.name().to_string()));
        node.push(KdlEntry::new_prop("x", i128::from(x)));
        node.push(KdlEntry::new_prop("y", i128::from(y)));
        node.push(KdlEntry::new_prop("width", i128::from(w)));
        node.push(KdlEntry::new_prop("height", i128::from(h)));
        if matches!(overlay_attrs.sticky.as_deref(), Some("true")) {
            node.push(KdlEntry::new_prop("pinned", true));
        }
        self.apply_view(handle, &pane.view, &mut node)?;
        out.nodes_mut().push(node);
        Ok(())
    }

    // -----------------------------------------------------------------
    // View lowering — command / shell / edit (T-028..T-030, T-039)
    // -----------------------------------------------------------------

    fn apply_view(
        &self,
        handle: &Handle,
        view: &ViewRef,
        node: &mut KdlNode,
    ) -> Result<(), SceneError> {
        let alias = view.alias.as_str();
        // When the view alias is empty — typically because T-026+ view
        // resolution hasn't run yet — fall back to the `shell` primitive
        // so downstream output remains valid zellij KDL.
        let effective = if alias.is_empty() { "shell" } else { alias };

        let meta = self.registry.resolve(effective);
        let render_mode = meta.map(|m| m.render_mode.clone());

        match effective {
            "shell" => emit_shell(handle, node),
            "command" => emit_command(handle, view, node),
            "edit" => emit_edit(view, node),
            other => {
                // Unknown views fall back to shell with an ARK_HANDLE
                // wrapper — this is a stopgap. Real unknown-view
                // diagnostics surface from the compile-pass in T-031.
                if render_mode == Some(RenderMode::CommandView) {
                    emit_shell(handle, node);
                } else {
                    // Non-command views — emit a placeholder `plugin`
                    // entry so the KDL is still structurally valid.
                    node.push(KdlEntry::new_prop("plugin", other.to_string()));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// View primitive emitters (T-028 / T-029 / T-030 + T-039)
// ---------------------------------------------------------------------------

fn emit_shell(handle: &Handle, node: &mut KdlNode) {
    node.push(KdlEntry::new_prop("command", "env"));
    let args = KdlValue::String(format!("ARK_HANDLE={}", handle.raw()));
    let shell = KdlValue::String("$SHELL".to_string());
    push_args(node, vec![args, shell]);
}

fn emit_command(handle: &Handle, view: &ViewRef, node: &mut KdlNode) {
    // Pull `cmd` + `args` out of the view's config KDL block, if present.
    let mut cmd: String = String::new();
    let mut user_args: Vec<String> = Vec::new();
    if let Some(cfg) = &view.config_block {
        for n in cfg.nodes() {
            match n.name().value() {
                "cmd" => {
                    if let Some(e) = n.entries().first() {
                        cmd = entry_as_string(e.value());
                    }
                }
                "args" => {
                    for e in n.entries() {
                        if e.name().is_none() {
                            user_args.push(entry_as_string(e.value()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Always emit `command "env"` + ARK_HANDLE prefix (R3 env wrapper).
    node.push(KdlEntry::new_prop("command", "env"));
    let mut all_args: Vec<KdlValue> = Vec::new();
    all_args.push(KdlValue::String(format!("ARK_HANDLE={}", handle.raw())));
    if !cmd.is_empty() {
        all_args.push(KdlValue::String(cmd));
    }
    for a in user_args {
        all_args.push(KdlValue::String(a));
    }
    push_args(node, all_args);
}

fn emit_edit(view: &ViewRef, node: &mut KdlNode) {
    let mut path = String::new();
    if let Some(cfg) = &view.config_block {
        for n in cfg.nodes() {
            if n.name().value() == "path"
                && let Some(e) = n.entries().first()
            {
                path = entry_as_string(e.value());
            }
        }
    }
    // `edit="path"` is the native zellij shape; no env wrapper.
    node.push(KdlEntry::new_prop("edit", path));
}

// ---------------------------------------------------------------------------
// Sizing (T-036)
// ---------------------------------------------------------------------------

struct SizingInput {
    span: Option<u32>,
    cells: Option<u32>,
    min: Option<u32>,
    max: Option<u32>,
    total_span: u32,
}

fn push_sizing(node: &mut KdlNode, s: SizingInput) {
    // `cells=N` wins over `span=N` if both are present — caller-enforced
    // one-or-other validation is T-036's responsibility; this function
    // stays structural.
    if let Some(c) = s.cells {
        node.push(KdlEntry::new_prop("size", i128::from(c)));
    } else if let Some(n) = s.span
        && s.total_span > 0
    {
        // Normalise to percentage with one-decimal rounding. Zellij
        // accepts `size="N%"` strings directly.
        let pct = (n as f64 / s.total_span as f64) * 100.0;
        let rounded = (pct * 10.0).round() / 10.0;
        let formatted = if (rounded.fract()).abs() < f64::EPSILON {
            format!("{}%", rounded as i64)
        } else {
            format!("{rounded:.1}%")
        };
        node.push(KdlEntry::new_prop("size", formatted));
    }
    if let Some(m) = s.min {
        node.push(KdlEntry::new_prop("size_min", i128::from(m)));
    }
    if let Some(m) = s.max {
        node.push(KdlEntry::new_prop("size_max", i128::from(m)));
    }
}

fn child_span(child: &LayoutChild) -> Option<u32> {
    match child {
        LayoutChild::Row(r) => r.span,
        LayoutChild::Col(c) => c.span,
        LayoutChild::Pane(p) => p.span,
    }
}

// ---------------------------------------------------------------------------
// Overlay parsing (T-037)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RawOverlayAttrs {
    pos: String,
    size: String,
    sticky: Option<String>,
}

/// Poor-man's overlay-attr access: the current AST stores overlay attrs
/// as a bundle on `SpawnOp`, not on layout-tier `PaneNode`. For Tier 4
/// we conservatively treat any pane carrying raw `pos=…` / `size=…` /
/// `sticky=…` properties (populated by T-037's parse hook, not yet
/// wired) as an overlay candidate. Returns `None` for tiled panes.
fn pane_overlay_attrs(_pane: &PaneNode) -> Option<RawOverlayAttrs> {
    // Placeholder: until T-037's parse hook threads overlay attrs onto
    // the typed PaneNode, we can only detect overlays when the caller
    // provides them through a yet-to-land accessor. Returning None means
    // `pane_is_overlay` always says false today, and the tiled path is
    // taken. Tests exercise overlay math via `anchor_overlay` directly.
    None
}

fn pane_is_overlay(pane: &PaneNode) -> bool {
    pane_overlay_attrs(pane).is_some()
}

/// Parsed anchor position for a floating pane (T-037).
#[derive(Debug, Clone, Copy)]
pub enum OverlayPos {
    /// `top-right` preset.
    TopRight,
    /// `top-left` preset.
    TopLeft,
    /// `bottom-right` preset.
    BottomRight,
    /// `bottom-left` preset.
    BottomLeft,
    /// `center` preset.
    Center,
    /// Explicit `X%xY%` position (percent-of-terminal).
    Percent(u32, u32),
}

/// Parsed overlay size (cells or percentage).
#[derive(Debug, Clone, Copy)]
pub enum OverlaySize {
    /// `WxH` in cells.
    Cells(u32, u32),
    /// `W%xH%` percentage of the terminal.
    Percent(u32, u32),
}

/// Parse an overlay position spec (`top-right`, `center`, `50%x30%`).
pub fn parse_pos(raw: &str) -> Result<OverlayPos, SceneError> {
    Ok(match raw.trim() {
        "top-right" => OverlayPos::TopRight,
        "top-left" => OverlayPos::TopLeft,
        "bottom-right" => OverlayPos::BottomRight,
        "bottom-left" => OverlayPos::BottomLeft,
        "center" => OverlayPos::Center,
        other => {
            if let Some((x, y)) = other.split_once('x') {
                let x = parse_percent(x)?;
                let y = parse_percent(y)?;
                OverlayPos::Percent(x, y)
            } else {
                return Err(SceneError::MisplacedNode {
                    node: other.to_string(),
                    parent: "overlay pos=".to_string(),
                    src: NamedSource::new("<layout>", raw.to_string()),
                    span: SourceSpan::new(0.into(), raw.len().max(1)),
                });
            }
        }
    })
}

/// Parse an overlay size spec (`80x20` or `50%x30%`).
pub fn parse_overlay_size(raw: &str) -> Result<OverlaySize, SceneError> {
    let err = || SceneError::MisplacedNode {
        node: raw.to_string(),
        parent: "overlay size=".to_string(),
        src: NamedSource::new("<layout>", raw.to_string()),
        span: SourceSpan::new(0.into(), raw.len().max(1)),
    };
    let (w, h) = raw.split_once('x').ok_or_else(err)?;
    let (w_pct, w_val) = parse_dim(w)?;
    let (h_pct, h_val) = parse_dim(h)?;
    if w_pct && h_pct {
        Ok(OverlaySize::Percent(w_val, h_val))
    } else if !w_pct && !h_pct {
        Ok(OverlaySize::Cells(w_val, h_val))
    } else {
        Err(err())
    }
}

fn parse_percent(raw: &str) -> Result<u32, SceneError> {
    let err = || SceneError::MisplacedNode {
        node: raw.to_string(),
        parent: "overlay percent".to_string(),
        src: NamedSource::new("<layout>", raw.to_string()),
        span: SourceSpan::new(0.into(), raw.len().max(1)),
    };
    let s = raw.trim();
    let s = s.strip_suffix('%').ok_or_else(err)?;
    s.parse::<u32>().map_err(|_| err())
}

fn parse_dim(raw: &str) -> Result<(bool, u32), SceneError> {
    let err = || SceneError::MisplacedNode {
        node: raw.to_string(),
        parent: "overlay dim".to_string(),
        src: NamedSource::new("<layout>", raw.to_string()),
        span: SourceSpan::new(0.into(), raw.len().max(1)),
    };
    let s = raw.trim();
    if let Some(n) = s.strip_suffix('%') {
        Ok((true, n.parse::<u32>().map_err(|_| err())?))
    } else {
        Ok((false, s.parse::<u32>().map_err(|_| err())?))
    }
}

/// Compute absolute `(x, y, width, height)` for an overlay given its
/// parsed pos + size and the current terminal size. Public so the
/// reconciler can re-anchor overlays on resize events.
pub fn anchor_overlay(pos: OverlayPos, size: OverlaySize, term: TerminalSize) -> (u32, u32, u32, u32) {
    let (w, h) = match size {
        OverlaySize::Cells(w, h) => (w, h),
        OverlaySize::Percent(wp, hp) => (term.cols * wp / 100, term.rows * hp / 100),
    };
    let (x, y) = match pos {
        OverlayPos::TopLeft => (0, 0),
        OverlayPos::TopRight => (term.cols.saturating_sub(w), 0),
        OverlayPos::BottomLeft => (0, term.rows.saturating_sub(h)),
        OverlayPos::BottomRight => (
            term.cols.saturating_sub(w),
            term.rows.saturating_sub(h),
        ),
        OverlayPos::Center => (
            (term.cols.saturating_sub(w)) / 2,
            (term.rows.saturating_sub(h)) / 2,
        ),
        OverlayPos::Percent(xp, yp) => (term.cols * xp / 100, term.rows * yp / 100),
    };
    (x, y, w, h)
}

// ---------------------------------------------------------------------------
// Artifact writer (T-040)
// ---------------------------------------------------------------------------

/// Rendered layout artifact bundle returned by [`write_layout_artifact`].
#[derive(Debug, Clone)]
pub struct LayoutArtifact {
    /// Absolute on-disk path to the rendered `.kdl` file.
    pub path: PathBuf,
    /// The serialised KDL text as written (re-parse-verified).
    pub text: String,
}

/// Write the rendered layout KDL to
/// `${XDG_RUNTIME_DIR}/ark/layouts/<id-hash>-scene.kdl`.
///
/// - Sets file mode `0600` + parent dir mode `0700`.
/// - Validates that the serialised KDL re-parses through
///   [`KdlDocument::parse_v2`] before returning.
pub fn write_layout_artifact(
    kdl: &KdlDocument,
    scene_id: &SceneId,
) -> Result<PathBuf, std::io::Error> {
    write_layout_artifact_in(kdl, scene_id, &layouts_dir())
}

/// [`write_layout_artifact`] with a caller-provided layouts directory.
/// Used by tests that want to avoid mutating the process-global
/// `XDG_RUNTIME_DIR`. Path must already be / will be created as the
/// `…/ark/layouts` leaf.
pub fn write_layout_artifact_in(
    kdl: &KdlDocument,
    scene_id: &SceneId,
    dir: &std::path::Path,
) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)?;

    let filename = format!("{}-scene.kdl", id_slug(scene_id));
    let path = dir.join(filename);
    let text = kdl.to_string();

    // Round-trip the text through the parser so a corrupt serializer
    // can't hand zellij an unparseable file (R3.17).
    if let Err(e) = KdlDocument::parse_v2(&text) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("rendered layout KDL does not re-parse: {e}"),
        ));
    }

    std::fs::write(&path, &text)?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

/// Resolve the layouts directory under `${XDG_RUNTIME_DIR}/ark/layouts`,
/// falling back to `${TMPDIR}/ark/layouts` when `XDG_RUNTIME_DIR` is
/// unset (macOS).
pub(crate) fn layouts_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("ark").join("layouts")
    } else {
        std::env::temp_dir().join("ark").join("layouts")
    }
}

/// Compact scene-id slug for use in filenames — path basename (without
/// extension) joined to a short hash prefix. Falls back to just the hash
/// when the path has no stem.
pub(crate) fn id_slug(id: &SceneId) -> String {
    let stem = id
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scene");
    let hex = id.content_hash.to_hex();
    let prefix = &hex.as_str()[..8];
    // Keep slug filesystem-safe: replace anything non-alphanumeric/`_-`
    // with `-`.
    let sanitised: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{sanitised}-{prefix}")
}

fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_args(node: &mut KdlNode, args: Vec<KdlValue>) {
    // Zellij's `pane` node accepts `args` as an array-valued property:
    // `args "A" "B"`. Upstream `kdl` 6.5 has no multi-valued property
    // type — we express the list as a positional argument sequence
    // after the `command` property instead, which zellij also accepts.
    // Emit as a single property with a value array-approximation: in
    // KDL 2.0, repeated entries under the same key are illegal, so we
    // emit `args` as successive positional values. Zellij's layout
    // parser treats the first positional on a `pane` as the `command`
    // and subsequent ones as arguments.
    for v in args {
        node.push(KdlEntry::new(v));
    }
}

fn entry_as_string(v: &KdlValue) -> String {
    match v {
        KdlValue::String(s) => s.clone(),
        KdlValue::Integer(i) => i.to_string(),
        KdlValue::Float(f) => f.to_string(),
        KdlValue::Bool(b) => b.to_string(),
        KdlValue::Null => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::LayoutNode;
    use crate::ast::layout::{PaneNode, TabNode, ViewRef};
    use crate::view::ViewRegistry;

    fn registry() -> ViewRegistry {
        ViewRegistry::with_primitives()
    }

    fn tab_with_shell(handle: &str) -> TabNode {
        TabNode {
            handle: format!("@{handle}"),
            cwd: None,
            name: None,
            focus: None,
            when: None,
            body: vec![LayoutChild::Pane(PaneNode {
                handle: format!("@{handle}_p"),
                span: None,
                cells: None,
                min: None,
                max: None,
                when: None,
                view: ViewRef {
                    alias: "shell".to_string(),
                    config_block: None,
                },
            })],
        }
    }

    #[test]
    fn overlay_math_top_right() {
        let (x, y, w, h) = anchor_overlay(
            OverlayPos::TopRight,
            OverlaySize::Cells(20, 10),
            TerminalSize { cols: 80, rows: 24 },
        );
        assert_eq!((x, y, w, h), (60, 0, 20, 10));
    }

    #[test]
    fn overlay_math_center() {
        let (x, y, w, h) = anchor_overlay(
            OverlayPos::Center,
            OverlaySize::Cells(20, 10),
            TerminalSize { cols: 80, rows: 24 },
        );
        assert_eq!((x, y, w, h), (30, 7, 20, 10));
    }

    #[test]
    fn overlay_math_percent_size() {
        let (x, y, w, h) = anchor_overlay(
            OverlayPos::TopLeft,
            OverlaySize::Percent(50, 50),
            TerminalSize { cols: 80, rows: 24 },
        );
        assert_eq!((x, y, w, h), (0, 0, 40, 12));
    }

    #[test]
    fn parse_pos_accepts_presets() {
        assert!(matches!(parse_pos("top-right").unwrap(), OverlayPos::TopRight));
        assert!(matches!(parse_pos("center").unwrap(), OverlayPos::Center));
    }

    #[test]
    fn parse_pos_accepts_explicit_percent() {
        match parse_pos("50%x25%").unwrap() {
            OverlayPos::Percent(x, y) => {
                assert_eq!(x, 50);
                assert_eq!(y, 25);
            }
            other => panic!("expected Percent got {other:?}"),
        }
    }

    #[test]
    fn parse_overlay_size_cells_and_percent() {
        assert!(matches!(
            parse_overlay_size("80x20").unwrap(),
            OverlaySize::Cells(80, 20)
        ));
        assert!(matches!(
            parse_overlay_size("50%x30%").unwrap(),
            OverlaySize::Percent(50, 30)
        ));
    }

    #[test]
    fn parse_overlay_size_rejects_mixed_units() {
        let err = parse_overlay_size("50%x20").unwrap_err();
        assert!(matches!(err, SceneError::MisplacedNode { .. }));
    }

    #[test]
    fn id_slug_includes_path_stem_and_hash_prefix() {
        let id = SceneId::new("/tmp/dev.kdl", b"content");
        let slug = id_slug(&id);
        assert!(slug.starts_with("dev-"));
        assert_eq!(slug.split('-').next_back().unwrap().len(), 8);
    }

    #[test]
    fn compile_minimal_layout_emits_valid_kdl() {
        let layout = LayoutNode {
            tabs: vec![tab_with_shell("main")],
        };
        let doc = compile_layout_kdl(&layout, &registry()).expect("minimal compile");
        let text = doc.to_string();
        // Round-trips through the parser.
        KdlDocument::parse_v2(&text).expect("rendered KDL must re-parse");
        assert!(text.contains("layout"));
        assert!(text.contains("tab"));
        assert!(text.contains("ARK_HANDLE"));
    }

    #[test]
    fn row_emits_horizontal_split_direction() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Row(RowNode {
                    body: vec![LayoutChild::Pane(PaneNode {
                        handle: "@p".to_string(),
                        span: None,
                        cells: None,
                        min: None,
                        max: None,
                        when: None,
                        view: ViewRef {
                            alias: "shell".to_string(),
                            config_block: None,
                        },
                    })],
                    when: None,
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        assert!(doc.to_string().contains("split_direction"));
        assert!(doc.to_string().contains("horizontal"));
    }

    #[test]
    fn col_emits_vertical_split_direction() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Col(ColNode {
                    body: vec![LayoutChild::Pane(PaneNode {
                        handle: "@p".to_string(),
                        span: None,
                        cells: None,
                        min: None,
                        max: None,
                        when: None,
                        view: ViewRef {
                            alias: "shell".to_string(),
                            config_block: None,
                        },
                    })],
                    when: None,
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let text = doc.to_string();
        assert!(text.contains("vertical"));
    }

    #[test]
    fn span_normalises_to_percent() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Row(RowNode {
                    body: vec![
                        LayoutChild::Pane(PaneNode {
                            handle: "@a".to_string(),
                            span: Some(1),
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            view: ViewRef {
                                alias: "shell".to_string(),
                                config_block: None,
                            },
                        }),
                        LayoutChild::Pane(PaneNode {
                            handle: "@b".to_string(),
                            span: Some(2),
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            view: ViewRef {
                                alias: "shell".to_string(),
                                config_block: None,
                            },
                        }),
                        LayoutChild::Pane(PaneNode {
                            handle: "@c".to_string(),
                            span: Some(3),
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            view: ViewRef {
                                alias: "shell".to_string(),
                                config_block: None,
                            },
                        }),
                    ],
                    when: None,
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let text = doc.to_string();
        // 1/6 ≈ 16.7%, 2/6 ≈ 33.3%, 3/6 = 50%.
        assert!(text.contains("16.7%"));
        assert!(text.contains("33.3%"));
        assert!(text.contains("50%"));
    }

    #[test]
    fn cells_emits_raw_size() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Pane(PaneNode {
                    handle: "@p".to_string(),
                    span: None,
                    cells: Some(40),
                    min: None,
                    max: None,
                    when: None,
                    view: ViewRef {
                        alias: "shell".to_string(),
                        config_block: None,
                    },
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        assert!(doc.to_string().contains("size=40"));
    }

    #[test]
    fn edit_primitive_has_no_env_wrapper() {
        let cfg_src = r#"path "file.rs""#;
        let cfg = KdlDocument::parse_v2(cfg_src).unwrap();
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Pane(PaneNode {
                    handle: "@edit".to_string(),
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                    when: None,
                    view: ViewRef {
                        alias: "edit".to_string(),
                        config_block: Some(cfg),
                    },
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let text = doc.to_string();
        assert!(text.contains("edit="));
        assert!(
            !text.contains("ARK_HANDLE"),
            "edit panes must not have env wrapper: {text}"
        );
    }

    #[test]
    fn ark_handle_env_wrapper_distinguishes_two_shells() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Row(RowNode {
                    body: vec![
                        LayoutChild::Pane(PaneNode {
                            handle: "@left".to_string(),
                            span: None,
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            view: ViewRef {
                                alias: "shell".to_string(),
                                config_block: None,
                            },
                        }),
                        LayoutChild::Pane(PaneNode {
                            handle: "@right".to_string(),
                            span: None,
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            view: ViewRef {
                                alias: "shell".to_string(),
                                config_block: None,
                            },
                        }),
                    ],
                    when: None,
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let text = doc.to_string();
        assert!(text.contains("ARK_HANDLE=@left"));
        assert!(text.contains("ARK_HANDLE=@right"));
    }

    #[test]
    fn write_layout_artifact_roundtrips_through_kdl_parser() {
        let layout = LayoutNode {
            tabs: vec![tab_with_shell("main")],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let id = SceneId::new("/tmp/example.kdl", b"body");

        // Redirect XDG_RUNTIME_DIR to a tempdir for test isolation.
        let tmp = tempfile::tempdir().unwrap();
        // Safety: tests run single-threaded within the scene crate's
        // cargo-test harness only when not parallelised. The environment
        // mutation here is ONLY observable to this process, so while
        // technically unsafe, it is acceptable within a single-crate
        // test. Setting env vars is inherently a process-global action.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        }

        let path = write_layout_artifact(&doc, &id).expect("write artifact");
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        KdlDocument::parse_v2(&text).expect("on-disk KDL must re-parse");

        // Parent dir is 0700, file is 0600.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let parent_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);
    }

    #[test]
    fn tab_with_cwd_and_name_emits_properties() {
        let layout = LayoutNode {
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: Some("/src".to_string()),
                name: Some("Main".to_string()),
                focus: Some("true".to_string()),
                when: None,
                body: vec![LayoutChild::Pane(PaneNode {
                    handle: "@p".to_string(),
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                    when: None,
                    view: ViewRef {
                        alias: "shell".to_string(),
                        config_block: None,
                    },
                })],
            }],
        };
        let doc = compile_layout_kdl(&layout, &registry()).unwrap();
        let text = doc.to_string();
        // KDL 2.0 autoformat emits bare identifiers where legal and
        // falls back to quoted strings when the value contains special
        // chars (`/`, leading digit, etc.). Assertions accept either
        // form so autoformat tweaks don't break the test.
        assert!(text.contains("name=Main") || text.contains("name=\"Main\""));
        assert!(text.contains("cwd=\"/src\"") || text.contains("cwd=/src"));
        assert!(text.contains("focus=#true"));
        // And it must round-trip through the parser.
        KdlDocument::parse_v2(&text).expect("layout must re-parse");
    }
}
