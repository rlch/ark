//! Plugin custom-section schemas.
//!
//! T-PP-016 (cavekit-plugin-protocol R3): [`CapsManifest`] + its
//! postcard on-disk format live in the `ark-caps:v1` custom section of
//! every compiled plugin. The host reads the section host-side via
//! `wasmparser` (see R8 Phase 1) without ever calling into guest code,
//! so the schema is a pure shape contract.
//!
//! T-PP-017 (cavekit-plugin-protocol R9): [`MetaManifest`] + its
//! postcard on-disk format live in the `ark-meta:v1` custom section.
//! Same host-side-only read path.
//!
//! Both sections are emitted by the `#[derive(Plugin)]` macro in
//! `ark-plugin-sdk` (T-PP-019+). Plugin authors never hand-encode
//! either section.
//!
//! Future schema bumps use a new section name (`ark-caps:v2` /
//! `ark-meta:v2`) — the host tries v2 first, falls back to v1 for at
//! least one major version (R3 acceptance).
//!
//! Encoding is `postcard` (pinned at the workspace level). The
//! manifests are structural only; enforcement (drift checks, ABI-gate,
//! name-collision) lives in `ark-host`.

use ark_types::{ARK_ABI_VERSION, SUPPORTED_PLUGIN_ABIS};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// ark-caps:v1 — capability display metadata (R3).
// ---------------------------------------------------------------------

/// Wasm custom-section name carrying the [`CapsManifest`] payload.
///
/// Kit R3 acceptance: "Section name `ark-caps:v1`. Future schema bumps
/// use a new section name (`ark-caps:v2`) per Cluster 4 §4.1 pattern
/// (3) — new hosts try v2 first, fall back to v1 for at least one
/// major version."
pub const CAPS_SECTION_NAME: &str = "ark-caps:v1";

/// Reserved for a future schema bump per R3. Not yet read; present as
/// a const so a future cavekit-writer can reach it without adding a
/// new one (and the existing constant documents the escape hatch).
pub const CAPS_SECTION_NAME_V2: &str = "ark-caps:v2";

/// Display metadata attached to a plugin's capability declarations.
///
/// The *authoritative* cap requirement list is the plugin's
/// `ark:cap/*` WIT imports (R3). This manifest exists purely for UX
/// presentation (`ark ext list`, error messages, future grant
/// prompts). Host-side drift checks ensure the two sources agree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CapsManifest {
    /// Plugin name — MUST equal the [`MetaManifest::name`] field on
    /// the same binary (cross-checked at load time in R8 Phase 2).
    pub plugin_name: String,
    /// First version of this plugin to declare this cap set. Present
    /// so a future grant-migration UX can show "since version X".
    /// Semver 2.0.0 string; validated at host-side decode.
    pub since_version: String,
    /// One entry per cap, in the same order the `#[derive(Plugin)]`
    /// macro visited them. Order is not semantically meaningful but is
    /// preserved for diagnostic stability.
    pub caps: Vec<CapDecl>,
}

/// Display metadata for a single capability declaration. The cap's
/// authoritative identity is the WIT import name (R3); this record
/// provides the strings the host needs to render an error or a grant
/// prompt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CapDecl {
    /// Identifier matching the `ark:cap/<id>` WIT interface — e.g.
    /// `fs-read`, `network`. The host cross-checks against the
    /// actual import set (R3 drift checks).
    pub id: String,
    /// Human-readable title shown in `ark ext list` / grant UX.
    pub display_name: String,
    /// Plugin-author-supplied reason string. Rendered verbatim in
    /// diagnostics so users can evaluate grant requests.
    pub reason: String,
}

impl CapsManifest {
    /// Serialise to postcard bytes — the on-disk representation in the
    /// `ark-caps:v1` custom section.
    ///
    /// Panics only on an OOM allocation failure from `postcard`, which
    /// is treated as a programmer bug (the schema is bounded). Callers
    /// that need non-panic encoding can reach `postcard::to_allocvec`
    /// directly.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("CapsManifest encode: postcard cannot fail for a bounded schema")
    }

    /// Deserialise from postcard bytes. The host calls this on the raw
    /// section payload extracted by `wasmparser`.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

// ---------------------------------------------------------------------
// ark-meta:v1 — plugin identity (R9).
// ---------------------------------------------------------------------

/// Wasm custom-section name carrying the [`MetaManifest`] payload.
///
/// Kit R9 acceptance: "Plugin binaries carry a custom section
/// literally named `ark-meta:v1`". See also the v2 schema-bump escape
/// hatch in [`META_SECTION_NAME_V2`].
pub const META_SECTION_NAME: &str = "ark-meta:v1";

/// Reserved for a future schema bump per R9. Same contract as
/// [`CAPS_SECTION_NAME_V2`].
pub const META_SECTION_NAME_V2: &str = "ark-meta:v2";

