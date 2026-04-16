//! T-13.6: Declare-only capability enforcement (scene-side).
//!
//! After the scene compiler loads an extension's
//! [`ExtensionMetadata`](ark_ext_metadata_types::ExtensionMetadata) via
//! [`crate::use_resolution::resolve_use`], we cross-check the declared
//! capability list against the trust file at
//! `${XDG_CONFIG_HOME}/ark/extension-trust.kdl` (the file CLI-side
//! `ark ext add` writes; see `crates/cli/src/commands/ext/trust.rs` for
//! the authoritative producer). Any declared cap that is NOT trusted
//! for the specific `<name>@<version>` surfaces as a
//! `warning[ext/untrusted-capability]` via `tracing::warn!`.
//!
//! This is **declare-only** — no runtime gating, no hard rejection.
//! The wasm host-function surface is still unrestricted (runtime
//! enforcement is deferred to v0.5+, per `build-site-scene.md`). The
//! warning exists so operators notice drift between what an extension
//! declares and what they trusted at install time (e.g. a local extension
//! under active development that gained a new cap without going through
//! `ark ext add`, or a trust file that was hand-edited).
//!
//! # Why inline the trust-file parser instead of reusing the CLI module?
//!
//! The CLI's trust module lives in `crates/cli/src/commands/ext/trust.rs`
//! and owns the write path + the interactive prompt surface. The scene
//! crate only needs a READ-ONLY view of the capability trust entries
//! and must not pick up a reverse `cli → scene` dep on the CLI (which
//! would break the workspace DAG). Rather than introduce a new
//! `ark-trust` crate for a handful of KDL nodes, we inline a tiny
//! read-only parser here. Consolidation into a shared crate is tracked
//! as future work — see `context/plans/build-site-scene.md` T-13.x.
//!
//! The parser recognises `capability "<cap>" extension="<name>@<version>"`
//! nodes (the shape CLI writes); everything else (publisher entries,
//! unrelated nodes) is ignored for membership checks. Malformed KDL is
//! degraded to an empty trust set — the alternative ("lock the user
//! out of scene compile behind a corrupt trust file") is worse than
//! "over-warn about caps until the file is repaired".

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ark_ext_metadata_types::ExtensionMetadata;

/// A single `(extension, capability)` pair the scene compiler wants
/// to warn about at compile time.
///
/// Built by [`check_extension_caps`] and carried through to the
/// caller so the scene pipeline can log + surface warnings in its
/// preferred sink. Today's wiring emits one `tracing::warn!` per
/// warning; the struct is retained so future tiers (e.g. a
/// `SceneCompileReport` aggregator) can render them inline with other
/// diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapWarning {
    /// Canonical `<name>@<version>` key — the same identifier the
    /// trust file uses, produced by [`ext_version_key`].
    pub ext_key: String,
    /// Capability the extension declares that is not trusted for
    /// this `<name>@<version>` pair.
    pub capability: String,
}

impl CapWarning {
    /// Render the warning body as a single-line string in the shape the
    /// `warning[ext/untrusted-capability]` R12-style code carries. The
    /// caller prepends the code tag when emitting.
    pub fn message(&self) -> String {
        format!(
            "extension `{}` declares capability `{}` that is not in the \
             trust file — run `ark ext add` or edit \
             `${{XDG_CONFIG_HOME}}/ark/extension-trust.kdl` to trust it",
            self.ext_key, self.capability,
        )
    }
}

/// Build the canonical per-version extension key used to scope
/// capability trust. Format: `<name>@<version>`.
///
/// Mirrors `crates/cli/src/commands/ext/trust.rs::ext_version_key` —
/// re-exposed here so scene callers don't have to reach across into
/// CLI internals for a two-line helper.
pub fn ext_version_key(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

/// Resolve the trust-file path: `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`.
///
/// Returns `None` when neither `XDG_CONFIG_HOME` nor `HOME` is set —
/// in that case the scene compiler degrades to an empty trust set
/// (every declared cap is "untrusted"), which is noisy but safe.
pub fn trust_file_path() -> Option<PathBuf> {
    let base = if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(v);
        if p.as_os_str().is_empty() {
            fallback_config_home()?
        } else {
            p
        }
    } else {
        fallback_config_home()?
    };
    Some(base.join("ark/extension-trust.kdl"))
}

fn fallback_config_home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config"))
}

