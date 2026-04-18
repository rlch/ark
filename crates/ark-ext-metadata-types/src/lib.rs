//! Shared types for ark extension metadata.
//!
//! Per `cavekit-scene.md` R10 "Extensions": every installed extension ships
//! an `extension.kdl` manifest at the extension root, which is the
//! human-readable KDL serialisation of the [`ExtensionMetadata`] struct
//! defined in this crate. The types are imported by two independent
//! producers and one consumer:
//!
//! * Producer A — `ark-ext-metadata` plugin-side helper: extension authors
//!   construct an `ExtensionMetadata` value, the helper serializes it via
//!   `facet-kdl` into a wasm custom section `ark.metadata` (for
//!   wasm-component extensions) or a `extension.kdl` sibling file (for
//!   subprocess extensions). Compiled-in extensions pass the struct
//!   directly to `register_extension!`.
//!
//! * Producer B — `ark ext inspect`: read the bytes from the custom
//!   section / file and parse back into this struct for display.
//!
//! * Consumer — `ark-scene` (and downstream `ark ext list` / `ark ext
//!   info`): walks the ext search path (see `ark-ext-metadata::search_path`),
//!   loads manifests, validates against the host ark version, populates
//!   the scene compile symbol table with declared intents + events.
//!
//! # Wire stability
//!
//! The KDL serialisation produced by facet-kdl is introspectable by
//! hand (`kdl` / `xq` / `cat`) and stable across facet version bumps
//! thanks to facet's backward-compat policy. Adding a new optional
//! field is a MINOR R16 change (rule #3: receivers MUST ignore unknown
//! fields). Renaming / removing a field is MAJOR.
//!
//! Every `#[derive(Facet)]` field carries a `///` doc-comment that
//! surfaces as LSP hover text once editor tooling consumes the
//! facet-generated JSON-Schema for `extension.kdl`.

#![deny(missing_docs)]

use facet::Facet;
// `#[facet(kdl::child)]` on [`ExtensionManifest::extension`] expands to a
// path through `kdl::…` — alias `facet_kdl` to keep the derive output
// compiling with the same idiom the scene crate uses.
use facet_kdl as kdl;

/// Top-level document wrapper for `extension.kdl` files.
///
/// facet-kdl's `to_string` emits a single root KDL node named after the
/// Rust struct identifier (lowercased). The parser's counterpart,
/// `from_str`, expects the top-level of the document to contain a
/// struct's fields directly — so we wrap [`ExtensionMetadata`] in this
/// document type with a `#[facet(kdl::child)]` field named `extension`.
/// This gives every `extension.kdl` file a stable human-recognisable
/// root node: `extension { name "…"; version "…"; … }`.
///
/// Callers that already have an [`ExtensionMetadata`] wrap it in
/// [`ExtensionManifest::new`] before serialising; callers that parse
/// the file go through [`ExtensionManifest`]'s deserialiser and then
/// read `.extension`.
#[derive(Facet, Debug, Clone)]
pub struct ExtensionManifest {
    /// Single `extension { … }` KDL child — the body of the manifest.
    #[facet(kdl::child)]
    pub extension: ExtensionMetadata,
}

impl ExtensionManifest {
    /// Wrap an [`ExtensionMetadata`] for serialisation to an
    /// `extension.kdl` document.
    pub fn new(extension: ExtensionMetadata) -> Self {
        Self { extension }
    }
}

/// Body of an `extension.kdl` manifest. Held inside
/// [`ExtensionManifest`] at rest; referenced directly by ark core at
/// runtime (after the manifest has been parsed).
///
/// Every field is marked `#[facet(kdl::child)]` so facet-kdl emits and
/// accepts each field as its own child KDL node. This is symmetric with
/// scene's AST layout (see `crates/scene/src/ast.rs`) and keeps the
/// on-disk shape readable: `name "…"`, `version "…"`, `requires { … }`,
/// etc., one field per node.
#[derive(Facet, Debug, Clone)]
pub struct ExtensionMetadata {
    /// Extension name — the identifier used by `use "<name>"` in scene
    /// files and by `ark ext resolve`. MUST match the directory name in
    /// the search path (R10 "first match wins"). Lower-case alphanumeric
    /// with `-` / `_` separators; the scene compiler rejects anything
    /// else with `error[ext/bad-name]`.
    #[facet(kdl::child)]
    pub name: StringNode,

    /// Semver version of the extension itself — shown in `ark ext list`
    /// and by the update-check flow.
    #[facet(kdl::child)]
    pub version: StringNode,

    /// Supported ark-protocol range, expressed as a semver range string
    /// per R16 ("extension manifests can express ranges like
    /// `>= 1.2, < 2.0`"). Validated by the scene compiler at `use`
    /// resolution; failure = `error[ext/version]`.
    #[facet(kdl::child, rename = "ark-range")]
    pub ark_range: StringNode,

    /// Supported zellij version range. Empty string = "no constraint".
    #[facet(kdl::child, rename = "zellij-range")]
    pub zellij_range: StringNode,

