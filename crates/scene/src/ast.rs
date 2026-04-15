//! Scene AST types parsed from KDL via `facet-kdl`.
//!
//! Each struct maps to a single KDL node shape per `cavekit-scene.md` R1
//! (scene file grammar) and R2 (scope rules). Every `#[derive(Facet)]` field
//! carries a Rust `///` doc-comment: facet's SHAPE reflection surfaces these
//! doc-comments as LSP hover documentation in T-12.x editor integration
//! (R13 `ark scene` tooling), so the doc-comments are load-bearing — not
//! cosmetic. Do not strip them.
//!
//! Span information is tracked automatically by facet-kdl's deserializer and
//! surfaces through `KdlDeserializeError` → `miette::Diagnostic`; individual
//! AST nodes do NOT carry their own span fields. When cross-file merges need
//! to attribute a node to its origin file (R11 `ark scene graph`), that
//! attribution happens in a sibling index built during the compile pipeline
//! (deferred to later tiers — see T-1.x in `build-site-scene.md`).
//!
//! Design notes (chosen over the literal spec wording where necessary):
//!
//! * Scene body is modeled as several `#[facet(kdl::children)]` vectors
//!   (one per permitted child node type) rather than a single enum-typed
//!   `Vec<SceneChild>`. facet-kdl routes multiple `children` fields by
//!   matching the node name to the singular form of the field name (see
//!   facet-kdl README), which is the idiomatic shape. Scope violations
//!   (e.g. `on` inside `layout`) therefore surface as facet-kdl
//!   "unexpected child" errors; the compile pipeline remaps those to
//!   `scene/misplaced-node` per R2 + R12.
//! * Unknown KDL value types inside op bodies (op vocabulary is R7, v1 has
//!   ~17 ops — deferred to T-3.x) are held as `serde_json::Value` for v1
//!   so the AST can compile without pinning the full op grammar before
//!   that tier. See TODO(T-3.2) markers below. For now ops are represented
//!   as opaque `OpNode` values containing a node name + raw arguments.
//! * KDL 2.0 allows many attribute shapes; this file encodes only what
//!   R1+R6+R7 name explicitly. Anything not enumerated is rejected by
//!   facet-kdl at parse time, which is the desired behaviour (R1: unknown
//!   node = parse error).

use facet::Facet;
// facet-kdl's attribute grammar lives under the `kdl` namespace (see
// `facet::define_attr_grammar!` in facet-kdl's lib.rs). The derive macro
// resolves `#[facet(kdl::property)]` etc. by looking up the `kdl`
// identifier in the current scope, so `facet_kdl` MUST be aliased as
// `kdl` for the derive to see the attr grammar.
use facet_kdl as kdl;

// ---------------------------------------------------------------------------
// Scene root
// ---------------------------------------------------------------------------

/// Top-level scene document.
///
/// A scene file (R1) contains exactly one `scene "<name>" { … }` node. This
/// wrapper struct collects it as a single child so the root of the KDL
/// document has the expected shape. Multiple top-level scenes = parse error
/// (enforced by facet-kdl: a single `kdl::child` field rejects duplicates).
#[derive(Facet, Debug)]
pub struct SceneDoc {
    /// The one-and-only `scene` node at the top of the file.
    #[facet(kdl::child)]
    pub scene: SceneNode,
}

/// A `scene "<name>" { … }` block — root of the reactive-config AST.
///
/// Body admits the node set listed in R1: `extends`, `include`, `use`,
/// `layout`, `plugin`, `on`, `keybind`, `engine`, `clear-reactions`,
/// `clear-keybind`, `disable-plugin`. Node ordering is semantically
/// irrelevant EXCEPT that `on` and `keybind` execute in textual order
/// within a single scene file (R11 merge rules).
#[derive(Facet, Debug)]
pub struct SceneNode {
    /// Human-chosen scene name, supplied as the first positional argument
    /// (`scene "<name>" { … }`). Must be unique within a profile for
    /// `--scene NAME` selection (R-Design-Decisions "Scene selection").
    #[facet(kdl::argument)]
    pub name: String,

