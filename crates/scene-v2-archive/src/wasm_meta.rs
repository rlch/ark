//! Wasm cartridge metadata reader (cavekit-scene R10, T-10.2).
//!
//! Reads the `ark.metadata` custom section from a `.wasm` byte slice and
//! decodes its UTF-8 KDL contents into the typed
//! [`ExtensionMetadata`] struct re-exported from
//! [`ark_ext_metadata_types`] (T-10.1). The struct is the single source
//! of truth — the scene compiler MUST NOT introduce a parallel schema.
//!
//! # On-wire shape
//!
//! Extension authors embed the KDL bytes via:
//!
//! ```ignore
//! #[link_section = "ark.metadata"]
//! pub static ARK_METADATA: [u8; N] =
//!     *include_bytes!(concat!(env!("OUT_DIR"), "/ark.metadata"));
//! ```
//!
//! `ark_ext_metadata::extension_metadata_kdl_bytes` produces the bytes
//! in the canonical format — a document rooted at a single `extension {
//! … }` node whose body matches [`ExtensionMetadata`]. We round-trip by
//! parsing as [`ExtensionManifest`] (the wrapper carrying the
//! `extension` child) and projecting to its inner field.
//!
//! # Failure modes
//!
//! * [`SceneError::WasmMetaMissing`] — the wasm payload contains zero
//!   custom sections named `ark.metadata`.
//! * [`SceneError::WasmMetaInvalid`] — the section is present but its
//!   bytes are not valid UTF-8, do not parse as KDL, or do not match
//!   the [`ExtensionMetadata`] shape. The underlying error message is
//!   carried verbatim for debugging.
//!
//! # Why wasmparser?
//!
//! `wasmparser` is the de-facto streaming wasm payload walker (used by
//! `wasm-tools`, `wasmtime`, and `cargo-component`). The
//! `Parser::parse_all` API yields a `Payload::CustomSection` for every
//! custom section without allocating; we simply find the first one
//! named `ark.metadata`. Pinning to 0.246.x matches the
//! `wasm-tools 1.246.x` release line.

use ark_ext_metadata_types::{ExtensionManifest, ExtensionMetadata};
use wasmparser::{Parser, Payload};

use crate::error::SceneError;

/// Custom-section name that authors embed via `#[link_section = "…"]`
/// on a static byte array. R10 fixes this name as part of the
/// extension wire-format; bumping it would be a MAJOR R16 change.
pub const ARK_METADATA_SECTION: &str = "ark.metadata";

/// Read and decode the `ark.metadata` custom section from a wasm
/// cartridge.
///
/// `wasm_bytes` is the full cartridge image. `path` is a best-effort
/// identifier for diagnostics (filesystem path when known, otherwise a
/// synthetic label like `"<bytes>"`); it is only used in error
/// messages and does NOT trigger any filesystem I/O.
///
/// Returns the typed [`ExtensionMetadata`] on success, or one of:
///
/// * [`SceneError::WasmMetaMissing`] — no custom section named
///   `ark.metadata` was found in the cartridge.
/// * [`SceneError::WasmMetaInvalid`] — section exists but its bytes
///   could not be decoded.
///
/// The decoder does **not** validate semver ranges, dependency
/// closure, or capability lists — those checks live in the use
/// resolver (T-10.4).
#[allow(clippy::result_large_err)] // SceneError carries diagnostic surface; matches parse_scene.
pub fn read_extension_metadata(
    wasm_bytes: &[u8],
    path: &str,
) -> Result<ExtensionMetadata, SceneError> {
    let raw = find_metadata_section(wasm_bytes, path)?;
    decode_metadata_bytes(raw, path)
}

/// Walk the wasm payload stream and return the first custom section
/// named [`ARK_METADATA_SECTION`].
///
/// Errors:
///
/// * Wasm-level parse failure surfaces as [`SceneError::WasmMetaInvalid`]
///   — a malformed cartridge can never carry a usable metadata section,
///   so treating the underlying parser error as "invalid" is the
///   correct user-facing diagnosis.
/// * No matching section → [`SceneError::WasmMetaMissing`].
fn find_metadata_section<'a>(
    wasm_bytes: &'a [u8],
    path: &str,
) -> Result<&'a [u8], SceneError> {
    for payload in Parser::new(0).parse_all(wasm_bytes) {
        let payload = payload.map_err(|e| SceneError::WasmMetaInvalid {
            path: path.to_string(),
            message: format!("wasm payload parse failed: {e}"),
        })?;
        if let Payload::CustomSection(reader) = payload {
            if reader.name() == ARK_METADATA_SECTION {
                return Ok(reader.data());
            }
        }
    }
    Err(SceneError::WasmMetaMissing {
        path: path.to_string(),
    })
}

