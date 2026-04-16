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
//! - `by_user_event_name`: dotted event name (`ark.acp.tool_call`,
//!   `myext.something`) → Vec<Entry>. Populated only for UserEvent
//!   selectors that pin a `name=<literal>` field pattern. Entries in
//!   this index are ALSO present in `by_kind` under `"user_event"` —
//!   the secondary index is purely a dispatcher fan-out optimisation.
//!
//! # Selector matching (T-058 + T-059)
//!
//! [`match_selector`] walks a selector against a live `AgentEvent`,
//! returning captured locals on match. Rules:
//!
//! 1. Field value comes from the event's flat JSON representation
//!    (round-trip via `serde_json`).
//! 2. For `UserEvent` selectors with bare field names (not `name`,
//!    `source`, `payload`), the lookup falls through to
//!    `payload.<field>`. `payload.X` is an explicit escape hatch.
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

use ark_types::AgentEvent;
use rhai::Dynamic;

use crate::ast::ops::OpNode;
use crate::ast::selector::{EventSelector, FieldPattern, MatchType};
use crate::ast::{OnNode, SceneBodyNode};
use crate::error::SceneError;
use crate::parse::SceneIR;
use crate::rhai::{compile_in_scope, Engine, Program, RhaiScope};

// ---------------------------------------------------------------------------
// EventKind — snake_case classification of `AgentEvent`.
// ---------------------------------------------------------------------------

/// Enumerated discriminator of [`AgentEvent`] — matches the serde
/// `tag = "kind"` rename (`snake_case`). Used as the primary-index key
/// in [`ReactionRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventKind {
    /// `AgentEvent::Started` — canonical `"started"`.
    Started,
    /// `AgentEvent::TabOpened` — canonical `"tab_opened"`.
    TabOpened,
    /// `AgentEvent::TabClosed` — canonical `"tab_closed"`.
    TabClosed,
    /// `AgentEvent::Progress` — canonical `"progress"`.
    Progress,
    /// `AgentEvent::TaskDone` — canonical `"task_done"`.
    TaskDone,
    /// `AgentEvent::Iteration` — canonical `"iteration"`.
    Iteration,
    /// `AgentEvent::PhaseTransition` — canonical `"phase_transition"`.
    PhaseTransition,
    /// `AgentEvent::ToolUse` — canonical `"tool_use"`.
    ToolUse,
    /// `AgentEvent::Message` — canonical `"message"`.
    Message,
    /// `AgentEvent::FileEdited` — canonical `"file_edited"`.
    FileEdited,
    /// `AgentEvent::ReviewComment` — canonical `"review_comment"`.
    ReviewComment,
    /// `AgentEvent::PermissionAsked` — canonical `"permission_asked"`.
    PermissionAsked,
    /// `AgentEvent::PermissionResolved` — canonical `"permission_resolved"`.
    PermissionResolved,
    /// `AgentEvent::Stall` — canonical `"stall"`.
    Stall,
    /// `AgentEvent::Log` — canonical `"log"`.
    Log,
    /// `AgentEvent::Error` — canonical `"error"`.
    Error,
    /// `AgentEvent::Done` — canonical `"done"`.
    Done,
    /// `AgentEvent::UserEvent` — canonical `"user_event"`.
    UserEvent,
}