/// Identity baked into every plugin binary at build time by
/// `#[derive(Plugin)]`. Carries the three fields R9 enumerates:
/// `name`, `version`, `ark_abi_version`.
///
/// The host extracts this host-side via `wasmparser` during R8 Phase
/// 1 and refuses any plugin whose `name` collides with an
/// already-loaded plugin (R9) or whose `ark_abi_version` is not in
/// [`SUPPORTED_PLUGIN_ABIS`] (R14).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MetaManifest {
    /// Snake-case identifier matching the regex `^[a-z][a-z0-9_]*$`.
    /// Validated via [`MetaManifest::validate`] at host-side decode.
    /// Immutable across versions of the same plugin (renaming = new
    /// plugin, fresh `on-install` dispatch).
    pub name: String,
    /// Semver 2.0.0 string. Validated at decode; non-conforming =
    /// [`ManifestValidationError::InvalidSemver`].
    pub version: String,
    /// ABI version this plugin was built against — integer matching
    /// [`ARK_ABI_VERSION`]. Mismatch = refuse to load.
    pub ark_abi_version: u32,
}

impl MetaManifest {
    /// Serialise to postcard bytes — the on-disk representation in the
    /// `ark-meta:v1` custom section. See [`CapsManifest::encode`] for
    /// the panic policy.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("MetaManifest encode: postcard cannot fail for a bounded schema")
    }

    /// Deserialise from postcard bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Structural validation: name + version + abi version. Called by
    /// the host after decode so the error reports carry the actual
    /// manifest values rather than a bare `postcard::Error`.
    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if !is_valid_plugin_name(&self.name) {
            return Err(ManifestValidationError::InvalidName);
        }
        if !is_valid_semver(&self.version) {
            return Err(ManifestValidationError::InvalidSemver);
        }
        if !SUPPORTED_PLUGIN_ABIS.contains(&self.ark_abi_version) {
            return Err(ManifestValidationError::UnsupportedAbi {
                plugin: self.ark_abi_version,
                host: ARK_ABI_VERSION,
            });
        }
        Ok(())
    }
}

/// Structured errors from [`MetaManifest::validate`]. Wrapped by the
/// host's richer diagnostic path in `ark-plugin-protocol::errors` so
/// the end-user sees a stable `error[plugin/*]` code prefix.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ManifestValidationError {
    /// `name` does not match `^[a-z][a-z0-9_]*$`.
    #[error("name must match ^[a-z][a-z0-9_]*$")]
    InvalidName,
    /// `version` is not a valid semver 2.0.0 string.
    #[error("version must be valid semver")]
    InvalidSemver,
    /// `ark_abi_version` is not in [`SUPPORTED_PLUGIN_ABIS`].
    #[error("ark_abi_version {plugin} not supported (host supports {host})")]
    UnsupportedAbi {
        /// ABI the plugin declared.
        plugin: u32,
        /// ABI the host was built with.
        host: u32,
    },
}

// ---------------------------------------------------------------------
// Internal validators — hand-rolled to avoid pulling `regex` /
// `semver` crates into every consumer of `ark-plugin-protocol`. The
// `semver` workspace dep is already pinned for scene, but bringing it
// into this crate (and thus the wasm guest dep graph, via
// `ark-plugin-sdk`) is deferred to a later tier if/when needed.
// ---------------------------------------------------------------------

/// `^[a-z][a-z0-9_]*$` — first char ASCII lowercase, rest
/// lowercase/digit/underscore. Matches R9 and the R5 KDL grammar for
/// `<plugin-name>` with the underscore-vs-dash note (KDL uses dashes,
/// the ABI identifier uses underscores; the derive macro converts
/// between them).
fn is_valid_plugin_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    true
}

/// Minimal semver 2.0.0 validator. Accepts `MAJOR.MINOR.PATCH` where
/// each component is a non-empty sequence of ASCII digits with no
/// leading zero (except for literal "0"), optionally followed by a
/// `-pre-release` and/or `+build` tail. This matches the subset
/// R9/R14 actually cares about; the full semver grammar (pre-release
/// identifier rules, build metadata character set) is enforced by the
/// real `semver` crate at the host boundary in a later tier.
fn is_valid_semver(s: &str) -> bool {
    // Strip build metadata (`+…`) first — semver treats it as opaque.
    let core = match s.find('+') {
        Some(i) => &s[..i],
        None => s,
    };
    // Then strip pre-release (`-…`) — permitted by the grammar but not
    // required; we don't validate its character set strictly here.
    let core = match core.find('-') {
        Some(i) => &core[..i],
        None => core,
    };
    let mut parts = core.split('.');
    let major = parts.next();
    let minor = parts.next();
    let patch = parts.next();
    if parts.next().is_some() {
        return false; // extra `.<something>` beyond patch
    }
    let (Some(major), Some(minor), Some(patch)) = (major, minor, patch) else {
        return false;
    };
    is_valid_version_component(major)
        && is_valid_version_component(minor)
        && is_valid_version_component(patch)
}