/// Decode the raw bytes of an `ark.metadata` section.
///
/// The bytes must be UTF-8 KDL produced by
/// `ark_ext_metadata::extension_metadata_kdl_bytes` — a document rooted
/// at a single `extension { … }` node whose body matches
/// [`ExtensionMetadata`]. We delegate to facet-kdl via
/// [`ExtensionManifest`] (the wrapper carrying the `extension` child)
/// and return its inner field.
fn decode_metadata_bytes(raw: &[u8], path: &str) -> Result<ExtensionMetadata, SceneError> {
    let text = std::str::from_utf8(raw).map_err(|e| SceneError::WasmMetaInvalid {
        path: path.to_string(),
        message: format!("ark.metadata is not valid UTF-8: {e}"),
    })?;
    let manifest: ExtensionManifest =
        facet_kdl::from_str(text).map_err(|e| SceneError::WasmMetaInvalid {
            path: path.to_string(),
            message: format!("KDL decode failed: {e}"),
        })?;
    Ok(manifest.extension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// Build a minimal wasm module that carries a single custom section
    /// named `ark.metadata` whose contents are the supplied bytes
    /// (passed through as the section body verbatim). Uses
    /// `wat::parse_str` so tests need no on-disk fixtures.
    ///
    /// The `(@custom "ark.metadata" "<text>")` form is the WAT-level
    /// syntax for "embed a custom section". WAT strings are
    /// **single-line** — the lexer rejects raw newlines (`\n`) inside
    /// the quotes — so callers MUST encode newlines as the WAT escape
    /// `\0a`. This helper does that translation, plus quote/backslash
    /// escaping, so callers can pass ordinary multi-line text.
    fn build_wasm_with_metadata(kdl_text: &str) -> Vec<u8> {
        let wat_src = format!(
            r#"(module (@custom "ark.metadata" "{}"))"#,
            wat_escape(kdl_text)
        );
        wat::parse_str(&wat_src).expect("wat fixture compiles")
    }

    /// Encode arbitrary text for embedding inside a WAT string literal.
    ///
    /// WAT strings use C-style escapes (`\\`, `\"`, `\n`-equivalents
    /// only via two-digit hex) and reject raw newlines. We emit each
    /// non-printable / non-ASCII byte as `\hh`. Printable ASCII
    /// (0x20..=0x7e) passes through untouched, except for `\` and `"`
    /// which need escaping.
    fn wat_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'\\' => out.push_str("\\\\"),
                b'"' => out.push_str("\\\""),
                0x20..=0x7e => out.push(b as char),
                other => out.push_str(&format!("\\{other:02x}")),
            }
        }
        out
    }

    /// Canonical sample metadata used across the round-trip tests. Kept
    /// minimal so the on-disk KDL stays small and easy to inspect when
    /// a test fails.
    ///
    /// Multi-line for readability — `build_wasm_with_metadata` encodes
    /// the newlines as `\0a` for WAT.
    fn sample_kdl() -> &'static str {
        // Hand-written KDL matching the `extension { … }` root that
        // `extension_metadata_kdl_bytes` produces. Using a hand-written
        // string here (rather than calling the helper) keeps this
        // crate's test independent of the metadata-helper's KDL
        // formatting tweaks.
        //
        // `config { }` is required because facet-kdl 0.42 does not
        // recognise the upstream `Default` impl on
        // `ExtensionMetadata::config`; the empty block satisfies the
        // deserializer without changing semantics.
        r#"