/// Load every trusted `(ext_key, capability)` pair from the default
/// trust-file location.
///
/// Missing file / unresolved path / KDL parse error all degrade to an
/// empty set, matching the defensive behaviour of
/// `cli::commands::ext::trust::load_trusted_caps`.
pub fn load_trusted_caps() -> HashSet<(String, String)> {
    let Some(path) = trust_file_path() else {
        return HashSet::new();
    };
    load_trusted_caps_at(&path)
}

/// Test-friendly variant: load trusted `(ext_key, capability)` pairs
/// from an explicit path. Callers pass a tempdir-rooted path so tests
/// can assert against a synthetic trust file without mutating process
/// env.
pub fn load_trusted_caps_at(path: &Path) -> HashSet<(String, String)> {
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };
    parse_trusted_caps(&text)
}

/// Return every trusted cap for any version of `name`.
///
/// Matches the T-13.6 spec convenience helper: given an extension
/// name (NOT a `<name>@<version>` key), union the caps trusted for
/// every recorded version. Useful when the caller only has the name
/// (e.g. scene compiler with a resolved `use "picker"` but no exact
/// version trust record — typical for local on-disk extensions where
/// the version string is developer-controlled and churns between
/// reloads).
///
/// Per-version trust inspection goes through [`load_trusted_caps`]
/// directly and filters on the exact `(ext_key, cap)` pair (that's
/// what [`check_extension_caps`] uses — caps trusted for 0.1.0 do
/// NOT apply to 0.2.0 per the T-13.5 version-bump re-prompt design).
pub fn load_trusted_caps_for(name: &str) -> HashSet<String> {
    load_trusted_caps_for_from(name, &load_trusted_caps())
}

