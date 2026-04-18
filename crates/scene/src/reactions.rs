//! Reaction registry + selector matcher (T-056, T-058, T-059, T-060).
//!
//! A reaction is the runtime shape of an `on <EventKind> field=pat …
//! [when="<Rhai>"] { <ops> }` node (R4). The [`ReactionRegistry`] holds
//! every reaction compiled from a scene indexed by event kind; the
//! [`Entry`] type bundles the selector, an optional compiled `when=`
//! predicate, the op list, and origin attribution.
//!
//! # Indexing (T-056)
//!
//! Two BTreeMaps:
//!
//! - `by_kind`: canonical snake_case `EventKind` string → Vec<Entry>.
//!   Every reaction lives here.
//! - `by_ext_name`: dotted event name (`myext.something`) → Vec<Entry>.
//!   Populated only for `Ext` selectors that pin a literal `name=<val>`
//!   field pattern. Entries in this index are ALSO present in `by_kind`
//!   under `"ext"` — the secondary index is purely a dispatcher
//!   fan-out optimisation.
//!
//! # Selector matching (T-058 + T-059)
//!
//! [`match_selector`] walks a selector against a live `CoreEvent`,
//! returning captured locals on match. Rules:
//!
//! 1. Event is first flattened to a [`FlatEvent`] (name + payload).
//! 2. Field value comes from the flat payload JSON.
//! 3. Glob patterns capture the matched string under the field name as
//!    a local; regex patterns also capture the full match plus any
//!    named groups.
//!
//! # when= evaluation (T-060)
//!
//! [`Entry`] stores the `when=` predicate as a compiled Rhai program.
//! The dispatcher (T-061) builds an event scope (via
//! [`crate::context::build_event_scope`]) that includes the captured
//! locals, evaluates the program, and skips the reaction on `false`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_types::{CoreEvent, FlatEvent};
use rhai::Dynamic;

use crate::ast::ops::OpNode;
use crate::ast::selector::{EventSelector, FieldPattern, MatchType};
use crate::ast::{OnNode, SceneBodyNode};
use crate::error::SceneError;
use crate::parse::SceneIR;
use crate::rhai::{Engine, Program, RhaiScope, compile_in_scope};

// ---------------------------------------------------------------------------
// EventKind — snake_case classification of `CoreEvent`.
// ---------------------------------------------------------------------------

/// Enumerated discriminator of [`CoreEvent`] — matches the serde
/// `tag = "type"` rename (`snake_case`). Used as the primary-index key
/// in [`ReactionRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventKind {
    /// `CoreEvent::Log` — canonical `"log"`.
    Log,
    /// `CoreEvent::Error` — canonical `"error"`.
    Error,
    /// `CoreEvent::SessionStarted` — canonical `"session_started"`.
    SessionStarted,
    /// `CoreEvent::SessionEnded` — canonical `"session_ended"`.
    SessionEnded,
    /// `CoreEvent::Ext(_)` — canonical `"ext"`. Extension-emitted events.
    /// Secondary index by `<ext>.<kind>` dotted name.
    Ext,
}

impl EventKind {
    /// Canonical snake_case rendering (matches `CoreEvent`'s
    /// `#[serde(tag = "type", rename_all = "snake_case")]`).
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Log => "log",
            EventKind::Error => "error",
            EventKind::SessionStarted => "session_started",
            EventKind::SessionEnded => "session_ended",
            EventKind::Ext => "ext",
        }
    }

    /// Parse a selector kind token, accepting both canonical snake_case
    /// and PascalCase spellings that scene authors typically write.
    /// Returns `None` for unknown tokens.
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "log" | "Log" => Some(EventKind::Log),
            "error" | "Error" => Some(EventKind::Error),
            "session_started" | "SessionStarted" => Some(EventKind::SessionStarted),
            "session_ended" | "SessionEnded" => Some(EventKind::SessionEnded),
            "ext" | "Ext" => Some(EventKind::Ext),
            _ => None,
        }
    }

    /// Classify a live [`CoreEvent`] into its [`EventKind`].
    pub fn of(event: &CoreEvent) -> Self {
        match event {
            CoreEvent::Log { .. } => EventKind::Log,
            CoreEvent::Error { .. } => EventKind::Error,
            CoreEvent::SessionStarted { .. } => EventKind::SessionStarted,
            CoreEvent::SessionEnded { .. } => EventKind::SessionEnded,
            CoreEvent::Ext(_) => EventKind::Ext,
        }
    }
}