    /// Other extensions this one depends on, by `<name>@<semver-range>`.
    /// The scene compiler walks the `use` DAG and rejects cycles as
    /// `error[ext/cycle]` (R11).
    #[facet(kdl::children, default)]
    pub requires: Vec<StringNode>,

    /// Intents the extension advertises. Each `IntentDecl` contributes
    /// a namespaced name + JSON-Schema for args validation at
    /// `intent/dispatch` time (R16).
    #[facet(kdl::children, default)]
    pub intents: Vec<IntentDecl>,

    /// User-events the extension emits. Declaring events up-front lets
    /// `ark scene check` validate `on "UserEvent:<name>" …` selectors
    /// against the set of known events and surface typo suggestions
    /// per R12.
    #[facet(kdl::children, default)]
    pub events: Vec<EventDecl>,

    /// Config schema for the extension. The user's scene `use "<name>" {
    /// config { … } }` block is validated against this schema before
    /// the extension is handed off to startup. See [`ConfigSchema`].
    #[facet(kdl::child, default)]
    pub config: ConfigSchema,

    /// Views the extension contributes. Each `ViewDecl` maps a named
    /// slot to a renderer the extension provides. The scene compiler
    /// validates `pane @h { view "<ext>.<view>" }` references against
    /// declared views at compile time.
    #[facet(kdl::children, default)]
    pub views: Vec<ViewDecl>,

    /// Requested capabilities — T-13.3 declared-caps surface (v0.4).
    /// See [`CapabilitySet`] for the vocabulary and semantics.
    #[facet(kdl::child, default)]
    pub capabilities: CapabilitySet,

    /// Named config sub-sections the extension exposes (cavekit-soul-
    /// phase-2-ext-surface.md R4). Each section has an independent
    /// schema, letting the extension partition user-facing config into
    /// named groups (e.g. `editor`, `telemetry`, `keybindings`) that
    /// scene consumers address via `use "<ext>" { config { <section>
    /// { … } } }`.
    #[facet(kdl::children, default)]
    pub config_sections: Vec<ConfigSectionDecl>,

    /// Named reload gates the extension registers; queried by ark's
    /// scene reload machinery before commit (cavekit-soul-phase-2-ext-
    /// surface.md R4 + host-dispatch R5). A gate refusal aborts the
    /// reload with the gate's human-readable description.
    #[facet(kdl::children, default)]
    pub reload_gates: Vec<ReloadGateDecl>,
}

/// v0.4 capability vocabulary (T-13.3 in `build-site-scene.md`).
///
/// Declared-caps values MUST come from this list. Any other value is
/// still parseable (see [`ExtensionMetadata::capabilities`] doc) but
/// surfaces as a `warning[ext/unknown-capability]` at inspection time;
/// the runtime-enforcement tier (v0.5+) will upgrade this to a hard
/// rejection once the wasm host-function gate lands (T-13.6+).
///
/// # Meanings (documentation only, not enforced at v0.4)
///
/// | Value      | What it declares                                   |
/// |------------|----------------------------------------------------|
/// | `exec`     | Spawns subprocesses (scene `exec` op, argv form).  |
/// | `fs-read`  | Reads files outside the ext's install directory.   |
/// | `fs-write` | Writes files outside the ext's install directory.  |
/// | `pipe`     | Emits pipe messages to other zellij panes/plugins. |
/// | `network`  | Opens outbound TCP/UDP/HTTP sockets.               |
/// | `hook`     | Registers scene reactions ([[hooks]] analog).      |
///
/// The cap set is fixed for v0.4 to keep the "Chrome-install-prompt"
/// disclosure surface tight; post-v0.4 additions slot in MINOR via
/// R16 rule #8 (flat manifest representation, append-only).
pub const ALLOWED_CAPABILITIES: &[&str] = &[
    "exec", "fs-read", "fs-write", "pipe", "network", "hook", "agent",
];

/// Structured agent capability declaration (T-102).
///
/// Extensions that speak a conversational protocol (e.g. ACP) declare
/// an `agent` child inside their `capabilities` block:
///
/// ```kdl
/// capabilities {
///     agent {
///         speaks "acp"
///         launch {
///             command "claude"
///             args "--acp"
///         }
///     }
/// }
/// ```
///
/// The scene compiler reads this via the [`CapabilitySet::agent`] field
/// and, when an `acp.*` op is dispatched, uses the [`LaunchSpec`] to
/// start the agent subprocess.
#[derive(Facet, Debug, Clone)]
pub struct AgentCapability {
    /// Protocol the agent speaks (e.g. `"acp"`). The scene compiler
    /// validates this against a known-protocol list; unknown protocols
    /// surface as `warning[ext/unknown-protocol]`.
    #[facet(kdl::child)]
    pub speaks: StringNode,

    /// How to launch the agent subprocess. See [`LaunchSpec`].
    #[facet(kdl::child)]
    pub launch: LaunchSpec,
}

