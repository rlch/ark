//! Scene compile/parse diagnostics.
//!
//! Every user-visible scene compile error lands in `SceneError`. Each
//! variant carries its source span(s) and implements
//! `miette::Diagnostic` with a stable `scene/*` code per
//! `cavekit-scene.md` R12. The codes enumerated here cover every
//! acceptance-criterion listed in R12's `scene/*` family:
//!
//! ```text
//! scene/parse
//! scene/grammar
//! scene/misplaced-node
//! scene/unknown-node
//! scene/duplicate-node
//! scene/plugin-ambiguous-lifecycle
//! scene/ext-not-used
//! scene/ambiguous-file-shape
//! scene/empty-or-unknown
//! scene/include-cycle
//! scene/engine-conflict
//! ```
//!
//! Cross-file errors (include-cycle, engine-conflict, etc.) attach the
//! additional files as `#[related]` siblings, each with its own
//! `NamedSource`. This avoids a separate aggregator type — miette
//! renders all related diagnostics in the same report.
//!
//! Error-code strings are exposed as `ErrorCode::as_str()` so call sites
//! (and tests) can reference them symbolically. The code enum also acts
//! as the canonical list of namespace entries for the wider `ark` error
//! taxonomy (R12 enumerates `ext/*`, `ext-proto/*`, `op/*`, `plugin/*`,
//! `cel/*`, `acp/*` families separately — those land in their owning
//! crates, not here).

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

/// Canonical enumeration of `scene/*` error codes (R12).
///
/// Used by tests and tooling that need to match on specific failure
/// modes without string-comparing the rendered miette code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// KDL 2.0 tokenizer / parser failure — the input isn't valid KDL.
    Parse,
    /// Valid KDL but not a shape the scene grammar recognises.
    Grammar,
    /// Node appeared in a place the scope table (R2) disallows.
    MisplacedNode,
    /// Unknown node at the scene root (R1 "did you mean …?" path).
    UnknownNode,
    /// Duplicate declaration of a singular node (e.g. two `engine { }`).
    DuplicateNode,
    /// `plugin { }` body declares both `summon` and `on` (R6).
    PluginAmbiguousLifecycle,
    /// `plugin { source "ext:X" }` without a matching `use "X"` (R6).
    ExtNotUsed,
    /// File contains both `scene { }` and top-level `layout { }` (R15).
    AmbiguousFileShape,
    /// File has neither `scene { }` wrapper nor top-level `layout { }` (R15).
    EmptyOrUnknown,
    /// `include` DAG forms a cycle (R11).
    IncludeCycle,
    /// Scene has both inline `engine { }` and `use "engine-*"` (R17).
    EngineConflict,
}

impl ErrorCode {
    /// Stable string form rendered through `miette::Diagnostic::code`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Parse => "scene/parse",
            ErrorCode::Grammar => "scene/grammar",
            ErrorCode::MisplacedNode => "scene/misplaced-node",
            ErrorCode::UnknownNode => "scene/unknown-node",
            ErrorCode::DuplicateNode => "scene/duplicate-node",
            ErrorCode::PluginAmbiguousLifecycle => "scene/plugin-ambiguous-lifecycle",
            ErrorCode::ExtNotUsed => "scene/ext-not-used",
            ErrorCode::AmbiguousFileShape => "scene/ambiguous-file-shape",
            ErrorCode::EmptyOrUnknown => "scene/empty-or-unknown",
            ErrorCode::IncludeCycle => "scene/include-cycle",
            ErrorCode::EngineConflict => "scene/engine-conflict",
        }
    }
}