/// Test-friendly variant of [`load_trusted_caps_for`] that takes the
/// full trust set as input instead of reading from disk. Lets test
/// suites that stash the trust file under a tempdir query caps
/// without mutating process env.
pub fn load_trusted_caps_for_from(
    name: &str,
    trusted: &HashSet<(String, String)>,
) -> HashSet<String> {
    let prefix = format!("{name}@");
    trusted
        .iter()
        .filter_map(|(k, c)| {
            if k.starts_with(&prefix) {
                Some(c.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Parse every `capability "<cap>" extension="<name>@<version>"` node
/// from a KDL trust document. Matches the producer shape in
/// `cli::commands::ext::trust::parse_trusted_caps`.
///
/// Malformed nodes (missing extension property, non-string argument)
/// are silently dropped.
fn parse_trusted_caps(text: &str) -> HashSet<(String, String)> {
    let doc = match kdl::KdlDocument::parse(text) {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!(
                target: "scene::cap_check",
                "extension-trust.kdl parse error — treating capability \
                 trust as empty"
            );
            return HashSet::new();
        }
    };
    let mut out = HashSet::new();
    for node in doc.nodes() {
        if node.name().to_string() != "capability" {
            continue;
        }
        let Some(arg) = node.entries().iter().find(|e| e.name().is_none()) else {
            continue;
        };
        let Some(cap) = arg.value().as_string() else {
            continue;
        };
        let ext_key = node
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.to_string()) == Some("extension".into()))
            .and_then(|e| e.value().as_string().map(|s| s.to_string()));
        let Some(ext_key) = ext_key else {
            continue;
        };
        out.insert((ext_key, cap.to_string()));
    }
    out
}

/// Cross-check an extension's declared capabilities against the trust
/// file. Pure function: takes the trust-pair set as input so callers
/// can inject a tempdir-rooted view for tests.
///
/// Returns one [`CapWarning`] per cap that's declared but NOT in the
/// trust set for this `<name>@<version>`. An empty return value means
/// every declared cap is trusted (or the extension declared no caps at
/// all). Declared-cap order is preserved so warnings render in the
/// same order as the manifest.
///
/// The function does not filter on [`ALLOWED_CAPABILITIES`] — unknown
/// caps still produce an untrusted warning. That's intentional:
/// unknown-cap warnings are surfaced by
/// [`ExtensionMetadata::unknown_capabilities`] (T-13.3) at a different
/// layer; T-13.6 is specifically about trust-file membership.
pub fn check_extension_caps(
    meta: &ExtensionMetadata,
    trusted: &HashSet<(String, String)>,
) -> Vec<CapWarning> {
    let ext_key = ext_version_key(&meta.name.value, &meta.version.value);
    let mut out = Vec::new();
    for cap in meta.capability_names() {
        let pair = (ext_key.clone(), cap.to_string());
        if trusted.contains(&pair) {
            continue;
        }
        out.push(CapWarning {
            ext_key: ext_key.clone(),
            capability: cap.to_string(),
        });
    }
    out
}

/// Default-path convenience: check an extension's caps against the
/// on-disk trust file at
/// `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`.
///
/// Thin wrapper around [`check_extension_caps`] that loads the trust
/// set from disk. The wrapper exists so the scene compile pipeline
/// (and `resolve_use`) can call a single function without threading a
/// trust-set argument through the public surface.
pub fn check_extension_caps_default(meta: &ExtensionMetadata) -> Vec<CapWarning> {
    let trusted = load_trusted_caps();
    check_extension_caps(meta, &trusted)
}

/// Emit each [`CapWarning`] via `tracing::warn!` under the
/// `scene::cap_check` target.
///
/// The target matches the canonical `warning[ext/untrusted-capability]`
/// R12 code in the message body so grep over `ark pane log` surfaces
/// the same symbolic code documented in
/// `context/refs/wasm-metadata-v1.md`.
///
/// Safe to call with an empty slice — emits nothing.
pub fn emit_cap_warnings(warnings: &[CapWarning]) {
    for w in warnings {
        tracing::warn!(
            target: "scene::cap_check",
            code = "ext/untrusted-capability",
            ext_key = %w.ext_key,
            capability = %w.capability,
            "warning[ext/untrusted-capability]: {}",
            w.message(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{ConfigSchema, StringNode};
    use tempfile::TempDir;

    fn meta_with(name: &str, version: &str, caps: &[&str]) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new(version),
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
    fn ext_version_key_round_trips_manifest_identifiers() {
        assert_eq!(ext_version_key("picker", "0.1.0"), "picker@0.1.0");
    }

    #[test]
    fn parse_trusted_caps_reads_capability_nodes() {
        let text = r#"
publisher "github:rlch"
capability "exec" extension="picker@0.1.0"
capability "pipe" extension="picker@0.1.0"
capability "network" extension="other@0.2.0"
unrelated "ignored"
"#;
        let got = parse_trusted_caps(text);
        assert!(got.contains(&("picker@0.1.0".into(), "exec".into())));
        assert!(got.contains(&("picker@0.1.0".into(), "pipe".into())));
        assert!(got.contains(&("other@0.2.0".into(), "network".into())));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn parse_trusted_caps_tolerates_malformed_kdl() {
        // Garbage KDL → empty set, no panic. Degrading to "empty
        // trust" is the conservative call per the module doc.
        let got = parse_trusted_caps("capability \"unclosed");
        assert!(got.is_empty());
    }

    #[test]
    fn parse_trusted_caps_skips_nodes_without_extension_property() {
        let text = r#"
capability "exec"
capability "pipe" extension="ok@0.1"
"#;
        let got = parse_trusted_caps(text);
        assert_eq!(got.len(), 1);
        assert!(got.contains(&("ok@0.1".into(), "pipe".into())));
    }

    #[test]
    fn load_trusted_caps_at_empty_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.kdl");
        assert!(load_trusted_caps_at(&path).is_empty());
    }

    #[test]
    fn load_trusted_caps_at_round_trips_kdl_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("extension-trust.kdl");
        fs::write(
            &path,
            "capability \"exec\" extension=\"picker@0.1.0\"\n\
             capability \"pipe\" extension=\"picker@0.1.0\"\n",
        )
        .unwrap();
        let got = load_trusted_caps_at(&path);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&("picker@0.1.0".into(), "exec".into())));
        assert!(got.contains(&("picker@0.1.0".into(), "pipe".into())));
    }

    #[test]
    fn check_extension_caps_empty_when_no_caps_declared() {
        let m = meta_with("picker", "0.1.0", &[]);
        let got = check_extension_caps(&m, &HashSet::new());
        assert!(got.is_empty());
    }

    #[test]
    fn check_extension_caps_warns_for_every_untrusted_cap() {
        let m = meta_with("picker", "0.1.0", &["exec", "pipe"]);
        // Trust set is empty → every declared cap is untrusted.
        let got = check_extension_caps(&m, &HashSet::new());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].ext_key, "picker@0.1.0");
        assert_eq!(got[0].capability, "exec");
        assert_eq!(got[1].capability, "pipe");
    }

    #[test]
    fn check_extension_caps_silences_trusted_caps() {
        let m = meta_with("picker", "0.1.0", &["exec", "pipe"]);
        let mut trusted = HashSet::new();
        trusted.insert(("picker@0.1.0".to_string(), "exec".to_string()));
        trusted.insert(("picker@0.1.0".to_string(), "pipe".to_string()));
        let got = check_extension_caps(&m, &trusted);
        assert!(got.is_empty());
    }

    #[test]
    fn check_extension_caps_mixed_trust_produces_one_warning() {
        // `exec` trusted, `pipe` not → exactly one warning for pipe.
        let m = meta_with("picker", "0.1.0", &["exec", "pipe"]);
        let mut trusted = HashSet::new();
        trusted.insert(("picker@0.1.0".to_string(), "exec".to_string()));
        let got = check_extension_caps(&m, &trusted);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].capability, "pipe");
    }

    #[test]
    fn check_extension_caps_is_version_scoped() {
        // A cap trusted for 0.1.0 does NOT apply to 0.2.0 — per T-13.5
        // version-bump re-prompt design, every version is its own trust
        // domain.
        let m = meta_with("picker", "0.2.0", &["exec"]);
        let mut trusted = HashSet::new();
        trusted.insert(("picker@0.1.0".to_string(), "exec".to_string()));
        let got = check_extension_caps(&m, &trusted);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ext_key, "picker@0.2.0");
    }

    #[test]
    fn check_extension_caps_preserves_declared_order() {
        // Manifest-declared order drives warning order so the user's
        // eye lines up with the source extension.kdl.
        let m = meta_with("x", "1.0.0", &["network", "pipe", "exec"]);
        let got = check_extension_caps(&m, &HashSet::new());
        let caps: Vec<&str> = got.iter().map(|w| w.capability.as_str()).collect();
        assert_eq!(caps, vec!["network", "pipe", "exec"]);
    }

    #[test]
    fn cap_warning_message_contains_identifier_and_cap() {
        let w = CapWarning {
            ext_key: "picker@0.1.0".into(),
            capability: "exec".into(),
        };
        let msg = w.message();
        assert!(msg.contains("picker@0.1.0"));
        assert!(msg.contains("exec"));
        assert!(msg.contains("trust"));
    }

    #[test]
    fn emit_cap_warnings_accepts_empty_slice() {
        // Smoke-test: no panic, no tracing output expected.
        emit_cap_warnings(&[]);
    }

    #[test]
    fn load_trusted_caps_for_from_unions_across_versions() {
        // The helper collapses per-version trust entries into a flat
        // set of caps, so callers who only have an extension NAME
        // (typical in scene compile) get a reasonable approximation
        // of "has this cap ever been trusted for this extension?".
        let mut trusted = HashSet::new();
        trusted.insert(("picker@0.1.0".to_string(), "exec".to_string()));
        trusted.insert(("picker@0.2.0".to_string(), "pipe".to_string()));
        trusted.insert(("other@1.0.0".to_string(), "network".to_string()));
        let got = load_trusted_caps_for_from("picker", &trusted);
        assert_eq!(got.len(), 2);
        assert!(got.contains("exec"));
        assert!(got.contains("pipe"));
        assert!(!got.contains("network"));
    }

    #[test]
    fn load_trusted_caps_for_from_matches_on_name_boundary() {
        // `picker` must not match `picker-helper@…` — the `@`
        // separator pins the boundary so name-prefix collisions don't
        // leak trust across distinct extensions.
        let mut trusted = HashSet::new();
        trusted.insert(("picker@0.1.0".to_string(), "exec".to_string()));
        trusted.insert(("picker-helper@0.1.0".to_string(), "pipe".to_string()));
        let got = load_trusted_caps_for_from("picker", &trusted);
        assert_eq!(got.len(), 1);
        assert!(got.contains("exec"));
    }

    #[test]
    fn check_extension_caps_ignores_unknown_cap_names_for_trust_purposes() {
        // A manifest declaring an "unknown" cap like the pre-T-13.3
        // dotted forms still flows through — trust-file membership is
        // an exact-string match, so the warning fires when there's no
        // matching trust entry. Unknown-name surfacing lives in T-13.3.
        let m = meta_with("legacy", "0.0.1", &["ui.keybind"]);
        let got = check_extension_caps(&m, &HashSet::new());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].capability, "ui.keybind");
    }
}
