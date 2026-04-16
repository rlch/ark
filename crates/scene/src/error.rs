//! Scene compile / runtime diagnostics (T-006).
//!
//! Every user-visible scene error lands in [`SceneError`]. Each variant
//! derives `miette::Diagnostic` with a stable code drawn from the
//! `scene/*`, `ext/*`, `ext-proto/*`, `op/*`, `acp/*` namespaces that R12
//! of `cavekit-scene.md` enumerates. All variants carry `severity(Error)`
//! (the default); there are no soft diagnostics in v3.
//!
//! Variants that have a natural source site carry `NamedSource<String>`
//! + `SourceSpan` field(s) so miette renders a file/line/column caret.
//! Runtime-origin variants (extension crashes, protocol errors, ACP
//! no-agent warnings) omit the span fields because no scene KDL byte
//! span maps to them.
//!
//! The snapshot-rendering tests for each code live under T-010; this
//! module's own tests assert only that `Diagnostic::code()` round-trips
//! to the exact string listed in R12.

// The combined `#[derive(thiserror::Error, miette::Diagnostic)]` pair
// expands into field-destructure patterns that rustc's
// `unused_assignments` lint misdiagnoses as "value never read" — every
// field here is consumed by the derived `source_code() / labels() /
// help()` impls. Mirror the silencing pattern used in the v2 archive
// so the crate builds warning-clean.
#![allow(unused_assignments)]

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

/// Crate-wide `Result` alias so call sites can write
/// `fn parse_scene(..) -> Result<SceneIR>`.
pub type Result<T> = std::result::Result<T, SceneError>;