// ---------------------------------------------------------------------------
// Reaction origin.
// ---------------------------------------------------------------------------

/// Provenance metadata for a reaction entry — lets
/// `ark scene explain` + the dispatcher's telemetry name exactly
/// where a reaction came from (user scene, extension, included
/// fragment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionOrigin {
    /// Source file path (scene author's scene.kdl, extension
    /// manifest, or included fragment).
    pub file: PathBuf,
    /// 1-based line number of the `on` node, when available from the
    /// KDL parser.
    pub line: Option<u32>,
    /// Kind of source. See [`OriginKind`].
    pub kind: OriginKind,
}

/// Attribution tag — which layer of the compose pipeline produced a
/// reaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginKind {
    /// Emitted directly by the user's scene file.
    UserScene,
    /// Contributed by an extension (named by its manifest name).
    Extension(String),
    /// Spliced in via an `include "…"` directive.
    Include(String),
}

impl ReactionOrigin {
    /// Convenience constructor for a user-scene-origin reaction.
    pub fn user_scene(path: impl Into<PathBuf>) -> Self {
        Self {
            file: path.into(),
            line: None,
            kind: OriginKind::UserScene,
        }
    }
}

// ---------------------------------------------------------------------------
// Reaction entry + registry.
// ---------------------------------------------------------------------------

/// One compiled reaction.
///
/// Carries the parsed selector, an optional compiled `when=` predicate,
/// the ordered op list, and origin attribution.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Parsed event selector (kind + field patterns).
    pub selector: EventSelector,
    /// Optional `when="<Rhai>"` guard compiled via
    /// [`crate::rhai::compile_in_scope`] in `RhaiScope::Event`. `None`
    /// = unconditional reaction.
    pub predicate: Option<Program>,
    /// Ordered op list from the reaction body. Textual order is
    /// preserved (R4.5).
    pub ops: Vec<OpNode>,
    /// Provenance — lets telemetry tag a dispatched reaction with its
    /// source file + origin layer.
    pub origin: ReactionOrigin,
}

/// Reaction index built at scene compile.
///
/// Primary lookup by [`EventKind`]; secondary lookup by
/// `Ext` dotted name (`<ext>.<kind>`). See module docs for indexing semantics.
#[derive(Debug, Clone, Default)]
pub struct ReactionRegistry {
    by_kind: BTreeMap<&'static str, Vec<Entry>>,
    by_ext_name: BTreeMap<String, Vec<Entry>>,
}

