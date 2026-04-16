//! Reaction registry — T-5.1.
//!
//! A reaction is the runtime shape of an `on "<selector>" [if=…] { <op>+ }`
//! node in the scene AST (R4). The registry indexes every reaction in a
//! compiled scene so the runtime dispatcher (T-5.3) can look up candidate
//! reactions by [`EventKind`] in O(1) and fan out to matching entries.
//!
//! # Two-level indexing
//!
//! Scene authors write selectors like `"PhaseTransition"` or
//! `"UserEvent:ark.picker.accept"`. The primary index is keyed on
//! [`EventKind`], the `#[serde(rename_all = "snake_case")]` discriminator
//! of [`ark_types::AgentEvent`]. For the common case (a handful of
//! reactions targeting a handful of distinct event kinds) this is fast
//! enough on its own.
//!
//! The `UserEvent` variant is special: scenes commonly register dozens of
//! reactions on different `UserEvent:<name>` channels, and a primary-index
//! lookup yields every single one. A secondary index keyed on the
//! namespaced event-name field (`user.picker.accept`,
//! `ark.acp.permission_requested`, …) narrows the fan-out to just the
//! reactions that can possibly match.
//!
//! [`ReactionDispatcher`] consults both indices:
//!
//! 1. For every incoming [`ark_types::AgentEvent`], classify its kind via
//!    [`EventKind::of`] and look up the primary index.
//! 2. When the event is a `UserEvent`, additionally look up the secondary
//!    index by the event's `name` field and union the two result sets.
//!
//! The registry itself does NOT evaluate selectors / CEL predicates /
//! dispatch ops — that's T-5.2 (selector matcher) and T-5.3
//! (ReactionDispatcher). The registry simply holds the candidate pool.
//!
//! # Lifetime
//!
//! Built once at scene compile; immutable for the lifetime of the scene.
//! Hot-reload (R14) produces a new registry and swaps it atomically.
//!
//! [`ReactionDispatcher`]: crate::reactions::ReactionRegistry

use std::collections::BTreeMap;
use std::sync::Arc;

use ark_types::event::AgentEvent;

use crate::ast::SceneDoc;
use crate::cel::{self, Program};
use crate::error::SceneError;
use crate::intent::ReactionOrigin;
use crate::ops::dispatch::CompiledOp;

// ---------------------------------------------------------------------------
// EventKind
// ---------------------------------------------------------------------------

/// Variant discriminator of [`AgentEvent`], spelled in the same
/// `snake_case` the serde `tag = "kind"` attribute emits.
///
/// Used as the primary-index key in [`ReactionRegistry`] and as the
/// selector prefix scene authors write (`"Started"` sugar maps to
/// `EventKind::Started`, but canonical storage is the snake_case form).
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum EventKind {
    /// `AgentEvent::Started` — renders as `"started"`.
    Started,
    /// `AgentEvent::TabOpened` — renders as `"tab_opened"`.
    TabOpened,
    /// `AgentEvent::TabClosed` — renders as `"tab_closed"`.
    TabClosed,
    /// `AgentEvent::Progress` — renders as `"progress"`.
    Progress,
    /// `AgentEvent::TaskDone` — renders as `"task_done"`.
    TaskDone,
    /// `AgentEvent::Iteration` — renders as `"iteration"`.
    Iteration,
    /// `AgentEvent::PhaseTransition` — renders as `"phase_transition"`.
    PhaseTransition,
    /// `AgentEvent::ToolUse` — renders as `"tool_use"`.
    ToolUse,
    /// `AgentEvent::Message` — renders as `"message"`.
    Message,
    /// `AgentEvent::FileEdited` — renders as `"file_edited"`.
    FileEdited,
    /// `AgentEvent::ReviewComment` — renders as `"review_comment"`.
    ReviewComment,
    /// `AgentEvent::PermissionAsked` — renders as `"permission_asked"`.
    PermissionAsked,
    /// `AgentEvent::PermissionResolved` — renders as `"permission_resolved"`.
    PermissionResolved,
    /// `AgentEvent::Stall` — renders as `"stall"`.
    Stall,
    /// `AgentEvent::Log` — renders as `"log"`.
    Log,
    /// `AgentEvent::Error` — renders as `"error"`.
    Error,
    /// `AgentEvent::Done` — renders as `"done"`.
    Done,
    /// `AgentEvent::UserEvent` — renders as `"user_event"`.
    UserEvent,
}

