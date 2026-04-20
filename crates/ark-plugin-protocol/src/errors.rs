//! Stable error-code enum for the plugin host.
//!
//! T-PP-008 (cavekit-plugin-protocol R3, R5, R6, R8, R9, R12, R14):
//! every diagnostic the plugin host emits has a stable code prefix —
//! `error[plugin/<sub>]`, `error[abi/<sub>]`, `error[ark-kdl/<sub>]` —
//! so end-users can grep, scripts can match, and kit acceptance
//! criteria have a single source of truth.
//!
//! These codes are part of the public interface of ark and follow the
//! same compatibility rules as WIT: adding a variant is a MINOR bump
//! (enum is `#[non_exhaustive]`), renaming or removing is a MAJOR
//! bump.
//!
//! The error types wrap rich structured context (plugin name, file
//! path, missing caps, …) so callers can render either the stable
//! code-prefixed one-liner (Display) or a full diagnostic.

use std::path::PathBuf;

use ark_types::AbiError;

/// Every failure mode the host raises while loading / validating a
/// plugin. Variants map 1:1 onto `error[*/*]` codes in the kit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PluginLoadError {
    // --------------------------------------------------------------
    // error[plugin/*] — runtime/manifest/capability issues
    // --------------------------------------------------------------
    /// `ark-caps:v1` custom section absent or unreadable.
    ///
    /// Kit: R3. Plugin `.wasm` missing the capability manifest; likely
    /// built without `#[derive(Plugin)]`.
    #[error("error[plugin/missing-caps]: plugin {plugin} has no ark-caps:v1 custom section")]
    MissingCaps { plugin: String },

    /// `ark-meta:v1` custom section absent or unreadable.
    ///
    /// Kit: R9. Almost always paired with `MissingCaps` in practice,
    /// but carries a distinct stable code.
    #[error("error[plugin/missing-meta]: plugin {plugin} has no ark-meta:v1 custom section")]
    MissingMeta { plugin: String },

    /// `ark-caps:v1` / `ark-meta:v1` postcard payload failed to
    /// deserialize (truncated, wrong encoding, schema drift).
    ///
    /// Kit: R3, R9.
    #[error("error[plugin/manifest-corrupt]: plugin {plugin} manifest section {section} failed to decode: {detail}")]
    ManifestCorrupt {
        plugin: String,
        section: &'static str,
        detail: String,
    },

    /// `ark-caps:v1` lists a cap the plugin does not actually import.
    ///
    /// Kit: R3 acceptance "display-metadata-only declarations that are
    /// not backed by an import are rejected". Closes the lie vector
    /// where a plugin under-declares caps to pass review.
    #[error(
        "error[plugin/cap-drift-section-extra]: plugin {plugin} declares cap {cap} in ark-caps:v1 but does not import ark:cap/{cap}"
    )]
    CapDriftSectionExtra { plugin: String, cap: String },

    /// Plugin imports `ark:cap/<cap>` but does not declare it in
    /// `ark-caps:v1`.
    ///
    /// Kit: R3 acceptance "imported caps not in the section are
    /// rejected". Closes the lie vector where a plugin hides a cap.
    #[error(
        "error[plugin/cap-drift-import-extra]: plugin {plugin} imports ark:cap/{cap} but does not declare it in ark-caps:v1"
    )]
    CapDriftImportExtra { plugin: String, cap: String },

    /// The user's `ark.kdl` grant set is not a superset of the plugin's
    /// declared caps.
    ///
    /// Kit: R5. Structured context: every missing cap is listed so the
    /// remediation block in the full diagnostic can be rendered.
    #[error(
        "error[plugin/insufficient-grants]: plugin {plugin} requires capabilities not granted in ark.kdl: {missing:?}"
    )]
    InsufficientGrants {
        plugin: String,
        missing: Vec<String>,
    },

    /// Two plugins share the same `name` in `ark-meta:v1` or the
    /// `ark.kdl` key.
    ///
    /// Kit: R9 acceptance "name is a process-global primary key".
    #[error("error[plugin/name-collision]: plugin name {name} declared by {first:?} and {second:?}")]
    NameCollision {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },

    /// The plugin's compiled WIT world name does not match the
    /// declared `name` in `ark-meta:v1`.
    ///
    /// Kit: R9 acceptance "WIT world name equals crate name".
    #[error(
        "error[plugin/world-name-mismatch]: plugin {plugin} declares name {declared} but WIT world is {world}"
    )]
    WorldNameMismatch {
        plugin: String,
        declared: String,
        world: String,
    },

    /// None of the plugin's exported views match the host's active
    /// render target.
    ///
    /// Kit: R6 acceptance "Zero matches = no-renderable-views".
    #[error(
        "error[plugin/no-renderable-views]: plugin {plugin} exports no views targeting {host_target}; declared targets: {declared_targets:?}"
    )]
    NoRenderableViews {
        plugin: String,
        host_target: String,
        declared_targets: Vec<String>,
    },

    /// A view-type export implements zero render-target marker traits.
    ///
    /// Kit: R6 acceptance "A view that implements none = view-no-target".
    #[error("error[plugin/view-no-target]: plugin {plugin} view {view} has no render-target marker")]
    ViewNoTarget { plugin: String, view: String },

    /// A view-type export implements more than one render-target
    /// marker trait.
    ///
    /// Kit: R6 acceptance "A view that implements both = view-multi-target".
    #[error(
        "error[plugin/view-multi-target]: plugin {plugin} view {view} implements multiple render-target markers"
    )]
    ViewMultiTarget { plugin: String, view: String },

    /// A widget tree returned from `render()` names a target arm that
    /// does not match the host's active target.
    ///
    /// Kit: R10 acceptance "terminal(...) on a GUI host = error".
    #[error(
        "error[plugin/widget-tree-target-mismatch]: plugin {plugin} returned a widget-tree targeting {returned} but host is {host}"
    )]
    WidgetTreeTargetMismatch {
        plugin: String,
        returned: String,
        host: String,
    },

    // --------------------------------------------------------------
    // error[ark-kdl/*] — ark.kdl plugins{} block grammar / semantics
    // --------------------------------------------------------------
    /// Two `<plugin-name>` entries under a single `plugins { }` block.
    ///
    /// Kit: R5 acceptance "Duplicate names = plugin-name-clash".
    #[error("error[ark-kdl/duplicate-plugin-name]: plugins block declares {name} twice")]
    DuplicatePluginName { name: String },

    /// A `<cap-id>` under a `capabilities { }` block is not in the
    /// closed set of ark:cap/* interface names.
    ///
    /// Kit: R5. Suggestions are Levenshtein-closest from the cap set
    /// (computed later in T-PP-037).
    #[error(
        "error[ark-kdl/unknown-capability]: unknown capability {cap} for plugin {plugin}; did you mean {suggestions:?}?"
    )]
    UnknownCapability {
        plugin: String,
        cap: String,
        suggestions: Vec<String>,
    },

    /// `location=` URL uses an unsupported scheme (v1: `file:` only).
    ///
    /// Kit: R5 + R12 acceptance "https: and oci: are post-v1".
    #[error(
        "error[ark-kdl/invalid-url-scheme]: plugin {plugin} location URL scheme {scheme} is not supported in v1"
    )]
    InvalidUrlScheme { plugin: String, scheme: String },

    /// Same kernel as `InsufficientGrants` but raised at KDL-parse time
    /// rather than load time (when the host has both sides to compare).
    ///
    /// Kit: R5.
    #[error(
        "error[ark-kdl/cap-not-granted]: plugin {plugin} requires capabilities not present in its capabilities{{}} block: {missing:?}"
    )]
    CapNotGranted {
        plugin: String,
        missing: Vec<String>,
    },

    // --------------------------------------------------------------
    // error[abi/*] — delegated to ark_types::AbiError
    // --------------------------------------------------------------
    /// Any ABI-version mismatch. Wraps `ark_types::AbiError` so the
    /// stable code prefix and structured context come along.
    ///
    /// Kit: R14.
    #[error(transparent)]
    Abi(#[from] AbiError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_display_starts_with_stable_code_prefix() {
        // Every Display must start with `error[...]:` — this is the
        // contract the kit promises to users and scripts.
        let cases: Vec<(PluginLoadError, &str)> = vec![
            (
                PluginLoadError::MissingCaps {
                    plugin: "p".into(),
                },
                "error[plugin/missing-caps]",
            ),
            (
                PluginLoadError::MissingMeta {
                    plugin: "p".into(),
                },
                "error[plugin/missing-meta]",
            ),
            (
                PluginLoadError::ManifestCorrupt {
                    plugin: "p".into(),
                    section: "ark-caps:v1",
                    detail: "trunc".into(),
                },
                "error[plugin/manifest-corrupt]",
            ),
            (
                PluginLoadError::CapDriftSectionExtra {
                    plugin: "p".into(),
                    cap: "x".into(),
                },
                "error[plugin/cap-drift-section-extra]",
            ),
            (
                PluginLoadError::CapDriftImportExtra {
                    plugin: "p".into(),
                    cap: "x".into(),
                },
                "error[plugin/cap-drift-import-extra]",
            ),
            (
                PluginLoadError::InsufficientGrants {
                    plugin: "p".into(),
                    missing: vec!["a".into()],
                },
                "error[plugin/insufficient-grants]",
            ),
            (
                PluginLoadError::NameCollision {
                    name: "n".into(),
                    first: PathBuf::from("/a"),
                    second: PathBuf::from("/b"),
                },
                "error[plugin/name-collision]",
            ),
            (
                PluginLoadError::WorldNameMismatch {
                    plugin: "p".into(),
                    declared: "a".into(),
                    world: "b".into(),
                },
                "error[plugin/world-name-mismatch]",
            ),
            (
                PluginLoadError::NoRenderableViews {
                    plugin: "p".into(),
                    host_target: "Terminal".into(),
                    declared_targets: vec!["Gui".into()],
                },
                "error[plugin/no-renderable-views]",
            ),
            (
                PluginLoadError::ViewNoTarget {
                    plugin: "p".into(),
                    view: "v".into(),
                },
                "error[plugin/view-no-target]",
            ),
            (
                PluginLoadError::ViewMultiTarget {
                    plugin: "p".into(),
                    view: "v".into(),
                },
                "error[plugin/view-multi-target]",
            ),
            (
                PluginLoadError::WidgetTreeTargetMismatch {
                    plugin: "p".into(),
                    returned: "Gui".into(),
                    host: "Terminal".into(),
                },
                "error[plugin/widget-tree-target-mismatch]",
            ),
            (
                PluginLoadError::DuplicatePluginName { name: "n".into() },
                "error[ark-kdl/duplicate-plugin-name]",
            ),
            (
                PluginLoadError::UnknownCapability {
                    plugin: "p".into(),
                    cap: "xx".into(),
                    suggestions: vec![],
                },
                "error[ark-kdl/unknown-capability]",
            ),
            (
                PluginLoadError::InvalidUrlScheme {
                    plugin: "p".into(),
                    scheme: "ftp".into(),
                },
                "error[ark-kdl/invalid-url-scheme]",
            ),
            (
                PluginLoadError::CapNotGranted {
                    plugin: "p".into(),
                    missing: vec!["a".into()],
                },
                "error[ark-kdl/cap-not-granted]",
            ),
            (
                PluginLoadError::Abi(AbiError::MissingVersion {
                    plugin: "p".into(),
                }),
                "error[abi/missing-version]",
            ),
        ];
        for (err, prefix) in cases {
            let s = format!("{err}");
            assert!(
                s.starts_with(prefix),
                "variant {err:?} Display {s:?} must start with {prefix:?}"
            );
        }
    }

    #[test]
    fn abi_error_converts_via_from() {
        let e: PluginLoadError = AbiError::HostTooOld {
            plugin: "p".into(),
            plugin_abi: 99,
            host_abi: 1,
        }
        .into();
        assert!(matches!(e, PluginLoadError::Abi(_)));
    }
}