/// Subprocess launch specification for an agent-capable extension
/// (T-102).
///
/// Serialised as a `launch { command "…"; args "…" "…" }` child node
/// inside the [`AgentCapability`] block. The scene runtime uses these
/// fields verbatim as `argv[0]` + `argv[1..]` when spawning the agent
/// process.
#[derive(Facet, Debug, Clone)]
pub struct LaunchSpec {
    /// Executable name or path (`argv[0]`). Resolved via `$PATH` at
    /// spawn time.
    #[facet(kdl::child)]
    pub command: StringNode,

    /// Additional command-line arguments (`argv[1..]`). Rendered as
    /// repeated `item "arg"` children inside the `args` node —
    /// facet-kdl 0.42's sequence convention.
    #[facet(kdl::children, default)]
    pub args: Vec<StringNode>,
}

impl ExtensionMetadata {
    /// Return the declared-capabilities list as plain `&str` slices.
    ///
    /// Delegates to [`CapabilitySet::names`].
    pub fn capability_names(&self) -> impl Iterator<Item = &str> {
        self.capabilities.names()
    }

    /// Return `true` iff every declared capability is a member of
    /// [`ALLOWED_CAPABILITIES`].
    ///
    /// Delegates to [`CapabilitySet::are_all_known`].
    pub fn capabilities_are_all_known(&self) -> bool {
        self.capabilities.are_all_known()
    }

    /// Return the capabilities the ext declares that are NOT in the
    /// v0.4 [`ALLOWED_CAPABILITIES`] vocabulary.
    ///
    /// Delegates to [`CapabilitySet::unknown`].
    pub fn unknown_capabilities(&self) -> Vec<&str> {
        self.capabilities.unknown()
    }

    /// Return the structured [`AgentCapability`] if the extension
    /// declared one (T-102).
    ///
    /// Delegates to [`CapabilitySet::agent_capability`].
    pub fn agent_capability(&self) -> Option<&AgentCapability> {
        self.capabilities.agent_capability()
    }
}

/// Set of declared capabilities for an extension (T-13.3).
///
/// v0.4 capability vocabulary: values MUST come from
/// [`ALLOWED_CAPABILITIES`] — `exec`, `fs-read`, `fs-write`, `pipe`,
/// `network`, `hook`. Empty set = "no special capabilities". The
/// scene compiler reads this via facet SHAPE reflection at ext
/// inspection time (`ark ext inspect`, `ark ext info`) and, at v0.4,
/// surfaces unknown values as `warning[ext/unknown-capability]`
/// rather than hard-failing — keeping the vocabulary extensible for
/// post-v0.4 cap additions without breaking R16 rule #3.
///
/// On-disk each entry renders as an `item "<capability>"` child node
/// inside a `capabilities { … }` parent node.
#[derive(Facet, Debug, Clone, Default)]
pub struct CapabilitySet {
    /// Individual capability entries. Empty = no special capabilities.
    #[facet(kdl::children, default)]
    pub entries: Vec<StringNode>,

    /// Structured agent capability (T-102). Present when the extension
    /// declares `capabilities { agent { speaks "acp"; launch { … } } }`.
    /// `None` = no agent capability. When set, the `"agent"` string
    /// SHOULD also appear in [`entries`] for backward-compat with code
    /// that only checks flat cap names.
    #[facet(kdl::child, default)]
    pub agent: Option<AgentCapability>,
}

impl CapabilitySet {
    /// Construct a [`CapabilitySet`] from string slices.
    pub fn from_strs(caps: &[&str]) -> Self {
        Self {
            entries: caps.iter().map(|c| StringNode::new(*c)).collect(),
            agent: None,
        }
    }

    /// Return the declared-capabilities list as plain `&str` slices.
    ///
    /// Peels the [`StringNode`] wrapper so call sites
    /// (`ark ext inspect`, `ark ext info`, scene compiler
    /// cap-disclosure) can iterate without pattern-matching on the
    /// wrapper.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|n| n.value.as_str())
    }

    /// Return `true` iff every declared capability is a member of
    /// [`ALLOWED_CAPABILITIES`].
    ///
    /// At v0.4 (T-13.3) this is **advisory**: unknown cap names are
    /// still accepted by the parser; the caller decides whether to
    /// warn or reject.
    pub fn are_all_known(&self) -> bool {
        self.names().all(|c| ALLOWED_CAPABILITIES.contains(&c))
    }

    /// Return the capabilities that are NOT in the v0.4
    /// [`ALLOWED_CAPABILITIES`] vocabulary.
    ///
    /// Order preserves the manifest's declared order so diagnostic
    /// output is stable across runs.
    pub fn unknown(&self) -> Vec<&str> {
        self.names()
            .filter(|c| !ALLOWED_CAPABILITIES.contains(c))
            .collect()
    }

    /// Return a reference to the structured [`AgentCapability`] if
    /// the extension declared one.
    pub fn agent_capability(&self) -> Option<&AgentCapability> {
        self.agent.as_ref()
    }
}

/// Wrapper around `String` so it can appear as a KDL child node body
/// (`name "some value"`). facet-kdl parses a KDL node's first
/// positional argument into this struct's `value` field; the `Display`
/// impl simply returns `self.value`.
#[derive(Facet, Debug, Clone, Default)]
pub struct StringNode {
    /// Payload string. KDL-side is the first positional argument of
    /// the node (`name "payload"` → `StringNode { value: "payload" }`).
    #[facet(kdl::argument)]
    pub value: String,
}

