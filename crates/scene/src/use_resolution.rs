//! `use "<name>"` resolution + inspection (cavekit-scene R10, T-10.4).
//!
//! Given a [`UseNode`] from a parsed scene and a [`UseResolveCtx`]
//! describing the host environment, [`resolve_use`]:
//!
//! 1. Walks the R10 search path (project-local → user → system →
//!    built-in) via
//!    [`ark_ext_metadata::search_path::resolve_extension_path`] (T-10.3).
//! 2. Loads the extension's manifest:
//!    - For an on-disk extension, reads `<root>/extension.kdl` (the
//!      canonical text form) and parses via facet-kdl.
//!    - For a wasm cartridge sitting in the extension dir
//!      (`<root>/<name>.wasm`, optional), reads the `ark.metadata`
//!      custom section via [`crate::wasm_meta::read_extension_metadata`]
//!      (T-10.2).
//!    - Built-in extensions fail with [`SceneError::Grammar`] for now —
//!      compiled-in metadata lookup is a separate code path the
//!      scene compiler will wire up when the built-in registry lands.
//! 3. Validates the manifest's `ark-range` / `zellij-range` against
//!    the host versions in `ctx`. Mismatch → [`SceneError::ExtVersionMismatch`]
//!    (code `ext/version-mismatch`).
//! 4. Splits intents/events into namespaced [`NamespacedIntent`] /
//!    [`NamespacedEvent`] entries (`<ext>.<name>` form, deduplicated
//!    against the metadata's already-namespaced declarations).
//! 5. If `<root>/scene.kdl` exists, parses it via
//!    [`crate::parse::parse_scene`] and returns the `SceneIR` as the
//!    sidecar fragment.
//! 6. Returns the user's `config { }` block (if any) untouched — full
//!    schema validation lives in T-10.5 ([`crate::use_config`]).
//!
//! # Repeated `use` (config override)
//!
//! Per the spec: a scene may write `use "ext"` more than once. Each
//! repetition contributes a `config { }` override; per-key last-wins
//! merge yields the effective config. Side-effects (intent / event
//! registration, sidecar load) run ONCE per name, off the FIRST
//! occurrence — repeated entries only contribute config overrides.
//!
//! [`merge_repeated_uses`] takes the full `Vec<UseNode>` from a
//! parsed scene and returns one [`MergedUse`] per distinct name with
//! the merged config block reconstructed.

use std::path::{Path, PathBuf};

use ark_ext_metadata::search_path::{ExtensionPath, resolve_extension_path};
use ark_ext_metadata_types::{EventDecl, ExtensionMetadata, IntentDecl};
use semver::{Version, VersionReq};

use crate::ast::{OpaqueBlock, UseNode};
use crate::error::SceneError;
use crate::parse::{SceneIR, parse_scene};
use crate::wasm_meta::read_extension_metadata;

/// Context describing the host environment for `use` resolution.
///
/// Pure-function input — callers (CLI, scene compiler) inject the real
/// environment; tests pass tempdirs / synthetic versions.
#[derive(Debug, Clone)]
pub struct UseResolveCtx {
    /// Session CWD. Project-local search rooted at
    /// `<cwd>/.ark/extensions/<name>/`.
    pub cwd: PathBuf,

    /// `${XDG_DATA_HOME}` if set. `None` skips the user-installed
    /// tier; the resolver does NOT guess `$HOME/.local/share`.
    pub xdg_data_home: Option<PathBuf>,

    /// System directories searched in order. Typically a single
    /// `/usr/share/ark/extensions/`; callers can add XDG `system_data_dirs`
    /// or homebrew prefixes.
    pub system_dirs: Vec<PathBuf>,

    /// Names of built-in extensions compiled into the host binary.
    /// Used as the fourth-tier fallback in
    /// [`resolve_extension_path`].
    pub builtin: Vec<&'static str>,

    /// Host's running ark version, semver-parsed against
    /// `ExtensionMetadata::ark_range`.
    pub host_ark_version: Version,

    /// Host's bundled zellij version, semver-parsed against
    /// `ExtensionMetadata::zellij_range`. `None` skips the zellij
    /// range check (useful in tests / non-zellij hosts).
    pub host_zellij_version: Option<Version>,
}