impl ReactionRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of reactions registered.
    pub fn len(&self) -> usize {
        self.by_kind.values().map(|v| v.len()).sum()
    }

    /// Returns `true` when no reactions are registered.
    pub fn is_empty(&self) -> bool {
        self.by_kind.is_empty()
    }

    /// Insert `entry` under the primary `kind` slot, mirroring into
    /// the secondary index when `ext_name` is `Some`. Callers
    /// that pass a non-`Ext` kind together with a name will see
    /// the name ignored (the mirror is only written for Ext).
    pub fn insert(&mut self, kind: EventKind, ext_name: Option<String>, entry: Entry) {
        self.by_kind
            .entry(kind.as_str())
            .or_default()
            .push(entry.clone());
        if let Some(name) = ext_name {
            if kind == EventKind::Ext {
                self.by_ext_name.entry(name).or_default().push(entry);
            }
        }
    }

    /// Fetch every reaction registered under the given kind. Returns
    /// an empty slice when none are registered.
    pub fn by_kind(&self, kind: EventKind) -> &[Entry] {
        self.by_kind
            .get(kind.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Fetch every reaction registered under an `Ext` dotted name
    /// (`<ext>.<kind>`). Subset of `by_kind(EventKind::Ext)`.
    pub fn by_ext_name(&self, name: &str) -> &[Entry] {
        self.by_ext_name
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Remove every reaction matching the clear selector's kind +
    /// field patterns. Used by [`crate::clear`] to apply
    /// `clear-reactions` directives.
    pub fn remove_matching(&mut self, clear_selector: &EventSelector) {
        // Walk primary index; drop entries whose selector matches.
        for entries in self.by_kind.values_mut() {
            entries.retain(|e| !clear_selector_matches(clear_selector, &e.selector));
        }
        // Drop empty slots so iter counts stay consistent.
        self.by_kind.retain(|_, v| !v.is_empty());
        // Rebuild secondary index from surviving Ext entries.
        self.by_ext_name.clear();
        if let Some(ext) = self.by_kind.get("ext") {
            for entry in ext {
                if let Some(name) = ext_name_of(&entry.selector) {
                    self.by_ext_name
                        .entry(name)
                        .or_default()
                        .push(entry.clone());
                }
            }
        }
    }

    /// Iterate every `(kind_str, entries)` pair in primary-index
    /// order. Stable across runs (BTreeMap).
    pub fn iter_primary(&self) -> impl Iterator<Item = (&&'static str, &Vec<Entry>)> {
        self.by_kind.iter()
    }

    /// Iterate every `(ext_name, entries)` pair.
    pub fn iter_ext_names(&self) -> impl Iterator<Item = (&String, &Vec<Entry>)> {
        self.by_ext_name.iter()
    }
}

// ---------------------------------------------------------------------------
// Registry build.
// ---------------------------------------------------------------------------

/// Build a [`ReactionRegistry`] from every `on` node in `ir`.
///
/// Walks `ir.scene.body`, extracting:
///
/// - The selector kind via AST (from [`OnNode::selector`] once
///   populated, falling back to raw KDL extraction).
/// - The `when=` predicate via [`compile_in_scope`] under
///   `RhaiScope::Event`.
/// - The op list verbatim.
///
/// Parse errors (unknown event kind, malformed Rhai predicate) surface
/// immediately — build_registry does NOT continue past the first bad
/// reaction.
#[allow(clippy::result_large_err)]
pub fn build_registry(ir: &SceneIR, engine: &Engine) -> Result<ReactionRegistry, SceneError> {
    let mut registry = ReactionRegistry::new();
    for (idx, node) in ir.scene.body.iter().enumerate() {
        if let SceneBodyNode::On(on) = node {
            let selector = resolve_selector_for_on(ir, idx, on)?;
            let kind =
                EventKind::parse(&selector.kind).ok_or_else(|| SceneError::UnknownEventField {
                    event_kind: selector.kind.clone(),
                    field: "<kind>".to_string(),
                    help: format!(
                        "unknown event kind `{}`; run `ark scene check` for the full list",
                        selector.kind
                    ),
                    src: miette::NamedSource::new(ir.path.display().to_string(), ir.src.clone()),
                    span: miette::SourceSpan::new(0.into(), ir.src.len().min(1)),
                })?;
            let predicate = match &on.when {
                Some(src) => Some(compile_in_scope(engine, src, RhaiScope::Event)?),
                None => None,
            };
            let ext_name = if kind == EventKind::Ext {
                ext_name_of(&selector)
            } else {
                None
            };
            let entry = Entry {
                selector,
                predicate,
                ops: on.ops.clone(),
                origin: ReactionOrigin::user_scene(ir.path.clone()),
            };
            registry.insert(kind, ext_name, entry);
        }
    }
    Ok(registry)
}

/// Extract the `name` field pattern value from an Ext selector
/// when pinned as an exact string — used to key the secondary index.
/// `None` for non-Ext or for selectors whose `name=` pattern is
/// a glob/regex (dispatchers still find those via the primary index).
fn ext_name_of(selector: &EventSelector) -> Option<String> {
    let fp = selector.field_patterns.get("name")?;
    if matches!(fp.match_type, MatchType::Exact) {
        Some(fp.raw.clone())
    } else {
        None
    }
}

/// Resolve the `EventSelector` for an `on` node, consulting the AST
/// first (when T-011 has populated it) and falling back to the raw
/// kdl_doc otherwise.
#[allow(clippy::result_large_err)]
fn resolve_selector_for_on(
    ir: &SceneIR,
    body_idx: usize,
    on: &OnNode,
) -> Result<EventSelector, SceneError> {
    if let Some(sel) = &on.selector {
        return Ok(sel.clone());
    }
    // Fall back to kdl_doc extraction.
    let doc = ir
        .kdl_doc
        .as_ref()
        .ok_or_else(|| malformed_on_node(ir, "raw KDL document unavailable"))?;
    let on_nodes: Vec<&kdl::KdlNode> = collect_on_nodes(doc);
    let preceding_ons = ir.scene.body[..body_idx]
        .iter()
        .filter(|n| matches!(n, SceneBodyNode::On(_)))
        .count();
    let raw = on_nodes
        .get(preceding_ons)
        .copied()
        .ok_or_else(|| malformed_on_node(ir, "could not locate `on` node in raw KDL"))?;
    selector_from_kdl_node(raw, ir)
}

/// Walk a parsed KDL document to find every `on` node under the
/// top-level `scene` wrapper in source order.
fn collect_on_nodes(doc: &kdl::KdlDocument) -> Vec<&kdl::KdlNode> {
    let mut out: Vec<&kdl::KdlNode> = Vec::new();
    let Some(scene_node) = doc.nodes().iter().find(|n| n.name().value() == "scene") else {
        return out;
    };
    let Some(children) = scene_node.children() else {
        return out;
    };
    for node in children.nodes() {
        if node.name().value() == "on" {
            out.push(node);
        }
    }
    out
}

/// Build an [`EventSelector`] from a raw `on <kind> field=pat …` KDL node.
#[allow(clippy::result_large_err)]
fn selector_from_kdl_node(node: &kdl::KdlNode, ir: &SceneIR) -> Result<EventSelector, SceneError> {
    let mut kind: Option<String> = None;
    let mut field_patterns: BTreeMap<String, FieldPattern> = BTreeMap::new();
    for entry in node.entries() {
        match entry.name() {
            None => {
                let val = entry.value().as_string().map(|s| s.to_string());
                match val {
                    Some(s) if kind.is_none() => kind = Some(s),
                    _ => {
                        return Err(malformed_on_node(
                            ir,
                            "positional argument on `on` node must be the event kind string",
                        ));
                    }
                }
            }
            Some(ident) => {
                let field_name = ident.value().to_string();
                if field_name == "when" {
                    continue;
                }
                let raw_value = entry
                    .value()
                    .as_string()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| match entry.value() {
                        kdl::KdlValue::Integer(i) => i.to_string(),
                        kdl::KdlValue::Float(f) => f.to_string(),
                        kdl::KdlValue::Bool(b) => b.to_string(),
                        _ => String::new(),
                    });
                let fp = FieldPattern::parse(&field_name, &raw_value).map_err(|e| {
                    SceneError::UnknownEventField {
                        event_kind: kind.clone().unwrap_or_default(),
                        field: field_name.clone(),
                        help: format!("selector field `{field_name}` has invalid pattern: {e}"),
                        src: miette::NamedSource::new(
                            ir.path.display().to_string(),
                            ir.src.clone(),
                        ),
                        span: miette::SourceSpan::new(0.into(), ir.src.len().min(1)),
                    }
                })?;
                field_patterns.insert(field_name, fp);
            }
        }
    }
    let kind = kind.ok_or_else(|| {
        malformed_on_node(
            ir,
            "`on` node is missing the event kind positional argument",
        )
    })?;
    Ok(EventSelector {
        kind,
        field_patterns,
    })
}

/// Build a diagnostic for an `on` node that couldn't be interpreted.
fn malformed_on_node(ir: &SceneIR, message: &str) -> SceneError {
    SceneError::UnknownEventField {
        event_kind: String::new(),
        field: "<selector>".to_string(),
        help: message.to_string(),
        src: miette::NamedSource::new(ir.path.display().to_string(), ir.src.clone()),
        span: miette::SourceSpan::new(0.into(), ir.src.len().min(1)),
    }
}

// ---------------------------------------------------------------------------
// Selector matching + captured locals (T-058 / T-059).
// ---------------------------------------------------------------------------

/// Match `selector` against a live [`CoreEvent`]. Returns the map of
/// captured locals on match, or `None` on no-match.
///
/// The event is flattened via [`FlatEvent`] first. The flat JSON has
/// two top-level keys: `name` (the dotted event name) and `payload`
/// (the event's own fields). Selector field patterns are resolved as
/// follows:
///
/// 1. `name` → matches the flat `name` field (e.g. `"ark.core.error"`).
/// 2. `payload` → matches the full payload JSON blob as a string.
/// 3. `payload.X` → explicit lookup into `payload.X`.
/// 4. Any other bare field → looked up in `payload` first; if absent,
///    falls through to the top-level flat JSON (covers `name` fallback).
pub fn match_selector(
    selector: &EventSelector,
    event: &CoreEvent,
) -> Option<BTreeMap<String, Dynamic>> {
    // Kind check.
    if EventKind::parse(&selector.kind).map(|k| k == EventKind::of(event)) != Some(true) {
        return None;
    }

    let flat = FlatEvent::from(event);

    // Build JSON from the flat event for field lookup.
    let flat_json = serde_json::to_value(&flat).ok()?;
    let payload_json = flat_json.get("payload").cloned();

    let mut captures: BTreeMap<String, Dynamic> = BTreeMap::new();
    for (field, pattern) in &selector.field_patterns {
        let lookup = lookup_field_value(field, &flat_json, payload_json.as_ref());
        let value_str = match lookup {
            Some(v) => v,
            None => return None, // field absent ⇒ no match
        };
        match match_field_pattern(pattern, &value_str, &mut captures) {
            true => {
                captures
                    .entry(field.clone())
                    .or_insert_with(|| Dynamic::from(value_str.clone()));
            }
            false => return None,
        }
    }
    Some(captures)
}

/// Resolve `field` against a flattened event JSON.
///
/// Lookup order:
///
/// 1. `name` → always resolves to the flat top-level `name` field
///    (e.g. `"ark.core.error"` or `"myext.tool.use"`).
/// 2. `payload.X` explicit prefix → directly into `payload.X`.
/// 3. Bare field → try `payload.<field>` first (event-specific fields
///    live here for all CoreEvent variants: `error`, `message`, `level`,
///    `spec`, `terminated_at`, and extension payload keys).
/// 4. Fall through to the flat JSON top-level.
fn lookup_field_value(
    field: &str,
    flat_json: &serde_json::Value,
    payload_json: Option<&serde_json::Value>,
) -> Option<String> {
    // `name` is a reserved top-level key — always the flat event name.
    if field == "name" {
        return flat_json.get("name").map(json_to_match_string);
    }

    // Explicit `payload.X` escape hatch.
    if let Some(rest) = field.strip_prefix("payload.") {
        return payload_json
            .and_then(|p| p.get(rest))
            .map(json_to_match_string);
    }

    // Try payload first (all variant-specific fields live here).
    if let Some(payload) = payload_json {
        if let Some(v) = payload.get(field) {
            return Some(json_to_match_string(v));
        }
    }

    // Fall through to flat top-level.
    flat_json.get(field).map(json_to_match_string)
}

/// Stringify a `serde_json::Value` for selector matching.
fn json_to_match_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// Test `pattern` against `value`, populating `captures` with any
/// named groups the match produces. Returns `true` on match.
fn match_field_pattern(
    pattern: &FieldPattern,
    value: &str,
    captures: &mut BTreeMap<String, Dynamic>,
) -> bool {
    match pattern.match_type {
        MatchType::Exact => value == pattern.raw,
        MatchType::Glob => globset::Glob::new(&pattern.raw)
            .ok()
            .map(|g| g.compile_matcher().is_match(value))
            .unwrap_or(false),
        MatchType::Regex => {
            let Ok(re) = regex::Regex::new(&pattern.raw) else {
                return false;
            };
            let Some(caps) = re.captures(value) else {
                return false;
            };
            for name in re.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    captures.insert(name.to_string(), Dynamic::from(m.as_str().to_string()));
                }
            }
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::ExtEvent;
    use std::path::PathBuf;

    // ---- EventKind ----

    #[test]
    fn event_kind_parse_accepts_both_cases() {
        assert_eq!(
            EventKind::parse("SessionStarted"),
            Some(EventKind::SessionStarted)
        );
        assert_eq!(
            EventKind::parse("session_started"),
            Some(EventKind::SessionStarted)
        );
        assert_eq!(EventKind::parse("nope"), None);
    }

    #[test]
    fn event_kind_of_classifies_ext() {
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "myext".into(),
            kind: "something".into(),
            payload: serde_json::json!({}),
        });
        assert_eq!(EventKind::of(&evt), EventKind::Ext);
    }

    #[test]
    fn event_kind_of_classifies_error() {
        let evt = CoreEvent::Error {
            error: "boom".into(),
        };
        assert_eq!(EventKind::of(&evt), EventKind::Error);
    }

    #[test]
    fn event_kind_of_classifies_log() {
        let evt = CoreEvent::Log {
            level: "info".into(),
            message: "hello".into(),
            target: None,
        };
        assert_eq!(EventKind::of(&evt), EventKind::Log);
    }

    // ---- Registry insert / lookup ----

    #[test]
    fn registry_insert_primary_and_secondary() {
        let mut reg = ReactionRegistry::new();
        let sel_err = EventSelector {
            kind: "Error".into(),
            field_patterns: BTreeMap::new(),
        };
        let entry = Entry {
            selector: sel_err,
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::Error, None, entry);
        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
        assert_eq!(reg.len(), 1);

        // Insert an Ext reaction with a pinned name for secondary index.
        let mut ext_sel = EventSelector {
            kind: "Ext".into(),
            field_patterns: BTreeMap::new(),
        };
        ext_sel.field_patterns.insert(
            "name".into(),
            FieldPattern {
                raw: "myext.something".into(),
                match_type: MatchType::Exact,
            },
        );
        let ext_entry = Entry {
            selector: ext_sel,
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::Ext, Some("myext.something".into()), ext_entry);
        assert_eq!(reg.by_kind(EventKind::Ext).len(), 1);
        assert_eq!(reg.by_ext_name("myext.something").len(), 1);
        assert_eq!(reg.by_ext_name("myext.missing").len(), 0);
    }

    #[test]
    fn registry_insert_rejects_name_on_non_ext() {
        let mut reg = ReactionRegistry::new();
        let sel = EventSelector {
            kind: "Error".into(),
            field_patterns: BTreeMap::new(),
        };
        let entry = Entry {
            selector: sel,
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::Error, Some("bogus".into()), entry);
        // Not mirrored to secondary index.
        assert_eq!(reg.by_ext_name("bogus").len(), 0);
        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
    }

    // ---- build_registry against real scenes ----

    #[test]
    fn build_registry_populates_both_indices() {
        let src = r#"
scene "s" {
    on Error { }
    on Ext name="myext.hello" { }
    on Ext name="myext.world" when="1 == 1" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
        assert_eq!(reg.by_kind(EventKind::Ext).len(), 2);
        assert_eq!(reg.by_ext_name("myext.hello").len(), 1);
        assert_eq!(reg.by_ext_name("myext.world").len(), 1);
        let world = &reg.by_ext_name("myext.world")[0];
        assert!(world.predicate.is_some(), "when= should compile");
    }

    #[test]
    fn build_registry_rejects_unknown_kind() {
        let src = r#"
scene "s" {
    on BogusKind { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let err = build_registry(&ir, &engine).expect_err("unknown kind should reject");
        assert!(matches!(err, SceneError::UnknownEventField { .. }));
    }

    #[test]
    fn build_registry_rejects_bad_predicate() {
        let src = r#"
scene "s" {
    on Error when="1 +" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let err = build_registry(&ir, &engine).expect_err("bad predicate should reject");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    // ---- match_selector ----

    #[test]
    fn match_exact_field_on_error() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "error".into(),
            FieldPattern {
                raw: "boom".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "Error".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Error {
            error: "boom".into(),
        };
        let caps = match_selector(&sel, &evt).expect("should match");
        assert_eq!(
            caps.get("error").unwrap().clone().into_string().unwrap(),
            "boom"
        );
    }

    #[test]
    fn no_match_different_kind() {
        let sel = EventSelector {
            kind: "Error".into(),
            field_patterns: BTreeMap::new(),
        };
        let evt = CoreEvent::Log {
            level: "info".into(),
            message: "hi".into(),
            target: None,
        };
        assert!(match_selector(&sel, &evt).is_none());
    }

    #[test]
    fn match_glob_on_log_message() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "message".into(),
            FieldPattern {
                raw: "hello*".into(),
                match_type: MatchType::Glob,
            },
        );
        let sel = EventSelector {
            kind: "Log".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Log {
            level: "info".into(),
            message: "hello world".into(),
            target: None,
        };
        let caps = match_selector(&sel, &evt).expect("glob should match");
        assert_eq!(
            caps.get("message").unwrap().clone().into_string().unwrap(),
            "hello world"
        );
    }

    #[test]
    fn match_regex_named_group_on_error() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "error".into(),
            FieldPattern {
                raw: r"^(?P<msg>\w+)$".into(),
                match_type: MatchType::Regex,
            },
        );
        let sel = EventSelector {
            kind: "Error".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Error {
            error: "boom".into(),
        };
        let caps = match_selector(&sel, &evt).expect("regex match");
        assert_eq!(
            caps.get("msg").unwrap().clone().into_string().unwrap(),
            "boom"
        );
    }

    // ---- Ext hybrid payload access ----

    #[test]
    fn ext_event_hybrid_bare_name_looks_in_payload() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "tool".into(),
            FieldPattern {
                raw: "Bash".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "Ext".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "claude-code".into(),
            kind: "tool.use".into(),
            payload: serde_json::json!({ "tool": "Bash" }),
        });
        let caps = match_selector(&sel, &evt).expect("hybrid access should match");
        assert_eq!(
            caps.get("tool").unwrap().clone().into_string().unwrap(),
            "Bash"
        );
    }

    #[test]
    fn ext_event_explicit_payload_prefix_matches() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "payload.tool".into(),
            FieldPattern {
                raw: "Bash".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "Ext".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "claude-code".into(),
            kind: "tool.use".into(),
            payload: serde_json::json!({ "tool": "Bash" }),
        });
        assert!(match_selector(&sel, &evt).is_some());
    }

    #[test]
    fn ext_event_name_field_bypasses_payload() {
        // `name=` should pin the flat event `name` field (e.g. `claude-code.tool.use`).
        let mut fps = BTreeMap::new();
        fps.insert(
            "name".into(),
            FieldPattern {
                raw: "claude-code.tool.use".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "Ext".into(),
            field_patterns: fps,
        };
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "claude-code".into(),
            kind: "tool.use".into(),
            payload: serde_json::json!({ "name": "different" }),
        });
        assert!(match_selector(&sel, &evt).is_some());
    }

    // ---- T-060: when= evaluation against captured locals ----

    #[test]
    fn when_predicate_can_see_captured_locals() {
        // Use a glob field selector so the value is captured into locals.
        let src = r#"
scene "s" {
    on Error error="(glob)*boom*" when="error.ends_with(\"boom\")" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        let entries = reg.by_kind(EventKind::Error);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let evt = CoreEvent::Error {
            error: "something boom".into(),
        };
        let locals = match_selector(&entry.selector, &evt).expect("match");
        // `error` should be captured as a local from the glob match.
        assert!(
            locals.contains_key("error"),
            "error not captured: {locals:?}"
        );
        use crate::context::{SessionSnapshot, build_event_scope};
        use crate::rhai::eval_bool_in_scope;
        let session = SessionSnapshot::default();
        let mut scope = build_event_scope(&evt, &session, &locals);
        let program = entry.predicate.as_ref().expect("predicate");
        let ok = eval_bool_in_scope(&engine, program, RhaiScope::Event, &mut scope).expect("eval");
        assert!(ok, "error ending with boom should match the predicate");
    }

    #[test]
    fn when_false_skips_reaction_at_eval_time() {
        let src = r#"
scene "s" {
    on Error when="false" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        let entry = &reg.by_kind(EventKind::Error)[0];
        let evt = CoreEvent::Error { error: "x".into() };
        let locals = match_selector(&entry.selector, &evt).expect("match");
        use crate::context::{SessionSnapshot, build_event_scope};
        use crate::rhai::eval_bool_in_scope;
        let mut scope = build_event_scope(&evt, &SessionSnapshot::default(), &locals);
        let ok = eval_bool_in_scope(
            &engine,
            entry.predicate.as_ref().unwrap(),
            RhaiScope::Event,
            &mut scope,
        )
        .expect("eval");
        assert!(!ok, "when=false should return false");
    }

    // ---- overlapping selectors each run ----

    #[test]
    fn overlapping_selectors_both_registered() {
        let src = r#"
scene "s" {
    on Ext name="myext.a" { }
    on Ext name="myext.b" { }
    on Ext { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        // Every `on Ext` block is registered — no dedup.
        assert_eq!(reg.by_kind(EventKind::Ext).len(), 3);
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "myext".into(),
            kind: "a".into(),
            payload: serde_json::json!({}),
        });
        let mut matched = 0usize;
        for e in reg.by_kind(EventKind::Ext) {
            if match_selector(&e.selector, &evt).is_some() {
                matched += 1;
            }
        }
        // Matches: `name="myext.a"` (flat name = "myext.a"), and bare `on Ext`.
        // `name="myext.b"` does not match. So 2.
        assert_eq!(matched, 2);
    }

    // ---- remove_matching ----

    #[test]
    fn remove_matching_drops_by_selector() {
        let src = r#"
scene "s" {
    on Error { }
    on Log { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let mut reg = build_registry(&ir, &engine).expect("build");
        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
        assert_eq!(reg.by_kind(EventKind::Log).len(), 1);

        let clear = EventSelector {
            kind: "Error".into(),
            field_patterns: BTreeMap::new(),
        };
        reg.remove_matching(&clear);
        assert_eq!(reg.by_kind(EventKind::Error).len(), 0);
        assert_eq!(reg.by_kind(EventKind::Log).len(), 1);
    }
}

// ---------------------------------------------------------------------------
// Clear-selector match helper.
// ---------------------------------------------------------------------------

/// Literal selector comparison used by [`ReactionRegistry::remove_matching`]
/// and by [`crate::clear::apply_clear_reactions`].
///
/// Two selectors "match" when they share the same kind AND every field
/// pattern on the clear selector exists verbatim on the candidate
/// selector (same raw + match_type). Clear selectors with no field
/// patterns match every reaction of the same kind.
pub fn clear_selector_matches(clear: &EventSelector, candidate: &EventSelector) -> bool {
    if clear.kind != candidate.kind {
        return false;
    }
    for (k, v) in &clear.field_patterns {
        match candidate.field_patterns.get(k) {
            Some(other) if other == v => {}
            _ => return false,
        }
    }
    true
}