/// Top-level scene compile error. Every variant renders as a
/// miette-formatted diagnostic with a stable `scene/*` error code,
/// per-variant help text, and at least one labeled source span.
#[derive(Debug, Error, Diagnostic)]
pub enum SceneError {
    /// KDL 2.0 parse failure — the file is not valid KDL at all. Usually
    /// originates as a `facet_kdl::KdlDeserializeError`; wrap it here so
    /// the rest of the compile pipeline has a single error type to
    /// shuttle around.
    #[error("scene file is not valid KDL")]
    #[diagnostic(
        code = "scene/parse",
        help("KDL 2.0 syntax error — see the caret below. Run `ark scene fmt` on a known-good reference to compare shapes.")
    )]
    Parse {
        /// Full source text of the offending file, keyed by path for
        /// miette's renderer.
        #[source_code]
        src: NamedSource<String>,

        /// Byte offset + length of the token that tripped the parser.
        #[label("here")]
        at: SourceSpan,

        /// Parser's raw message (KDL-level detail).
        message: String,
    },

    /// Valid KDL but not a shape the scene grammar accepts. This is the
    /// catch-all for node-level schema mismatches the parser can't
    /// repair (e.g. missing required argument on `plugin`).
    #[error("scene grammar violation: {message}")]
    #[diagnostic(
        code = "scene/grammar",
        help("Consult `context/kits/cavekit-scene.md` R1 for the scene-root grammar, or run `ark scene check` for a full validation pass.")
    )]
    Grammar {
        /// Human-readable description of the mismatch.
        message: String,

        /// File whose parse failed.
        #[source_code]
        src: NamedSource<String>,

        /// Offending span.
        #[label("not accepted here")]
        at: SourceSpan,
    },

    /// A node appeared in a position the scope table (R2) disallows —
    /// e.g. `on` inside `layout`, or `plugin` inside `tab`.
    #[error("{node} not allowed inside {parent}")]
    #[diagnostic(
        code = "scene/misplaced-node",
        help("R2 scope table: `on`, `keybind`, `plugin`, `use`, `extends`, `include` are scene-root only; `tab`/`pane` belong inside `layout`; `when=` attrs are legal on `tab`/`pane` only.")
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
        #[label("{node} cannot live inside {parent}")]
        at: SourceSpan,
    },

    /// Unknown top-level node (R1 "did you mean …?" path). Suggestion is
    /// computed by the caller (facet has a built-in typo suggester;
    /// when that fires it populates `suggestion`).
    #[error("unknown node `{node}` at scene root")]
    #[diagnostic(
        code = "scene/unknown-node",
        help("Scene-root admits: extends, include, use, layout, plugin, on, keybind, engine, clear-reactions, clear-keybind, disable-plugin.")
    )]
    UnknownNode {
        /// Name of the offending node.
        node: String,

        /// Optional "did you mean X?" hint if the compile pipeline
        /// could derive one (not shown if `None`).
        suggestion: Option<String>,

        /// File containing the unknown node.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the unknown node name.
        #[label("not a recognised scene-root node")]
        at: SourceSpan,
    },

    /// Duplicate declaration of a singular node — the second occurrence
    /// is reported here with the first one as a related label.
    #[error("duplicate `{node}` declaration")]
    #[diagnostic(
        code = "scene/duplicate-node",
        help("This node only accepts one declaration per scene. Remove the later occurrence or merge them upstream in `extends`.")
    )]
    DuplicateNode {
        /// Name of the duplicated node kind.
        node: String,

        /// File containing the second occurrence.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the FIRST (earlier) declaration.
        #[label("first declared here")]
        first: SourceSpan,

        /// Span of the duplicate (later) declaration.
        #[label("duplicate here")]
        second: SourceSpan,
    },

    /// A `plugin { … }` block declares both `summon` and `on`, which
    /// makes its lifecycle ambiguous (R6).
    #[error("plugin `{name}` declares both `summon` and `on` — lifecycle is ambiguous")]
    #[diagnostic(
        code = "scene/plugin-ambiguous-lifecycle",
        help("R6: a plugin is either summon-mode (has `summon`) or event-mount (has `on`), not both. Pick one or split into two `plugin {{ }}` blocks.")
    )]
    PluginAmbiguousLifecycle {
        /// Plugin name as declared.
        name: String,

        /// File containing the plugin block.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the `summon` child.
        #[label("`summon` declared here")]
        summon_at: SourceSpan,

        /// Span of the `on` child.
        #[label("`on` declared here")]
        on_at: SourceSpan,
    },

    /// `plugin "<name>" { source "ext:X" }` used without a matching
    /// `use "X"` (transitive or direct) in the same scene (R6).
    #[error("plugin `{plugin}` references extension `{ext}` but no `use \"{ext}\"` is in scope")]
    #[diagnostic(
        code = "scene/ext-not-used",
        help("Add `use \"{ext}\"` to the scene root to activate the extension, or change the plugin `source` to `shipped:*` / `file:*` / `url:*`.")
    )]
    ExtNotUsed {
        /// Plugin whose source points at the extension.
        plugin: String,

        /// Name of the extension being referenced.
        ext: String,

        /// File containing the plugin block.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the `source "ext:X"` argument.
        #[label("references `ext:{ext}` with no matching `use`")]
        at: SourceSpan,
    },

    /// File contains both a `scene { }` wrapper AND a top-level
    /// (non-scene-child) `layout { }` node (R15 file-shape detection).
    #[error("scene file has both a `scene {{ }}` wrapper and a top-level `layout {{ }}`")]
    #[diagnostic(
        code = "scene/ambiguous-file-shape",
        help("Pick one. Either move the top-level `layout` into the `scene {{ }}` body, or delete the `scene` wrapper so the file parses in legacy layout-only mode (R15).")
    )]
    AmbiguousFileShape {
        /// File containing both shapes.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the `scene { }` wrapper.
        #[label("scene wrapper here")]
        scene_at: SourceSpan,

        /// Span of the stray top-level `layout { }`.
        #[label("and top-level layout here")]
        layout_at: SourceSpan,
    },

    /// File has neither `scene { }` nor a top-level `layout { }` (R15).
    #[error("scene file has neither a `scene {{ }}` wrapper nor a top-level `layout {{ }}`")]
    #[diagnostic(
        code = "scene/empty-or-unknown",
        help("Wrap the contents in `scene \"<name>\" {{ … }}`; see `cavekit-scene.md` R1 for the full grammar.")
    )]
    EmptyOrUnknown {
        /// File that failed the shape probe.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the first (offending) node, or `(0, 0)` when the
        /// file is entirely empty.
        #[label("no recognisable scene or layout root")]
        at: SourceSpan,
    },

    /// `include` DAG contains a cycle (R11). The `trace` is stored as
    /// related diagnostics so miette renders each file's span in turn.
    #[error("include cycle detected through `{starting_file}`")]
    #[diagnostic(
        code = "scene/include-cycle",
        help("Break the cycle: remove one `include` along the chain, or refactor shared content into an `extends` base scene.")
    )]
    IncludeCycle {
        /// File where the cycle detector first noticed the loop.
        starting_file: String,

        /// Entry file's source for the primary label.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the offending `include` statement.
        #[label("this `include` closes the cycle")]
        at: SourceSpan,

        /// Per-step sources, each carrying its own `NamedSource` so
        /// miette prints the full trail with file/line/caret context.
        #[related]
        trail: Vec<IncludeCycleStep>,
    },

    /// Scene file contains both an inline `engine { }` block AND a
    /// `use "engine-*"` extension (R17 intra-scene mutual exclusion).
    #[error("scene declares both an inline `engine` block and a `use \"{use_name}\"` engine extension")]
    #[diagnostic(
        code = "scene/engine-conflict",
        help("R17: pick one. Delete the inline `engine {{ }}` block or remove the `use \"{use_name}\"` entry. Future versions may allow inline-wins layering.")
    )]
    EngineConflict {
        /// Name of the clashing `use` target (e.g. `"engine-claude"`).
        use_name: String,

        /// File containing both declarations.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the inline `engine { }` block.
        #[label("inline engine block here")]
        engine_at: SourceSpan,

        /// Span of the `use "engine-*"` declaration.
        #[label("extension engine declared here")]
        use_at: SourceSpan,
    },
}