impl UseResolveCtx {
    /// Convenience constructor for a host with empty system dirs +
    /// no built-ins, useful in tests.
    pub fn new(
        cwd: impl Into<PathBuf>,
        host_ark_version: Version,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            xdg_data_home: None,
            system_dirs: Vec::new(),
            builtin: Vec::new(),
            host_ark_version,
            host_zellij_version: None,
        }
    }
}

/// Namespaced intent declaration — what gets registered in the scene
/// compiler's symbol table after `use "<ext>"`.
///
/// `IntentDecl::name` may already be in `<ext>.<intent>` form when the
/// extension author followed the convention; this struct keeps the
/// extension name explicit so the symbol table can use a sorted
/// `BTreeMap<String, IntentDecl>` keyed by the canonical
/// `"<ext>.<intent>"` string regardless.
#[derive(Debug, Clone)]
pub struct NamespacedIntent {
    /// Owning extension name (the `use "<name>"` argument).
    pub ext: String,
    /// Fully-qualified intent name (`<ext>.<intent>`). Auto-prefixed
    /// when the manifest's name is unprefixed (R11 merger rule).
    pub qualified: String,
    /// Original intent declaration from the manifest, retained so
    /// `dispatch` callers can read the `args-schema` without round-trip.
    pub decl: IntentDecl,
}

/// Namespaced event declaration. Same shape as [`NamespacedIntent`]
/// for consistency.
#[derive(Debug, Clone)]
pub struct NamespacedEvent {
    /// Owning extension name.
    pub ext: String,
    /// Fully-qualified event name (`<ext>.<event>`).
    pub qualified: String,
    /// Original event declaration from the manifest.
    pub decl: EventDecl,
}

/// Result of resolving a single `use "<name>"` decl.
///
/// Returned in canonical first-occurrence order. The scene compiler
/// then folds the symbol-table entries into its global registry and
/// queues the sidecar `SceneIR` for fragment merge (R11).
#[derive(Debug)]
pub struct ResolvedUse {
    /// Name as written in `use "<name>"`.
    pub name: String,
    /// Loaded manifest.
    pub metadata: ExtensionMetadata,
    /// Path the extension was resolved at (`File(path)` for on-disk;
    /// `BuiltIn(name)` mirrored as `None` here for now).
    pub root_path: Option<PathBuf>,
    /// Optional sidecar `scene.kdl` parsed via T-1.1.
    pub sidecar_scene: Option<SceneIR>,
    /// Namespaced intents the manifest contributes.
    pub intents: Vec<NamespacedIntent>,
    /// Namespaced events the manifest contributes.
    pub events: Vec<NamespacedEvent>,
    /// User-supplied `config { }` block from the FIRST `use "<name>"`
    /// site, reconciled with later overrides via [`merge_repeated_uses`].
    /// Untyped at this layer; T-10.5 validates against
    /// `metadata.config`.
    pub config_block: Option<OpaqueBlock>,
}

