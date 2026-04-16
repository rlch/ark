//! Layout subtree AST types (R3 of `context/kits/cavekit-scene.md`).
//!
//! Every struct here maps to a single KDL node shape that appears inside a
//! `layout { … }` or `mode { … }` block. Each `#[derive(Facet)]` field
//! carries a Rust `///` doc-comment: facet's `SHAPE` reflection surfaces
//! these doc-comments as LSP hover documentation (see
//! `context/impl/impl-scene-architecture-v3.md` §"Layout subtree"), so the
//! doc-comments are load-bearing, not cosmetic.
//!
//! Span information is tracked automatically by facet-kdl's deserializer
//! and surfaces through `KdlDeserializeError` → `miette::Diagnostic`;
//! individual AST nodes do NOT carry their own span fields.
//!
//! Note: the task T-004 scope is the AST *shape*. Actual parsing of these
//! types against `layout { … }` source happens in T-011 (`parse_scene`).
//! Semantic validation (exactly one focus per layout, sizing consistency,
//! overlay attr parsing) happens in T-036 / T-037. View resolution against
//! the `ViewRegistry` happens in T-026+.

use facet::Facet;
use ::kdl::KdlDocument;

// ---------------------------------------------------------------------------
// Handles (R3 — `@handle` required on every tab + pane)
// ---------------------------------------------------------------------------

/// Pane / tab identity key.
///
/// Stored as the full `@name` form (e.g. `@main`, `@editor_1`) so downstream
/// renderers can emit the raw token when useful, while [`Handle::name`]
/// exposes the bare identifier (`main`, `editor_1`) for reconciler identity
/// lookups (R3 env-wrapper: `ARK_HANDLE=@<handle>`).
///
/// Newtype rather than `String` so misuse is caught at the type system
/// level. Construction goes through [`Handle::new`] which validates the
/// `@<ident>` prefix; a bare identifier without the leading `@` is
/// rejected.
///
/// The inner `String` field is left public to keep the
/// `#[repr(transparent)]` layout contract discoverable — a custom
/// facet-kdl deserializer can be layered in T-011+ to parse `@handle`
/// tokens directly, at which point the public field keeps round-tripping
/// simple.
#[derive(Facet, Debug, Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Handle(
    /// Raw handle token including the leading `@`.
    pub String,
);

impl Handle {
    /// Construct a handle from its raw `@name` form, validating the
    /// `@<ident>` prefix.
    ///
    /// Rejected inputs:
    /// - missing leading `@` (e.g. `main`)
    /// - `@` on its own (no identifier)
    /// - the empty string
    /// - whitespace or embedded `@` inside the identifier (e.g. `@ x`,
    ///   ` @x`, `@x@`)
    pub fn new(raw: &str) -> Result<Self, HandleParseError> {
        if !raw.starts_with('@') {
            return Err(HandleParseError::MissingAtPrefix);
        }
        let ident = &raw[1..];
        if ident.is_empty() {
            return Err(HandleParseError::EmptyName);
        }
        for ch in ident.chars() {
            if ch.is_whitespace() || ch == '@' {
                return Err(HandleParseError::InvalidChar(ch));
            }
        }
        Ok(Self(raw.to_string()))
    }

    /// The bare identifier (everything after the leading `@`).
    pub fn name(&self) -> &str {
        &self.0[1..]
    }

    /// The full `@name` form as written in the scene source.
    pub fn raw(&self) -> &str {
        &self.0
    }
}

/// Errors produced by [`Handle::new`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum HandleParseError {
    /// The handle source did not start with `@`.
    #[error("handle must start with '@'")]
    MissingAtPrefix,

    /// The handle was just `@` with no identifier, or an empty string.
    #[error("handle name must be non-empty")]
    EmptyName,

    /// The handle contained whitespace or another invalid character.
    #[error("handle name contains whitespace or invalid char: `{0}`")]
    InvalidChar(char),
}

// ---------------------------------------------------------------------------
// Tab node (R3 — `tab @handle { … }`)
// ---------------------------------------------------------------------------

/// `tab @handle cwd=… name=… focus=… when=… { row|col|pane|… }` — a tab
/// in a `layout { … }` block.
///
/// All string attributes that admit Rhai interpolation (`cwd`, `name`) are
/// stored raw here; actual interpolation happens in T-022 (`{Rhai}` hole
/// expansion) during compile. The `when` predicate is likewise kept raw so
/// the Rhai engine (T-019) can compile it against the event scope later.
#[derive(Facet, Debug, Clone)]
pub struct TabNode {
    /// Identity key for reconciler match + op targeting (R3 — `@handle`
    /// required on every tab).
    pub handle: Handle,

    /// Working directory for panes in this tab (R3 `cwd` attr). Raw
    /// string; Rhai holes expanded at spawn in T-022 / T-024.
    pub cwd: Option<String>,

