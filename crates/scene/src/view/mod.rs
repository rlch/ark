//! View registry (R6) — T-026 / T-027 / T-031.
//!
//! A **view** is what fills a pane (see `cavekit-scene.md` R6). Views live
//! in a flat namespace across four tiers:
//!
//! 1. **Primitives** — kernel-builtin (`command`, `shell`, `edit`).
//! 2. **Shipped** — ark-bundled compiled-in extensions (`diff`, `status`,
//!    `picker`).
//! 3. **User** — `ark ext add`-installed extensions (`nvim`, `glow`, …).
//! 4. **Project** — project-local extensions.
//!
//! Resolution is **first-match-wins** in registration order. The canonical
//! caller populates the registry primitives-first, then shipped, then user,
//! then project; this way primitives always shadow user-installed views
//! with the same name. Callers that need a different precedence control it
//! by reordering the registration calls.
//!
//! # Config schema reflection (T-027)
//!
//! [`ViewMeta::config_schema`] carries an optional `&'static facet::Shape`
//! pointer so downstream passes can reflect a view's config struct at
//! compile time — e.g. `ark scene check` walks the schema to validate
//! the KDL pane body against the declared field set, and the LSP
//! surfaces per-field doc-comments on hover. A `None` means the view
//! takes no config (the `shell` primitive today). Shapes are `&'static`
//! because facet derives them into const pointers, so no lazy init is
//! required — call sites just write `<ConfigStruct as Facet>::SHAPE`.

use crate::suggest::suggest;
use facet::{Facet, Shape};

pub mod primitives;

// Re-export the primitive name constants at the module root for
// ergonomic callsites (`view::COMMAND`, `view::SHELL`, `view::EDIT`).
pub use primitives::{COMMAND, EDIT, SHELL, register_primitives};

// ---------------------------------------------------------------------------
// Source tier (R6 — primitive / shipped / user / project)
// ---------------------------------------------------------------------------

/// Which of R6's four view tiers a view came from.
///
/// Determines shadowing precedence when multiple tiers register the same
/// view name — the earliest-registered wins, so callers that register
/// primitives first and project-local last mirror R6's resolution order
/// naturally.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum ViewSource {
    /// Compiled-in kernel primitive — `command`, `shell`, `edit`.
    Primitive,
    /// Ark-shipped compiled-in extension — `diff`, `status`, `picker`.
    Shipped,
    /// User-installed extension via `ark ext add` — `nvim`, `glow`, …
    User,
    /// Project-local extension.
    Project,
}

// ---------------------------------------------------------------------------
// Render mode (R6 — command / zellij / data-only)
// ---------------------------------------------------------------------------

/// The runtime render mode a view binds to.
///
/// Inferred from the extension's trait impl in R17: `impl CommandView` →
/// `CommandView`, `impl ZellijView` → `ZellijView`. Headless extensions
/// (no visible pane) use `DataOnly`. The compiler uses this field to
/// reject spawn-target mismatches (e.g. a pane declaring a DataOnly view
/// is illegal).
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum RenderMode {
    /// Pane runs a subprocess command (zellij native subprocess).
    CommandView,
    /// Pane loads a zellij wasm plugin.
    ZellijView,
    /// Headless extension — no pane rendered (e.g. event emitters,
    /// background services).
    DataOnly,
}

// ---------------------------------------------------------------------------
// View metadata (T-027)
// ---------------------------------------------------------------------------

/// Metadata entry for a single registered view.
///
/// One record per `(tier, view-name)` pair. Not stored behind a hashmap
/// because the flat list preserves registration order (which drives
/// shadowing) and `n` is small — O(n) linear scan is fine.
#[derive(Facet, Debug, Clone)]
pub struct ViewMeta {
    /// View name as written in scene source (`command`, `shell`, `nvim`,
    /// …). Matched case-sensitively — scene grammar is case-sensitive.
    pub name: String,

    /// Which tier the view came from. Surfaces in `ark ext info` and
    /// drives the "Available views" section of diagnostic help text.
    pub source: ViewSource,