/// Resolve a single `use "<name>"` declaration.
///
/// Side-effect-free: it reads files, decodes wasm, parses the sidecar
/// scene, and validates ranges. The caller folds the result into a
/// scene-wide symbol table (typically a `BTreeMap<String, IntentDecl>`)
/// and the cross-file fragment list.
///
/// Errors:
///
/// * [`SceneError::Grammar`] — extension not found, or the resolved
///   path is a built-in (built-in metadata lookup is a separate path).
///   The miette `code` for the not-found case is reused as `scene/grammar`
///   — cavekit-scene.md does not enumerate a dedicated `ext/not-found`
///   code in R12 and `Grammar` carries enough surface for v0.1.
/// * [`SceneError::WasmMetaMissing`] / [`SceneError::WasmMetaInvalid`]
///   — wasm cartridge cannot be decoded (T-10.2).
/// * [`SceneError::ExtVersionMismatch`] — host does not satisfy
///   `ark-range` or `zellij-range`.
#[allow(clippy::result_large_err)] // SceneError carries diagnostic surface.
pub fn resolve_use(
    decl: &UseNode,
    ctx: &UseResolveCtx,
) -> Result<ResolvedUse, SceneError> {
    // 1. Path resolution via T-10.3.
    let system_refs: Vec<&Path> = ctx.system_dirs.iter().map(|p| p.as_path()).collect();
    let resolved = resolve_extension_path(
        &decl.name,
        &ctx.cwd,
        ctx.xdg_data_home.as_deref(),
        &system_refs,
        &ctx.builtin,
    )
    .ok_or_else(|| extension_not_found(&decl.name))?;

    let (metadata, root_path, sidecar_scene) = match resolved {
        ExtensionPath::File(root) => {
            let meta = load_metadata_from_dir(&decl.name, &root)?;
            let sidecar = load_sidecar_scene(&root)?;
            (meta, Some(root), sidecar)
        }
        ExtensionPath::BuiltIn(_) => {
            // Built-in resolution is a separate code path (compiled-in
            // metadata via `register_extension!`). Until that wiring
            // lands, surface a clear error so callers don't silently
            // skip the built-in tier.
            return Err(SceneError::Grammar {
                message: format!(
                    "extension `{}` resolved to a built-in; built-in metadata loading is not yet wired into scene",
                    decl.name
                ),
                src: miette::NamedSource::new(
                    "<scene compile>".to_string(),
                    String::new(),
                ),
                at: (0, 0).into(),
            });
        }
    };

    // 2. Version-range validation.
    check_version_range(
        &decl.name,
        "ark",
        &metadata.ark_range.value,
        &ctx.host_ark_version,
    )?;
    if let Some(host_zellij) = &ctx.host_zellij_version {
        check_version_range(
            &decl.name,
            "zellij",
            &metadata.zellij_range.value,
            host_zellij,
        )?;
    }

    // 3. Namespace intents + events.
    let intents = metadata
        .intents
        .iter()
        .cloned()
        .map(|d| NamespacedIntent {
            ext: decl.name.clone(),
            qualified: qualify_name(&decl.name, &d.name),
            decl: d,
        })
        .collect();
    let events = metadata
        .events
        .iter()
        .cloned()
        .map(|d| NamespacedEvent {
            ext: decl.name.clone(),
            qualified: qualify_name(&decl.name, &d.name),
            decl: d,
        })
        .collect();

    Ok(ResolvedUse {
        name: decl.name.clone(),
        metadata,
        root_path,
        sidecar_scene,
        intents,
        events,
        config_block: clone_opaque_block(decl.config.as_ref()),
    })
}

/// Result of folding repeated `use "<name>"` blocks into one effective
/// resolution per name.
///
/// Side-effects (manifest load, sidecar parse, intent/event
/// registration) come from the FIRST occurrence; later occurrences
/// only contribute config-block overrides.
#[derive(Debug)]
pub struct MergedUse {
    /// Inner resolution (from the first occurrence).
    pub resolved: ResolvedUse,
    /// Number of `use "<name>"` sites that contributed (1+).
    pub occurrences: usize,
}