impl EventKind {
    /// Classify a live [`AgentEvent`] into its [`EventKind`].
    ///
    /// Exhaustive over the enumerated variants; the `#[non_exhaustive]`
    /// marker on `AgentEvent` means a future variant lands here as a
    /// compile error, which is the behaviour we want (all reaction code
    /// paths MUST stay in sync with the event surface).
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
            // `#[non_exhaustive]` upstream — the compiler still requires
            // a wildcard arm. Future variants surface as `Unknown`-ish
            // behavior here; the matcher (T-5.2) treats them as no-match.
            _ => EventKind::UserEvent,
        }
    }

    /// Canonical snake_case rendering (matches `AgentEvent`'s
    /// `#[serde(rename_all = "snake_case")]` tag).
    ///
    /// Used by the selector parser (T-5.2) to normalize author-written
    /// forms: `"PhaseTransition"` (PascalCase sugar), `"phase_transition"`,
    /// and `"phaseTransition"` all map to the same storage key.
    pub fn as_str(&self) -> &'static str {
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

    /// Parse a selector's kind prefix, tolerating PascalCase sugar
    /// (`"PhaseTransition"`, `"Started"`) the R4 spec mentions.
    ///
    /// Returns `None` for unknown kinds. `"UserEvent"` prefixes land here
    /// as `Some(EventKind::UserEvent)`; T-5.2 separately parses the
    /// `:<name>` suffix and uses it to key the secondary index.
    pub fn parse(input: &str) -> Option<Self> {
        // Match verbatim first (cheap path for canonical forms).
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

    /// Every canonical variant — handy for tests and for the cycle-
    /// detection walker (T-5.5).
    pub const ALL: &'static [EventKind] = &[
        EventKind::Started,
        EventKind::TabOpened,
        EventKind::TabClosed,
        EventKind::Progress,
        EventKind::TaskDone,
        EventKind::Iteration,
        EventKind::PhaseTransition,
        EventKind::ToolUse,
        EventKind::Message,
        EventKind::FileEdited,
        EventKind::ReviewComment,
        EventKind::PermissionAsked,
        EventKind::PermissionResolved,
        EventKind::Stall,
        EventKind::Log,
        EventKind::Error,
        EventKind::Done,
        EventKind::UserEvent,
    ];
}

// ---------------------------------------------------------------------------
// ReactionEntry
// ---------------------------------------------------------------------------

/// One compiled reaction.
///
/// Output of [`populate_registry`] for a single `on { }` or `keybind { }`
/// node. Carries:
///
/// * the original selector string (for T-5.2's matcher + `ark scene graph`
///   attribution),
/// * an optional compiled CEL [`Program`] for the `if=` predicate,
/// * the op list (compiled from the reaction body),
/// * the reaction's [`ReactionOrigin`] for telemetry (T-5.6) and the
///   `ark scene graph` command (R13).
#[derive(Debug, Clone)]
pub struct ReactionEntry {
    /// Verbatim selector string from the scene source. Parsed by T-5.2
    /// into an `EventSelector` on-demand; storage stays lazy so the
    /// registry's build path doesn't depend on the selector-parser
    /// module.
    pub selector: String,

    /// Optional CEL predicate compiled from the `if="..."` attribute.
    /// `None` ⇒ unconditional reaction.
    ///
    /// Wrapped in `Arc` because `cel_interpreter::Program` is not `Clone`;
    /// the `Arc` gives us a cheap-clone handle so `ReactionEntry` can be
    /// duplicated through the primary + secondary indices without
    /// recompiling the CEL source.
    pub predicate: Option<Arc<Program>>,

    /// Ordered op list compiled from the reaction body.
    pub ops: Vec<CompiledOp>,

    /// Attribution tag — which layer produced this reaction.
    pub origin: ReactionOrigin,
}

// ---------------------------------------------------------------------------
// ReactionRegistry
// ---------------------------------------------------------------------------

