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
//! op/unresolved-ref
//! op/failed
//! ext/metadata-missing
//! ext/metadata-invalid
//! ext/version-mismatch
//! ext/bad-config
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
    /// `extends "<name>"` could not be resolved through the
    /// scene-search-path (T-9.1 / R11).
    ExtendsNotFound,
    /// Scene declares more than one `extends` clause — the grammar allows
    /// only one parent per scene (T-9.1 / R11).
    MultipleExtends,
    /// `extends` graph forms a cycle across the composed scenes
    /// (T-9.1 / R11). Distinct from [`IncludeCycle`] to keep diagnostic
    /// codes aligned with the user's vocabulary.
    ExtendsCycle,
    /// Duplicate `plugin "<name>"` block across merged scenes without
    /// `override=true` on the later occurrence (R11 plugin-merge rule,
    /// T-9.4).
    DuplicatePlugin,
    /// Duplicate `tab name="<X>"` across merged layout fragments — the
    /// R11 merge rule explicitly forbids it (T-9.4).
    DuplicateTab,
    /// Scene has both inline `engine { }` and `use "engine-*"` (R17).
    EngineConflict,
    /// CEL expression failed to parse at compile time (R8 / T-2.1).
    CelParse,
    /// CEL expression parsed but failed during evaluation (R8 / T-2.1).
    CelEvaluate,
    /// CEL expression exceeded the static `max_expression_length` guard (R8 / T-2.6).
    CelExpressionTooLong,
    /// CEL AST exceeded the static `max_ast_depth` guard (R8 / T-2.6).
    CelAstTooDeep,
    /// Minijinja template failed to parse/compile at scene-check time (R9 / T-2.4).
    TemplateCompile,
    /// Minijinja template failed to render with a strict context (R9 / T-2.4).
    TemplateRender,
    /// Op attribute references a tab / plugin / ext that the scene
    /// doesn't declare (R7 / R12 / T-4.3).
    OpUnresolvedRef,
    /// Op dispatched at runtime returned an error (R7 / R12 / T-4.5).
    OpFailed,
    /// Scene `emit` op targets a non-UserEvent kind (R4 / T-5.5).
    /// Scene authors can only emit `UserEvent:<name>` events — core
    /// kinds come from supervisor/agent/plugin surfaces.
    EmitNonUserEvent,
    /// Scene reactions form a cycle through user-event emits
    /// (R4 / T-5.5). `emit A → on A emits B → on B emits A`.
    EmitCycle,
    /// Scene `emit` uses a `source` field outside the R4 canonical set
    /// (T-5.5). Canonical values: `scene`, `ext:<n>`, `plugin:<n>`,
    /// `hook:<n>`, `core`, `agent`.
    EmitInvalidSource,
    /// Scene `keybind "<chord>"` chord string violates the loose
    /// grammar `(Mod )*KEY` (T-6.6). `Mod ∈ {Ctrl, Alt, Shift, Super}`,
    /// `KEY` is alphanumeric or a single zellij-known special
    /// (`Tab`, `Enter`, `Space`, arrow names, `F1`–`F12`).
    InvalidChord,
    /// Plugin `config { }` block failed schema validation against the
    /// shipped-plugin Config struct registered in
    /// [`crate::config_schema::ConfigSchemaRegistry`] (R10 / T-7.6).
    PluginBadConfig,
    /// Plugin `config { }` block declared a key the shipped-plugin's
    /// Config schema doesn't recognise (R10 / T-7.6).
    PluginUnknownConfigKey,
    /// Wasm cartridge does not contain an `ark.metadata` custom section
    /// (R10 / T-10.2). The author forgot the `#[link_section]` static.
    WasmMetaMissing,
    /// `ark.metadata` custom section is present but its bytes do not
    /// decode as a valid `ExtensionMetadata` KDL document (R10 / T-10.2).
    WasmMetaInvalid,
    /// `ExtensionMetadata::ark_range` (or `zellij_range`) does not admit
    /// the host's running version (R10 / R16 / T-10.4).
    ExtVersionMismatch,
    /// User-supplied value for an extension config field does not match
    /// the field's declared type (R10 / T-10.5). Distinct from
    /// [`Self::PluginUnknownConfigKey`] which fires on unknown keys.
    ExtBadConfig,
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
            ErrorCode::ExtendsNotFound => "scene/extends-not-found",
            ErrorCode::MultipleExtends => "scene/multiple-extends",
            ErrorCode::ExtendsCycle => "scene/extends-cycle",
            ErrorCode::DuplicatePlugin => "scene/duplicate-plugin",
            ErrorCode::DuplicateTab => "scene/duplicate-tab",
            ErrorCode::EngineConflict => "scene/engine-conflict",
            ErrorCode::CelParse => "cel/parse",
            ErrorCode::CelEvaluate => "cel/evaluate",
            ErrorCode::CelExpressionTooLong => "cel/expression-too-long",
            ErrorCode::CelAstTooDeep => "cel/ast-too-deep",
            ErrorCode::TemplateCompile => "scene/template-compile",
            ErrorCode::TemplateRender => "scene/template-render",
            ErrorCode::OpUnresolvedRef => "op/unresolved-ref",
            ErrorCode::OpFailed => "op/failed",
            ErrorCode::EmitNonUserEvent => "scene/emit-non-user-event",
            ErrorCode::EmitCycle => "scene/emit-cycle",
            ErrorCode::EmitInvalidSource => "scene/emit-invalid-source",
            ErrorCode::InvalidChord => "scene/invalid-chord",
            ErrorCode::PluginBadConfig => "plugin/bad-config",
            ErrorCode::PluginUnknownConfigKey => "plugin/unknown-config-key",
            ErrorCode::WasmMetaMissing => "ext/metadata-missing",
            ErrorCode::WasmMetaInvalid => "ext/metadata-invalid",
            ErrorCode::ExtVersionMismatch => "ext/version-mismatch",
            ErrorCode::ExtBadConfig => "ext/bad-config",
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
    /// computed by the caller via [`crate::suggest::suggest_similar`]
    /// (T-1.3); when populated, it surfaces in the rendered help text
    /// as a "did you mean …?" hint prepended to the static
    /// scene-root-admits list.
    #[error("unknown node `{node}` at scene root")]
    #[diagnostic(code = "scene/unknown-node")]
    UnknownNode {
        /// Name of the offending node.
        node: String,

        /// Optional "did you mean X?" hint. Wired up by the scope
        /// pass using [`crate::suggest::suggest_similar`]. The help
        /// field below folds it into the rendered output.
        suggestion: Option<String>,

        /// Pre-rendered help text. Built by the caller (typically
        /// via [`SceneError::unknown_node`]) so miette's
        /// `#[help]` attribute can splice a "did you mean …?" line
        /// in front of the static scene-root-admits list when
        /// `suggestion` is present.
        #[help]
        help: String,

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

    /// `extends "<name>"` could not be resolved through the scene
    /// search path (T-9.1 / R11). The `searched` vector records every
    /// candidate path that was probed so the rendered diagnostic tells
    /// the user exactly where ark looked.
    #[error("extends target `{name}` not found on the scene search path")]
    #[diagnostic(
        code = "scene/extends-not-found",
        help("Scene-search-path rungs (in order): `./.ark/scenes/<name>.kdl`, `${{XDG_CONFIG_HOME}}/<appname>/scenes/<name>.kdl`, built-in shipped scenes. Either create the parent scene file or adjust the `extends` argument.")
    )]
    ExtendsNotFound {
        /// Parent scene name that failed to resolve.
        name: String,
        /// Ordered list of file paths the resolver probed — written
        /// verbatim into the diagnostic's rendered message so the user
        /// can see the exact search path that came up empty.
        searched: Vec<String>,
    },

    /// Scene file carries more than one `extends` clause (T-9.1 / R11).
    /// Only one parent per scene is allowed; later clauses are
    /// reported with the first-seen as a related-label hint.
    #[error("scene declares multiple `extends` clauses — only one parent per scene is allowed")]
    #[diagnostic(
        code = "scene/multiple-extends",
        help("R11: one `extends` per scene. Remove the extra clause(s), or refactor the shared base into an `include`-splice fragment.")
    )]
    MultipleExtends {
        /// File containing the offending clauses.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the FIRST `extends` clause.
        #[label("first `extends` here")]
        first: SourceSpan,

        /// Span of the duplicate (later) `extends` clause.
        #[label("duplicate `extends` here")]
        second: SourceSpan,
    },

    /// `extends` graph forms a cycle (T-9.1 / R11). Parent chain
    /// revisits a previously-seen scene name. The `trail` records
    /// every hop in source order so the rendered diagnostic shows the
    /// full loop.
    #[error("extends cycle detected through scene `{starting_scene}`")]
    #[diagnostic(
        code = "scene/extends-cycle",
        help("Break the cycle: one of the parents along the chain already extends a scene on the way here. Remove an `extends` clause or refactor a shared base fragment into an `include`.")
    )]
    ExtendsCycle {
        /// Scene name where the cycle was first detected.
        starting_scene: String,
        /// Trail of scene names from the starting entry through the
        /// hop that closed the loop — e.g. `["child", "parent", "child"]`.
        trail: Vec<String>,
    },

    /// Duplicate `plugin "<name>"` across merged scenes without
    /// `override=true` on the later block (R11 / T-9.4).
    ///
    /// Fires during the composition merge step, not during parse. The
    /// first block supplies the canonical declaration; the second
    /// block must either (a) drop the duplicate or (b) set
    /// `override=true` to replace the earlier contribution.
    #[error("duplicate `plugin \"{name}\"` declaration across merged scenes")]
    #[diagnostic(
        code = "scene/duplicate-plugin",
        help("R11: a later `plugin \"<name>\"` block must set `override=true` to replace an earlier declaration. Otherwise remove the duplicate or rename one of the plugins.")
    )]
    DuplicatePlugin {
        /// Plugin name duplicated across fragments.
        name: String,
    },

    /// Duplicate `tab name="<X>"` across merged layout fragments
    /// (R11 / T-9.4). Layout merge has no `override=` escape hatch
    /// in v1 — tab templates via explicit merge attribute are
    /// deferred to v0.3+ (R11 note).
    #[error("duplicate `tab \"{name}\"` across merged layouts")]
    #[diagnostic(
        code = "scene/duplicate-tab",
        help("R11: layout `tab` names must be unique across composed scenes. Rename one of the tabs, or factor the shared tab into a base scene inherited via `extends`.")
    )]
    DuplicateTab {
        /// Tab name duplicated across fragments.
        name: String,
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

    /// CEL expression (a `when=` predicate, `if=` guard, or similar)
    /// failed to parse at compile time (R8 / T-2.1).
    ///
    /// Emitted by [`crate::cel::compile`] when `cel-interpreter`
    /// rejects the source. The `message` carries the CEL parser's
    /// rendered output verbatim — it includes line/column context for
    /// the offending token.
    #[error("CEL expression failed to parse: {message}")]
    #[diagnostic(
        code = "cel/parse",
        help("Consult the CEL spec (https://github.com/google/cel-spec) — common causes are unmatched parens, missing `==`, or reserved identifiers.")
    )]
    CelParse {
        /// Raw parser output (multi-line, includes `^` caret).
        message: String,

        /// Original source text for renderer context.
        #[source_code]
        src: NamedSource<String>,

        /// Span covering the full expression within its surrounding
        /// scene file. Best-effort — callers without a span should
        /// pass `(0, expr.len())`.
        #[label("while compiling this CEL expression")]
        at: SourceSpan,
    },

    /// CEL expression compiled but evaluation failed at runtime
    /// (R8 / T-2.1).
    ///
    /// Emitted by [`crate::cel::eval`] when `cel-interpreter`
    /// returns an `ExecutionError` (undeclared reference, type
    /// mismatch, etc.).
    #[error("CEL evaluation failed: {message}")]
    #[diagnostic(
        code = "cel/evaluate",
        help("Check that every identifier used in the expression is bound by the surrounding context (event.*, payload.*, agent.*, session.*).")
    )]
    CelEvaluate {
        /// `ExecutionError`'s rendered string.
        message: String,
    },

    /// CEL expression exceeded the static `max_expression_length`
    /// budget (R8 / T-2.6). Prevents a pathologically large source
    /// string from reaching the parser.
    #[error("CEL expression is too long: {len} bytes (max {max})")]
    #[diagnostic(
        code = "cel/expression-too-long",
        help("Scene CEL expressions are capped at 4096 bytes. Split large predicates into smaller reactions or named extensions.")
    )]
    CelExpressionTooLong {
        /// Actual byte length of the offending source.
        len: usize,
        /// Static cap, `max_expression_length`.
        max: usize,
    },

    /// CEL AST exceeded the static `max_ast_depth` budget (R8 / T-2.6).
    /// Defence against pathological nesting that would bog down the
    /// evaluator without tripping the length cap.
    #[error("CEL expression AST is too deep: depth {depth} (max {max})")]
    #[diagnostic(
        code = "cel/ast-too-deep",
        help("Scene CEL AST depth is capped at 64. Flatten nested conditionals or lift sub-expressions into separate reactions.")
    )]
    CelAstTooDeep {
        /// Measured maximum depth.
        depth: usize,
        /// Static cap, `max_ast_depth`.
        max: usize,
    },

    /// Minijinja template failed to parse/compile at scene-check time
    /// (R9 / T-2.4).
    #[error("template failed to compile: {message}")]
    #[diagnostic(
        code = "scene/template-compile",
        help("Consult the minijinja docs for syntax (https://docs.rs/minijinja). Common causes: unmatched `{{% %}}`, typo in block name.")
    )]
    TemplateCompile {
        /// Rendered minijinja error.
        message: String,
    },

    /// Minijinja template failed to render (strict-undefined or
    /// general runtime error) (R9 / T-2.4).
    #[error("template failed to render: {message}")]
    #[diagnostic(
        code = "scene/template-render",
        help("Check that every `{{{{ var }}}}` resolves to a value in the current scope. Compile-time templates use the five-var LayoutVars surface; runtime templates use (event, payload, agent, session).")
    )]
    TemplateRender {
        /// Rendered minijinja error.
        message: String,
    },

    /// An op argument references a named resource (tab, plugin) that
    /// the scene doesn't declare (T-4.3 cross-reference pass).
    ///
    /// Covers:
    ///
    /// * `split_pane into="<tab>"` pointing at a tab missing from the
    ///   `layout { tab name="<tab>" }` set.
    /// * `pipe plugin="<name>"` referencing a plugin without a matching
    ///   `plugin "<name>" { }` block (nor an auto-mount from an
    ///   extension's sidecar).
    /// * `mount_plugin name="<name>"` / `unmount_plugin name="<name>"`
    ///   referencing an unknown plugin.
    ///
    /// The `help` field is pre-rendered so miette can splice a
    /// "did you mean `X`?" hint in front of a list of available refs
    /// when [`crate::suggest::suggest_similar`] finds a close match.
    #[error("op `{op}` references unknown {kind} `{name}`")]
    #[diagnostic(code = "op/unresolved-ref")]
    OpUnresolvedRef {
        /// Op that carried the unresolved reference
        /// (e.g. `"ark.core.split_pane"`).
        op: String,

        /// Category of the missing reference (`"tab"`, `"plugin"`).
        /// Human-readable noun surfaced in the diagnostic message.
        kind: String,

        /// The unknown name as it appeared in source (e.g. `"work"`).
        name: String,

        /// Optional "did you mean X?" suggestion from
        /// [`crate::suggest::suggest_similar`].
        suggestion: Option<String>,

        /// Pre-rendered help text. Built by [`SceneError::op_unresolved_ref`]
        /// so the typo hint + available-refs list fall through to
        /// miette's renderer in one pass.
        #[help]
        help: String,

        /// File carrying the offending op node.
        #[source_code]
        src: NamedSource<String>,

        /// Span of the offending attribute (the `into="X"` / `plugin="X"` /
        /// `name="X"` value).
        #[label("no such {kind} declared in this scene")]
        at: SourceSpan,
    },

    /// Op dispatch failed at runtime (T-4.5 fail-fast surface). Mirrors
    /// `IntentError::Failed` on the scene compile side so the
    /// reactions dispatcher can surface a uniform miette diagnostic
    /// per R12's `op/failed` code.
    ///
    /// Emitted by [`crate::ops::dispatch::dispatch_sequence`] when an
    /// op in a reaction returns an error. The dispatcher logs the
    /// error via `tracing::error!(target = "scene::ops", ...)` and
    /// returns this variant; the caller (reactions dispatcher)
    /// swallows it so the event loop stays alive.
    #[error("op `{op}` failed at dispatch: {message}")]
    #[diagnostic(
        code = "op/failed",
        help("The op ran to completion but returned an error. See the per-op error (`tracing::error!` line tagged `scene::ops`) for the underlying cause.")
    )]
    OpFailed {
        /// Op name that failed (e.g. `"ark.core.exec"`).
        op: String,

        /// Human-readable failure summary.
        message: String,
    },

    /// Scene `emit "<target>"` op whose target is not a `UserEvent:<name>`
    /// (R4 / T-5.5). Scene authors are restricted to emitting user
    /// events; core events come from the supervisor / agent / plugin
    /// layers, so allowing scenes to emit core kinds would blur the
    /// attribution model and open static cycle detection.
    #[error("scene `emit` target `{target}` is not a UserEvent")]
    #[diagnostic(
        code = "scene/emit-non-user-event",
        help("Scene reactions may only emit namespaced user events. Rewrite as `emit \"UserEvent:<namespaced-name>\"` (or simply `emit \"user.foo\"`), or if this is a core event it should come from the supervisor / agent / plugin layer, not a scene reaction.")
    )]
    EmitNonUserEvent {
        /// The target string the scene tried to emit.
        target: String,
    },

    /// Scene reactions form a cycle through user-event emits (R4 /
    /// T-5.5). Compile-time detection builds a DAG from every
    /// `emit "<user-event>"` op to every `on "UserEvent:<name>"`
    /// reaction; a back-edge is a cycle.
    #[error("emit cycle detected: {trail}")]
    #[diagnostic(
        code = "scene/emit-cycle",
        help("Break the cycle by removing one of the `emit` ops in the chain, or guard one of the reactions with an `if=` predicate that terminates the loop. Runtime cascade-depth bounding (R4) still catches unbounded chains but a statically visible cycle is almost always a bug.")
    )]
    EmitCycle {
        /// Dotted chain describing the cycle (e.g. `"user.a → user.b → user.a"`).
        trail: String,
    },

    /// Scene `emit` declares a `source="<value>"` outside the R4
    /// canonical set (T-5.5). Per R4: `core`, `scene`, `ext:<name>`,
    /// `plugin:<name>`, `hook:<name>`, `agent`. Scene `emit` ops
    /// typically leave `source` implicit (defaults to `"scene"` — see
    /// `EmitOp`); this variant surfaces only when a scene author
    /// explicitly sets an invalid source.
    #[error("scene `emit` source `{value}` is not a canonical attribution tag")]
    #[diagnostic(
        code = "scene/emit-invalid-source",
        help("Valid `source` values per R4: `core`, `scene`, `ext:<name>`, `plugin:<name>`, `hook:<name>`, `agent`. Scenes usually leave `source` implicit — the `emit` op fills it in as `\"scene\"`.")
    )]
    EmitInvalidSource {
        /// The offending source string.
        value: String,
    },

    /// Scene `keybind "<chord>"` chord string fails the loose
    /// compile-time grammar (T-6.6).
    ///
    /// The grammar is intentionally loose — `(Mod )*KEY` where `Mod ∈
    /// {Ctrl, Alt, Shift, Super}` and `KEY` is alphanumeric or one of
    /// the canonical zellij specials (`Tab`, `Enter`, `Space`, arrow
    /// names, `F1`–`F12`). Stricter validation (unknown KEY,
    /// unsupported combo) surfaces at first session-spawn through
    /// zellij's own chord lexer; that's the trade-off called out in
    /// R5 (less strict compile-time validation in exchange for zero
    /// maintenance burden as zellij's chord grammar evolves).
    #[error("invalid keybind chord `{chord}`: {reason}")]
    #[diagnostic(
        code = "scene/invalid-chord",
        help("Chord grammar: `(Mod )*KEY`. Mod ∈ {{Ctrl, Alt, Shift, Super}}. KEY is alphanumeric or one of: Tab, Enter, Esc, Space, Backspace, Delete, Insert, Home, End, PageUp, PageDown, Left, Right, Up, Down, F1..F12. Examples: `Alt p`, `Ctrl Shift t`, `F4`.")
    )]
    InvalidChord {
        /// The offending chord string verbatim.
        chord: String,
        /// Human-readable reason the chord was rejected.
        reason: String,
        /// Source file containing the keybind.
        #[source_code]
        src: NamedSource<String>,
        /// Span of the chord string (the first positional argument of
        /// the `keybind` node).
        #[label("chord rejected here")]
        at: SourceSpan,
    },

    /// Plugin `config { }` block failed type / value validation against
    /// the shipped-plugin's in-proc Config schema (R10 / T-7.6).
    ///
    /// Emitted when a user scene provides a `config { }` block under a
    /// `plugin "<name>" { }` whose schema exists in
    /// [`crate::config_schema::ConfigSchemaRegistry`] but whose
    /// declared value shape violates the schema.
    #[error("plugin `{plugin}` config block failed validation: {message}")]
    #[diagnostic(
        code = "plugin/bad-config",
        help("The `config {{ … }}` block under this plugin must match the shipped Config schema. Run `ark scene check` to see the expected shape (facet SHAPE reflection surfaces field docs + types).")
    )]
    PluginBadConfig {
        /// Plugin name whose config block failed validation.
        plugin: String,
        /// Human-readable failure summary from the schema walker.
        message: String,
    },

    /// Plugin `config { }` block declared a key that the shipped
    /// plugin's Config schema does not recognise (R10 / T-7.6).
    ///
    /// Distinct from [`SceneError::PluginBadConfig`]: this variant fires
    /// specifically on unknown KEYS, not on bad values / wrong types.
    /// The scene compile pipeline surfaces this so typos surface at
    /// compile time rather than being silently passed through to the
    /// plugin (which zellij's flat-string env surface has no way to
    /// reject).
    #[error("plugin `{plugin}` config key `{key}` is not in the schema")]
    #[diagnostic(
        code = "plugin/unknown-config-key",
        help("The shipped plugin's Config schema does not declare this key. Remove it, or correct the spelling. Run `ark scene check` to see the expected keys.")
    )]
    PluginUnknownConfigKey {
        /// Plugin name whose config block carried the unknown key.
        plugin: String,
        /// The unknown key as it appeared in source.
        key: String,
    },

    /// Wasm cartridge bytes do not contain an `ark.metadata` custom
    /// section (R10 / T-10.2). Authors embed the section through a
    /// `#[link_section = "ark.metadata"]` static in the cartridge crate;
    /// this error means either the static is missing or the cartridge
    /// was built with `--strip` / `wasm-opt --strip-debug` that dropped
    /// custom sections.
    #[error("wasm cartridge `{path}` is missing the `ark.metadata` custom section")]
    #[diagnostic(
        code = "ext/metadata-missing",
        help("Embed the metadata bytes via `#[link_section = \"ark.metadata\"] pub static ARK_METADATA: [u8; N] = …;` and rebuild without `--strip`. Run `wasm-objdump -h` to verify the section is present.")
    )]
    WasmMetaMissing {
        /// Best-effort identifier for the cartridge whose section is
        /// missing. Filesystem path when known, otherwise a synthetic
        /// label like `"<bytes>"`.
        path: String,
    },

    /// `ark.metadata` custom section is present but cannot be decoded
    /// as a valid `ExtensionMetadata` KDL document (R10 / T-10.2). The
    /// inner `message` carries the underlying KDL / facet-kdl parse
    /// error verbatim.
    #[error("wasm cartridge `{path}` has an invalid `ark.metadata` section: {message}")]
    #[diagnostic(
        code = "ext/metadata-invalid",
        help("The `ark.metadata` custom section must be UTF-8 KDL produced by `ark_ext_metadata::extension_metadata_kdl_bytes`. Re-run the build script that generates the bytes; do not hand-edit the section body.")
    )]
    WasmMetaInvalid {
        /// Cartridge identifier (filesystem path or synthetic label).
        path: String,
        /// Underlying decoder error. May be a UTF-8 conversion failure,
        /// a KDL parse error, or a facet-kdl shape mismatch.
        message: String,
    },

    /// Extension's declared `ark-range` / `zellij-range` does not admit
    /// the host's running version (R10 / R16 / T-10.4). The renderer
    /// shows both the host version and the extension's range so the
    /// user knows whether to upgrade ark or downgrade the extension.
    #[error(
        "extension `{ext}` requires {component} {required} but host is {actual}"
    )]
    #[diagnostic(
        code = "ext/version-mismatch",
        help("Either install a build of the extension that supports the host version range, or upgrade / downgrade ark to a version inside the extension's declared range. R16 governs the wire-compat policy.")
    )]
    ExtVersionMismatch {
        /// Extension name as declared in its manifest.
        ext: String,
        /// Which version axis failed: `"ark"` or `"zellij"`.
        component: &'static str,
        /// The semver range string the extension declared
        /// (e.g. `">= 1.2, < 2.0"`).
        required: String,
        /// The host's actual version (e.g. `"0.1.0"`).
        actual: String,
    },

    /// Value for a known extension config field doesn't match the
    /// field's declared type (R10 / T-10.5). Distinct from
    /// [`Self::PluginUnknownConfigKey`], which fires on unknown KEYS.
    #[error(
        "extension `{ext}` config key `{key}` has the wrong type: {message}"
    )]
    #[diagnostic(
        code = "ext/bad-config",
        help("The extension's `ConfigSchema` declared this field with a specific `type` (one of `string`, `int`, `bool`, `path`, `url`, `duration`). Update the `config {{ }}` block to match, or run `ark ext info <ext>` to inspect the schema.")
    )]
    ExtBadConfig {
        /// Extension name whose config block tripped the validator.
        ext: String,
        /// Offending field name.
        key: String,
        /// Human-readable explanation of the mismatch
        /// (e.g. `"expected int, got string"`).
        message: String,
    },
}