impl EventKind {
    /// Canonical snake_case rendering (matches `AgentEvent`'s
    /// `#[serde(rename_all = "snake_case")]` tag).
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Started => "started",
            EventKind::TabOpened => "tab_opened",
            EventKind::TabClosed => "tab_closed",
            EventKind::Progress => "progress",
            EventKind::TaskDone => "task_done",
            EventKind::Iteration => "iteration",
            EventKind::PhaseTransition => "phase_transition",
            EventKind::ToolUse => "tool_use",
            EventKind::Message => "message",
            EventKind::FileEdited => "file_edited",
            EventKind::ReviewComment => "review_comment",
            EventKind::PermissionAsked => "permission_asked",
            EventKind::PermissionResolved => "permission_resolved",
            EventKind::Stall => "stall",
            EventKind::Log => "log",
            EventKind::Error => "error",
            EventKind::Done => "done",
            EventKind::UserEvent => "user_event",
        }
    }

    /// Parse a selector kind token, accepting both canonical snake_case
    /// and the PascalCase spelling scene authors typically use (per R4
    /// — `on FileEdited`, `on PhaseTransition`). Returns `None` for
    /// unknown tokens.
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "started" | "Started" => Some(EventKind::Started),
            "tab_opened" | "TabOpened" => Some(EventKind::TabOpened),
            "tab_closed" | "TabClosed" => Some(EventKind::TabClosed),
            "progress" | "Progress" => Some(EventKind::Progress),
            "task_done" | "TaskDone" => Some(EventKind::TaskDone),
            "iteration" | "Iteration" => Some(EventKind::Iteration),
            "phase_transition" | "PhaseTransition" => Some(EventKind::PhaseTransition),
            "tool_use" | "ToolUse" => Some(EventKind::ToolUse),
            "message" | "Message" => Some(EventKind::Message),
            "file_edited" | "FileEdited" => Some(EventKind::FileEdited),
            "review_comment" | "ReviewComment" => Some(EventKind::ReviewComment),
            "permission_asked" | "PermissionAsked" => Some(EventKind::PermissionAsked),
            "permission_resolved" | "PermissionResolved" => Some(EventKind::PermissionResolved),
            "stall" | "Stall" => Some(EventKind::Stall),
            "log" | "Log" => Some(EventKind::Log),
            "error" | "Error" => Some(EventKind::Error),
            "done" | "Done" => Some(EventKind::Done),
            "user_event" | "UserEvent" => Some(EventKind::UserEvent),
            _ => None,
        }
    }

    /// Classify a live [`AgentEvent`] into its [`EventKind`].
    pub fn of(event: &AgentEvent) -> Self {
        match event {
            AgentEvent::Started { .. } => EventKind::Started,
            AgentEvent::TabOpened { .. } => EventKind::TabOpened,
            AgentEvent::TabClosed { .. } => EventKind::TabClosed,
            AgentEvent::Progress { .. } => EventKind::Progress,
            AgentEvent::TaskDone { .. } => EventKind::TaskDone,
            AgentEvent::Iteration { .. } => EventKind::Iteration,
            AgentEvent::PhaseTransition { .. } => EventKind::PhaseTransition,
            AgentEvent::ToolUse { .. } => EventKind::ToolUse,
            AgentEvent::Message { .. } => EventKind::Message,
            AgentEvent::FileEdited { .. } => EventKind::FileEdited,
            AgentEvent::ReviewComment { .. } => EventKind::ReviewComment,
            AgentEvent::PermissionAsked { .. } => EventKind::PermissionAsked,
            AgentEvent::PermissionResolved { .. } => EventKind::PermissionResolved,
            AgentEvent::Stall { .. } => EventKind::Stall,
            AgentEvent::Log { .. } => EventKind::Log,
            AgentEvent::Error { .. } => EventKind::Error,
            AgentEvent::Done { .. } => EventKind::Done,
            AgentEvent::UserEvent { .. } => EventKind::UserEvent,
            // `#[non_exhaustive]` upstream — any future variant lands
            // here with a synthetic classification as UserEvent so the
            // dispatcher degrades predictably. T-057-style validation
            // gates user-facing unknowns at parse time.
            _ => EventKind::UserEvent,
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
/// the ordered op list, and origin attribution. Cloneable so the
/// registry can stash the entry in both primary + secondary indices
/// cheaply (op list is an `Arc`-friendly `Vec`; the compiled program
/// clones the underlying `rhai::AST` cheaply thanks to its internal
/// `Arc`).
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
/// `UserEvent` name. See module docs for indexing semantics.
#[derive(Debug, Clone, Default)]
pub struct ReactionRegistry {
    by_kind: BTreeMap<&'static str, Vec<Entry>>,
    by_user_event_name: BTreeMap<String, Vec<Entry>>,
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
    /// the secondary index when `user_event_name` is `Some`. Callers
    /// that pass a non-`UserEvent` kind together with a name will see
    /// the name ignored (the mirror is only written for UserEvent).
    pub fn insert(
        &mut self,
        kind: EventKind,
        user_event_name: Option<String>,
        entry: Entry,
    ) {
        self.by_kind
            .entry(kind.as_str())
            .or_default()
            .push(entry.clone());
        if let Some(name) = user_event_name {
            if kind == EventKind::UserEvent {
                self.by_user_event_name
                    .entry(name)
                    .or_default()
                    .push(entry);
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

    /// Fetch every reaction registered under a `UserEvent:<name>`
    /// selector. Subset of `by_kind(EventKind::UserEvent)`.
    pub fn by_user_event_name(&self, name: &str) -> &[Entry] {
        self.by_user_event_name
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
        // Rebuild secondary index from surviving UserEvent entries.
        self.by_user_event_name.clear();
        if let Some(ue) = self.by_kind.get("user_event") {
            for entry in ue {
                if let Some(name) = user_event_name_of(&entry.selector) {
                    self.by_user_event_name
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

    /// Iterate every `(user_event_name, entries)` pair.
    pub fn iter_user_events(&self) -> impl Iterator<Item = (&String, &Vec<Entry>)> {
        self.by_user_event_name.iter()
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
/// reaction. Callers that want collect-all behaviour should wrap with
/// their own driver.
#[allow(clippy::result_large_err)]
pub fn build_registry(
    ir: &SceneIR,
    engine: &Engine,
) -> Result<ReactionRegistry, SceneError> {
    let mut registry = ReactionRegistry::new();
    for (idx, node) in ir.scene.body.iter().enumerate() {
        if let SceneBodyNode::On(on) = node {
            let selector = resolve_selector_for_on(ir, idx, on)?;
            let kind = EventKind::parse(&selector.kind).ok_or_else(|| {
                // An unknown selector kind at registry build time
                // falls back to `scene/unknown-event-field`-class
                // diagnostic; we reuse `UnknownEventField` with a
                // synthetic "kind" field name rather than adding a
                // brand-new variant until the compile-pass pipeline
                // catches up.
                SceneError::UnknownEventField {
                    event_kind: selector.kind.clone(),
                    field: "<kind>".to_string(),
                    help: format!(
                        "unknown event kind `{}`; run `ark scene check` for the full list",
                        selector.kind
                    ),
                    src: miette::NamedSource::new(
                        ir.path.display().to_string(),
                        ir.src.clone(),
                    ),
                    span: miette::SourceSpan::new(0.into(), ir.src.len().min(1)),
                }
            })?;
            let predicate = match &on.when {
                Some(src) => Some(compile_in_scope(engine, src, RhaiScope::Event)?),
                None => None,
            };
            let user_event_name = if kind == EventKind::UserEvent {
                user_event_name_of(&selector)
            } else {
                None
            };
            let entry = Entry {
                selector,
                predicate,
                ops: on.ops.clone(),
                origin: ReactionOrigin::user_scene(ir.path.clone()),
            };
            registry.insert(kind, user_event_name, entry);
        }
    }
    Ok(registry)
}

/// Extract the `name` field pattern value from a UserEvent selector
/// when pinned as an exact string — used to key the secondary index.
/// `None` for non-UserEvent or for selectors whose `name=` pattern is
/// a glob/regex (dispatchers still find those via the primary index).
fn user_event_name_of(selector: &EventSelector) -> Option<String> {
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
    // Fall back to kdl_doc extraction. The scene body of `ir.scene`
    // is what facet-kdl produced; the raw `ir.kdl_doc` preserves the
    // `on` nodes under the top-level `scene "<name>" { … }` wrapper.
    let doc = ir
        .kdl_doc
        .as_ref()
        .ok_or_else(|| malformed_on_node(ir, "raw KDL document unavailable"))?;
    let on_nodes: Vec<&kdl::KdlNode> = collect_on_nodes(doc);
    // Count the number of preceding `on` nodes in `ir.scene.body`
    // before `body_idx` to find the matching raw node.
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

/// Build an [`EventSelector`] from a raw `on <kind> field=pat …` KDL
/// node.
///
/// Positional arguments (strings) after the node name form the kind
/// token — we accept the FIRST positional string as the kind, so both
/// `on FileEdited` (bare identifier, parsed as an implicit string by
/// kdl) and `on "FileEdited"` shapes flow through the same code path.
/// Subsequent non-string positional arguments surface as a
/// [`SceneError::UnknownEventField`] (they are grammatically ill-formed).
///
/// Named properties (`path="x"`, `tool="Bash"`) become field patterns
/// via [`FieldPattern::parse`].
#[allow(clippy::result_large_err)]
fn selector_from_kdl_node(
    node: &kdl::KdlNode,
    ir: &SceneIR,
) -> Result<EventSelector, SceneError> {
    let mut kind: Option<String> = None;
    let mut field_patterns: BTreeMap<String, FieldPattern> = BTreeMap::new();
    for entry in node.entries() {
        match entry.name() {
            None => {
                // Positional arg — first one is the kind.
                let val = entry
                    .value()
                    .as_string()
                    .map(|s| s.to_string())
                    .or_else(|| {
                        // Some KDL 2.0 parsers lift bare identifiers
                        // into typed `KdlValue::String` automatically;
                        // when they don't, try `to_string()` via
                        // Display. Integers / bools aren't valid
                        // kinds so we reject them.
                        None
                    });
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
                // `when` is the Rhai guard predicate, not a selector
                // field — skip it so it doesn't pollute the field-
                // pattern map and cause false negatives in matching.
                if field_name == "when" {
                    continue;
                }
                let raw_value = entry
                    .value()
                    .as_string()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        // Coerce numeric / bool values to strings for
                        // field-pattern matching. This is loose by
                        // design — field values in `AgentEvent` are
                        // usually strings; matching against a
                        // stringified int is still predictable.
                        match entry.value() {
                            kdl::KdlValue::Integer(i) => i.to_string(),
                            kdl::KdlValue::Float(f) => f.to_string(),
                            kdl::KdlValue::Bool(b) => b.to_string(),
                            _ => String::new(),
                        }
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
        malformed_on_node(ir, "`on` node is missing the event kind positional argument")
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

/// Match `selector` against a live [`AgentEvent`]. Returns the map of
/// captured locals on match, or `None` on no-match.
///
/// Captured locals currently include:
///
/// - Every field name matched by the selector whose value is a string
///   — the matched string is bound as `{field_name}` in the event
///   scope.
/// - For regex matches, the full match plus every named capture
///   group.
pub fn match_selector(
    selector: &EventSelector,
    event: &AgentEvent,
) -> Option<BTreeMap<String, Dynamic>> {
    // Kind check.
    if EventKind::parse(&selector.kind).map(|k| k == EventKind::of(event)) != Some(true) {
        return None;
    }
    let event_json = serde_json::to_value(event).ok()?;
    let is_user_event = matches!(event, AgentEvent::UserEvent { .. });
    let payload_json = if is_user_event {
        event_json.get("payload").cloned()
    } else {
        None
    };

    let mut captures: BTreeMap<String, Dynamic> = BTreeMap::new();
    for (field, pattern) in &selector.field_patterns {
        let lookup = lookup_field_value(field, &event_json, payload_json.as_ref(), is_user_event);
        let value_str = match lookup {
            Some(v) => v,
            None => return None, // field absent ⇒ no match
        };
        match match_field_pattern(pattern, &value_str, &mut captures) {
            true => {
                // On successful match, bind the raw matched string
                // under the field name unless it's a reserved
                // UserEvent top-level name whose semantic matches the
                // field binding convention already.
                captures
                    .entry(field.clone())
                    .or_insert_with(|| Dynamic::from(value_str.clone()));
            }
            false => return None,
        }
    }
    Some(captures)
}

/// Resolve `field` against a flattened event JSON. For UserEvent
/// selectors the lookup hybrid-accesses the payload (T-059):
///
/// 1. Reserved top-level keys (`name`, `source`, `payload`) bypass the
///    payload redirect.
/// 2. `payload.X` explicit prefix routes directly into the payload.
/// 3. Bare field names on UserEvent fall through to `payload.<field>`
///    when they're not present at the top level.
fn lookup_field_value(
    field: &str,
    event_json: &serde_json::Value,
    payload_json: Option<&serde_json::Value>,
    is_user_event: bool,
) -> Option<String> {
    const RESERVED: &[&str] = &["name", "source", "payload"];

    // Explicit `payload.X` escape hatch.
    if let Some(rest) = field.strip_prefix("payload.") {
        return payload_json
            .and_then(|p| p.get(rest))
            .map(json_to_match_string);
    }

    if is_user_event && !RESERVED.contains(&field) {
        // Hybrid access: try payload first; the top-level keys for
        // UserEvent are limited to `kind`, `name`, `source`,
        // `payload`, none of which are a meaningful lookup target
        // under the hybrid rule.
        if let Some(payload) = payload_json {
            if let Some(v) = payload.get(field) {
                return Some(json_to_match_string(v));
            }
        }
    }

    event_json.get(field).map(json_to_match_string)
}

/// Stringify a `serde_json::Value` for selector matching. Strings
/// come through verbatim; numbers / bools via `Display`; objects /
/// arrays / null never match (we return an empty string so glob /
/// regex attempts skip them).
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
            // Named groups become top-level captures.
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
    use ark_types::AgentId;
    use std::path::PathBuf;

    fn sample_id() -> AgentId {
        AgentId::new("test", "agent")
    }

    // ---- EventKind ----

    #[test]
    fn event_kind_parse_accepts_both_cases() {
        assert_eq!(EventKind::parse("FileEdited"), Some(EventKind::FileEdited));
        assert_eq!(
            EventKind::parse("file_edited"),
            Some(EventKind::FileEdited)
        );
        assert_eq!(EventKind::parse("nope"), None);
    }

    #[test]
    fn event_kind_of_classifies_user_event() {
        let evt = AgentEvent::UserEvent {
            name: "x".into(),
            source: "scene".into(),
            payload: serde_json::json!({}),
        };
        assert_eq!(EventKind::of(&evt), EventKind::UserEvent);
    }

    #[test]
    fn event_kind_of_classifies_error() {
        let evt = AgentEvent::Error {
            id: sample_id(),
            message: "boom".into(),
        };
        assert_eq!(EventKind::of(&evt), EventKind::Error);
    }

    // ---- Registry insert / lookup ----

    #[test]
    fn registry_insert_primary_and_secondary() {
        let mut reg = ReactionRegistry::new();
        let mut sel_fe = EventSelector {
            kind: "FileEdited".into(),
            field_patterns: BTreeMap::new(),
        };
        sel_fe.field_patterns.insert(
            "path".into(),
            FieldPattern {
                raw: "**/*.md".into(),
                match_type: MatchType::Glob,
            },
        );
        let entry = Entry {
            selector: sel_fe,
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::FileEdited, None, entry);
        assert_eq!(reg.by_kind(EventKind::FileEdited).len(), 1);
        assert_eq!(reg.len(), 1);

        let mut ue_sel = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: BTreeMap::new(),
        };
        ue_sel.field_patterns.insert(
            "name".into(),
            FieldPattern {
                raw: "user.hello".into(),
                match_type: MatchType::Exact,
            },
        );
        let ue_entry = Entry {
            selector: ue_sel,
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::UserEvent, Some("user.hello".into()), ue_entry);
        assert_eq!(reg.by_kind(EventKind::UserEvent).len(), 1);
        assert_eq!(reg.by_user_event_name("user.hello").len(), 1);
        assert_eq!(reg.by_user_event_name("user.missing").len(), 0);
    }

    #[test]
    fn registry_insert_rejects_name_on_non_user_event() {
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
        assert_eq!(reg.by_user_event_name("bogus").len(), 0);
        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
    }

    // ---- build_registry against real scenes ----

    #[test]
    fn build_registry_populates_both_indices() {
        let src = r#"
scene "s" {
    on FileEdited path="**/*.md" { }
    on UserEvent name="user.hello" { }
    on UserEvent name="user.world" when="1 == 1" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        assert_eq!(reg.by_kind(EventKind::FileEdited).len(), 1);
        assert_eq!(reg.by_kind(EventKind::UserEvent).len(), 2);
        assert_eq!(reg.by_user_event_name("user.hello").len(), 1);
        assert_eq!(reg.by_user_event_name("user.world").len(), 1);
        let world = &reg.by_user_event_name("user.world")[0];
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
    on FileEdited when="1 +" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let err = build_registry(&ir, &engine).expect_err("bad predicate should reject");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    // ---- match_selector ----

    #[test]
    fn match_exact_field() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "tool".into(),
            FieldPattern {
                raw: "Bash".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "ToolUse".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::ToolUse {
            id: sample_id(),
            tool: "Bash".into(),
            input_summary: "ls".into(),
        };
        let caps = match_selector(&sel, &evt).expect("should match");
        assert_eq!(caps.get("tool").unwrap().clone().into_string().unwrap(), "Bash");
    }

    #[test]
    fn no_match_different_kind() {
        let sel = EventSelector {
            kind: "FileEdited".into(),
            field_patterns: BTreeMap::new(),
        };
        let evt = AgentEvent::Error {
            id: sample_id(),
            message: "x".into(),
        };
        assert!(match_selector(&sel, &evt).is_none());
    }

    #[test]
    fn match_glob_captures_path() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "path".into(),
            FieldPattern {
                raw: "**/*.md".into(),
                match_type: MatchType::Glob,
            },
        );
        let sel = EventSelector {
            kind: "FileEdited".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::FileEdited {
            id: sample_id(),
            path: PathBuf::from("src/README.md"),
            additions: 1,
            deletions: 0,
        };
        let caps = match_selector(&sel, &evt).expect("glob should match");
        assert_eq!(
            caps.get("path").unwrap().clone().into_string().unwrap(),
            "src/README.md"
        );
    }

    #[test]
    fn match_regex_captures_named_groups() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "tool".into(),
            FieldPattern {
                raw: r"^(?P<verb>\w+)$".into(),
                match_type: MatchType::Regex,
            },
        );
        let sel = EventSelector {
            kind: "ToolUse".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::ToolUse {
            id: sample_id(),
            tool: "Bash".into(),
            input_summary: "ls".into(),
        };
        let caps = match_selector(&sel, &evt).expect("regex match");
        assert_eq!(
            caps.get("verb").unwrap().clone().into_string().unwrap(),
            "Bash"
        );
    }

    // ---- T-059: UserEvent hybrid payload access ----

    #[test]
    fn user_event_hybrid_bare_name_looks_in_payload() {
        // `on UserEvent tool=Bash` with the event carrying
        // `payload.tool = "Bash"` matches via hybrid access.
        let mut fps = BTreeMap::new();
        fps.insert(
            "tool".into(),
            FieldPattern {
                raw: "Bash".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::UserEvent {
            name: "ark.acp.tool_call".into(),
            source: "ext:foo".into(),
            payload: serde_json::json!({ "tool": "Bash" }),
        };
        let caps = match_selector(&sel, &evt).expect("hybrid access should match");
        assert_eq!(caps.get("tool").unwrap().clone().into_string().unwrap(), "Bash");
    }

    #[test]
    fn user_event_explicit_payload_prefix_matches() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "payload.tool".into(),
            FieldPattern {
                raw: "Bash".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::UserEvent {
            name: "ark.acp.tool_call".into(),
            source: "ext:foo".into(),
            payload: serde_json::json!({ "tool": "Bash" }),
        };
        assert!(match_selector(&sel, &evt).is_some());
    }

    #[test]
    fn user_event_reserved_name_bypasses_payload() {
        // `name=` should pin the top-level `name` field, not the
        // payload.name (which may differ or be absent).
        let mut fps = BTreeMap::new();
        fps.insert(
            "name".into(),
            FieldPattern {
                raw: "ark.acp.tool_call".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::UserEvent {
            name: "ark.acp.tool_call".into(),
            source: "ext:foo".into(),
            payload: serde_json::json!({ "name": "different" }),
        };
        assert!(match_selector(&sel, &evt).is_some());
        // Sanity: a selector on the different value would NOT match
        // via top-level name.
        let mut fps2 = BTreeMap::new();
        fps2.insert(
            "name".into(),
            FieldPattern {
                raw: "different".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel2 = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: fps2,
        };
        assert!(match_selector(&sel2, &evt).is_none());
    }

    #[test]
    fn user_event_source_reserved_key() {
        let mut fps = BTreeMap::new();
        fps.insert(
            "source".into(),
            FieldPattern {
                raw: "ext:foo".into(),
                match_type: MatchType::Exact,
            },
        );
        let sel = EventSelector {
            kind: "UserEvent".into(),
            field_patterns: fps,
        };
        let evt = AgentEvent::UserEvent {
            name: "x".into(),
            source: "ext:foo".into(),
            payload: serde_json::json!({}),
        };
        assert!(match_selector(&sel, &evt).is_some());
    }

    // ---- T-060: when= evaluation against captured locals ----

    #[test]
    fn when_predicate_can_see_captured_locals() {
        let src = r#"
scene "s" {
    on FileEdited path="**/*.md" when="path.ends_with(\"README.md\")" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        let entries = reg.by_kind(EventKind::FileEdited);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let evt = AgentEvent::FileEdited {
            id: sample_id(),
            path: PathBuf::from("src/README.md"),
            additions: 1,
            deletions: 0,
        };
        let locals = match_selector(&entry.selector, &evt).expect("match");
        // Build an event scope including captured locals and eval.
        use crate::context::{build_event_scope, AgentSnapshot, SessionSnapshot};
        use crate::rhai::eval_bool_in_scope;
        let agent = AgentSnapshot::default();
        let session = SessionSnapshot::default();
        let mut scope = build_event_scope(&evt, &agent, &session, &locals);
        let program = entry.predicate.as_ref().expect("predicate");
        let ok = eval_bool_in_scope(&engine, program, RhaiScope::Event, &mut scope)
            .expect("eval");
        assert!(ok, "README.md should match the predicate");
    }

    #[test]
    fn when_false_skips_reaction_at_eval_time() {
        let src = r#"
scene "s" {
    on FileEdited path="**/*.md" when="false" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        let entry = &reg.by_kind(EventKind::FileEdited)[0];
        let evt = AgentEvent::FileEdited {
            id: sample_id(),
            path: PathBuf::from("x.md"),
            additions: 0,
            deletions: 0,
        };
        let locals = match_selector(&entry.selector, &evt).expect("match");
        use crate::context::{build_event_scope, AgentSnapshot, SessionSnapshot};
        use crate::rhai::eval_bool_in_scope;
        let mut scope = build_event_scope(
            &evt,
            &AgentSnapshot::default(),
            &SessionSnapshot::default(),
            &locals,
        );
        let ok = eval_bool_in_scope(
            &engine,
            entry.predicate.as_ref().unwrap(),
            RhaiScope::Event,
            &mut scope,
        )
        .expect("eval");
        assert!(!ok, "when=false should return false");
    }

    // ---- T-063: overlapping selectors each run ----

    #[test]
    fn overlapping_selectors_both_registered() {
        let src = r#"
scene "s" {
    on FileEdited path="**/*.md" { }
    on FileEdited path="src/**" { }
    on FileEdited { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let reg = build_registry(&ir, &engine).expect("build");
        // Every `on FileEdited` block is registered — no dedup.
        assert_eq!(reg.by_kind(EventKind::FileEdited).len(), 3);
        let evt = AgentEvent::FileEdited {
            id: sample_id(),
            path: PathBuf::from("src/README.md"),
            additions: 0,
            deletions: 0,
        };
        // Count reactions that actually match — glob `**/*.md` matches,
        // glob `src/**` matches, unfiltered matches.
        let mut matched = 0usize;
        for e in reg.by_kind(EventKind::FileEdited) {
            if match_selector(&e.selector, &evt).is_some() {
                matched += 1;
            }
        }
        assert_eq!(matched, 3);
    }

    // ---- remove_matching ----

    #[test]
    fn remove_matching_drops_by_selector() {
        let src = r#"
scene "s" {
    on FileEdited path="**/*.md" { }
    on FileEdited path="**/*.rs" { }
}
"#;
        let ir = crate::parse::parse_scene(src, "test.kdl").expect("parse");
        let engine = Engine::new();
        let mut reg = build_registry(&ir, &engine).expect("build");
        assert_eq!(reg.by_kind(EventKind::FileEdited).len(), 2);

        // Clear: drop entries whose path pattern equals `**/*.md`.
        let mut fps = BTreeMap::new();
        fps.insert(
            "path".into(),
            FieldPattern {
                raw: "**/*.md".into(),
                match_type: MatchType::Glob,
            },
        );
        let clear = EventSelector {
            kind: "FileEdited".into(),
            field_patterns: fps,
        };
        reg.remove_matching(&clear);
        assert_eq!(reg.by_kind(EventKind::FileEdited).len(), 1);
        let remaining = &reg.by_kind(EventKind::FileEdited)[0];
        assert_eq!(remaining.selector.field_patterns.get("path").unwrap().raw, "**/*.rs");
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
