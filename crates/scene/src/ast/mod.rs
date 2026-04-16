//! Core AST node types for the v3 scene grammar (R1 / R2).
//!
//! One file per scene-root concern, populated across Tier 0 tasks:
//!
//! - this module (T-003): scene-root node types — [`SceneNode`],
//!   [`SceneBodyNode`], [`UseNode`], [`IncludeNode`], [`LayoutNode`],
//!   [`ModeNode`], [`OnNode`], [`BindNode`], [`ClearReactionsNode`],
//!   [`ClearBindNode`], [`DisableExtensionNode`].
//! - [`layout`] (T-004): `TabNode`, `RowNode`, `ColNode`, `PaneNode`, sizing
//!   + overlay attrs.
//! - [`ops`] (T-005): one facet-derived struct per canonical op verb (R7).
//! - [`selector`] (T-009): `EventSelector`, `FieldPattern`, `MatchType`.
//!
//! Span preservation is handled by `facet-kdl` internally — each parsed node
//! carries source spans through the derive macro, surfaced via miette
//! diagnostics in T-006 / T-011. No manual span field is required.

pub mod layout;
pub mod ops;
pub mod selector;

use facet::Facet;
use facet_kdl as kdl;
use ::kdl::KdlDocument;

use crate::ast::layout::TabNode;
use crate::ast::ops::OpNode;
use crate::ast::selector::EventSelector;

/// Top-level `scene "<name>" { … }` node — the single root of every scene
/// file per R1.1.
///
/// Carries the declared name, the optional `max-cascade-depth` property that
/// bounds `emit`-op chains (R4.10, default 4), and the ordered body of
/// scene-root children.
#[derive(Facet, Debug, Clone)]
pub struct SceneNode {
    /// Declared scene name (first positional argument on `scene`).
    #[facet(kdl::argument)]
    pub name: String,

    /// Optional cascade-depth override for `emit` op chains (R4.10).
    /// `None` = default (4); `Some(n)` = user-declared cap.
    #[facet(kdl::property, rename = "max-cascade-depth", default)]
    pub max_cascade_depth: Option<u32>,

    /// Ordered body of scene-root children — `use`, `include`, `layout`,
    /// `mode`, `on`, `bind`, `clear-reactions`, `clear-bind`,
    /// `disable-extension` (R1.2). Textual order is preserved so T-016 can
    /// honour the on/bind execution-order rule.
    #[facet(kdl::children, default)]
    pub body: Vec<SceneBodyNode>,
}

/// Enumeration of every legal scene-root child per R1.2.
///
/// Variant renames match the canonical KDL node name exactly; facet-kdl
/// dispatches by element name at parse time. Unknown node names surface as
/// `error[scene/unknown-node]` in T-015 via the parse-error path.
#[derive(Facet, Debug, Clone)]
#[repr(u8)]
pub enum SceneBodyNode {
    /// `use "<ext-name>" [config { … }]` — extension activation (R10).
    #[facet(rename = "use")]
    Use(UseNode),

    /// `include "<path-or-ext:fragment>"` — fragment splicing (R11).
    #[facet(rename = "include")]
    Include(IncludeNode),

    /// `layout { tab @handle { … } … }` — primary layout subtree (R3).
    #[facet(rename = "layout")]
    Layout(LayoutNode),

    /// `mode "<name>" { tab @handle { … } }` — alternate whole-tab layout (R9).
    #[facet(rename = "mode")]
    Mode(ModeNode),

    /// `on <EventKind> field=pattern … { ops }` — reaction (R4).
    #[facet(rename = "on")]
    On(OnNode),

    /// `bind "<chord>" { ops }` — keybind (R5).
    #[facet(rename = "bind")]
    Bind(BindNode),

    /// `clear-reactions event="<selector>"` — remove inherited reactions (R11.6).
    #[facet(rename = "clear-reactions")]
    ClearReactions(ClearReactionsNode),

    /// `clear-bind "<chord>"` — remove inherited keybind (R11.7 / R5).
    #[facet(rename = "clear-bind")]
    ClearBind(ClearBindNode),

    /// `disable-extension "<name>"` — prevent activation (R11.8).
    #[facet(rename = "disable-extension")]
    DisableExtension(DisableExtensionNode),
}

/// `use "<ext-name>" [config { … }]` — extension activation (R10.10).
///
/// The `config { … }` body is preserved as an opaque `KdlDocument`; T-096
/// validates it against the extension's facet SHAPE at scene compile.
#[derive(Facet, Debug, Clone)]
pub struct UseNode {
    /// Extension name (the positional argument on `use`).
    #[facet(kdl::argument)]
    pub name: String,

    /// Optional `config { … }` child block, preserved verbatim for deferred
    /// schema validation (T-096). Populated by the parse pass in T-011
    /// (facet-kdl has no direct mapping for an opaque sub-document; skipped
    /// from derive-dispatch so the surrounding `UseNode` stays facet-derivable).
    #[facet(opaque)]
    pub config_block: Option<KdlDocument>,
}

