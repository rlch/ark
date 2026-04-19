//! Handle validation pass (T-014).
//!
//! Walks the typed [`SceneIR`] AST and enforces the R2 handle rules:
//!
//! 1. Every `tab` and `pane` must carry a non-empty handle string.
//! 2. Every handle must pass [`Handle::new`] — valid `@<ident>` grammar.
//!    Invalid or empty handles surface as `error[scene/handle-missing]`.
//! 3. Flat scene-scoped namespace: tab + pane handles share a single
//!    namespace. Duplicate handle names produce `error[scene/handle-clash]`.
//! 4. Handles inside `mode` blocks participate in the same namespace
//!    (R9 — handles survive swap).

use miette::{NamedSource, SourceSpan};

use crate::ast::layout::{ColNode, Handle, LayoutChild, PaneNode, RowNode, StackNode, TabNode};
use crate::ast::{LayoutNode, ModeNode, SceneBodyNode};
use crate::error::SceneError;
use crate::parse::SceneIR;

/// A collected handle occurrence: the raw handle string, its source context
/// label (e.g. `"tab"` or `"pane"`), and the block it came from
/// (`"layout"` or `"mode:<name>"`).
struct HandleEntry {
    /// The raw handle string as written in source (e.g. `"@main"`, `"main"`, `""`).
    raw: String,
    /// Node kind: `"tab"` or `"pane"`.
    node_kind: &'static str,
    /// Context label for diagnostics (e.g. `"layout"`, `"mode:review"`).
    _context: String,
}

/// Validate handle rules across the entire scene AST.
///
/// Returns an empty `Vec` when all handles are valid and unique. Returns
/// one [`SceneError`] per violation otherwise.
pub fn validate_handles(ir: &SceneIR) -> Vec<SceneError> {
    let mut errors = Vec::new();
    let mut entries: Vec<HandleEntry> = Vec::new();
    let path = ir.path.display().to_string();

    // Collect all handle entries from layout and mode blocks.
    for node in &ir.scene.body {
        match node {
            SceneBodyNode::Layout(layout) => {
                collect_layout_handles(layout, "layout", &mut entries);
            }
            SceneBodyNode::Mode(mode) => {
                let ctx = format!("mode:{}", mode.name);
                collect_mode_handles(mode, &ctx, &mut entries);
            }
            _ => {}
        }
    }

    // Phase 1: validate each handle individually (non-empty + @ident grammar).
    for entry in &entries {
        if entry.raw.is_empty() {
            errors.push(SceneError::HandleMissing {
                node: entry.node_kind,
                src: NamedSource::new(path.clone(), ir.src.clone()),
                span: SourceSpan::new(0.into(), 0),
            });
            continue;
        }

        if Handle::new(&entry.raw).is_err() {
            errors.push(SceneError::HandleMissing {
                node: entry.node_kind,
                src: NamedSource::new(path.clone(), ir.src.clone()),
                span: SourceSpan::new(0.into(), 0),
            });
        }
    }

    // Phase 2: detect duplicates across the flat namespace.
    // Only consider entries that passed grammar validation (have a valid @ident).
    let valid_entries: Vec<&HandleEntry> = entries
        .iter()
        .filter(|e| !e.raw.is_empty() && Handle::new(&e.raw).is_ok())
        .collect();

    // Use a simple O(n^2) scan — handle counts are tiny in practice.
    let mut seen_indices: Vec<usize> = Vec::new();
    for (i, a) in valid_entries.iter().enumerate() {
        if seen_indices.contains(&i) {
            continue;
        }
        for (j, b) in valid_entries.iter().enumerate().skip(i + 1) {
            if a.raw == b.raw && !seen_indices.contains(&j) {
                seen_indices.push(j);
                errors.push(SceneError::HandleClash {
                    handle: a.raw.clone(),
                    src: NamedSource::new(path.clone(), ir.src.clone()),
                    first: SourceSpan::new(0.into(), 0),
                    second: SourceSpan::new(0.into(), 0),
                });
            }
        }
    }

    errors
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

/// Collect handles from a `layout { }` block.
fn collect_layout_handles(layout: &LayoutNode, context: &str, entries: &mut Vec<HandleEntry>) {
    for tab in &layout.tabs {
        collect_tab_handles(tab, context, entries);
    }
}

/// Collect handles from a `mode { }` block.
fn collect_mode_handles(mode: &ModeNode, context: &str, entries: &mut Vec<HandleEntry>) {
    for tab in &mode.tabs {
        collect_tab_handles(tab, context, entries);
    }
}

/// Collect the tab's own handle, then recurse into its body.
fn collect_tab_handles(tab: &TabNode, context: &str, entries: &mut Vec<HandleEntry>) {
    entries.push(HandleEntry {
        raw: tab.handle.clone(),
        node_kind: "tab",
        _context: context.to_string(),
    });
    for child in &tab.body {
        collect_layout_child_handles(child, context, entries);
    }
}

/// Recurse through `LayoutChild` variants.
fn collect_layout_child_handles(
    child: &LayoutChild,
    context: &str,
    entries: &mut Vec<HandleEntry>,
) {
    match child {
        LayoutChild::Row(row) => collect_row_handles(row, context, entries),
        LayoutChild::Col(col) => collect_col_handles(col, context, entries),
        LayoutChild::Pane(pane) => collect_pane_handle(pane, context, entries),
        LayoutChild::Stack(stack) => collect_stack_handle(stack, context, entries),
    }
}

fn collect_row_handles(row: &RowNode, context: &str, entries: &mut Vec<HandleEntry>) {
    for child in &row.body {
        collect_layout_child_handles(child, context, entries);
    }
}

fn collect_col_handles(col: &ColNode, context: &str, entries: &mut Vec<HandleEntry>) {
    for child in &col.body {
        collect_layout_child_handles(child, context, entries);
    }
}

fn collect_pane_handle(pane: &PaneNode, context: &str, entries: &mut Vec<HandleEntry>) {
    entries.push(HandleEntry {
        raw: pane.handle.clone(),
        node_kind: "pane",
        _context: context.to_string(),
    });
}

/// scene-2026-04-18 T-011: stacks share the flat scene-scoped handle
/// namespace with tabs + panes. Duplicate `@h` across any combination
/// surfaces as `error[scene/handle-clash]` via the Phase-2 duplicate
/// detector (Phase 2 below). Nested stack / pane children are walked
/// recursively.
fn collect_stack_handle(stack: &StackNode, context: &str, entries: &mut Vec<HandleEntry>) {
    entries.push(HandleEntry {
        raw: stack.handle.clone(),
        node_kind: "stack",
        _context: context.to_string(),
    });
    for child in &stack.body {
        collect_layout_child_handles(child, context, entries);
    }
}