    /// Runtime render mode — drives pane rendering + handle-type
    /// inference (R17: `CommandView` → `CommandPane`, `ZellijView` →
    /// `PluginPane`).
    pub render_mode: RenderMode,

    /// Facet `SHAPE` pointer for the view's config struct, or `None`
    /// when the view takes no config (T-027).
    ///
    /// Populated via `<ConfigStruct as facet::Facet>::SHAPE` at register
    /// time. Consumers:
    ///
    /// * The scene compiler walks the shape's fields to validate the
    ///   KDL pane body against the declared field set and emit
    ///   `error[ext/bad-config]` with field-level `did you mean?`
    ///   suggestions on typos.
    /// * `ark ext info` renders the field list + Rust doc-comments.
    /// * The LSP reports per-field hover docs.
    ///
    /// The field is `#[facet(opaque)]` because `&'static Shape` is not
    /// itself a `Facet` type — the derive's opaque marker tells the
    /// scene-schema generator ([`crate::bin::gen_scene_schema`]) to
    /// ignore this slot rather than recurse into facet's internal
    /// reflection types.
    #[facet(opaque, default)]
    pub config_schema: Option<&'static Shape>,
}

// ---------------------------------------------------------------------------
// Registry (T-026)
// ---------------------------------------------------------------------------

/// Flat-namespace registry of all known views.
///
/// Resolution is **first-match wins** in registration order — so callers
/// register primitives first, then shipped extensions, then user, then
/// project. Primitives thus shadow extension views of the same name,
/// which matches R6's "three tiers, same namespace" guarantee.
///
/// The registry is deliberately a flat `Vec` rather than a `HashMap` so
/// `n` is small, registration order is preserved (drives
/// `all_names()` stability), and iteration beats hashing at typical
/// `|views| < 50`.
#[derive(Debug, Clone, Default)]
pub struct ViewRegistry {
    /// Registered view metadata in insertion order.
    entries: Vec<ViewMeta>,
}

impl ViewRegistry {
    /// Construct an empty registry with no views registered.
    ///
    /// Use [`ViewRegistry::with_primitives`] in most call sites — the
    /// `new()` entry point is for bespoke bootstrap scenarios
    /// (e.g. tests that want to probe the primitive resolution path
    /// independently).
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Construct a registry pre-populated with the three primitive views
    /// (`command`, `shell`, `edit`) in canonical registration order.
    ///
    /// Downstream call sites layer on shipped / user / project extensions
    /// by calling [`ViewRegistry::register`] after this constructor —
    /// primitives are registered first so they shadow any same-named
    /// extension view.
    pub fn with_primitives() -> Self {
        let mut reg = Self::new();
        register_primitives(&mut reg);
        reg
    }

    /// Append a view entry to the registry.
    ///
    /// Duplicate names are **not** rejected here — the registry is a flat
    /// list and resolution is first-match-wins, so later duplicates are
    /// simply shadowed. The policy decision of "should we warn on
    /// duplicate registration?" lives with the caller (typically the
    /// extension loader in T-096+).
    pub fn register(&mut self, meta: ViewMeta) {
        self.entries.push(meta);
    }

    /// Look up a view by name. Returns the first matching entry.
    ///
    /// Matches the R6 shadowing rule: because primitives are registered
    /// first, a user-installed `nvim` extension shadows nothing but a
    /// hypothetical user-installed `command` extension would be shadowed
    /// by the primitive.
    pub fn resolve(&self, name: &str) -> Option<&ViewMeta> {
        self.entries.iter().find(|m| m.name == name)
    }

    /// List every registered view name in insertion order.
    ///
    /// Used by [`resolve_or_suggest`] to drive "did you mean …?" help
    /// text and by `ark ext list` to populate the views table.
    pub fn all_names(&self) -> Vec<&str> {
        self.entries.iter().map(|m| m.name.as_str()).collect()
    }

    /// Read-only access to every registered entry in insertion order.
    ///
    /// Consumed by `ark ext list` / `ark ext info` for enumeration with
    /// full tier + render-mode context.
    pub fn all(&self) -> &[ViewMeta] {
        &self.entries
    }
}

// ---------------------------------------------------------------------------
// Lookup-with-suggestions helper (T-031)
// ---------------------------------------------------------------------------