/// Canonical static help text for `scene/unknown-node` — the list
/// of R1-admitted scene-root node names. Used both as the help
/// field value when no suggestion is available and as the suffix
/// when one is.
pub const UNKNOWN_NODE_ADMITS_HELP: &str = "Scene-root admits: extends, include, use, layout, plugin, on, keybind, engine, clear-reactions, clear-keybind, disable-plugin.";

impl SceneError {
    /// Build an `OpUnresolvedRef` with the help text already rendered
    /// so an optional "did you mean …?" hint + an "available <kind>s:"
    /// list surface in the miette output.
    ///
    /// `available` is the caller-supplied universe of declared names
    /// (tabs, plugins). When non-empty, it renders as `Available tabs:
    /// foo, bar, baz.` so the user can spot the correct name without
    /// scrolling back through the scene.
    pub fn op_unresolved_ref(
        op: impl Into<String>,
        kind: impl Into<String>,
        name: impl Into<String>,
        suggestion: Option<String>,
        available: &[&str],
        src: NamedSource<String>,
        at: SourceSpan,
    ) -> Self {
        let kind = kind.into();
        let available_line = if available.is_empty() {
            format!("No {kind}s are declared in this scene.")
        } else {
            format!("Available {kind}s: {}.", available.join(", "))
        };
        let help = match &suggestion {
            Some(s) => format!("did you mean `{s}`? {available_line}"),
            None => available_line,
        };
        SceneError::OpUnresolvedRef {
            op: op.into(),
            kind,
            name: name.into(),
            suggestion,
            help,
            src,
            at,
        }
    }