/// Resolve every `use "<name>"` in a scene's `uses` list and merge
/// repeated occurrences per the R10 last-wins config rule.
///
/// Returns one [`MergedUse`] per distinct name in stable
/// first-occurrence order. The merged config block — accessible via
/// `merged.resolved.config_block` — has positional args from the LAST
/// site that supplied any (a v0.1 simplification over per-key
/// last-wins; v0.1 `OpaqueBlock` only stores positional args, with
/// child-key merging deferred to T-10.5 once schema-typed config
/// arrives — see TODO below).
///
/// Errors short-circuit on the first unresolved use, surfacing the
/// underlying [`resolve_use`] error.
//
// TODO(T-10.5): once `UseNode::config` carries typed key/value pairs
// (or facet-kdl exposes a structured `KdlNode` view), implement the
// per-key last-wins merge specified by R10. Today's behaviour is
// "last writer wins on the WHOLE block" because OpaqueBlock holds
// nothing finer-grained than positional args.
#[allow(clippy::result_large_err)]
pub fn merge_repeated_uses(
    uses: &[UseNode],
    ctx: &UseResolveCtx,
) -> Result<Vec<MergedUse>, SceneError> {
    // Group by name preserving first-occurrence order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, Vec<&UseNode>> =
        std::collections::BTreeMap::new();
    for u in uses {
        if !groups.contains_key(&u.name) {
            order.push(u.name.clone());
        }
        groups.entry(u.name.clone()).or_default().push(u);
    }

    let mut out = Vec::with_capacity(order.len());
    for name in order {
        let group = &groups[&name];
        // Side-effects from the first occurrence.
        let mut resolved = resolve_use(group[0], ctx)?;
        // Last writer wins on the config block. Walk the group; the
        // last element with `Some(config)` overrides earlier ones.
        // (Group has at least 1 element; the first occurrence's config
        // is already on `resolved`.)
        for later in &group[1..] {
            if let Some(cfg) = &later.config {
                resolved.config_block = Some(clone_opaque_block(Some(cfg)).unwrap());
            }
        }
        out.push(MergedUse {
            resolved,
            occurrences: group.len(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Clone an `OpaqueBlock` reference into an owned `Option<OpaqueBlock>`.
///
/// `OpaqueBlock` is not `Clone` upstream (yet) — it derives only
/// `Default + Debug`. This shallow clone copies the positional-args
/// vec so callers can take ownership without bumping the AST's
/// derive set.
fn clone_opaque_block(b: Option<&OpaqueBlock>) -> Option<OpaqueBlock> {
    b.map(|x| OpaqueBlock {
        args: x.args.clone(),
    })
}

/// Build the `<ext>.<intent>` qualified name. If the input already
/// starts with `<ext>.`, returns it unchanged so manifests that
/// already namespace their decls don't double-prefix.
fn qualify_name(ext: &str, raw: &str) -> String {
    let prefix = format!("{ext}.");
    if raw.starts_with(&prefix) {
        raw.to_string()
    } else {
        format!("{prefix}{raw}")
    }
}

/// Build the canonical "extension not found" error.
///
/// Falls under [`SceneError::Grammar`] with a clear message
/// (`scene/grammar` code) until R12 grows a dedicated `ext/not-found`
/// code; the grammar code is appropriate because the error is
/// detected during the structural validation pass.
fn extension_not_found(name: &str) -> SceneError {
    SceneError::Grammar {
        message: format!(
            "`use \"{name}\"` could not resolve extension `{name}` on any search-path tier"
        ),
        src: miette::NamedSource::new(
            "<scene compile>".to_string(),
            String::new(),
        ),
        at: (0, 0).into(),
    }
}

/// Load `<root>/extension.kdl` and decode into `ExtensionMetadata`.
///
/// On-disk text manifests use the same KDL shape as the wasm-embedded
/// custom section (R10), so we route through the
/// [`ark_ext_metadata_types::ExtensionManifest`] wrapper for symmetry
/// with [`crate::wasm_meta::read_extension_metadata`].
fn load_metadata_from_dir(
    name: &str,
    root: &Path,
) -> Result<ExtensionMetadata, SceneError> {
    let manifest_path = root.join("extension.kdl");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        SceneError::WasmMetaInvalid {
            path: manifest_path.display().to_string(),
            message: format!("read failed: {e}"),
        }
    })?;
    let manifest: ark_ext_metadata_types::ExtensionManifest =
        facet_kdl::from_str(&text).map_err(|e| SceneError::WasmMetaInvalid {
            path: manifest_path.display().to_string(),
            message: format!("KDL decode failed for `{name}`: {e}"),
        })?;
    Ok(manifest.extension)
}

/// Optionally load `<root>/scene.kdl` and parse via T-1.1.
///
/// Missing file is NOT an error — most extensions ship pure intent
/// adapters with no scene fragment.
fn load_sidecar_scene(root: &Path) -> Result<Option<SceneIR>, SceneError> {
    let path = root.join("scene.kdl");
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| SceneError::Grammar {
        message: format!("sidecar scene `{}` read failed: {e}", path.display()),
        src: miette::NamedSource::new(path.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    let ir = parse_scene(&text, &path)?;
    Ok(Some(ir))
}

/// Validate the host version against a semver range string.
///
/// Empty range string = "no constraint" (per R10's
/// `zellij-range = ""` convention for extensions that are
/// zellij-agnostic). A non-empty range that fails to parse, or
/// parses but does not admit `actual`, surfaces as
/// [`SceneError::ExtVersionMismatch`].
fn check_version_range(
    ext: &str,
    component: &'static str,
    range: &str,
    actual: &Version,
) -> Result<(), SceneError> {
    if range.trim().is_empty() {
        return Ok(());
    }
    let req = VersionReq::parse(range).map_err(|e| SceneError::ExtVersionMismatch {
        ext: ext.to_string(),
        component,
        required: format!("{range} (parse error: {e})"),
        actual: actual.to_string(),
    })?;
    if !req.matches(actual) {
        return Err(SceneError::ExtVersionMismatch {
            ext: ext.to_string(),
            component,
            required: range.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::fs;
    use tempfile::TempDir;

    /// Build a synthetic extension dir under
    /// `<cwd>/.ark/extensions/<name>/` with the supplied manifest text
    /// and (optional) sidecar scene text. Returns the cwd tempdir.
    fn make_project_extension(
        name: &str,
        manifest: &str,
        sidecar: Option<&str>,
    ) -> TempDir {
        let cwd = TempDir::new().unwrap();
        let dir = cwd.path().join(".ark/extensions").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extension.kdl"), manifest).unwrap();
        if let Some(s) = sidecar {
            fs::write(dir.join("scene.kdl"), s).unwrap();
        }
        cwd
    }

    /// Canonical sample manifest (text form). Mirrors
    /// `wasm_meta::tests::sample_kdl` but with `intents` + `events`
    /// to exercise namespacing.
    fn sample_manifest_with_intents() -> &'static str {
        r#"
extension {
    name "demo"
    version "0.1.0"
    ark-range ">=0.1, <0.2"
    zellij-range ""
    intent "demo.hello" {
        args-schema "{}"
    }
    intent "world" {
        args-schema "{}"
    }
    event "demo.greeted" {
        payload-schema "{}"
    }
    config { }
}
"#
    }

    fn ctx_for(cwd: &TempDir) -> UseResolveCtx {
        UseResolveCtx::new(cwd.path(), Version::parse("0.1.5").unwrap())
    }

    fn use_decl(name: &str) -> UseNode {
        UseNode {
            name: name.to_string(),
            config: None,
        }
    }

    #[test]
    fn resolves_single_use_with_intent_namespacing() {
        let cwd =
            make_project_extension("demo", sample_manifest_with_intents(), None);
        let ctx = ctx_for(&cwd);
        let decl = use_decl("demo");

        let r = resolve_use(&decl, &ctx).expect("resolves");
        assert_eq!(r.name, "demo");
        assert_eq!(r.metadata.name.value, "demo");
        assert!(r.sidecar_scene.is_none());
        // Two intents — one already-prefixed, one not.
        assert_eq!(r.intents.len(), 2);
        let qualified: Vec<&str> =
            r.intents.iter().map(|i| i.qualified.as_str()).collect();
        assert!(qualified.contains(&"demo.hello"));
        assert!(qualified.contains(&"demo.world"));
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.events[0].qualified, "demo.greeted");
    }

    #[test]
    fn loads_sidecar_scene_when_present() {
        let sidecar = r#"scene "demo-sidecar" { }"#;
        let cwd = make_project_extension(
            "demo",
            sample_manifest_with_intents(),
            Some(sidecar),
        );
        let ctx = ctx_for(&cwd);

        let r = resolve_use(&use_decl("demo"), &ctx).expect("resolves");
        let sc = r.sidecar_scene.expect("sidecar present");
        assert_eq!(sc.scene.name, "demo-sidecar");
    }

    #[test]
    fn missing_extension_errors_with_grammar_code() {
        let cwd = TempDir::new().unwrap();
        let ctx = UseResolveCtx::new(
            cwd.path(),
            Version::parse("0.1.0").unwrap(),
        );
        let err = resolve_use(&use_decl("nope"), &ctx).expect_err("missing must error");
        assert_eq!(err.code_enum(), ErrorCode::Grammar);
    }

    #[test]
    fn version_mismatch_surfaces_dedicated_code() {
        let cwd =
            make_project_extension("demo", sample_manifest_with_intents(), None);
        // ark-range is `>=0.1, <0.2`; pick a host outside that.
        let mut ctx = ctx_for(&cwd);
        ctx.host_ark_version = Version::parse("0.2.0").unwrap();
        let err = resolve_use(&use_decl("demo"), &ctx)
            .expect_err("host outside range must error");
        assert_eq!(err.code_enum(), ErrorCode::ExtVersionMismatch);
        match err {
            SceneError::ExtVersionMismatch {
                ext,
                component,
                required,
                actual,
            } => {
                assert_eq!(ext, "demo");
                assert_eq!(component, "ark");
                assert!(required.contains(">=0.1"));
                assert_eq!(actual, "0.2.0");
            }
            other => panic!("expected ExtVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn empty_range_skips_check() {
        let manifest = r#"
extension {
    name "demo"
    version "0.1.0"
    ark-range ""
    zellij-range ""
    config { }
}
"#;
        let cwd = make_project_extension("demo", manifest, None);
        let ctx = ctx_for(&cwd);
        // Any host version admits an empty range.
        let r = resolve_use(&use_decl("demo"), &ctx).expect("empty range admits any host");
        assert_eq!(r.metadata.ark_range.value, "");
    }

    #[test]
    fn zellij_range_checked_when_host_version_supplied() {
        let manifest = r#"
extension {
    name "demo"
    version "0.1.0"
    ark-range ""
    zellij-range ">=0.40"
    config { }
}
"#;
        let cwd = make_project_extension("demo", manifest, None);
        let mut ctx = ctx_for(&cwd);
        ctx.host_zellij_version = Some(Version::parse("0.39.0").unwrap());
        let err = resolve_use(&use_decl("demo"), &ctx)
            .expect_err("zellij below range must error");
        match err {
            SceneError::ExtVersionMismatch { component, .. } => {
                assert_eq!(component, "zellij");
            }
            other => panic!("expected ExtVersionMismatch(zellij), got {other:?}"),
        }
    }

    #[test]
    fn zellij_range_skipped_when_host_version_missing() {
        let manifest = r#"
extension {
    name "demo"
    version "0.1.0"
    ark-range ""
    zellij-range ">=99.0"
    config { }
}
"#;
        let cwd = make_project_extension("demo", manifest, None);
        let ctx = ctx_for(&cwd); // host_zellij_version = None
        let r = resolve_use(&use_decl("demo"), &ctx)
            .expect("zellij check skipped when host has no zellij version");
        assert_eq!(r.metadata.zellij_range.value, ">=99.0");
    }

    #[test]
    fn merge_repeated_uses_dedupes_side_effects() {
        let cwd =
            make_project_extension("demo", sample_manifest_with_intents(), None);
        let ctx = ctx_for(&cwd);
        // Three `use "demo"` sites.
        let uses = vec![use_decl("demo"), use_decl("demo"), use_decl("demo")];
        let merged = merge_repeated_uses(&uses, &ctx).expect("merges");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].occurrences, 3);
        // Intents loaded ONCE (from the first occurrence).
        assert_eq!(merged[0].resolved.intents.len(), 2);
    }

    #[test]
    fn merge_repeated_uses_preserves_order_across_distinct_names() {
        // Two extensions, in order: alpha, beta. Repeats interleaved.
        let cwd = TempDir::new().unwrap();
        for name in ["alpha", "beta"] {
            let dir = cwd.path().join(".ark/extensions").join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("extension.kdl"),
                format!(
                    r#"
extension {{
    name "{name}"
    version "0.1.0"
    ark-range ""
    zellij-range ""
    config {{ }}
}}
"#
                ),
            )
            .unwrap();
        }
        let ctx = UseResolveCtx::new(
            cwd.path(),
            Version::parse("0.1.0").unwrap(),
        );

        let uses = vec![
            use_decl("alpha"),
            use_decl("beta"),
            use_decl("alpha"),
            use_decl("beta"),
        ];
        let merged = merge_repeated_uses(&uses, &ctx).expect("merges");
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].resolved.name, "alpha");
        assert_eq!(merged[1].resolved.name, "beta");
        assert_eq!(merged[0].occurrences, 2);
        assert_eq!(merged[1].occurrences, 2);
    }

    #[test]
    fn merge_repeated_uses_last_writer_wins_on_config_block() {
        let cwd =
            make_project_extension("demo", sample_manifest_with_intents(), None);
        let ctx = ctx_for(&cwd);
        // First site: no config. Second: positional args ["a"]. Third:
        // positional args ["b"]. Last-writer-wins → final args = ["b"].
        let uses = vec![
            use_decl("demo"),
            UseNode {
                name: "demo".to_string(),
                config: Some(OpaqueBlock {
                    args: vec!["a".to_string()],
                }),
            },
            UseNode {
                name: "demo".to_string(),
                config: Some(OpaqueBlock {
                    args: vec!["b".to_string()],
                }),
            },
        ];
        let merged = merge_repeated_uses(&uses, &ctx).expect("merges");
        let cfg = merged[0]
            .resolved
            .config_block
            .as_ref()
            .expect("config carried");
        assert_eq!(cfg.args, vec!["b".to_string()]);
    }
}