    /// Display name in the tab bar (R3 `name` attr). Defaults to the
    /// handle identifier when absent. Raw string; Rhai interpolation
    /// applied at spawn.
    pub name: Option<String>,

    /// Initial focus for the session (R3 `focus` attr). Exactly one
    /// focused tab per layout is validated in T-036.
    pub focus: Option<bool>,

    /// Conditional-existence predicate (R3 `when=` on tab). Raw Rhai
    /// source; compiled in T-023, evaluated by the reconciler on
    /// context change to include / exclude this tab from the rendered
    /// layout.
    pub when: Option<String>,

    /// Nested `row` / `col` / `pane` children. Heterogeneous body kept
    /// in source order so the compiler can render zellij-compatible
    /// split direction nesting verbatim.
    pub body: Vec<LayoutChild>,
}

// ---------------------------------------------------------------------------
// Layout children — what can appear inside a tab or nested split
// ---------------------------------------------------------------------------

/// Children admissible inside a `tab { … }` body or nested inside a
/// `row { … }` / `col { … }` container (R3 — `row` / `col` / `pane`).
#[derive(Facet, Debug, Clone)]
#[repr(u8)]
pub enum LayoutChild {
    /// Horizontal split container.
    Row(RowNode),
    /// Vertical split container.
    Col(ColNode),
    /// Leaf pane running a view.
    Pane(PaneNode),
}

// ---------------------------------------------------------------------------
// Row / Col (R3 — split containers)
// ---------------------------------------------------------------------------

/// `row { row|col|pane … }` — horizontal split container (R3).
///
/// Rows and columns themselves accept sizing attributes (`span` / `cells`
/// / `min` / `max`) because they can be siblings of other sized children
/// inside a parent row or column. The sizing applies only when this node
/// is a child of another sized container; T-036 validates that sizing on
/// a root-level row / col is inert but not an error.
#[derive(Facet, Debug, Clone)]
pub struct RowNode {
    /// Nested `row` / `col` / `pane` children in source order.
    pub body: Vec<LayoutChild>,

    /// Optional `when=` predicate for conditional inclusion (R3 `when=`
    /// legal on rows / cols). Raw Rhai source; compiled in T-023.
    pub when: Option<String>,

    /// Sizing attributes (`span`, `cells`, `min`, `max`) when this row
    /// is a sized sibling inside a parent container. Only one of
    /// `span` / `cells` is normally set — T-036 validates.
    pub sizing: SizingAttrs,
}

/// `col { row|col|pane … }` — vertical split container (R3).
///
/// Same shape as [`RowNode`]; split direction differs at compile time
/// (row → `split_direction="horizontal"`, col →
/// `split_direction="vertical"`).
#[derive(Facet, Debug, Clone)]
pub struct ColNode {
    /// Nested `row` / `col` / `pane` children in source order.
    pub body: Vec<LayoutChild>,

    /// Optional `when=` predicate for conditional inclusion (R3 `when=`
    /// legal on rows / cols). Raw Rhai source; compiled in T-023.
    pub when: Option<String>,

    /// Sizing attributes (`span`, `cells`, `min`, `max`) when this col
    /// is a sized sibling inside a parent container. Only one of
    /// `span` / `cells` is normally set — T-036 validates.
    pub sizing: SizingAttrs,
}

// ---------------------------------------------------------------------------
// Pane (R3 — leaf with exactly one view child)
// ---------------------------------------------------------------------------

/// `pane @handle … { <view> }` — leaf layout node running a view (R3).
///
/// Panes always carry an `@handle` (reconciler identity key) and MUST
/// contain exactly one view child. Zero or multiple view children =
/// compile error (R3), enforced in T-036.
#[derive(Facet, Debug, Clone)]
pub struct PaneNode {
    /// Identity key for reconciler match + op targeting (R3 — `@handle`
    /// required on every pane).
    pub handle: Handle,

    /// Sizing attributes (`span`, `cells`, `min`, `max`) when this pane
    /// is a sized sibling of other panes / splits inside a parent
    /// container. T-036 validates that `span` and `cells` aren't both
    /// set.
    pub sizing: SizingAttrs,

    /// Overlay (floating-pane) attributes (R3 — `pane @h overlay pos=…
    /// size=…`). `None` = tiled pane (the default). `Some` = floating
    /// pane; inner strings are raw and parsed / validated in T-037.
    pub overlay: Option<OverlayAttrs>,

    /// Optional `when=` predicate for conditional inclusion (R3 `when=`
    /// legal on panes). Raw Rhai source; compiled in T-023.
    pub when: Option<String>,

    /// The view that fills this pane (R6 views). Exactly one view per
    /// pane; zero or multiple view child nodes = compile error (R3).
    /// Marked `#[facet(opaque)]` because `ViewRef` holds a foreign
    /// `kdl::KdlDocument` and the facet `SHAPE` only needs to announce
    /// the field's existence — full tracing happens in T-026+ against
    /// each view's own facet schema.
    #[facet(opaque)]
    pub view: ViewRef,
}