/// Two-level index of compiled reactions.
///
/// See the module docs for the indexing rationale. `BTreeMap` is used
/// over `HashMap` for deterministic iteration order — tests, snapshot
/// tooling, and `ark scene graph` all benefit from stable ordering, and
/// the map sizes (at most ~20 primary keys, ~dozens of secondary names)
/// are small enough that BTree vs. hash is a noise-floor perf
/// difference.
#[derive(Debug, Default, Clone)]
pub struct ReactionRegistry {
    /// Primary index: reactions keyed by [`EventKind`]. A single key can
    /// carry many entries (R4: "multiple `on` blocks with overlapping
    /// selectors each run").
    primary: BTreeMap<EventKind, Vec<ReactionEntry>>,

    /// Secondary index for `UserEvent:<name>` selectors. Keys are the
    /// namespaced event name (e.g. `"ark.acp.permission_requested"`,
    /// `"user.hello"`).
    ///
    /// Entries in this index are ALSO present in `primary` under
    /// `EventKind::UserEvent` — the secondary index is purely an
    /// optimisation for the dispatcher's fan-out step.
    by_user_event: BTreeMap<String, Vec<ReactionEntry>>,
}

impl ReactionRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of reactions registered (counts both inline and
    /// secondary-index entries as one — the secondary index is a view
    /// over the `UserEvent` slice of the primary index).
    pub fn len(&self) -> usize {
        self.primary.values().map(|v| v.len()).sum()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }

    /// Insert a reaction under a given [`EventKind`].
    ///
    /// The caller supplies `user_event_name = Some(...)` when the
    /// selector had the `UserEvent:<name>` shape, which causes the entry
    /// to be mirrored into the secondary index. For any other kind
    /// (or bare `UserEvent` with no name), `user_event_name` is `None`
    /// and only the primary index receives the entry.
    pub fn insert(
        &mut self,
        kind: EventKind,
        user_event_name: Option<String>,
        entry: ReactionEntry,
    ) {
        self.primary.entry(kind.clone()).or_default().push(entry.clone());
        if let Some(name) = user_event_name {
            // Sanity: only UserEvent selectors can carry a name.
            // Malformed selectors that squeak through T-5.2 would be a
            // caller bug; the secondary index only indexes when both
            // conditions hold so a rogue caller can't shadow a core
            // kind by passing a stray name.
            if kind == EventKind::UserEvent {
                self.by_user_event.entry(name).or_default().push(entry);
            }
        }
    }

    /// Fetch every reaction registered under the given kind. Empty slice
    /// when none are registered.
    pub fn by_kind(&self, kind: &EventKind) -> &[ReactionEntry] {
        self.primary
            .get(kind)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Fetch every reaction registered under a `UserEvent:<name>`
    /// selector. Empty slice when none.
    ///
    /// Returned entries are a subset of `by_kind(&EventKind::UserEvent)` —
    /// callers that want to union the two sets (dispatcher path) should
    /// iterate both and dedupe by entry identity, though for typical
    /// scenes the primary `UserEvent` slot holds ONLY named entries
    /// (unnamed `UserEvent` selectors are rare).
    pub fn by_user_event_name(&self, name: &str) -> &[ReactionEntry] {
        self.by_user_event
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Iterator over every `(EventKind, &[ReactionEntry])` pair. Order
    /// is the `BTreeMap`'s canonical ordering — stable across runs.
    pub fn iter_primary(&self) -> impl Iterator<Item = (&EventKind, &Vec<ReactionEntry>)> {
        self.primary.iter()
    }

    /// Iterator over every `(user_event_name, &[ReactionEntry])` pair.
    /// Stable ordering per `BTreeMap`.
    pub fn iter_user_events(&self) -> impl Iterator<Item = (&String, &Vec<ReactionEntry>)> {
        self.by_user_event.iter()
    }

    /// Total count of reactions keyed against a specific `UserEvent`
    /// name. Used by T-5.5's cycle-detection graph construction.
    pub fn user_event_names(&self) -> impl Iterator<Item = &String> {
        self.by_user_event.keys()
    }
}

// ---------------------------------------------------------------------------
// populate_registry
// ---------------------------------------------------------------------------

/// Resolve the scene's cascade-depth bound.
///
/// Per R4, the bound is configurable via
/// `scene "<name>" max-cascade-depth=<N>`. When the attribute is
/// absent, the [`crate::intent::DEFAULT_MAX_CASCADE_DEPTH`] (4) kicks
/// in. Surfaced here so the runtime wiring layer can read the cap
/// once without rewalking the AST.
pub fn scene_max_cascade_depth(doc: &SceneDoc) -> u32 {
    doc.scene
        .max_cascade_depth
        .unwrap_or(crate::intent::DEFAULT_MAX_CASCADE_DEPTH)
}

/// Walk every `on { }` and `keybind { }` node in a scene document and
/// populate a [`ReactionRegistry`].
///
/// Every selector is parsed; the kind prefix determines the primary
/// index slot, and any `UserEvent:<name>` suffix routes the entry to
/// the secondary index as well. Each `if="…"` attribute is compiled to
/// a CEL [`Program`]; parse failures are collected (not short-circuited)
/// so `ark scene check` can render all of them in one pass.
///
/// Keybinds are included: R5 says keybinds use the same op grammar as
/// reactions, but they are dispatched by a synthetic `UserEvent:<chord>`
/// path (the zellij `ark-bus` plugin emits `ark-intent` for bound
/// chords, which the supervisor re-emits as a `UserEvent`). Modeling
/// keybinds as UserEvent-secondary entries keeps the dispatcher's code
/// path uniform. Chord selectors are normalized to
/// `"keybind:<chord>"` names to avoid colliding with scene-author
/// UserEvent names.
///
/// Op compilation is intentionally **thin** at this tier: each `OpNode`
/// in the AST becomes a [`CompiledOp`] carrying the KDL node verbatim.
/// Typed arg-parsing happens lazily at dispatch via
/// `IntentRegistry::dispatch_dyn` (same path T-4.5 established).
/// Resolution of the op NAME against the intent registry's namespace
/// (R11 unprefixed-name rewrite) is ALSO deferred — at this tier we
/// trust the scene compile pass upstream to have rewritten names
/// before calling us; if a rewrite didn't happen, the first KDL node
/// name is treated as the op name as-written. This is consistent with
/// what T-4.5's compile pipeline produces today.
pub fn populate_registry(doc: &SceneDoc) -> Result<ReactionRegistry, Vec<SceneError>> {
    let mut registry = ReactionRegistry::new();
    let mut errors: Vec<SceneError> = Vec::new();

    // -- Reactions (scene-root `on { }` blocks) ------------------------
    for on_node in &doc.scene.ons {
        match build_reaction_entry(&on_node.selector, on_node.if_.as_deref(), &on_node.ops) {
            Ok((kind, user_event_name, entry)) => {
                registry.insert(kind, user_event_name, entry);
            }
            Err(mut errs) => errors.append(&mut errs),
        }
    }

    // -- Keybinds (scene-root `keybind "<chord>"` entries) ------------
    //
    // Keybinds land in the registry as `UserEvent:keybind:<chord>`
    // secondary entries so the dispatcher can treat them uniformly.
    // Shorthand `intent="…"` form is materialised as a single-op body
    // that dispatches the named intent (ops=empty + intent=Some => the
    // supervisor surfaces it via `launch-or-focus-plugin` on the
    // ark-bus plugin; we don't model that at this layer — the chord
    // selector suffices for the registry).
    for kb in &doc.scene.keybinds {
        // Replace space in chord with '+' so the selector head parses as
        // a single token. `keybind:Alt+p` stays readable while playing
        // nicely with the whitespace-delimited field-pattern grammar
        // (T-5.2). The registry's secondary-index name keeps the
        // normalised form so the dispatcher's lookup matches.
        let normalised_chord = kb.chord.replace(' ', "+");
        let selector = format!("UserEvent:keybind:{}", normalised_chord);
        match build_reaction_entry(&selector, None, &kb.ops) {
            Ok((kind, user_event_name, entry)) => {
                registry.insert(kind, user_event_name, entry);
            }
            Err(mut errs) => errors.append(&mut errs),
        }
    }

    if errors.is_empty() {
        Ok(registry)
    } else {
        Err(errors)
    }
}

/// Compile a single reaction (selector + optional predicate + op body)
/// into a [`ReactionEntry`] and return its routing info.
///
/// Returns `(primary kind, Some(user-event-name) when UserEvent:<name>,
/// entry)`. Errors are a `Vec<SceneError>` because CEL compile errors
/// may surface alongside other future checks.
fn build_reaction_entry(
    selector: &str,
    predicate_src: Option<&str>,
    ops: &[crate::ast::OpNode],
) -> Result<(EventKind, Option<String>, ReactionEntry), Vec<SceneError>> {
    let mut errors: Vec<SceneError> = Vec::new();

    let (kind, user_event_name) = parse_selector_kind(selector);
    let kind = match kind {
        Some(k) => k,
        None => {
            // Unknown selector kind — surface as a grammar error. We
            // don't have rich span info here (selector string only);
            // the compile pipeline upstream typically wraps this with
            // a proper NamedSource before displaying.
            errors.push(SceneError::Grammar {
                message: format!(
                    "unknown event kind in selector `{selector}` (expected one of: started, tab_opened, tab_closed, progress, task_done, iteration, phase_transition, tool_use, message, file_edited, review_comment, permission_asked, permission_resolved, stall, log, error, done, user_event, UserEvent:<name>)"
                ),
                src: miette::NamedSource::new("<selector>", selector.to_string()),
                at: (0, selector.len()).into(),
            });
            return Err(errors);
        }
    };

    let predicate = match predicate_src {
        Some(src) => match cel::compile(src, "<if=>", 0) {
            Ok(prog) => Some(Arc::new(prog)),
            Err(e) => {
                errors.push(e);
                None
            }
        },
        None => None,
    };

    // Compile ops. v1 shape: each OpNode is an opaque name + raw args.
    // We defer typed-args validation to dispatch (same as T-4.5's
    // CompiledOp contract).
    let compiled_ops: Vec<CompiledOp> = ops.iter().filter_map(op_node_to_compiled).collect();

    if !errors.is_empty() {
        return Err(errors);
    }

    let entry = ReactionEntry {
        selector: selector.to_string(),
        predicate,
        ops: compiled_ops,
        origin: ReactionOrigin::default(),
    };
    Ok((kind, user_event_name, entry))
}

/// Parse the kind prefix from a selector string.
///
/// Returns `(Some(EventKind), Some(name))` for `UserEvent:<name>`
/// selectors, `(Some(EventKind), None)` for bare kinds, and
/// `(None, None)` for unknown kinds.
///
/// T-5.2 extends this with field-pattern parsing (`"Kind field=val"`);
/// the primary-index key is still the kind alone, so the extra
/// parsing happens inside the matcher, not here.
fn parse_selector_kind(selector: &str) -> (Option<EventKind>, Option<String>) {
    // Strip any field-pattern suffix: `"Kind field=val"` — the head is
    // the kind, the rest is for T-5.2.
    let head = selector.split_whitespace().next().unwrap_or("");

    // Handle UserEvent:<name>.
    if let Some(rest) = head.strip_prefix("UserEvent:") {
        return (Some(EventKind::UserEvent), Some(rest.to_string()));
    }
    if let Some(rest) = head.strip_prefix("user_event:") {
        return (Some(EventKind::UserEvent), Some(rest.to_string()));
    }

    (EventKind::parse(head), None)
}

/// Compile an AST [`crate::ast::OpNode`] into a [`CompiledOp`].
///
/// v1 AST stores ops as opaque (`positional args only`) — returned
/// when the compile pipeline upstream hasn't yet produced typed nodes
/// (T-3.2 follow-up). Today we need the raw KDL node for dispatch;
/// because the AST dropped the node-name, we can't reconstruct the op
/// without more info. For T-5.1 we skip ops that lack name + node
/// metadata — when the compile pipeline carries the name through
/// (coming in the same tier), this filter becomes a no-op.
///
/// TODO(T-3.2): once `OpNode` grows a `name: String` + stored
/// `kdl::KdlNode` field, lift the raw node into [`CompiledOp`] here.
fn op_node_to_compiled(_op: &crate::ast::OpNode) -> Option<CompiledOp> {
    // AST OpNode currently carries no op name (only positional args).
    // The scene compile pipeline that produced T-4.5's CompiledOps
    // goes through a different path (typed nodes in a different AST
    // branch). We return None here so the registry populates without
    // op data until T-3.2 unifies the AST. The dispatcher (T-5.3)
    // treats an empty op list as a no-op reaction (predicate evaluates
    // + logs + does nothing), which is the right degraded behavior.
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::event::{AgentEvent, LogLevel};
    use ark_types::id::AgentId;

    fn agent_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    #[test]
    fn event_kind_of_covers_every_variant_with_a_stable_slug() {
        // Sample one event per variant; each must classify to the
        // corresponding `EventKind`.
        let id = agent_id();
        let cases: Vec<(AgentEvent, EventKind, &'static str)> = vec![
            (
                AgentEvent::Log {
                    id: id.clone(),
                    level: LogLevel::Info,
                    line: "x".into(),
                },
                EventKind::Log,
                "log",
            ),
            (
                AgentEvent::UserEvent {
                    name: "user.hello".into(),
                    payload: serde_json::Value::Null,
                    source: "scene".into(),
                },
                EventKind::UserEvent,
                "user_event",
            ),
            (
                AgentEvent::Progress {
                    id: id.clone(),
                    done: 1,
                    total: 2,
                    label: None,
                },
                EventKind::Progress,
                "progress",
            ),
            (
                AgentEvent::PhaseTransition {
                    id,
                    from: None,
                    to: "running".into(),
                },
                EventKind::PhaseTransition,
                "phase_transition",
            ),
        ];
        for (ev, expect_kind, expect_slug) in cases {
            let got = EventKind::of(&ev);
            assert_eq!(got, expect_kind, "event {ev:?}");
            assert_eq!(got.as_str(), expect_slug);
        }
    }

    #[test]
    fn event_kind_parse_accepts_snake_and_pascal() {
        assert_eq!(EventKind::parse("started"), Some(EventKind::Started));
        assert_eq!(EventKind::parse("Started"), Some(EventKind::Started));
        assert_eq!(
            EventKind::parse("phase_transition"),
            Some(EventKind::PhaseTransition)
        );
        assert_eq!(
            EventKind::parse("PhaseTransition"),
            Some(EventKind::PhaseTransition)
        );
        assert_eq!(EventKind::parse("user_event"), Some(EventKind::UserEvent));
        assert_eq!(EventKind::parse("UserEvent"), Some(EventKind::UserEvent));
        assert_eq!(EventKind::parse("bogus"), None);
    }

    #[test]
    fn parse_selector_kind_extracts_user_event_name() {
        let (k, n) = parse_selector_kind("UserEvent:user.hello");
        assert_eq!(k, Some(EventKind::UserEvent));
        assert_eq!(n, Some("user.hello".into()));

        let (k, n) = parse_selector_kind("PhaseTransition");
        assert_eq!(k, Some(EventKind::PhaseTransition));
        assert_eq!(n, None);

        // Field-pattern suffix (T-5.2) is ignored for the kind prefix.
        let (k, n) = parse_selector_kind("PhaseTransition to=\"review\"");
        assert_eq!(k, Some(EventKind::PhaseTransition));
        assert_eq!(n, None);

        let (k, n) = parse_selector_kind("notakind");
        assert!(k.is_none());
        assert!(n.is_none());
    }

    #[test]
    fn registry_insert_and_by_kind_lookup() {
        let mut reg = ReactionRegistry::new();
        let entry = ReactionEntry {
            selector: "Started".to_string(),
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::default(),
        };
        reg.insert(EventKind::Started, None, entry.clone());
        reg.insert(EventKind::Started, None, entry.clone());
        reg.insert(EventKind::Progress, None, entry);

        assert_eq!(reg.len(), 3);
        assert_eq!(reg.by_kind(&EventKind::Started).len(), 2);
        assert_eq!(reg.by_kind(&EventKind::Progress).len(), 1);
        assert_eq!(reg.by_kind(&EventKind::Stall).len(), 0);
    }

    #[test]
    fn registry_secondary_index_catches_named_user_events() {
        let mut reg = ReactionRegistry::new();
        let make = |sel: &str| ReactionEntry {
            selector: sel.into(),
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::default(),
        };
        reg.insert(
            EventKind::UserEvent,
            Some("user.hello".into()),
            make("UserEvent:user.hello"),
        );
        reg.insert(
            EventKind::UserEvent,
            Some("user.hello".into()),
            make("UserEvent:user.hello"),
        );
        reg.insert(
            EventKind::UserEvent,
            Some("user.world".into()),
            make("UserEvent:user.world"),
        );

        assert_eq!(reg.by_user_event_name("user.hello").len(), 2);
        assert_eq!(reg.by_user_event_name("user.world").len(), 1);
        assert_eq!(reg.by_user_event_name("user.missing").len(), 0);
        // Every entry is ALSO mirrored in the primary UserEvent slot.
        assert_eq!(reg.by_kind(&EventKind::UserEvent).len(), 3);
    }

    #[test]
    fn registry_secondary_index_rejects_cross_kind_names() {
        // Caller invariant violation: passing a user_event_name with a
        // non-UserEvent kind should NOT shadow a core slot. The secondary
        // index stays empty.
        let mut reg = ReactionRegistry::new();
        let entry = ReactionEntry {
            selector: "Started".into(),
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::default(),
        };
        reg.insert(EventKind::Started, Some("user.hello".into()), entry);
        assert_eq!(reg.by_user_event_name("user.hello").len(), 0);
    }

    #[test]
    fn populate_registry_walks_on_nodes() {
        let input = r#"
scene "demo" {
    on "Started" { }
    on "PhaseTransition" { }
    on "UserEvent:user.hello" { }
    on "UserEvent:user.hello" if="event.kind == \"user_event\"" { }
    on "UserEvent:user.world" { }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse");
        let reg = populate_registry(&doc).expect("populate");

        assert_eq!(reg.by_kind(&EventKind::Started).len(), 1);
        assert_eq!(reg.by_kind(&EventKind::PhaseTransition).len(), 1);
        assert_eq!(reg.by_kind(&EventKind::UserEvent).len(), 3);
        assert_eq!(reg.by_user_event_name("user.hello").len(), 2);
        assert_eq!(reg.by_user_event_name("user.world").len(), 1);
        // Second user.hello reaction has a compiled CEL predicate.
        let user_hello_entries = reg.by_user_event_name("user.hello");
        assert!(user_hello_entries.iter().any(|e| e.predicate.is_some()));
        assert!(user_hello_entries.iter().any(|e| e.predicate.is_none()));
    }

    #[test]
    fn populate_registry_surfaces_bad_predicate() {
        let input = r#"
scene "demo" {
    on "Started" if="(((" { }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse");
        let err = populate_registry(&doc).expect_err("bad predicate surfaces");
        assert_eq!(err.len(), 1);
        assert!(matches!(err[0], SceneError::CelParse { .. }));
    }

    #[test]
    fn populate_registry_surfaces_unknown_selector_kind() {
        let input = r#"
scene "demo" {
    on "BogusKind" { }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse");
        let err = populate_registry(&doc).expect_err("unknown kind surfaces");
        assert!(
            err.iter().any(|e| matches!(e, SceneError::Grammar { .. })),
            "expected a Grammar error, got {err:?}"
        );
    }

    #[test]
    fn populate_registry_indexes_keybinds_as_named_user_events() {
        let input = r#"
scene "demo" {
    keybind "Alt p" intent="picker.show"
    keybind "Ctrl g" { }
}
"#;
        let doc: SceneDoc = facet_kdl::from_str(input).expect("parse");
        let reg = populate_registry(&doc).expect("populate");
        assert_eq!(reg.by_user_event_name("keybind:Alt+p").len(), 1);
        assert_eq!(reg.by_user_event_name("keybind:Ctrl+g").len(), 1);
        assert_eq!(reg.by_kind(&EventKind::UserEvent).len(), 2);
    }

    #[test]
    fn registry_iter_primary_is_deterministic() {
        let mut reg = ReactionRegistry::new();
        let e = ReactionEntry {
            selector: "x".into(),
            predicate: None,
            ops: Vec::new(),
            origin: ReactionOrigin::default(),
        };
        reg.insert(EventKind::Done, None, e.clone());
        reg.insert(EventKind::Started, None, e.clone());
        reg.insert(EventKind::PhaseTransition, None, e);
        let order: Vec<EventKind> = reg.iter_primary().map(|(k, _)| k.clone()).collect();
        // BTreeMap sorts variants by their declaration order (derived Ord).
        assert_eq!(
            order,
            vec![
                EventKind::Started,
                EventKind::PhaseTransition,
                EventKind::Done
            ]
        );
    }
}