extension {
    name "demo"
    version "0.1.0"
    ark-range ">=0.1, <0.2"
    zellij-range ""
    config { }
}
"#
    }

    #[test]
    fn round_trips_minimal_metadata_section() {
        let wasm = build_wasm_with_metadata(sample_kdl());
        let meta =
            read_extension_metadata(&wasm, "test.wasm").expect("metadata decodes");
        assert_eq!(meta.name.value, "demo");
        assert_eq!(meta.version.value, "0.1.0");
        assert_eq!(meta.ark_range.value, ">=0.1, <0.2");
        assert_eq!(meta.zellij_range.value, "");
    }

    #[test]
    fn missing_section_errors_with_metadata_missing_code() {
        // Empty module — wasm magic header only, no custom sections.
        let wasm = wat::parse_str("(module)").expect("empty module");
        let err = read_extension_metadata(&wasm, "empty.wasm")
            .expect_err("missing section must error");
        assert_eq!(err.code_enum(), ErrorCode::WasmMetaMissing);
        match err {
            SceneError::WasmMetaMissing { path } => assert_eq!(path, "empty.wasm"),
            other => panic!("expected WasmMetaMissing, got {other:?}"),
        }
    }

    #[test]
    fn invalid_utf8_in_section_errors_with_metadata_invalid_code() {
        // Hand-craft a wasm module with an `ark.metadata` section
        // containing invalid UTF-8. WAT's `@custom` directive embeds
        // the bytes verbatim, so a `\\ff` sequence yields a 0xff byte
        // — invalid as UTF-8 leading byte for a 7-bit character.
        let wat_src =
            r#"(module (@custom "ark.metadata" "\ff\fe\fd"))"#;
        let wasm = wat::parse_str(wat_src).expect("wat with raw bytes");
        let err = read_extension_metadata(&wasm, "bad.wasm")
            .expect_err("invalid UTF-8 must error");
        assert_eq!(err.code_enum(), ErrorCode::WasmMetaInvalid);
        match err {
            SceneError::WasmMetaInvalid { path, message } => {
                assert_eq!(path, "bad.wasm");
                assert!(
                    message.to_lowercase().contains("utf-8"),
                    "expected UTF-8 mention in: {message}"
                );
            }
            other => panic!("expected WasmMetaInvalid, got {other:?}"),
        }
    }

    #[test]
    fn malformed_kdl_in_section_errors_with_metadata_invalid_code() {
        // Section body is valid UTF-8 but not a parseable KDL document
        // (unterminated string).
        let wasm = build_wasm_with_metadata("extension { name \"missing-close ");
        let err = read_extension_metadata(&wasm, "bad.wasm")
            .expect_err("malformed KDL must error");
        assert_eq!(err.code_enum(), ErrorCode::WasmMetaInvalid);
    }

    #[test]
    fn malformed_wasm_bytes_error_with_metadata_invalid_code() {
        // Random bytes — not a valid wasm module at all.
        let err = read_extension_metadata(b"not a wasm file", "garbage.wasm")
            .expect_err("malformed wasm must error");
        // The wasm parse failure surfaces as WasmMetaInvalid (we
        // cannot find a section in a file we cannot parse).
        assert_eq!(err.code_enum(), ErrorCode::WasmMetaInvalid);
    }

    #[test]
    fn picks_first_matching_section_when_multiple_present() {
        // A cartridge built with a misconfigured macro could end up
        // with two `ark.metadata` sections. The reader picks the first
        // — there's no rule for which "wins" in the spec, so we
        // document the first-found behaviour and lock it in via test.
        let first = wat_escape(
            r#"extension {
    name "first"
    version "0.1.0"
    ark-range ""
    zellij-range ""
    config { }
}"#,
        );
        let second = wat_escape(
            r#"extension {
    name "second"
    version "0.2.0"
    ark-range ""
    zellij-range ""
    config { }
}"#,
        );
        let wat_src = format!(
            "(module\n  (@custom \"ark.metadata\" \"{first}\")\n  (@custom \"ark.metadata\" \"{second}\")\n)\n"
        );
        let wasm = wat::parse_str(&wat_src).expect("wat fixture compiles");
        let meta = read_extension_metadata(&wasm, "dup.wasm").expect("decodes");
        assert_eq!(meta.name.value, "first");
    }

    #[test]
    fn ignores_unrelated_custom_sections() {
        let body = wat_escape(sample_kdl());
        let wat_src = format!(
            "(module\n  (@custom \"name\" \"irrelevant\")\n  (@custom \"ark.metadata\" \"{body}\")\n  (@custom \"producers\" \"ignored\")\n)\n"
        );
        let wasm = wat::parse_str(&wat_src).expect("wat fixture compiles");
        let meta = read_extension_metadata(&wasm, "many.wasm").expect("decodes");
        assert_eq!(meta.name.value, "demo");
    }
}