// ---------------------------------------------------------------------------
// Overlay + sizing attribute bags (R3)
// ---------------------------------------------------------------------------

/// Overlay / floating-pane attributes carried on `pane @h overlay …` (R3).
///
/// All three fields are stored as raw strings here; the `pos` and `size`
/// grammars (e.g. `top-right`, `60%x40%`, `80x20`) are parsed and
/// validated in T-037. Keeping them as strings at the AST layer means the
/// parser stays schema-agnostic and diagnostics attach to the concrete
/// source span.
#[derive(Facet, Debug, Clone)]
pub struct OverlayAttrs {
    /// Anchor position: one of `top-right`, `top-left`, `bottom-right`,
    /// `bottom-left`, `center`, or an explicit `X%xY%` / cell form.
    /// Raw string here; parsed in T-037.
    pub pos: String,

    /// Overlay dimensions: `WxH` in cells or `W%xH%` in percent of tab.
    /// Raw string here; parsed in T-037.
    pub size: String,

    /// `sticky=true` survives tab switch (compiles to zellij
    /// `pinned=true`). Optional because overlays default to
    /// non-sticky.
    pub sticky: Option<bool>,
}

/// Sizing attributes shared by `row`, `col`, and `pane` siblings (R3).
///
/// `span` and `cells` are mutually exclusive in practice but this AST
/// layer accepts either to keep parsing dumb; T-036 rejects the
/// simultaneous-set case with a dedicated diagnostic. `min` / `max` are
/// always expressed in cells (R3 "bounds in cells").
#[derive(Facet, Debug, Clone, Default)]
pub struct SizingAttrs {
    /// Relative weight within the parent container (R3 `span=N`).
    /// Siblings normalize to 100 % at render time.
    pub span: Option<u32>,

    /// Fixed size in cells (R3 `cells=N`).
    pub cells: Option<u32>,

    /// Lower bound in cells (R3 `min=N`).
    pub min: Option<u32>,

    /// Upper bound in cells (R3 `max=N`).
    pub max: Option<u32>,
}

// ---------------------------------------------------------------------------
// View reference (R6)
// ---------------------------------------------------------------------------

/// Reference to the view that fills a pane (R6 views).
///
/// Holds the view alias as written in the scene source (`command`,
/// `shell`, `status`, or any extension-registered view name) together
/// with an optional raw KDL body. The body is held as `kdl::KdlDocument`
/// so view-specific config flows through untyped at this layer — actual
/// resolution against the `ViewRegistry` and schema validation against
/// the view's facet `SHAPE` happens in T-026+.
///
/// `ViewRef` is not `#[derive(Facet)]`: `kdl::KdlDocument` is a foreign
/// type that does not implement `Facet`. It is a plain field type the
/// T-011 parser materializes manually from the pane's single view child
/// node.
#[derive(Debug, Clone)]
pub struct ViewRef {
    /// View alias as written in the scene source (e.g. `command`,
    /// `shell`, `status`, `nvim`). Resolution goes through the view
    /// registry in T-026; unknown aliases surface as
    /// `error[scene/unknown-view]`.
    pub alias: String,

    /// Raw KDL body of the view child node, when present. Passed
    /// verbatim to the view-config deserializer in T-026+ (per-view
    /// facet `SHAPE`).
    pub config_block: Option<KdlDocument>,
}

// ---------------------------------------------------------------------------
// Tests — handle validation matrix
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_accepts_simple_ident() {
        let h = Handle::new("@main").expect("@main should parse");
        assert_eq!(h.raw(), "@main");
        assert_eq!(h.name(), "main");
    }

    #[test]
    fn handle_accepts_underscore_and_digit() {
        let h = Handle::new("@x_1").expect("@x_1 should parse");
        assert_eq!(h.raw(), "@x_1");
        assert_eq!(h.name(), "x_1");
    }

    #[test]
    fn handle_rejects_missing_at_prefix() {
        let err = Handle::new("main").expect_err("missing @ must reject");
        assert!(matches!(err, HandleParseError::MissingAtPrefix));
    }

    #[test]
    fn handle_rejects_bare_at() {
        let err = Handle::new("@").expect_err("bare @ must reject");
        assert!(matches!(err, HandleParseError::EmptyName));
    }

    #[test]
    fn handle_rejects_leading_whitespace() {
        let err = Handle::new(" @x").expect_err("leading whitespace must reject");
        assert!(matches!(err, HandleParseError::MissingAtPrefix));
    }

    #[test]
    fn handle_rejects_interior_whitespace() {
        let err = Handle::new("@ x").expect_err("interior whitespace must reject");
        assert!(matches!(err, HandleParseError::InvalidChar(' ')));
    }

    #[test]
    fn handle_rejects_empty_string() {
        let err = Handle::new("").expect_err("empty string must reject");
        assert!(matches!(err, HandleParseError::MissingAtPrefix));
    }
}