impl StringNode {
    /// Construct a [`StringNode`] from any stringy value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl From<&str> for StringNode {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for StringNode {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Declaration of a single intent an extension contributes. Serialised
/// as `intent "<name>" { args-schema "<json>" }`.
#[derive(Facet, Debug, Clone)]
pub struct IntentDecl {
    /// Fully-qualified intent name (`<ext-name>.<intent>`). Scene uses
    /// namespaced form; unprefixed name in an extension's sidecar is
    /// auto-prefixed by the scene merger (R11). KDL-side is the
    /// node's first positional argument: `intent "name" { … }`.
    #[facet(kdl::argument)]
    pub name: String,

    /// JSON-Schema document (as a UTF-8 string) describing the
    /// `intent/dispatch` args. Transported as a string rather than a
    /// structured value because facet 0.42 has no blanket SHAPE impl
    /// for `serde_json::Value`; foreign-language bindings treat this
    /// as `{ "type": "string", "format": "json-schema" }`.
    #[facet(kdl::child, rename = "args-schema")]
    pub args_schema: StringNode,
}

/// Declaration of a single event an extension emits. Serialised as
/// `event "<name>" { payload-schema "<json>" }`.
#[derive(Facet, Debug, Clone)]
pub struct EventDecl {
    /// Fully-qualified event name (`<ext-name>.<event>`). KDL-side is
    /// the node's first positional argument.
    #[facet(kdl::argument)]
    pub name: String,

    /// JSON-Schema document describing the event payload. Same
    /// stringification convention as [`IntentDecl::args_schema`].
    #[facet(kdl::child, rename = "payload-schema")]
    pub payload_schema: StringNode,
}

/// Declaration of a view an extension contributes. Each view maps a
/// named slot in the scene layout to a renderer provided by the
/// extension. Serialised as `view "<name>" { component "<component>" }`.
///
/// Views let extensions surface UI panes — e.g. a "git-status" sidebar
/// or "ai-chat" panel — that the scene file can mount via
/// `pane @handle { view "<ext>.<view>" }`. The scene compiler validates
/// view references against the union of all installed extensions'
/// declared views at compile time.
#[derive(Facet, Debug, Clone)]
pub struct ViewDecl {
    /// Fully-qualified view name (`<ext-name>.<view>`). Scene uses
    /// namespaced form; unprefixed name in an extension's sidecar is
    /// auto-prefixed by the scene merger (R11). KDL-side is the
    /// node's first positional argument: `view "name" { … }`.
    #[facet(kdl::argument)]
    pub name: String,

    /// Component identifier the extension registers for this view.
    /// The runtime resolves this to the extension's view renderer at
    /// mount time. KDL-side is a child node: `component "<id>"`.
    #[facet(kdl::child)]
    pub component: StringNode,

    /// Handle kind this view applies to — `"pane"` or `"stack"`.
    /// Mirrors `ark_view::HandleKind` lowercase serde tag (see
    /// `crates/ark-view/src/handle.rs`). ark-view sits below this
    /// crate in the layer hierarchy, so the value is a string
    /// discriminant here and resolved to the typed enum at scene-
    /// compile time.
    ///
    /// Absent in the manifest = "pane" per the R17 conservative
    /// default — existing pre-T-023 manifests (which omit this
    /// node) continue to parse as pane views. KDL-side is an
    /// optional child node: `kind "pane"` or `kind "stack"`.
    ///
    /// Per cavekit-soul-phase-2-ext-surface.md R4 +
    /// build-site-soul-phase-2.md T-023.
    #[facet(kdl::child, default)]
    pub kind: Option<StringNode>,
}

/// Declaration of a named config sub-section exposed by the extension
/// (cavekit-soul-phase-2-ext-surface.md R4).
///
/// The `name` is the scene-facing section key used in
/// `use "<ext>" { config { <section> { … } } }`. The `schema` carries
/// a serialized JSON-Schema document describing the section's allowed
/// keys; sub-kits may later replace this with a facet SHAPE reference
/// once the reflection-driven config pipeline lands, so authors should
/// treat the schema as an opaque string at this tier.
///
/// Serialised as `section "<name>" { schema "<json>" }`.
#[derive(Facet, Debug, Clone)]
pub struct ConfigSectionDecl {
    /// Section key used by scene's `config.<section>` accessor. Node's
    /// first positional argument: `section "name" { … }`.
    #[facet(kdl::argument)]
    pub name: String,