fn is_valid_version_component(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.len() > 1 && s.starts_with('0') {
        return false; // semver forbids leading zeroes on numeric idents
    }
    s.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_section_name_is_stable() {
        assert_eq!(CAPS_SECTION_NAME, "ark-caps:v1");
    }

    #[test]
    fn meta_section_name_is_stable() {
        assert_eq!(META_SECTION_NAME, "ark-meta:v1");
    }

    #[test]
    fn v2_section_names_reserved() {
        assert_eq!(CAPS_SECTION_NAME_V2, "ark-caps:v2");
        assert_eq!(META_SECTION_NAME_V2, "ark-meta:v2");
    }

    #[test]
    fn caps_manifest_roundtrip() {
        let m = CapsManifest {
            plugin_name: "echo".into(),
            since_version: "0.1.0".into(),
            caps: vec![
                CapDecl {
                    id: "fs-read".into(),
                    display_name: "Read files".into(),
                    reason: "Echo example reads project files".into(),
                },
                CapDecl {
                    id: "network".into(),
                    display_name: "Network".into(),
                    reason: "Make outbound requests".into(),
                },
            ],
        };
        let bytes = m.encode();
        let back = CapsManifest::decode(&bytes).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn caps_manifest_decode_rejects_garbage() {
        let bad = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(CapsManifest::decode(&bad).is_err());
    }

    #[test]
    fn meta_manifest_roundtrip() {
        let m = MetaManifest {
            name: "claude_code".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        let bytes = m.encode();
        let back = MetaManifest::decode(&bytes).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn meta_manifest_validates_valid_input() {
        let m = MetaManifest {
            name: "claude_code".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        m.validate().unwrap();
    }

    #[test]
    fn meta_manifest_rejects_invalid_name() {
        // starts with digit
        let m = MetaManifest {
            name: "1plugin".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestValidationError::InvalidName)
        ));
        // uppercase
        let m = MetaManifest {
            name: "Plugin".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestValidationError::InvalidName)
        ));
        // dashes (ark-meta uses underscores; dashes are the ark.kdl
        // grammar, translated by the derive macro).
        let m = MetaManifest {
            name: "claude-code".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestValidationError::InvalidName)
        ));
        // empty
        let m = MetaManifest {
            name: "".into(),
            version: "0.1.0".into(),
            ark_abi_version: ARK_ABI_VERSION,
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestValidationError::InvalidName)
        ));
    }

    #[test]
    fn meta_manifest_accepts_valid_names() {
        for name in ["a", "a1", "echo", "claude_code", "a_b_c", "foo2_bar"] {
            let m = MetaManifest {
                name: name.into(),
                version: "0.1.0".into(),
                ark_abi_version: ARK_ABI_VERSION,
            };
            m.validate()
                .unwrap_or_else(|_| panic!("name `{name}` should be valid"));
        }
    }

    #[test]
    fn meta_manifest_rejects_invalid_semver() {
        for bad in ["", "1", "1.0", "v1.0.0", "1.0.0.0", "01.0.0", "1.a.0"] {
            let m = MetaManifest {
                name: "plugin".into(),
                version: bad.into(),
                ark_abi_version: ARK_ABI_VERSION,
            };
            let got = m.validate();
            assert!(
                matches!(got, Err(ManifestValidationError::InvalidSemver)),
                "version `{bad}` should fail semver check, got {got:?}"
            );
        }
    }

    #[test]
    fn meta_manifest_accepts_semver_with_prerelease_and_build() {
        for good in ["0.1.0", "1.2.3", "1.2.3-alpha", "1.2.3+build", "1.2.3-rc.1+build.7"] {
            let m = MetaManifest {
                name: "plugin".into(),
                version: good.into(),
                ark_abi_version: ARK_ABI_VERSION,
            };
            m.validate()
                .unwrap_or_else(|_| panic!("version `{good}` should validate"));
        }
    }

    #[test]
    fn meta_manifest_rejects_unsupported_abi() {
        let m = MetaManifest {
            name: "plugin".into(),
            version: "0.1.0".into(),
            ark_abi_version: 99,
        };
        let got = m.validate();
        match got {
            Err(ManifestValidationError::UnsupportedAbi { plugin, host }) => {
                assert_eq!(plugin, 99);
                assert_eq!(host, ARK_ABI_VERSION);
            }
            other => panic!("expected UnsupportedAbi, got {other:?}"),
        }
    }

    #[test]
    fn caps_manifest_encode_is_deterministic() {
        let m = CapsManifest {
            plugin_name: "echo".into(),
            since_version: "0.1.0".into(),
            caps: vec![],
        };
        assert_eq!(m.encode(), m.encode());
    }
}