impl SceneError {
    /// Return the canonical `scene/*` error-code enum for this variant.
    /// Useful for tests and dispatch tables that prefer symbolic
    /// matching over `miette::Diagnostic::code`'s boxed `Display`.
    pub fn code_enum(&self) -> ErrorCode {
        match self {
            SceneError::Parse { .. } => ErrorCode::Parse,
            SceneError::Grammar { .. } => ErrorCode::Grammar,
            SceneError::MisplacedNode { .. } => ErrorCode::MisplacedNode,
            SceneError::UnknownNode { .. } => ErrorCode::UnknownNode,
            SceneError::DuplicateNode { .. } => ErrorCode::DuplicateNode,
            SceneError::PluginAmbiguousLifecycle { .. } => ErrorCode::PluginAmbiguousLifecycle,
            SceneError::ExtNotUsed { .. } => ErrorCode::ExtNotUsed,
            SceneError::AmbiguousFileShape { .. } => ErrorCode::AmbiguousFileShape,
            SceneError::EmptyOrUnknown { .. } => ErrorCode::EmptyOrUnknown,
            SceneError::IncludeCycle { .. } => ErrorCode::IncludeCycle,
            SceneError::EngineConflict { .. } => ErrorCode::EngineConflict,
        }
    }
}

/// One step in an include-cycle trail. Each step carries its own
/// `NamedSource` so miette prints file/line/caret for every hop in the
/// cycle, not just the closing include.
#[derive(Debug, Error, Diagnostic)]
#[error("include chain step via `{via_file}`")]
#[diagnostic(
    code = "scene/include-cycle-step",
    severity(Advice),
    help("Each step in the cycle; follow from entry file through these to see the loop.")
)]
pub struct IncludeCycleStep {
    /// File at this hop in the cycle.
    pub via_file: String,