    /// JSON-Schema describing the section's allowed keys (serialized).
    /// Same stringification convention as [`IntentDecl::args_schema`]
    /// and [`EventDecl::payload_schema`].
    #[facet(kdl::child)]
    pub schema: StringNode,
}

/// Declaration of a reload gate the extension contributes (cavekit-
/// soul-phase-2-ext-surface.md R4 + host-dispatch R5).
///
/// Ark queries the gate before committing a scene reload; a refusal
/// aborts the reload and surfaces the gate's description to the
/// operator. Extensions receive the gate `name` back in the gate-
/// invocation RPC so a single extension can route among multiple gates
/// (e.g. `"unsaved-buffers"`, `"in-flight-agent"`).
///
/// Serialised as `gate "<name>" { description "<text>" }`.
#[derive(Facet, Debug, Clone)]
pub struct ReloadGateDecl {
    /// Gate identifier. Node's first positional argument: `gate "name"
    /// { … }`. Surfaces back to the extension in the gate-invocation
    /// RPC payload for routing.
    #[facet(kdl::argument)]
    pub name: String,

    /// Human-readable description shown in `ark doctor` / reload logs
    /// when the gate refuses a reload. Child node: `description
    /// "<text>"`.
    #[facet(kdl::child)]
    pub description: StringNode,
}

/// Declarative config schema for an extension.
///
/// v0.1 shape: flat list of fields, each with a name + type-name +
/// `required` flag + optional default. Nested objects / unions / enums
/// deferred until the real type system lands (tracked alongside R10's
/// "Config schema for the extension" criterion). The scene compiler
/// validates the user's `use "<name>" { config { … } }` block against
/// this list field-by-field.
#[derive(Facet, Debug, Clone, Default)]
pub struct ConfigSchema {
    /// Declared config fields. Empty = the extension accepts no config.
    /// Rendered as repeated `item "name" { … }` children — facet-kdl
    /// 0.42 hard-codes the sequence item name.
    #[facet(kdl::children, default)]
    pub fields: Vec<ConfigField>,
}

/// One entry in a [`ConfigSchema`]. Serialised as `field "<name>" {
/// type "<type>"; required #true|#false; default "…" }`.
#[derive(Facet, Debug, Clone)]
pub struct ConfigField {
    /// Field name — node's first positional argument.
    #[facet(kdl::argument)]
    pub name: String,

    /// Type tag. Accepted v0.1 values: `"string"`, `"int"`, `"bool"`,
    /// `"path"`, `"url"`, `"duration"`. Anything else surfaces as
    /// `error[ext/bad-config]` at manifest-load time.
    #[facet(kdl::child, rename = "type")]
    pub type_name: StringNode,

    /// Whether the scene MUST supply this field. Missing required field
    /// = `error[ext/bad-config]`.
    #[facet(kdl::property)]
    pub required: bool,

    /// Optional default value encoded as a string (parsed by the
    /// extension based on `type_name`). Omitted KDL node means "no
    /// default; the extension receives no entry for this key".
    #[facet(kdl::child, default)]
    pub default: Option<StringNode>,
}

/// Lightweight registration record submitted via `inventory::submit!`
/// by the `#[derive(Extension)]` proc macro (T-089).
///
/// This is intentionally simpler than [`ExtensionMetadata`] — it holds
/// only the scalar fields that `#[extension(…)]` attributes provide.
/// The scene compiler collects all submitted `ExtensionMeta` values at
/// startup and can inflate them into full [`ExtensionMetadata`] when
/// needed (intents, events, views, config, and capabilities are
/// declared separately by the extension runtime, not at derive time).
///
/// `module_path` captures `module_path!()` at the derive site so the
/// scene compiler can group all registrations from the same crate into
/// a single logical extension (one crate = one extension convention).
pub struct ExtensionMeta {
    /// Extension name — must match the search-path directory name per
    /// R10's "first match wins" rule.
    pub name: &'static str,

    /// Semver version of the extension.
    pub version: &'static str,

    /// Human-readable description shown in `ark ext list` / `ark ext info`.
    pub description: &'static str,

    /// Supported ark-protocol semver range (e.g. `">=0.1, <1.0"`).
    /// Empty string = "no constraint".
    pub ark_range: &'static str,

    /// `module_path!()` captured at the derive site. Used by the scene
    /// compiler to group all registrations from the same crate.
    pub module_path: &'static str,
}

inventory::collect!(ExtensionMeta);

/// Lightweight registration record submitted via `inventory::submit!`
/// by the `#[derive(View)]` proc macro (T-090).
///
/// Each `#[derive(View)]` struct produces one `ViewRegistration` that the
/// scene compiler collects at startup to discover all compiled-in views
/// without manual wiring. The struct captures only the static scalar
/// fields the derive can stamp; richer metadata (render mode, config
/// schema from facet SHAPE) is resolved later by the view registry
/// builder.
///
/// `module_path` captures `module_path!()` at the derive site so the
/// scene compiler can group the view with its owning extension crate.
pub struct ViewRegistration {
    /// View name as written in scene source (e.g. `"edit"`, `"git-status"`).
    pub name: &'static str,

    /// Component identifier — the Rust struct's type name. The runtime
    /// resolves this to the extension's view renderer at mount time.
    pub component: &'static str,

    /// Human-readable description shown in `ark ext info`. Empty string
    /// if the derive attribute omitted `description`.
    pub description: &'static str,

