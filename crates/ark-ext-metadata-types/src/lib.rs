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

    /// Requested capabilities — T-13.3 declared-caps surface (v0.4).
    ///
    /// v0.4 capability vocabulary (task T-13.3 in
    /// `build-site-scene.md`): values MUST come from
    /// [`ALLOWED_CAPABILITIES`] — `exec`, `fs-read`, `fs-write`, `pipe`,
    /// `network`, `hook`. Empty vec = "no special capabilities". The
    /// scene compiler reads this list off `ExtensionMetadata::SHAPE`
    /// (facet reflection) at ext inspection time (`ark ext inspect`,
    /// `ark ext info`) and, at v0.4, surfaces unknown values as a
    /// `warning[ext/unknown-capability]` rather than hard-failing —
    /// this keeps the vocabulary extensible for post-v0.4 cap additions
    /// without breaking R16 rule #3 (receivers MUST ignore unknown
    /// fields, and by extension unknown cap names).
    ///
    /// Historical note: earlier drafts used dotted leaf names
    /// (`ui.keybind`, `intents.provide`). The T-13.3 pass flattened
    /// the vocabulary to the six-name list above to match the Chrome-
    /// extension analog (install-time disclosure, then per-cap trust
    /// prompt in T-13.4). Any leftover drafts using the old dotted
    /// names are intentionally still parseable — the warning surface
    /// catches them at inspection without breaking the file.
    ///
    /// On-disk each entry renders as an `item "<capability>"` node —
    /// facet-kdl 0.42 hard-codes the sequence item name.
    #[facet(kdl::children, default)]
    pub capabilities: Vec<StringNode>,
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
    "exec",
    "fs-read",
    "fs-write",
    "pipe",
    "network",
    "hook",
];

impl ExtensionMetadata {
    /// Return the declared-capabilities list as plain `&str` slices.
    ///
    /// Thin wrapper over [`Self::capabilities`] that peels the
    /// [`StringNode`] wrapper so call sites (`ark ext inspect`,
    /// `ark ext info`, scene compiler cap-disclosure) can iterate
    /// without pattern-matching on the wrapper. T-13.3 spec text
    /// says "read at ext inspection via facet SHAPE" — in practice
    /// that means this helper: the outer SHAPE traversal lands on
    /// `capabilities`, and this method gives a trivial view of the
    /// underlying strings.
    pub fn capability_names(&self) -> impl Iterator<Item = &str> {
        self.capabilities.iter().map(|n| n.value.as_str())
    }

    /// Return `true` iff every declared capability is a member of
    /// [`ALLOWED_CAPABILITIES`].
    ///
    /// At v0.4 (T-13.3) this is **advisory**: unknown cap names are
    /// still accepted by the parser; the caller decides whether to
    /// warn (inspection-time default) or reject (strict install-path
    /// in T-13.4 / T-13.5). The method itself is a pure check over
    /// [`Self::capabilities`].
    pub fn capabilities_are_all_known(&self) -> bool {
        self.capability_names()
            .all(|c| ALLOWED_CAPABILITIES.contains(&c))
    }

    /// Return the capabilities the ext declares that are NOT in the
    /// v0.4 [`ALLOWED_CAPABILITIES`] vocabulary.
    ///
    /// Used by `ark ext inspect` + `ark ext info` to surface
    /// `warning[ext/unknown-capability]` one entry per unknown name.
    /// Order preserves the manifest's declared order so diagnostic
    /// output is stable across runs.
    pub fn unknown_capabilities(&self) -> Vec<&str> {
        self.capability_names()
            .filter(|c| !ALLOWED_CAPABILITIES.contains(c))
            .collect()
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
            config: ConfigSchema::default(),
            capabilities: vec![],
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
            config: ConfigSchema {
                fields: vec![ConfigField {
                    name: "greeting".into(),
                    type_name: StringNode::new("string"),
                    required: false,
                    default: Some(StringNode::new("hi")),
                }],
            },
            capabilities: vec![StringNode::new("exec")],
        };
        assert_eq!(m.name.value, "demo");
        assert_eq!(m.intents.len(), 1);
        assert_eq!(
            m.config.fields[0].default.as_ref().map(|n| n.value.as_str()),
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
            config: ConfigSchema::default(),
            capabilities: caps.iter().map(|c| StringNode::new(*c)).collect(),
        }
    }

    #[test]
    fn allowed_capabilities_is_the_v04_vocabulary() {
        // Reaffirms the T-13.3 "Values from {exec, fs-read, fs-write,
        // pipe, network, hook}" spec text. Adding a value requires a
        // MINOR version bump + corresponding scene-compiler warning
        // surface; dropping one is MAJOR.
        assert_eq!(
            ALLOWED_CAPABILITIES,
            &["exec", "fs-read", "fs-write", "pipe", "network", "hook"]
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
        let m = meta_with_caps(&ALLOWED_CAPABILITIES.iter().copied().collect::<Vec<_>>());
        let names: Vec<&str> = m.capability_names().collect();
        assert_eq!(names, Vec::from(ALLOWED_CAPABILITIES));
        assert!(m.capabilities_are_all_known());
        assert!(m.unknown_capabilities().is_empty());
    }
}