    /// Source text of this file for renderer context.
    #[source_code]
    pub src: NamedSource<String>,

    /// Span of the `include` statement in this file that points to the
    /// next hop.
    #[label("continues here")]
    pub at: SourceSpan,
}

// ---------------------------------------------------------------------------
// Tests — one per variant, validating diagnostic surface per R12
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic;

    fn src(name: &str, text: &str) -> NamedSource<String> {
        NamedSource::new(name, text.to_string())
    }

    /// Assert that the rendered `Display` of `Diagnostic::code` matches
    /// the expected `scene/*` code string.
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

    /// Assert that the Diagnostic surfaces at least one label (miette
    /// `labels()` returns an iterator of `LabeledSpan`).
    fn assert_has_label(err: &dyn Diagnostic) {
        let mut labels = err.labels().expect("diagnostic has labels() impl");
        assert!(labels.next().is_some(), "expected at least one label");
    }

    /// Assert that `help()` renders to something non-empty — every
    /// variant sets help text per R12.
    fn assert_has_help(err: &dyn Diagnostic) {
        let help = err.help().map(|h| h.to_string()).unwrap_or_default();
        assert!(!help.is_empty(), "expected non-empty help text");
    }

    #[test]
    fn parse_error() {
        let err = SceneError::Parse {
            src: src("scene.kdl", "scene \"x\" { !!! }"),
            at: (12, 3).into(),
            message: "unexpected `!`".to_string(),
        };
        assert_code(&err, "scene/parse");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::Parse);
    }

    #[test]
    fn grammar_error() {
        let err = SceneError::Grammar {
            message: "plugin block is missing `source`".to_string(),
            src: src("scene.kdl", "plugin \"status\" { mount \"floating\" }"),
            at: (0, 6).into(),
        };
        assert_code(&err, "scene/grammar");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::Grammar);
    }

    #[test]
    fn misplaced_node_error() {
        let err = SceneError::MisplacedNode {
            node: "on".to_string(),
            parent: "layout".to_string(),
            src: src("scene.kdl", "layout { on \"AgentReady\" { } }"),
            at: (9, 2).into(),
        };
        assert_code(&err, "scene/misplaced-node");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::MisplacedNode);
    }

    #[test]
    fn unknown_node_error() {
        let err = SceneError::UnknownNode {
            node: "reaction".to_string(),
            suggestion: Some("on".to_string()),
            src: src("scene.kdl", "reaction \"AgentReady\" { }"),
            at: (0, 8).into(),
        };
        assert_code(&err, "scene/unknown-node");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::UnknownNode);
    }

    #[test]
    fn duplicate_node_error() {
        let err = SceneError::DuplicateNode {
            node: "engine".to_string(),
            src: src("scene.kdl", "engine { } engine { }"),
            first: (0, 6).into(),
            second: (12, 6).into(),
        };
        assert_code(&err, "scene/duplicate-node");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::DuplicateNode);
    }

    #[test]
    fn plugin_ambiguous_lifecycle_error() {
        let err = SceneError::PluginAmbiguousLifecycle {
            name: "status".to_string(),
            src: src(
                "scene.kdl",
                "plugin \"status\" { summon \"Alt p\"; on \"UserEvent\" { } }",
            ),
            summon_at: (18, 6).into(),
            on_at: (33, 2).into(),
        };
        assert_code(&err, "scene/plugin-ambiguous-lifecycle");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::PluginAmbiguousLifecycle);
    }

    #[test]
    fn ext_not_used_error() {
        let err = SceneError::ExtNotUsed {
            plugin: "status".to_string(),
            ext: "statusbar".to_string(),
            src: src("scene.kdl", "plugin \"status\" { source \"ext:statusbar\" }"),
            at: (25, 15).into(),
        };
        assert_code(&err, "scene/ext-not-used");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::ExtNotUsed);
    }

    #[test]
    fn ambiguous_file_shape_error() {
        let err = SceneError::AmbiguousFileShape {
            src: src("scene.kdl", "scene \"x\" { }\nlayout { }"),
            scene_at: (0, 5).into(),
            layout_at: (14, 6).into(),
        };
        assert_code(&err, "scene/ambiguous-file-shape");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::AmbiguousFileShape);
    }

    #[test]
    fn empty_or_unknown_error() {
        let err = SceneError::EmptyOrUnknown {
            src: src("scene.kdl", ""),
            at: (0, 0).into(),
        };
        assert_code(&err, "scene/empty-or-unknown");
        // Note: empty-file empty-span still counts as a label because
        // the field is present. Check label exists:
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::EmptyOrUnknown);
    }

    #[test]
    fn include_cycle_error() {
        let err = SceneError::IncludeCycle {
            starting_file: "scene.kdl".to_string(),
            src: src("scene.kdl", "include \"child.kdl\""),
            at: (0, 7).into(),
            trail: vec![IncludeCycleStep {
                via_file: "child.kdl".to_string(),
                src: src("child.kdl", "include \"scene.kdl\""),
                at: (0, 7).into(),
            }],
        };
        assert_code(&err, "scene/include-cycle");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::IncludeCycle);

        // Related diagnostics must surface through `.related()`.
        let related: Vec<_> = err.related().expect("related impl").collect();
        assert_eq!(related.len(), 1);
    }

    #[test]
    fn engine_conflict_error() {
        let err = SceneError::EngineConflict {
            use_name: "engine-claude".to_string(),
            src: src(
                "scene.kdl",
                "use \"engine-claude\"\nengine { name \"claude\" }",
            ),
            engine_at: (20, 6).into(),
            use_at: (0, 3).into(),
        };
        assert_code(&err, "scene/engine-conflict");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::EngineConflict);
    }

    /// Sanity-check that `ErrorCode::as_str` covers every variant and
    /// the strings match the R12 spec exactly.
    #[test]
    fn error_code_strings_match_spec() {
        assert_eq!(ErrorCode::Parse.as_str(), "scene/parse");
        assert_eq!(ErrorCode::Grammar.as_str(), "scene/grammar");
        assert_eq!(ErrorCode::MisplacedNode.as_str(), "scene/misplaced-node");
        assert_eq!(ErrorCode::UnknownNode.as_str(), "scene/unknown-node");
        assert_eq!(ErrorCode::DuplicateNode.as_str(), "scene/duplicate-node");
        assert_eq!(
            ErrorCode::PluginAmbiguousLifecycle.as_str(),
            "scene/plugin-ambiguous-lifecycle"
        );
        assert_eq!(ErrorCode::ExtNotUsed.as_str(), "scene/ext-not-used");
        assert_eq!(
            ErrorCode::AmbiguousFileShape.as_str(),
            "scene/ambiguous-file-shape"
        );
        assert_eq!(ErrorCode::EmptyOrUnknown.as_str(), "scene/empty-or-unknown");
        assert_eq!(ErrorCode::IncludeCycle.as_str(), "scene/include-cycle");
        assert_eq!(ErrorCode::EngineConflict.as_str(), "scene/engine-conflict");
    }
}