/// Resolve a view name, returning either the metadata or a ranked list of
/// typo-candidate suggestions.
///
/// On miss, returns up to 3 candidates via Jaro-Winkler similarity with
/// threshold 0.75 — the same tuning used by op / node / extension
/// suggesters across the crate for consistent UX. The caller is
/// responsible for converting the `Err(Vec<String>)` arm into a
/// [`crate::error::SceneError::UnknownView`] with formatted help text
/// (the raw suggestion list keeps this helper span-agnostic so it can be
/// reused from both the compile pass and the `ark ext info` CLI path).
#[allow(clippy::result_large_err)]
pub fn resolve_or_suggest<'r>(
    registry: &'r ViewRegistry,
    name: &str,
) -> Result<&'r ViewMeta, Vec<String>> {
    if let Some(m) = registry.resolve(name) {
        Ok(m)
    } else {
        let candidates = registry.all_names();
        Err(suggest(name, &candidates, 0.75, 3))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, source: ViewSource, render_mode: RenderMode) -> ViewMeta {
        ViewMeta {
            name: name.to_string(),
            source,
            render_mode,
            config_schema: None,
        }
    }

    #[test]
    fn registry_resolves_registered() {
        let mut reg = ViewRegistry::new();
        reg.register(sample("nvim", ViewSource::User, RenderMode::CommandView));
        let meta = reg.resolve("nvim").expect("registered view should resolve");
        assert_eq!(meta.name, "nvim");
        assert_eq!(meta.source, ViewSource::User);
        assert_eq!(meta.render_mode, RenderMode::CommandView);
    }

    #[test]
    fn registry_miss_on_unknown() {
        let reg = ViewRegistry::new();
        assert!(reg.resolve("nope").is_none());
    }

    #[test]
    fn with_primitives_includes_command_shell_edit() {
        let reg = ViewRegistry::with_primitives();
        assert!(reg.resolve(COMMAND).is_some());
        assert!(reg.resolve(SHELL).is_some());
        assert!(reg.resolve(EDIT).is_some());
    }

    #[test]
    fn all_names_returns_sorted_or_insertion_order() {
        // Insertion order preserved: primitives register in command,
        // shell, edit order per `register_primitives`.
        let reg = ViewRegistry::with_primitives();
        let names = reg.all_names();
        assert_eq!(names, vec![COMMAND, SHELL, EDIT]);
    }

    #[test]
    fn earlier_registered_shadows_later() {
        // R6 resolution: primitives win against same-named extension view.
        let mut reg = ViewRegistry::new();
        reg.register(sample(
            "command",
            ViewSource::Primitive,
            RenderMode::CommandView,
        ));
        reg.register(sample("command", ViewSource::User, RenderMode::ZellijView));
        let m = reg.resolve("command").unwrap();
        assert_eq!(m.source, ViewSource::Primitive);
    }

    #[test]
    fn resolve_or_suggest_returns_suggestions() {
        let reg = ViewRegistry::with_primitives();
        // `commnd` (missing `a`) should suggest `command`.
        match resolve_or_suggest(&reg, "commnd") {
            Ok(_) => panic!("typo should not resolve"),
            Err(suggestions) => {
                assert!(
                    suggestions.iter().any(|s| s == "command"),
                    "expected `command` in suggestions, got {suggestions:?}"
                );
            }
        }
    }

    #[test]
    fn resolve_or_suggest_returns_match_when_present() {
        let reg = ViewRegistry::with_primitives();
        let meta = resolve_or_suggest(&reg, COMMAND).expect("should resolve");
        assert_eq!(meta.name, COMMAND);
    }

    #[test]
    fn resolve_or_suggest_empty_suggestions_when_far() {
        let reg = ViewRegistry::with_primitives();
        // Totally unrelated string below the 0.75 threshold.
        match resolve_or_suggest(&reg, "xxxxxxxxxxxxx") {
            Ok(_) => panic!("should miss"),
            Err(suggestions) => {
                assert!(
                    suggestions.is_empty(),
                    "far miss should yield no suggestions"
                );
            }
        }
    }
}