    /// Max cascade depth for `emit` op chains (R4 acceptance criterion,
    /// `max-cascade-depth=<N>`). Default 4 when absent; exceeding at
    /// runtime is an error log + drop.
    #[facet(kdl::property, rename = "max-cascade-depth", default)]
    pub max_cascade_depth: Option<u32>,

    /// Zero or one `extends "<scene-name>"` child. R11: one `extends` per
    /// scene; parent contributions merge before child's.
    #[facet(kdl::child, default)]
    pub extends: Option<ExtendsNode>,

    /// `include "<path>"` children. R11: splice another fragment at this
    /// position in load order. Multiple allowed.
    #[facet(kdl::children, default)]
    pub includes: Vec<IncludeNode>,

    /// `use "<name>"` children. R10 extension activation; transitive. One
    /// per named extension.
    #[facet(kdl::children, default)]
    pub uses: Vec<UseNode>,

    /// `layout { … }` block (R3). Compiled to a zellij-compatible KDL file
    /// at spawn time. Optional: a scene may exist purely to add reactions
    /// or to `extends` a parent that supplies the layout.
    #[facet(kdl::child, default)]
    pub layout: Option<LayoutNode>,

    /// `plugin "<name>" { … }` blocks (R6). Each declares a zellij wasm
    /// plugin lifecycle.
    #[facet(kdl::children, default)]
    pub plugins: Vec<PluginNode>,

    /// `on "<selector>" { <ops> }` reactions (R4). Executed in textual
    /// order within this file; see R11 for cross-file ordering.
    #[facet(kdl::children, default)]
    pub ons: Vec<OnNode>,

    /// `keybind "<chord>" …` declarations (R5).
    #[facet(kdl::children, default)]
    pub keybinds: Vec<KeybindNode>,

    /// `engine { name "…"; command "…"; args … }` — direct ACP launch
    /// spec per R17. At most one; mutual-exclusion with `use "engine-*"`
    /// is enforced in the compile pipeline as `scene/engine-conflict`.
    #[facet(kdl::child, default)]
    pub engine: Option<EngineNode>,

    /// `clear-reactions selector="<sel>"` entries (R11). Drops prior
    /// reactions matching the selector during merge.
    #[facet(kdl::children, rename = "clear-reactions", default)]
    pub clear_reactions: Vec<ClearReactionsNode>,

    /// `clear-keybind "<chord>"` entries (R11). Drops prior keybind on
    /// the named chord.
    #[facet(kdl::children, rename = "clear-keybinds", default)]
    pub clear_keybinds: Vec<ClearKeybindNode>,

    /// `disable-plugin "<name>"` entries (R10). Suppresses an
    /// extension-contributed auto-mount.
    #[facet(kdl::children, rename = "disable-plugins", default)]
    pub disable_plugins: Vec<DisablePluginNode>,
}

// ---------------------------------------------------------------------------
// Composition / activation nodes
// ---------------------------------------------------------------------------

/// `extends "<parent-scene>"` — inherits a base scene (R11). At most one
/// per scene file. Child overrides parent per the merge rules documented
/// in R11.
#[derive(Facet, Debug)]
pub struct ExtendsNode {
    /// Name of the parent scene to inherit from. Resolution goes through
    /// the same scene search path as `--scene NAME` (see
    /// Design-Decisions "Scene selection").
    #[facet(kdl::argument)]
    pub parent: String,
}

/// `include "<path>"` — splices another KDL fragment at this point in
/// load order (R11). Multiple `include` nodes allowed per scene; they are
/// applied in source position.
#[derive(Facet, Debug)]
pub struct IncludeNode {
    /// Path (relative to the current scene file) of the fragment to
    /// splice. Cycle detection raises `scene/include-cycle` (R12).
    #[facet(kdl::argument)]
    pub path: String,
}