/// `include "<path-or-ext:fragment>"` — splice a KDL fragment verbatim (R11.2).
///
/// No merge logic at parse time; conflicts surface at compose stage (T-077).
#[derive(Facet, Debug, Clone)]
pub struct IncludeNode {
    /// Target: either a filesystem path or an `ext:<name>/<fragment>` spec.
    #[facet(kdl::argument)]
    pub target: String,
}

/// `layout { tab @handle { … } … }` — primary layout subtree (R3.1).
///
/// Body is a list of [`TabNode`]s from [`layout`] (T-004). No bare panes /
/// rows / cols at layout root — T-013 enforces this via scope validation.
#[derive(Facet, Debug, Clone)]
pub struct LayoutNode {
    /// Tabs declared inside the `layout { }` block, in source order.
    #[facet(kdl::children, default)]
    pub tabs: Vec<TabNode>,
}

/// `mode "<name>" { tab @handle { … } }` — named alternate whole-tab layout (R9).
///
/// Switched via `use_mode "<name>"` op; handles survive swap so subprocesses
/// are preserved across the override-layout reconciliation.
#[derive(Facet, Debug, Clone)]
pub struct ModeNode {
    /// Mode name (positional argument; `"default"` is reserved for revert).
    #[facet(kdl::argument)]
    pub name: String,

    /// Tabs declared inside the mode body, in source order.
    #[facet(kdl::children, default)]
    pub tabs: Vec<TabNode>,
}

/// `on <EventKind> field=pattern … { ops }` — reaction declaration (R4).
///
/// The selector and `when` predicate are stored in a parse-deferred form
/// here: the typed [`EventSelector`] from [`selector`] (T-009) is populated
/// by the parse pass in T-011 once field-pattern capture is wired up, and
/// the `when` attribute stays as raw Rhai source for compilation in T-024.
/// Ops are the list of [`OpNode`] children from [`ops`] (T-005).
#[derive(Facet, Debug, Clone)]
pub struct OnNode {
    /// Typed selector (event kind + field-pattern map). Skipped from KDL
    /// derive-dispatch because T-011 builds it from the raw node head
    /// (`on <kind> field=pat …`); facet-kdl has no direct mapping for the
    /// event-kind-plus-free-field-props shape.
    #[facet(skip)]
    pub selector: Option<EventSelector>,

    /// Optional `when="<Rhai>"` guard. Raw source text; compiled to a
    /// `rhai::AST` at scene compile (T-023 / T-024).
    #[facet(kdl::property, default)]
    pub when: Option<String>,

    /// Ordered op list — body of the `on` block. Textual order matters
    /// (R4.5 — ops execute in source order).
    #[facet(kdl::children, default)]
    pub ops: Vec<OpNode>,
}

/// `bind "<chord>" { ops }` — keybind declaration (R5).
///
/// Chord notation follows zellij (`"Alt d"`, `"Alt Shift v"`, `"Ctrl c"`) and
/// is validated at T-064; the op body uses the same grammar as `on`
/// reactions.
#[derive(Facet, Debug, Clone)]
pub struct BindNode {
    /// Zellij-flavored key chord (e.g. `"Alt d"`).
    #[facet(kdl::argument)]
    pub chord: String,

    /// Ordered op list — body of the `bind` block.
    #[facet(kdl::children, default)]
    pub ops: Vec<OpNode>,
}

/// `clear-reactions event="<selector>"` — remove matching reactions from
/// included fragments (R11.6).
///
/// The raw selector string is parsed lazily at compose stage (T-067) so this
/// node stays cheap to construct and preserves the original source for
/// diagnostic attribution.
#[derive(Facet, Debug, Clone)]
pub struct ClearReactionsNode {
    /// Raw selector source (e.g. `"FileEdited path=**/*.md"`). Captured
    /// from the `event=` property on the node.
    #[facet(kdl::property, rename = "event")]
    pub selector: String,
}

/// `clear-bind "<chord>"` — remove a specific inherited keybind (R5 / R11.7).
#[derive(Facet, Debug, Clone)]
pub struct ClearBindNode {
    /// Zellij chord notation matching the bind to remove.
    #[facet(kdl::argument)]
    pub chord: String,
}

/// `disable-extension "<name>"` — prevent activation of an included
/// fragment's extension (R11.8).
#[derive(Facet, Debug, Clone)]
pub struct DisableExtensionNode {
    /// Extension name to disable.
    #[facet(kdl::argument)]
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: minimal `scene "x" { }` parses through facet-kdl.
    /// Exercises the scene-node derive macro (name argument, empty body) per
    /// the T-003 acceptance criterion. Richer body-level coverage lands with
    /// T-011's fixture suite once T-004 / T-005 / T-009 populate the real
    /// child types.
    #[test]
    #[ignore = "T-011 wires parse_scene entry point; facet-kdl expects a document wrapper over SceneNode"]
    fn parses_minimal_empty_scene() {
        let kdl = r#"scene "x" { }"#;
        let parsed =
            facet_kdl::from_str::<SceneNode>(kdl).expect("minimal empty scene should parse");
        assert_eq!(parsed.name, "x");
        assert!(parsed.max_cascade_depth.is_none());
        assert!(parsed.body.is_empty());
    }
}