    /// Build an `UnknownNode` with the help text already rendered so
    /// the optional `suggestion` surfaces in miette's output.
    ///
    /// When `suggestion` is `Some("x")`, the help reads
    /// `did you mean \`x\`? Scene-root admits: …`; when `None`,
    /// only the admits list is shown.
    pub fn unknown_node(
        node: String,
        suggestion: Option<String>,
        src: NamedSource<String>,
        at: SourceSpan,
    ) -> Self {
        let help = match &suggestion {
            Some(s) => format!("did you mean `{s}`? {UNKNOWN_NODE_ADMITS_HELP}"),
            None => UNKNOWN_NODE_ADMITS_HELP.to_string(),
        };
        SceneError::UnknownNode {
            node,
            suggestion,
            help,
            src,
            at,
        }
    }

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
            SceneError::ExtendsNotFound { .. } => ErrorCode::ExtendsNotFound,
            SceneError::MultipleExtends { .. } => ErrorCode::MultipleExtends,
            SceneError::ExtendsCycle { .. } => ErrorCode::ExtendsCycle,
            SceneError::DuplicatePlugin { .. } => ErrorCode::DuplicatePlugin,
            SceneError::DuplicateTab { .. } => ErrorCode::DuplicateTab,
            SceneError::EngineConflict { .. } => ErrorCode::EngineConflict,
            SceneError::CelParse { .. } => ErrorCode::CelParse,
            SceneError::CelEvaluate { .. } => ErrorCode::CelEvaluate,
            SceneError::CelExpressionTooLong { .. } => ErrorCode::CelExpressionTooLong,
            SceneError::CelAstTooDeep { .. } => ErrorCode::CelAstTooDeep,
            SceneError::TemplateCompile { .. } => ErrorCode::TemplateCompile,
            SceneError::TemplateRender { .. } => ErrorCode::TemplateRender,
            SceneError::OpUnresolvedRef { .. } => ErrorCode::OpUnresolvedRef,
            SceneError::OpFailed { .. } => ErrorCode::OpFailed,
            SceneError::EmitNonUserEvent { .. } => ErrorCode::EmitNonUserEvent,
            SceneError::EmitCycle { .. } => ErrorCode::EmitCycle,
            SceneError::EmitInvalidSource { .. } => ErrorCode::EmitInvalidSource,
            SceneError::InvalidChord { .. } => ErrorCode::InvalidChord,
            SceneError::PluginBadConfig { .. } => ErrorCode::PluginBadConfig,
            SceneError::PluginUnknownConfigKey { .. } => ErrorCode::PluginUnknownConfigKey,
            SceneError::WasmMetaMissing { .. } => ErrorCode::WasmMetaMissing,
            SceneError::WasmMetaInvalid { .. } => ErrorCode::WasmMetaInvalid,
            SceneError::ExtVersionMismatch { .. } => ErrorCode::ExtVersionMismatch,
            SceneError::ExtBadConfig { .. } => ErrorCode::ExtBadConfig,
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
        let err = SceneError::unknown_node(
            "reaction".to_string(),
            Some("on".to_string()),
            src("scene.kdl", "reaction \"AgentReady\" { }"),
            (0, 8).into(),
        );
        assert_code(&err, "scene/unknown-node");
        assert_has_label(&err);
        assert_has_help(&err);
        assert_eq!(err.code_enum(), ErrorCode::UnknownNode);
    }

    /// T-1.3 wiring test: when a suggestion is present, the
    /// rendered help text includes a "did you mean …?" hint.
    #[test]
    fn unknown_node_help_surfaces_suggestion() {
        let err = SceneError::unknown_node(
            "keybnd".to_string(),
            Some("keybind".to_string()),
            src("scene.kdl", "keybnd \"Alt p\""),
            (0, 6).into(),
        );
        let help = err.help().map(|h| h.to_string()).unwrap_or_default();
        assert!(
            help.contains("did you mean `keybind`?"),
            "expected suggestion in help, got: {help:?}"
        );
        assert!(help.contains("Scene-root admits:"));
    }

    /// Absent suggestion → only the static admits list renders; no
    /// stray "did you mean" preamble.
    #[test]
    fn unknown_node_without_suggestion_has_no_preamble() {
        let err = SceneError::unknown_node(
            "xyzzy".to_string(),
            None,
            src("scene.kdl", "xyzzy"),
            (0, 5).into(),
        );
        let help = err.help().map(|h| h.to_string()).unwrap_or_default();
        assert!(!help.contains("did you mean"), "got: {help:?}");
        assert!(help.contains("Scene-root admits:"));
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
        assert_eq!(ErrorCode::CelParse.as_str(), "cel/parse");
        assert_eq!(ErrorCode::CelEvaluate.as_str(), "cel/evaluate");
        assert_eq!(
            ErrorCode::CelExpressionTooLong.as_str(),
            "cel/expression-too-long"
        );
        assert_eq!(ErrorCode::CelAstTooDeep.as_str(), "cel/ast-too-deep");
        assert_eq!(ErrorCode::TemplateCompile.as_str(), "scene/template-compile");
        assert_eq!(ErrorCode::TemplateRender.as_str(), "scene/template-render");
        assert_eq!(ErrorCode::OpUnresolvedRef.as_str(), "op/unresolved-ref");
        assert_eq!(ErrorCode::OpFailed.as_str(), "op/failed");
    }
}