    /// `module_path!()` captured at the derive site. Used by the scene
    /// compiler to associate the view with its owning extension crate.
    pub module_path: &'static str,
}

inventory::collect!(ViewRegistration);

/// Scope of an intent registration (T-092).
///
/// `Global` intents are available regardless of which view is focused;
/// `Targeted` intents are scoped to a specific view (the `impl ViewStruct`
/// they were defined on). v1 always emits `Global`; location-based scope
/// detection is deferred to a follow-up task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentScope {
    /// Intent is available globally — not scoped to any particular view.
    Global,
    /// Intent is scoped to a specific view.
    Targeted,
}

/// Lightweight registration record submitted via `inventory::submit!`
/// by the `#[ark_intent]` attribute macro (T-092).
///
/// Each decorated method produces one `IntentMeta` entry. The scene
/// compiler collects all submitted values at startup and merges them
/// with the extension's declared [`IntentDecl`] list from
/// `extension.kdl`. `module_path` captures `module_path!()` at the
/// macro-expansion site so the compiler can attribute the intent to
/// the correct extension crate.
pub struct IntentMeta {
    /// Kebab-case intent name (e.g. `"open-file"`). Derived from the
    /// method name (snake_case -> kebab-case) unless overridden via
    /// `#[ark_intent(name = "custom-name")]`.
    pub name: &'static str,

    /// `module_path!()` captured at the attribute-macro expansion site.
    /// Used by the scene compiler to group intent registrations by crate.
    pub module_path: &'static str,

    /// Whether this intent is global or targeted to a specific view.
    /// v1 always sets [`IntentScope::Global`]; location-based detection
    /// is deferred.
    pub scope: IntentScope,
}

inventory::collect!(IntentMeta);

/// Lightweight registration record submitted via `inventory::submit!`
/// by the `#[derive(Event)]` proc macro (T-091).
///
/// Each struct annotated with `#[derive(Event)]` produces one
/// `EventMeta` entry. The event name is auto-derived from the struct
/// name via snake_case conversion (e.g. `FileEdited` → `file_edited`),
/// or overridden with `#[event(name = "custom-name")]`.
///
/// At emit time the runtime auto-namespaces the event name by the
/// owning extension's name, so `file_edited` from extension `editor`
/// becomes `editor.file_edited` on the bus.
///
/// `module_path` captures `module_path!()` at the derive site so the
/// scene compiler can associate the event with its owning extension
/// crate (same grouping convention as [`ExtensionMeta::module_path`]).
pub struct EventMeta {
    /// Event name — snake_case identifier derived from the struct name
    /// or overridden via `#[event(name = "…")]`.
    pub name: &'static str,

    /// Rust type name of the payload struct (`core::any::type_name`
    /// captured at monomorphisation time via `type_name::<T>()`).
    pub payload_type: &'static str,

    /// `module_path!()` captured at the derive site. Used by the scene
    /// compiler to associate the event with its owning extension crate.
    pub module_path: &'static str,
}