/// `use "<ext-name>"` — activates an ark-native extension (R10). May carry
/// an optional `config { … }` block validated against the extension's
/// declared schema.
#[derive(Facet, Debug)]
pub struct UseNode {
    /// Extension name. Resolved via the R10 search path
    /// (`./.ark/extensions/…`, `${XDG_DATA_HOME}/ark/extensions/…`,
    /// `/usr/share/ark/extensions/…`, built-in).
    #[facet(kdl::argument)]
    pub name: String,

    /// Optional `config { … }` block. Contents are untyped at this AST
    /// layer — the extension declares a schema that's applied during the
    /// compile pipeline. Represented as a catch-all `OpaqueBlock` so the
    /// facet-kdl derive compiles without the schema known statically.
    // TODO(T-2.5 / T-4.x): replace with a typed config-node once the
    // extension-manifest schema surface lands.
    #[facet(kdl::child, default)]
    pub config: Option<OpaqueBlock>,
}

// ---------------------------------------------------------------------------
// Layout subtree (R3)
// ---------------------------------------------------------------------------

/// `layout { … }` — preprocessed superset of zellij's layout KDL (R3).
/// The compiler renders it to a zellij-compatible KDL file at spawn.
///
/// Note: the full zellij layout grammar is large (tab templates, swap
/// layouts, floating panes, pane templates, stacked panes, …). This
/// v1 AST enumerates only the nodes that scene-level scope rules (R2)
/// reference: `tab` and `pane`. Any additional zellij-native attributes
/// pass through untouched via the catch-all raw-argument bag — see the
/// `extra` field on `TabNode` / `PaneNode`.
#[derive(Facet, Debug)]
pub struct LayoutNode {
    /// `tab "<name>" { … }` children. Multiple allowed; iteration order
    /// matches zellij's layout semantics.
    #[facet(kdl::children, default)]
    pub tabs: Vec<TabNode>,

    /// Top-level `pane { … }` children (rare — most panes live inside a
    /// `tab`, but zellij allows layout-level panes).
    #[facet(kdl::children, default)]
    pub panes: Vec<PaneNode>,
}

/// `tab "<name>" { … }` — zellij tab declaration with ark-added `when=`
/// predicate (R2, R3). Attributes ark does not own pass through
/// unchanged to the rendered layout.
#[derive(Facet, Debug)]
pub struct TabNode {
    /// Optional tab name (first positional arg if supplied). Zellij
    /// treats missing names as index-only.
    #[facet(kdl::argument, default)]
    pub name: Option<String>,

    /// `when="<CEL>"` predicate (R3). Evaluated against initial agent
    /// state at compile time; false = prune branch before emission.
    /// Legal only on `tab` / `pane` per R2.
    #[facet(kdl::property, default)]
    pub when: Option<String>,

    /// `focus=#true/#false` — zellij pass-through.
    #[facet(kdl::property, default)]
    pub focus: Option<bool>,

    /// Nested `pane { … }` children.
    #[facet(kdl::children, default)]
    pub panes: Vec<PaneNode>,
}

/// `pane { … }` or `pane "<cmd>"` — zellij pane declaration with ark-added
/// `when=` predicate (R2). Command, args, cwd, size, and split-direction
/// are zellij pass-throughs.
#[derive(Facet, Debug)]
pub struct PaneNode {
    /// `when="<CEL>"` predicate (R3). Same semantics as `TabNode::when`.
    #[facet(kdl::property, default)]
    pub when: Option<String>,

    /// Pane display name (zellij `name=…`).
    #[facet(kdl::property, default)]
    pub name: Option<String>,

    /// Shell command to run in the pane.
    #[facet(kdl::property, default)]
    pub command: Option<String>,

    /// Pane size (percent or cell count — zellij parses the suffix).
    /// Kept as `String` at the AST layer so both `50` and `"50%"` round-trip.
    #[facet(kdl::property, default)]
    pub size: Option<String>,

    /// Split direction (zellij `split_direction`). Kept as `String` because
    /// zellij accepts several spellings (`vertical`, `horizontal`, `v`, `h`).
    #[facet(kdl::property, rename = "split_direction", default)]
    pub split_direction: Option<String>,