/// Top-level scene error enum. One variant per R12 diagnostic code.
#[derive(Debug, Error, Diagnostic)]
pub enum SceneError {
    /// KDL 2.0 tokenizer / parser failure — the input isn't valid KDL.
    #[error("scene file is not valid KDL: {message}")]
    #[diagnostic(
        code = "scene/parse",
        severity(Error),
        help("KDL 2.0 syntax error — see the caret below. Run `ark scene fmt` on a known-good reference to compare shapes.")
    )]
    Parse {
        /// Parser's raw message (KDL-level detail).
        message: String,
        /// Full source text of the offending file, keyed by path.
        #[source_code]
        src: NamedSource<String>,
        /// Byte offset + length of the token that tripped the parser.
        #[label("here")]
        span: SourceSpan,
    },

    /// A node appeared in a position the scope table (R2) disallows —
    /// e.g. `on` inside `layout`, or bare `pane` at layout root.
    #[error("`{node}` not allowed inside `{parent}`")]
    #[diagnostic(
        code = "scene/misplaced-node",
        severity(Error),
        help("R2 scope table: `use`, `include`, `on`, `bind`, `mode`, `clear-*`, `disable-extension` live only at scene root; `tab` only inside `layout`; `row`/`col`/`pane` only inside `tab` (or nested).")
    )]
    MisplacedNode {
        /// Name of the offending node (e.g. `"on"`).
        node: String,
        /// Name of the enclosing parent (e.g. `"layout"`).
        parent: String,
        /// File containing the mistake.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the misplaced node's name.
        #[label("not allowed here")]
        span: SourceSpan,
    },

    /// Unknown top-level or child node that the scene grammar does not
    /// recognise. Caller should pre-render a "did you mean …?" hint
    /// into `help` via T-015's Jaro-Winkler suggester.
    #[error("unknown node `{node}`")]
    #[diagnostic(code = "scene/unknown-node", severity(Error))]
    UnknownNode {
        /// Name of the offending node.
        node: String,
        /// Pre-rendered help text (typically "did you mean `X`? Scene-root admits: …").
        #[help]
        help: String,
        /// File containing the unknown node.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the unknown node name.
        #[label("not a recognised node")]
        span: SourceSpan,
    },

    /// Pane's view child is a name that the view registry cannot resolve
    /// in any of its tiers (primitive → compiled-in → user → project).
    #[error("unknown view `{view}`")]
    #[diagnostic(code = "scene/unknown-view", severity(Error))]
    UnknownView {
        /// Name of the unknown view (e.g. `"mystery"`).
        view: String,
        /// Pre-rendered "did you mean `X`? Available views: …" text.
        #[help]
        help: String,
        /// File containing the unknown view reference.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the view name inside the pane body.
        #[label("no view registered under this name")]
        span: SourceSpan,
    },

    /// Two tabs / panes share the same `@handle` within the flat
    /// scene-scoped handle namespace (R2.6).
    #[error("handle `{handle}` declared more than once")]
    #[diagnostic(
        code = "scene/handle-clash",
        severity(Error),
        help("Handles are flat-namespaced across tabs and panes. Rename one of the occurrences so the reconciler can tell them apart.")
    )]
    HandleClash {
        /// The duplicated handle (including the leading `@`).
        handle: String,
        /// File carrying both occurrences.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the FIRST declaration.
        #[label("first declared here")]
        first: SourceSpan,
        /// Span of the DUPLICATE (later) declaration.
        #[label("duplicate here")]
        second: SourceSpan,
    },

    /// An op expected one handle type but received another — e.g.
    /// `rename @pane to="x"` when `@pane` is a pane (rename is tab-only).
    #[error("handle `{handle}` is a {actual} but op `{op}` expects a {expected}")]
    #[diagnostic(
        code = "scene/handle-type-mismatch",
        severity(Error),
        help("Op vocabulary R7: some ops are tab-only (`rename`, `new_tab`), others pane-only (`resize`, `move`, `pin`, `unpin`). The compiler infers handle type from the declaration site.")
    )]
    HandleTypeMismatch {
        /// Op name (e.g. `"rename"`).
        op: String,
        /// Handle string (including `@`).
        handle: String,
        /// Expected handle type (`"tab"` or `"pane"`).
        expected: &'static str,
        /// Actual handle type as declared.
        actual: &'static str,
        /// File containing the offending op site.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the handle site where the op used it.
        #[label("used here")]
        use_span: SourceSpan,
        /// Span of the original handle declaration.
        #[label("declared here")]
        decl_span: SourceSpan,
    },

    /// A `tab` or `pane` node is missing its required `@handle`
    /// attribute (R2.5).
    #[error("`{node}` node is missing its required `@handle`")]
    #[diagnostic(
        code = "scene/handle-missing",
        severity(Error),
        help("Every `tab` and `pane` needs an `@handle`. Handles are the reconciler's identity keys — without them ark cannot map desired-state panes back to running zellij panes.")
    )]
    HandleMissing {
        /// Node kind that lacks a handle (`"tab"` or `"pane"`).
        node: &'static str,
        /// File carrying the handle-less node.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the node declaration.
        #[label("needs an `@handle` here")]
        span: SourceSpan,
    },

    /// An event-selector field pattern references a field that the
    /// target `AgentEvent` variant does not carry (R4.2).
    #[error("event `{event_kind}` has no field `{field}`")]
    #[diagnostic(code = "scene/unknown-event-field", severity(Error))]
    UnknownEventField {
        /// Event kind the selector targets.
        event_kind: String,
        /// Field name the selector tried to match.
        field: String,
        /// Pre-rendered "did you mean `X`? Available fields: …" text.
        #[help]
        help: String,
        /// File containing the selector.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the offending field name.
        #[label("not a field of this event")]
        span: SourceSpan,
    },

    /// An op dispatched from a reaction returned a runtime error. Logged
    /// at `warn` by the reactions dispatcher — remaining ops in the
    /// reaction are skipped, but the event loop keeps running (R4.9).
    #[error("op `{op}` failed: {message}")]
    #[diagnostic(
        code = "scene/op-failed",
        severity(Error),
        help("The op ran but returned an error. Check the op's own logs for the underlying cause; subsequent ops in this reaction are skipped.")
    )]
    OpFailed {
        /// Op name that failed (e.g. `"spawn"`).
        op: String,
        /// Human-readable failure summary from the op handler.
        message: String,
    },

    /// An op verb appeared in source that the op registry does not
    /// recognise. Caller pre-renders a "did you mean …?" hint.
    #[error("unknown op `{op}`")]
    #[diagnostic(code = "scene/unknown-op", severity(Error))]
    UnknownOp {
        /// Offending op name.
        op: String,
        /// Pre-rendered "did you mean `X`? Available ops: …" text.
        #[help]
        help: String,
        /// File containing the offending op.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the unknown op verb.
        #[label("no op registered under this name")]
        span: SourceSpan,
    },

    /// File contains both a `scene { }` wrapper AND a top-level bare
    /// `layout { }` node — the file-shape probe (R15) cannot decide
    /// which to use.
    #[error("scene file has both a `scene {{ }}` wrapper and a top-level `layout {{ }}`")]
    #[diagnostic(
        code = "scene/ambiguous-file-shape",
        severity(Error),
        help("Pick one. Either move the top-level `layout` into the `scene {{ }}` body, or delete the `scene` wrapper so the file parses in legacy layout-only mode. Run `ark scene fmt` to auto-convert.")
    )]
    AmbiguousFileShape {
        /// File containing both shapes.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the `scene { }` wrapper.
        #[label("scene wrapper here")]
        scene_span: SourceSpan,
        /// Span of the stray top-level `layout { }`.
        #[label("and top-level layout here")]
        layout_span: SourceSpan,
    },

    /// File has neither a `scene { }` wrapper nor a top-level
    /// `layout { }` (R15).
    #[error("scene file has neither a `scene {{ }}` wrapper nor a top-level `layout {{ }}`")]
    #[diagnostic(
        code = "scene/empty-or-unknown",
        severity(Error),
        help("Wrap the contents in `scene \"<name>\" {{ … }}`; see `cavekit-scene.md` R1 for the full grammar.")
    )]
    EmptyOrUnknown {
        /// File that failed the shape probe.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the first offending node, or `(0, 0)` when empty.
        #[label("no recognisable scene or layout root")]
        span: SourceSpan,
    },

    /// Scene file carries both an inline engine-like block and a `use`
    /// that also provides an engine / ACP agent capability (R17
    /// intra-scene mutual exclusion, preserved for compat with v2
    /// scene files during the migration window).
    #[error("scene declares two conflicting engine sources: inline and `use \"{use_name}\"`")]
    #[diagnostic(
        code = "scene/engine-conflict",
        severity(Error),
        help("Pick one. Delete the inline engine declaration or remove the `use \"{use_name}\"` entry.")
    )]
    EngineConflict {
        /// Name of the clashing `use` target.
        use_name: String,
        /// File containing both declarations.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the inline engine block.
        #[label("inline engine declared here")]
        inline_span: SourceSpan,
        /// Span of the `use` declaration.
        #[label("extension engine declared here")]
        use_span: SourceSpan,
    },

    /// A Rhai predicate or `{expr}` interpolation hole failed to parse
    /// (R8). The embedded `Position` is mapped back onto the containing
    /// KDL attribute span for the caret.
    #[error("Rhai expression failed to parse: {message}")]
    #[diagnostic(
        code = "scene/rhai-parse",
        severity(Error),
        help("Rhai runs in expression-only mode: no `fn`, no loops, no assignment. Predicates with string literals require KDL raw strings (`when=#\"agent.phase == \"review\"\"#`) because Rhai also uses double quotes. `ark scene fmt` auto-promotes plain → raw when a predicate body contains `\"`.")
    )]
    RhaiParse {
        /// Rhai parser output (includes line/col within the expression).
        message: String,
        /// File containing the expression.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the expression within the scene file.
        #[label("while compiling this Rhai expression")]
        span: SourceSpan,
    },

    /// A Rhai expression references bindings that do not exist in the
    /// scope it was attached to — e.g. a `when=` in a layout referencing
    /// `event.*`, or an event-context predicate referencing `cwd` (R8).
    #[error("Rhai expression scope mismatch: {message}")]
    #[diagnostic(
        code = "scene/rhai-scope-mismatch",
        severity(Error),
        help("Layout predicates see only the spawn scope (`cwd`, `id`, `name`, `env`). Reaction / bind predicates see only the event scope (`event`, `payload`, `agent`, `session`, plus selector-captured locals). Move the expression, or use a different binding.")
    )]
    RhaiScopeMismatch {
        /// Human-readable mismatch (e.g. "`event` unavailable in spawn scope").
        message: String,
        /// File containing the expression.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the offending expression.
        #[label("scope not available here")]
        span: SourceSpan,
    },

    /// A Rhai expression parsed successfully but failed at runtime
    /// (nil access, type mismatch). Logged at `warn`; the reaction / op
    /// is skipped and the session continues (R8, R12.5).
    #[error("Rhai evaluation failed: {message}")]
    #[diagnostic(
        code = "scene/rhai-eval",
        severity(Error),
        help("Check that every identifier used in the expression is bound by the surrounding context. Reactions see `event.*`, `payload.*`, `agent.*`, `session.*`, plus selector-captured locals.")
    )]
    RhaiEval {
        /// Rhai runtime error string.
        message: String,
    },

    /// A Rhai expression exceeded `set_max_operations` (10_000 default).
    /// Treated as programmer error — the engine's guard is a
    /// defence-in-depth against runaway evaluation (R8).
    #[error("Rhai expression exceeded the operation limit ({limit})")]
    #[diagnostic(
        code = "scene/rhai-oom",
        severity(Error),
        help("Scene expressions are capped at 10,000 operations. Flatten nested conditionals or factor the predicate into multiple reactions.")
    )]
    RhaiOom {
        /// The operation cap that was hit.
        limit: usize,
    },

    /// A `use "<name>"` could not be resolved through the extension
    /// search path (compiled-in → user-installed → project-local).
    #[error("extension `{name}` not found")]
    #[diagnostic(code = "ext/missing", severity(Error))]
    ExtMissing {
        /// Extension name that failed to resolve.
        name: String,
        /// Pre-rendered "did you mean `X`? Searched: …" text (Levenshtein
        /// suggestions via T-015).
        #[help]
        help: String,
        /// File containing the offending `use`.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the `use` argument.
        #[label("no extension found with this name")]
        span: SourceSpan,
    },

    /// Transitive `use` graph revisits an extension already on the
    /// resolution stack (R10.5 / R11).
    #[error("extension dependency cycle: {}", trail.join(" -> "))]
    #[diagnostic(
        code = "ext/cycle",
        severity(Error),
        help("Break the cycle by removing one of the `use` declarations along the chain. Transitive `use` is one-way — extensions contribute intents/events once; there's no need to re-use an ancestor.")
    )]
    ExtCycle {
        /// Ordered name trail — first element = cycle start, last = the
        /// hop that closed the loop (equal to the first).
        trail: Vec<String>,
    },

    /// An extension subprocess exited unexpectedly (R16.5). Runtime-only
    /// — no scene span.
    #[error("extension `{name}` crashed: {reason}")]
    #[diagnostic(
        code = "ext/crashed",
        severity(Error),
        help("The extension subprocess exited unexpectedly. Check the extension's logs for the underlying cause; subsequent intents dispatched to this extension will fail until it is restarted.")
    )]
    ExtCrashed {
        /// Extension name that crashed.
        name: String,
        /// Human-readable reason (signal, non-zero exit code, etc.).
        reason: String,
    },

    /// An extension tried to register a namespace that collides with
    /// ark's reserved `ark.core.*` prefix (R11.4).
    #[error("extension `{ext}` namespace `{attempted}` collides with reserved `ark.core.*`")]
    #[diagnostic(
        code = "ext/reserved-namespace",
        severity(Error),
        help("`ark.core.*` is reserved for host-owned ops. Rename the extension (or the offending declaration) so the resulting namespace is anything other than `ark.core`.")
    )]
    ExtReservedNamespace {
        /// Extension name whose declarations were walked.
        ext: String,
        /// The fully-qualified name that would have been written.
        attempted: String,
    },

    /// Extension's `config { }` block failed schema validation against
    /// the extension's declared config schema (R17.11).
    #[error("extension `{ext}` config is invalid: {message}")]
    #[diagnostic(
        code = "ext/bad-config",
        severity(Error),
        help("The extension's `ConfigSchema` declared a specific shape for this field. Run `ark ext info <ext>` to inspect the schema.")
    )]
    ExtBadConfig {
        /// Extension name whose config failed validation.
        ext: String,
        /// Human-readable validation failure.
        message: String,
        /// File containing the config block.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the offending config key / value.
        #[label("invalid here")]
        span: SourceSpan,
    },

    /// Extension advertised a protocol version the host cannot speak
    /// (R16.2). Runtime-only — surfaces during `initialize` handshake.
    #[error("extension `{ext}` requires protocol {required} but host speaks {actual}")]
    #[diagnostic(
        code = "ext-proto/unsupported-version",
        severity(Error),
        help("Either install a build of the extension that targets the host protocol version, or upgrade/downgrade ark to a version that speaks the extension's required protocol.")
    )]
    ExtProtoUnsupportedVersion {
        /// Extension name.
        ext: String,
        /// Protocol semver range the extension requires.
        required: String,
        /// Host's own protocol version.
        actual: String,
    },

    /// Extension attempted to invoke a capability the host refused to
    /// grant. Runtime-only (R16).
    #[error("extension `{ext}` denied capability `{capability}`")]
    #[diagnostic(
        code = "ext-proto/capability-denied",
        severity(Error),
        help("The host refused this capability for the extension. Check the extension's manifest against the host's capability policy, or grant the capability at install time.")
    )]
    ExtProtoCapabilityDenied {
        /// Extension name that tried to use the capability.
        ext: String,
        /// Capability name that was denied.
        capability: String,
    },

    /// An op argument references a handle / extension / view that the
    /// scene did not declare (T-4.3 cross-reference pass).
    #[error("op `{op}` references unknown {kind} `{name}`")]
    #[diagnostic(code = "op/unresolved-ref", severity(Error))]
    OpUnresolvedRef {
        /// Op carrying the unresolved reference.
        op: String,
        /// Category of the missing reference (`"handle"`, `"extension"`, …).
        kind: String,
        /// The unknown name as it appeared in source.
        name: String,
        /// Pre-rendered "did you mean `X`? Available <kind>s: …" text.
        #[help]
        help: String,
        /// File carrying the offending op node.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the offending attribute value.
        #[label("no such {kind} declared in this scene")]
        span: SourceSpan,
    },

    /// Op-level handle-type mismatch (e.g. the op takes a pane handle
    /// but the scene passed a tab handle). Distinct from
    /// [`SceneError::HandleTypeMismatch`] which fires during the
    /// scene-level type inference pass; this one fires during op
    /// argument binding.
    #[error("op `{op}` argument `{arg}`: handle `{handle}` is a {actual}, expected a {expected}")]
    #[diagnostic(
        code = "op/handle-type-mismatch",
        severity(Error),
        help("Check R7's op vocabulary for the required handle type on this argument. Pane-only ops (`resize`, `pin`, `unpin`, `move`) reject tab handles; tab-only ops (`rename`, `new_tab`) reject pane handles.")
    )]
    OpHandleTypeMismatch {
        /// Op name.
        op: String,
        /// Op argument name (e.g. `"target"`).
        arg: String,
        /// Handle as written.
        handle: String,
        /// Handle type required by the op argument.
        expected: &'static str,
        /// Actual handle type.
        actual: &'static str,
        /// File containing the op.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the handle argument.
        #[label("wrong handle type here")]
        span: SourceSpan,
    },

    /// Scene dispatched an `acp.*` op but no ACP-capable extension is
    /// active (R7 ACP ops). No-ops with this warning surfaced via the
    /// status bar.
    #[error("`{op}` requires an ACP-capable extension but none is active")]
    #[diagnostic(
        code = "acp/no-agent",
        severity(Error),
        help("Add a `use \"<ext>\"` for an extension that declares `capabilities {{ agent {{ speaks \"acp\" }} }}`, or remove the `acp.*` op from the scene.")
    )]
    AcpNoAgent {
        /// The ACP op that was dispatched.
        op: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic;

    fn src(name: &str, text: &str) -> NamedSource<String> {
        NamedSource::new(name, text.to_string())
    }

    fn assert_code(err: &dyn Diagnostic, expected: &str) {
        let code = err
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "<no code>".to_string());
        assert_eq!(
            code, expected,
            "diagnostic code mismatch: got {code:?}, expected {expected:?}"
        );
    }

    #[test]
    fn parse_code() {
        let err = SceneError::Parse {
            message: "unexpected `!`".into(),
            src: src("scene.kdl", "scene \"x\" { !!! }"),
            span: (12, 3).into(),
        };
        assert_code(&err, "scene/parse");
    }

    #[test]
    fn misplaced_node_code() {
        let err = SceneError::MisplacedNode {
            node: "on".into(),
            parent: "layout".into(),
            src: src("scene.kdl", "layout { on AgentReady { } }"),
            span: (9, 2).into(),
        };
        assert_code(&err, "scene/misplaced-node");
    }

    #[test]
    fn unknown_node_code() {
        let err = SceneError::UnknownNode {
            node: "reaction".into(),
            help: "did you mean `on`?".into(),
            src: src("scene.kdl", "reaction { }"),
            span: (0, 8).into(),
        };
        assert_code(&err, "scene/unknown-node");
    }

    #[test]
    fn unknown_view_code() {
        let err = SceneError::UnknownView {
            view: "mystery".into(),
            help: "did you mean `shell`?".into(),
            src: src("scene.kdl", "pane @a { mystery }"),
            span: (10, 7).into(),
        };
        assert_code(&err, "scene/unknown-view");
    }

    #[test]
    fn handle_clash_code() {
        let err = SceneError::HandleClash {
            handle: "@work".into(),
            src: src("scene.kdl", "tab @work { } tab @work { }"),
            first: (4, 5).into(),
            second: (18, 5).into(),
        };
        assert_code(&err, "scene/handle-clash");
    }

    #[test]
    fn handle_type_mismatch_code() {
        let err = SceneError::HandleTypeMismatch {
            op: "rename".into(),
            handle: "@p".into(),
            expected: "tab",
            actual: "pane",
            src: src("scene.kdl", "rename @p to=\"x\""),
            use_span: (7, 2).into(),
            decl_span: (0, 2).into(),
        };
        assert_code(&err, "scene/handle-type-mismatch");
    }

    #[test]
    fn handle_missing_code() {
        let err = SceneError::HandleMissing {
            node: "pane",
            src: src("scene.kdl", "pane { shell }"),
            span: (0, 4).into(),
        };
        assert_code(&err, "scene/handle-missing");
    }

    #[test]
    fn unknown_event_field_code() {
        let err = SceneError::UnknownEventField {
            event_kind: "AgentTurn".into(),
            field: "typo".into(),
            help: "did you mean `phase`?".into(),
            src: src("scene.kdl", "on AgentTurn typo=\"x\" { }"),
            span: (13, 4).into(),
        };
        assert_code(&err, "scene/unknown-event-field");
    }

    #[test]
    fn op_failed_code() {
        let err = SceneError::OpFailed {
            op: "spawn".into(),
            message: "no such handle".into(),
        };
        assert_code(&err, "scene/op-failed");
    }

    #[test]
    fn unknown_op_code() {
        let err = SceneError::UnknownOp {
            op: "frobnicate".into(),
            help: "did you mean `focus`?".into(),
            src: src("scene.kdl", "frobnicate @x"),
            span: (0, 10).into(),
        };
        assert_code(&err, "scene/unknown-op");
    }

    #[test]
    fn ambiguous_file_shape_code() {
        let err = SceneError::AmbiguousFileShape {
            src: src("scene.kdl", "scene \"x\" { }\nlayout { }"),
            scene_span: (0, 5).into(),
            layout_span: (14, 6).into(),
        };
        assert_code(&err, "scene/ambiguous-file-shape");
    }

    #[test]
    fn empty_or_unknown_code() {
        let err = SceneError::EmptyOrUnknown {
            src: src("scene.kdl", ""),
            span: (0, 0).into(),
        };
        assert_code(&err, "scene/empty-or-unknown");
    }

    #[test]
    fn engine_conflict_code() {
        let err = SceneError::EngineConflict {
            use_name: "engine-claude".into(),
            src: src("scene.kdl", "use \"engine-claude\"\nengine { }"),
            inline_span: (20, 6).into(),
            use_span: (0, 3).into(),
        };
        assert_code(&err, "scene/engine-conflict");
    }

    #[test]
    fn rhai_parse_code() {
        let err = SceneError::RhaiParse {
            message: "unexpected token".into(),
            src: src("scene.kdl", "when=\"1 +\""),
            span: (5, 5).into(),
        };
        assert_code(&err, "scene/rhai-parse");
    }

    #[test]
    fn rhai_scope_mismatch_code() {
        let err = SceneError::RhaiScopeMismatch {
            message: "`event` unavailable in spawn scope".into(),
            src: src("scene.kdl", "when=\"event.kind == \\\"AgentReady\\\"\""),
            span: (6, 10).into(),
        };
        assert_code(&err, "scene/rhai-scope-mismatch");
    }

    #[test]
    fn rhai_eval_code() {
        let err = SceneError::RhaiEval {
            message: "nil access".into(),
        };
        assert_code(&err, "scene/rhai-eval");
    }

    #[test]
    fn rhai_oom_code() {
        let err = SceneError::RhaiOom { limit: 10_000 };
        assert_code(&err, "scene/rhai-oom");
    }

    #[test]
    fn ext_missing_code() {
        let err = SceneError::ExtMissing {
            name: "missing".into(),
            help: "did you mean `status`?".into(),
            src: src("scene.kdl", "use \"missing\""),
            span: (5, 7).into(),
        };
        assert_code(&err, "ext/missing");
    }

    #[test]
    fn ext_cycle_code() {
        let err = SceneError::ExtCycle {
            trail: vec!["a".into(), "b".into(), "a".into()],
        };
        assert_code(&err, "ext/cycle");
    }

    #[test]
    fn ext_crashed_code() {
        let err = SceneError::ExtCrashed {
            name: "status".into(),
            reason: "exit code 1".into(),
        };
        assert_code(&err, "ext/crashed");
    }

    #[test]
    fn ext_reserved_namespace_code() {
        let err = SceneError::ExtReservedNamespace {
            ext: "core".into(),
            attempted: "ark.core.foo".into(),
        };
        assert_code(&err, "ext/reserved-namespace");
    }

    #[test]
    fn ext_bad_config_code() {
        let err = SceneError::ExtBadConfig {
            ext: "status".into(),
            message: "expected int, got string".into(),
            src: src("scene.kdl", "config { port \"nope\" }"),
            span: (14, 6).into(),
        };
        assert_code(&err, "ext/bad-config");
    }

    #[test]
    fn ext_proto_unsupported_version_code() {
        let err = SceneError::ExtProtoUnsupportedVersion {
            ext: "status".into(),
            required: ">= 2.0".into(),
            actual: "1.0".into(),
        };
        assert_code(&err, "ext-proto/unsupported-version");
    }

    #[test]
    fn ext_proto_capability_denied_code() {
        let err = SceneError::ExtProtoCapabilityDenied {
            ext: "status".into(),
            capability: "fs.write".into(),
        };
        assert_code(&err, "ext-proto/capability-denied");
    }

    #[test]
    fn op_unresolved_ref_code() {
        let err = SceneError::OpUnresolvedRef {
            op: "focus".into(),
            kind: "handle".into(),
            name: "@missing".into(),
            help: "Available handles: @main".into(),
            src: src("scene.kdl", "focus @missing"),
            span: (6, 8).into(),
        };
        assert_code(&err, "op/unresolved-ref");
    }

    #[test]
    fn op_handle_type_mismatch_code() {
        let err = SceneError::OpHandleTypeMismatch {
            op: "resize".into(),
            arg: "target".into(),
            handle: "@t".into(),
            expected: "pane",
            actual: "tab",
            src: src("scene.kdl", "resize @t direction=up"),
            span: (7, 2).into(),
        };
        assert_code(&err, "op/handle-type-mismatch");
    }

    #[test]
    fn acp_no_agent_code() {
        let err = SceneError::AcpNoAgent {
            op: "acp.prompt".into(),
        };
        assert_code(&err, "acp/no-agent");
    }
}