inventory::collect!(EventMeta);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_schema_is_empty() {
        let s = ConfigSchema::default();
        assert!(s.fields.is_empty());
    }

    #[test]
    fn manifest_wraps_metadata() {
        let m = ExtensionMetadata {
            name: StringNode::new("a"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        };
        let doc = ExtensionManifest::new(m);
        assert_eq!(doc.extension.name.value, "a");
    }

    #[test]
    fn extension_metadata_builder() {
        let m = ExtensionMetadata {
            name: StringNode::new("demo"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::new(">=0.1, <0.2"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![IntentDecl {
                name: "demo.hello".into(),
                args_schema: StringNode::new("{\"type\":\"object\"}"),
            }],
            events: vec![EventDecl {
                name: "demo.greeted".into(),
                payload_schema: StringNode::new("{\"type\":\"object\"}"),
            }],
            views: vec![ViewDecl {
                name: "demo.panel".into(),
                component: StringNode::new("DemoPanel"),
                kind: None,
            }],
            config: ConfigSchema {
                fields: vec![ConfigField {
                    name: "greeting".into(),
                    type_name: StringNode::new("string"),
                    required: false,
                    default: Some(StringNode::new("hi")),
                }],
            },
            capabilities: CapabilitySet::from_strs(&["exec"]),
            config_sections: vec![],
            reload_gates: vec![],
        };
        assert_eq!(m.name.value, "demo");
        assert_eq!(m.intents.len(), 1);
        assert_eq!(m.views.len(), 1);
        assert_eq!(m.views[0].name, "demo.panel");
        assert_eq!(m.views[0].component.value, "DemoPanel");
        assert_eq!(
            m.config.fields[0]
                .default
                .as_ref()
                .map(|n| n.value.as_str()),
            Some("hi")
        );
    }

    // -----------------------------------------------------------------
    // T-13.3: declared-capabilities surface
    // -----------------------------------------------------------------

    fn meta_with_caps(caps: &[&str]) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new("caps-demo"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::from_strs(caps),
            config_sections: vec![],
            reload_gates: vec![],
        }
    }

    #[test]
    fn allowed_capabilities_is_the_v04_vocabulary() {
        // Reaffirms the T-13.3 "Values from {exec, fs-read, fs-write,
        // pipe, network, hook}" spec text plus T-102 `agent`. Adding a
        // value requires a MINOR version bump + corresponding
        // scene-compiler warning surface; dropping one is MAJOR.
        assert_eq!(
            ALLOWED_CAPABILITIES,
            &[
                "exec", "fs-read", "fs-write", "pipe", "network", "hook", "agent"
            ]
        );
    }

    #[test]
    fn capability_names_peels_string_node_wrapper() {
        let m = meta_with_caps(&["exec", "network"]);
        let names: Vec<&str> = m.capability_names().collect();
        assert_eq!(names, vec!["exec", "network"]);
    }

    #[test]
    fn empty_capabilities_means_no_special_caps() {
        let m = meta_with_caps(&[]);
        assert_eq!(m.capability_names().count(), 0);
        assert!(m.capabilities_are_all_known());
        assert!(m.unknown_capabilities().is_empty());
    }

    #[test]
    fn capabilities_are_all_known_accepts_only_v04_vocab() {
        let m = meta_with_caps(&["exec", "fs-read", "fs-write", "pipe", "network", "hook"]);
        assert!(m.capabilities_are_all_known());
        assert!(m.unknown_capabilities().is_empty());
    }

    #[test]
    fn unknown_capabilities_surfaces_non_vocab_values() {
        // Pre-T-13.3 drafts used dotted names like `ui.keybind`. The
        // parser still accepts them (R16 rule #3), but the inspection
        // surface must flag them so operators notice during the v0.4
        // upgrade.
        let m = meta_with_caps(&["exec", "ui.keybind", "host.fs"]);
        assert!(!m.capabilities_are_all_known());
        assert_eq!(m.unknown_capabilities(), vec!["ui.keybind", "host.fs"]);
    }

    /// Facet SHAPE reflection surfaces the `capabilities` field by
    /// name — the very access path T-13.3's spec text calls out
    /// ("read at ext inspection via facet SHAPE"). We don't iterate
    /// the SHAPE tree here (that's facet's internal surface) but we
    /// do prove the field name is present and maps to a struct-of-
    /// fields shape so downstream SHAPE walkers (e.g. `ark scene
    /// schema-dump`, `ark ext inspect`) can find it.
    #[test]
    fn capabilities_field_is_present_on_shape() {
        use facet::Facet;
        let shape = ExtensionMetadata::SHAPE;
        let debug_repr = format!("{shape:?}");
        assert!(
            debug_repr.contains("capabilities"),
            "expected `capabilities` field on ExtensionMetadata SHAPE, got:\n{debug_repr}"
        );
    }

    /// Serialize: build an `ExtensionMetadata` with caps, wrap in
    /// [`ExtensionManifest`], serialize via facet-kdl, confirm every
    /// declared cap value appears in the emitted KDL text.
    ///
    /// Full re-parse round-trip of sequence fields is gated by a
    /// facet-kdl 0.42 limitation (`Vec<T>` renders as `item` children
    /// with a hard-coded name and the parser can't disambiguate
    /// multiple `Vec<T>` fields by the singularised field-name alone
    /// — documented in `ark-ext-metadata::round_trip_through_kdl_*`).
    /// Once facet-kdl ships per-field `rename=` on sequence
    /// serialisation (tracked as TODO post-v0.1 in that crate) the
    /// assertion here can be strengthened to full-value round trip.
    #[test]
    fn capabilities_emit_every_value_in_kdl() {
        let original = meta_with_caps(&["exec", "network", "hook"]);
        let manifest = ExtensionManifest::new(original.clone());
        let kdl_text = facet_kdl::to_string(&manifest).expect("serialize manifest");
        for cap in ["exec", "network", "hook"] {
            assert!(
                kdl_text.contains(cap),
                "expected cap `{cap}` in KDL output:\n{kdl_text}"
            );
        }
    }

    /// End-to-end T-13.3 exercise: build an `ExtensionMetadata` with
    /// T-13.3 v0.4 vocabulary values, round-trip the capability
    /// accessor surface (the API surface `ark ext inspect` /
    /// `ark ext info` actually call), and confirm every declared cap
    /// is iterated in order.
    ///
    /// Facet-kdl 0.42's `Vec<T>` children render as bare `item` nodes
    /// regardless of field name (documented in
    /// `ark-ext-metadata::round_trip_through_kdl_*`). Disambiguation
    /// across sibling `Vec` fields (`requires`, `intents`, `events`,
    /// `capabilities`) relies on facet-kdl's source-position ordering
    /// during parse, which is a fragile surface to exercise from
    /// outside. The consumer-side API — `capability_names`,
    /// `capabilities_are_all_known`, `unknown_capabilities` — is the
    /// T-13.3 SHAPE-read contract and is what we pin here.
    #[test]
    fn capabilities_consumer_surface_matches_v04_spec() {
        let m = meta_with_caps(&ALLOWED_CAPABILITIES.to_vec());
        let names: Vec<&str> = m.capability_names().collect();
        assert_eq!(names, Vec::from(ALLOWED_CAPABILITIES));
        assert!(m.capabilities_are_all_known());
        assert!(m.unknown_capabilities().is_empty());
    }

    // -----------------------------------------------------------------
    // T-088: ViewDecl
    // -----------------------------------------------------------------

    #[test]
    fn view_decl_stores_name_and_component() {
        let v = ViewDecl {
            name: "ext.sidebar".into(),
            component: StringNode::new("SidebarView"),
            kind: None,
        };
        assert_eq!(v.name, "ext.sidebar");
        assert_eq!(v.component.value, "SidebarView");
    }

    #[test]
    fn views_field_is_present_on_shape() {
        use facet::Facet;
        let shape = ExtensionMetadata::SHAPE;
        let debug_repr = format!("{shape:?}");
        assert!(
            debug_repr.contains("views"),
            "expected `views` field on ExtensionMetadata SHAPE, got:\n{debug_repr}"
        );
    }

    #[test]
    fn metadata_with_views_accessible() {
        let m = ExtensionMetadata {
            name: StringNode::new("viewer"),
            version: StringNode::new("1.0.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![
                ViewDecl {
                    name: "viewer.main".into(),
                    component: StringNode::new("MainPanel"),
                    kind: None,
                },
                ViewDecl {
                    name: "viewer.sidebar".into(),
                    component: StringNode::new("SidePanel"),
                    kind: None,
                },
            ],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        };
        assert_eq!(m.views.len(), 2);
        assert_eq!(m.views[0].name, "viewer.main");
        assert_eq!(m.views[1].component.value, "SidePanel");
    }

    // -----------------------------------------------------------------
    // T-088: CapabilitySet
    // -----------------------------------------------------------------

    #[test]
    fn capability_set_default_is_empty() {
        let cs = CapabilitySet::default();
        assert_eq!(cs.names().count(), 0);
        assert!(cs.are_all_known());
        assert!(cs.unknown().is_empty());
    }

    #[test]
    fn capability_set_from_strs_round_trips() {
        let cs = CapabilitySet::from_strs(&["exec", "hook"]);
        let names: Vec<&str> = cs.names().collect();
        assert_eq!(names, vec!["exec", "hook"]);
    }

    #[test]
    fn capability_set_unknown_detection() {
        let cs = CapabilitySet::from_strs(&["exec", "magic"]);
        assert!(!cs.are_all_known());
        assert_eq!(cs.unknown(), vec!["magic"]);
    }

    // -----------------------------------------------------------------
    // T-102: AgentCapability + LaunchSpec
    // -----------------------------------------------------------------

    /// Helper: build an [`AgentCapability`] with the given protocol and
    /// launch command/args.
    fn agent_cap(speaks: &str, command: &str, args: &[&str]) -> AgentCapability {
        AgentCapability {
            speaks: StringNode::new(speaks),
            launch: LaunchSpec {
                command: StringNode::new(command),
                args: args.iter().map(|a| StringNode::new(*a)).collect(),
            },
        }
    }

    #[test]
    fn agent_capability_stores_speaks_and_launch() {
        let ac = agent_cap("acp", "claude", &["--acp"]);
        assert_eq!(ac.speaks.value, "acp");
        assert_eq!(ac.launch.command.value, "claude");
        assert_eq!(ac.launch.args.len(), 1);
        assert_eq!(ac.launch.args[0].value, "--acp");
    }

    #[test]
    fn capability_set_with_agent() {
        let cs = CapabilitySet {
            entries: vec![StringNode::new("agent")],
            agent: Some(agent_cap("acp", "claude", &["--acp"])),
        };
        assert!(cs.agent_capability().is_some());
        let ac = cs.agent_capability().unwrap();
        assert_eq!(ac.speaks.value, "acp");
        assert_eq!(ac.launch.command.value, "claude");
    }

    #[test]
    fn capability_set_without_agent() {
        let cs = CapabilitySet::from_strs(&["exec"]);
        assert!(cs.agent_capability().is_none());
    }

    #[test]
    fn extension_metadata_agent_capability_delegates() {
        let m = ExtensionMetadata {
            name: StringNode::new("claude-code"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet {
                entries: vec![StringNode::new("agent")],
                agent: Some(agent_cap("acp", "claude", &["--acp"])),
            },
            config_sections: vec![],
            reload_gates: vec![],
        };
        let ac = m.agent_capability().unwrap();
        assert_eq!(ac.speaks.value, "acp");
        assert_eq!(ac.launch.command.value, "claude");
        assert_eq!(ac.launch.args[0].value, "--acp");
    }

    #[test]
    fn launch_spec_with_multiple_args() {
        let ac = agent_cap("acp", "claude", &["--acp", "--verbose", "--model=opus"]);
        assert_eq!(ac.launch.args.len(), 3);
        assert_eq!(ac.launch.args[2].value, "--model=opus");
    }

    #[test]
    fn agent_is_in_allowed_capabilities() {
        assert!(ALLOWED_CAPABILITIES.contains(&"agent"));
    }
}