    /// Zellij focus flag.
    #[facet(kdl::property, default)]
    pub focus: Option<bool>,

    /// Working directory at launch.
    #[facet(kdl::property, default)]
    pub cwd: Option<String>,

    /// Nested panes (zellij recursion).
    #[facet(kdl::children, default)]
    pub panes: Vec<PaneNode>,
}

// ---------------------------------------------------------------------------
// Plugin block (R6)
// ---------------------------------------------------------------------------

/// `plugin "<name>" { … }` — zellij wasm plugin declaration (R6).
///
/// Note: `plugin` references ONLY zellij wasm cartridges. ark-native
/// extensions use `use "<name>"` (R10 / `UseNode`). The two keywords are
/// NOT interchangeable; the compile pipeline enforces the separation.
#[derive(Facet, Debug)]
pub struct PluginNode {
    /// Plugin name, first positional argument.
    #[facet(kdl::argument)]
    pub name: String,

    /// `override=true` lets a user scene replace an extension-contributed
    /// default plugin block without a duplicate-decl error (R6).
    #[facet(kdl::property, rename = "override", default)]
    pub override_: Option<bool>,

    /// `source` child (required): one of `shipped:<name>`, `ext:<name>`,
    /// `file:<path>`, `url:<…>`.
    #[facet(kdl::child, default)]
    pub source: Option<SourceNode>,

    /// `mount` child (required): target = status-bar | floating | pane |
    /// hidden, plus positional attrs.
    #[facet(kdl::child, default)]
    pub mount: Option<MountNode>,

    /// `summon "<selector>"` — lifecycle marker (summon-mode plugin).
    /// Presence along with `on` is `scene/plugin-ambiguous-lifecycle`.
    #[facet(kdl::child, default)]
    pub summon: Option<SummonNode>,

    /// `dismiss "<selector>"` — close selector for summon/event-mount
    /// plugins.
    #[facet(kdl::child, default)]
    pub dismiss: Option<DismissNode>,

    /// `on "<event-selector>"` inside a plugin body — lifecycle marker
    /// (event-mount-mode plugin). See R6 and `PluginOnNode`.
    #[facet(kdl::children, default)]
    pub on: Vec<PluginOnNode>,

    /// `subscribes "<selector>"` children — events forwarded via
    /// `zellij pipe --plugin <url>` regardless of mount state.
    #[facet(kdl::children, default)]
    pub subscribes: Vec<SubscribesNode>,

    /// Optional `config { … }` block validated against the plugin's
    /// declared schema. Untyped at AST layer; see UseNode::config.
    // TODO(T-3.x / T-4.x): typed once the plugin-config schema is live.
    #[facet(kdl::child, default)]
    pub config: Option<OpaqueBlock>,
}

/// `source "<uri>"` child of a `plugin { }` block (R6). URI formats:
/// `shipped:<name>`, `ext:<name>`, `file:<path>`, `url:<https://…>`.
#[derive(Facet, Debug)]
pub struct SourceNode {
    /// The source URI string, validated at compile time.
    #[facet(kdl::argument)]
    pub uri: String,
}

/// `mount <target> <key=val>*` child of a `plugin { }` block (R6).
/// Positional attrs: `into`, `split`, `size`, `x`, `y`, `width`, `height`.
#[derive(Facet, Debug)]
pub struct MountNode {
    /// Mount target: one of `status-bar`, `floating`, `pane`, `hidden`.
    #[facet(kdl::argument)]
    pub target: String,

    /// Named pane slot to fill (`into="<slot>"`).
    #[facet(kdl::property, default)]
    pub into: Option<String>,

    /// `split` direction when mounting to `pane`.
    #[facet(kdl::property, default)]
    pub split: Option<String>,

    /// Size (percent or cell count).
    #[facet(kdl::property, default)]
    pub size: Option<String>,

    /// X coordinate for `floating` mounts.
    #[facet(kdl::property, default)]
    pub x: Option<String>,

    /// Y coordinate for `floating` mounts.
    #[facet(kdl::property, default)]
    pub y: Option<String>,

    /// Width for `floating` mounts.
    #[facet(kdl::property, default)]
    pub width: Option<String>,

    /// Height for `floating` mounts.
    #[facet(kdl::property, default)]
    pub height: Option<String>,
}

/// `summon "<event-selector>"` child of a `plugin { }` block — dormant
/// plugin activated on first selector match (R6).
#[derive(Facet, Debug)]
pub struct SummonNode {
    /// Event selector string. Same grammar as the top-level `on` selector
    /// (R4): `<EventKind>`, `<EventKind> field="val"` (sugar), or
    /// `UserEvent:<namespaced-name>`.
    #[facet(kdl::argument)]
    pub selector: String,
}

/// `dismiss "<event-selector>"` child of a `plugin { }` block — closes a
/// summon/event-mount plugin when the selector matches (R6).
#[derive(Facet, Debug)]
pub struct DismissNode {
    /// Event selector to close on.
    #[facet(kdl::argument)]
    pub selector: String,
}

/// `on "<event-selector>"` child of a `plugin { }` block — lifecycle
/// marker making the plugin event-mounted (R6). Distinct from the
/// scene-root `on { }` reaction node (see `OnNode`).
#[derive(Facet, Debug)]
pub struct PluginOnNode {
    /// Event selector to mount on.
    #[facet(kdl::argument)]
    pub selector: String,
}

/// `subscribes "<event-selector>"` child of a `plugin { }` block —
/// forwards matching events to the plugin via `zellij pipe` regardless
/// of mount state (R6).
#[derive(Facet, Debug)]
pub struct SubscribesNode {
    /// Selector string.
    #[facet(kdl::argument)]
    pub selector: String,
}

// ---------------------------------------------------------------------------
// Reaction + keybind nodes (R4, R5)
// ---------------------------------------------------------------------------

/// `on "<event-selector>" [if="<CEL>"] { <op>+ }` — reaction declaration
/// at scene root (R4). Distinct from `PluginOnNode`, which is a lifecycle
/// marker inside `plugin { }`.
#[derive(Facet, Debug)]
pub struct OnNode {
    /// Event selector. Grammar per R4.
    #[facet(kdl::argument)]
    pub selector: String,

    /// Optional `if="<CEL>"` predicate. Legal on `on` only (R2).
    #[facet(kdl::property, rename = "if", default)]
    pub if_: Option<String>,

    /// Ordered op list in the reaction body. Op vocabulary is R7; each
    /// `OpNode` is an opaque name + raw KDL entries until T-3.x nails
    /// down the typed surface.
    #[facet(kdl::children, default)]
    pub ops: Vec<OpNode>,
}

/// `keybind "<chord>" [intent="<name>"] [{ <op>+ }]` — keybind
/// declaration (R5). Two legal shapes: the `intent=` shorthand and the
/// block form carrying ops in the body.
#[derive(Facet, Debug)]
pub struct KeybindNode {
    /// The chord string, e.g. `"Alt p"`. Validated against zellij's key
    /// chord lexer at compile time.
    #[facet(kdl::argument)]
    pub chord: String,

    /// Shorthand: `intent="<name>"` dispatches a single intent. Legal on
    /// `keybind` only (R2). Mutually exclusive with a non-empty body.
    #[facet(kdl::property, default)]
    pub intent: Option<String>,

    /// Block-form op list. Same op grammar as `on { }` reaction bodies.
    #[facet(kdl::children, default)]
    pub ops: Vec<OpNode>,
}

// ---------------------------------------------------------------------------
// Engine + clear-* / disable-* markers
// ---------------------------------------------------------------------------

/// `engine { name "…"; command "…"; args "…"; env { … } }` — direct
/// ACP launch spec per R17. At most one per scene. Mutual-exclusion with
/// `use "engine-*"` enforced in the compile pipeline (`scene/engine-conflict`).
///
/// The compile pipeline lowers this into [`crate::ops::EngineLaunch`]
/// (see also [`crate::path`] for scene resolution); this AST shape
/// preserves source fidelity for span-rich diagnostics.
#[derive(Facet, Debug)]
pub struct EngineNode {
    /// Human-friendly engine identifier (e.g. `"claude"`). Surfaces in
    /// `ark scene graph` attribution.
    #[facet(kdl::child, default)]
    pub name: Option<EngineStringNode>,

    /// Argv-0 of the engine process.
    #[facet(kdl::child, default)]
    pub command: Option<EngineStringNode>,

    /// Additional argv entries. Each `args "<v1>" "<v2>" …` line in
    /// the source contributes one [`EngineArgsNode`] whose `values`
    /// vector holds the positional strings on that line. Multiple
    /// `args` lines are allowed and concatenated in source order; the
    /// lowering step (T-ACP.3 / [`crate::engine::lower_engine`])
    /// flattens them into a single argv slice.
    ///
    /// Renamed to `"args"` (matching the field name verbatim, which
    /// is also the spec form per R17) so the generated KDL schema
    /// announces the correct node name. Without the rename the
    /// schema-emitter would singularize `args` → `arg`; facet-kdl's
    /// runtime accepts `args` regardless, but the schema would lie
    /// to editor tooling.
    #[facet(kdl::children, rename = "args", default)]
    pub args: Vec<EngineArgsNode>,

    /// `env { KEY "VAL" }` block (R17). Each child is a single
    /// environment variable: child node name = env-var key, first
    /// positional argument = value. Allows arbitrary key names by
    /// routing through `kdl::node_name`.
    #[facet(kdl::child, default)]
    pub env: Option<EngineEnvNode>,
}

/// Shared shape for single-argument string children inside `engine { }`
/// (e.g. `name "claude"` or `command "claude"`).
#[derive(Facet, Debug)]
pub struct EngineStringNode {
    /// The string value (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// `args "<v1>" "<v2>" …` child of `engine { }` — positional argv
/// entries appended after `command`. Multiple `args` lines are
/// permitted and flattened in source order during lowering.
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct EngineArgsNode {
    /// The argv strings on this line, in source order.
    #[facet(kdl::arguments, default)]
    pub values: Vec<String>,
}

/// `env { KEY "VAL"; … }` child of `engine { }` — environment-variable
/// bag. Each [`EngineEnvVarNode`] is a single variable (node name =
/// key, first argument = value).
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct EngineEnvNode {
    /// Ordered list of env-var entries (preserves source order so
    /// duplicate-key diagnostics can point at the second occurrence).
    /// Children carry arbitrary node names (the env-var keys); the
    /// `kdl::children` routing accepts every node, with each one
    /// captured as an [`EngineEnvVarNode`] via `kdl::node_name`.
    #[facet(kdl::children, default)]
    pub vars: Vec<EngineEnvVarNode>,
}

/// One `KEY "VAL"` line inside `engine { env { … } }`.
///
/// `kdl::node_name` captures the env-var name (allowing arbitrary
/// identifier shapes — UPPER_SNAKE, mixedCase, kebab-case, quoted
/// strings with spaces, etc.). The first positional argument carries
/// the value.
#[derive(Facet, Debug)]
pub struct EngineEnvVarNode {
    /// The env-var key (captured from the KDL node name).
    #[facet(kdl::node_name)]
    pub key: String,

    /// The env-var value (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// `clear-reactions selector="<sel>"` — drops prior reactions matching
/// the selector during merge (R11).
#[derive(Facet, Debug)]
pub struct ClearReactionsNode {
    /// Selector to clear. Same grammar as R4 selectors.
    #[facet(kdl::property)]
    pub selector: String,
}

/// `clear-keybind "<chord>"` — drops a prior keybind on the named chord
/// (R11).
#[derive(Facet, Debug)]
pub struct ClearKeybindNode {
    /// Chord to clear.
    #[facet(kdl::argument)]
    pub chord: String,
}

/// `disable-plugin "<name>"` — suppresses an extension-contributed
/// plugin auto-mount (R10).
#[derive(Facet, Debug)]
pub struct DisablePluginNode {
    /// Plugin name to disable.
    #[facet(kdl::argument)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Opaque + op scaffolding (TODO markers for later tiers)
// ---------------------------------------------------------------------------

/// Untyped KDL block placeholder for sub-grammars not yet schematized
/// (e.g. `config { … }` bodies, `args { … }` inside `engine { }`). The
/// compile pipeline re-parses these with the right schema once the
/// owning extension or plugin is resolved.
///
/// TODO(T-3.x / T-4.x): replace each call site with a typed node once
/// the extension-manifest and op schemas land.
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct OpaqueBlock {
    /// Catch-all positional args (if any).
    #[facet(kdl::arguments, default)]
    pub args: Vec<String>,
}

/// Opaque op node inside an `on { }` or `keybind { }` body.
///
/// R7 defines ~17 canonical ops (`open_tab`, `split_pane`, `emit`, `pipe`,
/// `prompt`, `acp/cancel`, …). Each has a distinct schema. Encoding all
/// of them as a typed enum is T-3.2 work; for T-0.2 we keep the AST open
/// by retaining a free-form name + argument bag that the compile pipeline
/// re-parses against the typed op registry.
// TODO(T-3.2): replace `OpNode` with a typed enum (one variant per R7 op).
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct OpNode {
    /// Positional arguments to the op, in source order.
    #[facet(kdl::arguments, default)]
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_scene_parses() {
        // Smoke test: the absolute-minimum scene file (R15 auto-wrap path
        // is handled elsewhere; this file exercises the explicit wrapper).
        let input = r#"scene "hello""#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse minimal scene");
        assert_eq!(doc.scene.name, "hello");
        assert!(doc.scene.layout.is_none());
        assert!(doc.scene.ons.is_empty());
        assert!(doc.scene.keybinds.is_empty());
    }

    #[test]
    fn scene_with_cascade_depth_property() {
        let input = r#"scene "demo" max-cascade-depth=8"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse cascade depth");
        assert_eq!(doc.scene.max_cascade_depth, Some(8));
    }

    #[test]
    fn scene_with_extends_and_uses() {
        let input = r#"
scene "ui" {
    extends "base"
    use "picker"
    use "status"
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse extends + uses");
        assert_eq!(doc.scene.name, "ui");
        assert_eq!(
            doc.scene.extends.as_ref().map(|e| e.parent.as_str()),
            Some("base")
        );
        assert_eq!(doc.scene.uses.len(), 2);
        assert_eq!(doc.scene.uses[0].name, "picker");
        assert_eq!(doc.scene.uses[1].name, "status");
    }

    #[test]
    fn scene_with_layout_and_tabs() {
        let input = r#"
scene "s" {
    layout {
        tab "work" {
            pane name="editor"
        }
        tab "logs"
    }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse layout");
        let layout = doc.scene.layout.as_ref().expect("layout present");
        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(layout.tabs[0].name.as_deref(), Some("work"));
        assert_eq!(layout.tabs[0].panes.len(), 1);
        assert_eq!(layout.tabs[0].panes[0].name.as_deref(), Some("editor"));
        assert_eq!(layout.tabs[1].name.as_deref(), Some("logs"));
    }

    #[test]
    fn scene_with_keybind_shorthand() {
        let input = r#"
scene "kb" {
    keybind "Alt p" intent="picker.show"
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse keybind");
        assert_eq!(doc.scene.keybinds.len(), 1);
        assert_eq!(doc.scene.keybinds[0].chord, "Alt p");
        assert_eq!(
            doc.scene.keybinds[0].intent.as_deref(),
            Some("picker.show")
        );
    }

    #[test]
    fn scene_with_engine_block() {
        let input = r#"
scene "e" {
    engine {
        name "claude"
        command "claude"
    }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse engine");
        let engine = doc.scene.engine.as_ref().expect("engine present");
        assert_eq!(
            engine.name.as_ref().map(|n| n.value.as_str()),
            Some("claude")
        );
        assert_eq!(
            engine.command.as_ref().map(|c| c.value.as_str()),
            Some("claude")
        );
    }
}
