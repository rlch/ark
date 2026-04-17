//! `ark ext inspect <path>` — dump a wasm cartridge's metadata as KDL.
//!
//! T-10.8 (cavekit-scene R13). Reads the `ark.metadata` custom section
//! from a `.wasm` file on disk (via [`ark_scene::wasm_meta`]) and
//! re-emits the decoded [`ExtensionMetadata`] as pretty-printed KDL
//! to stdout. No execution, no network, no filesystem writes — this
//! command is strictly for offline inspection.
//!
//! # Behavior
//!
//! * Exit 0 on success with the KDL document on stdout.
//! * Exit non-zero (maps through [`CliError::Generic`]) on:
//!   - path doesn't exist / isn't readable
//!   - wasm parse failure
//!   - missing `ark.metadata` custom section
//!   - malformed metadata section
//!   - KDL re-emission failure
//!
//! The input is expected to be the canonical wasm cartridge produced
//! by an extension author embedding
//! [`ark_ext_metadata::extension_metadata_kdl_bytes`]. Other `.wasm`
//! files surface as [`ark_scene::error::SceneError::WasmMetaMissing`]
//! (no `ark.metadata` section) or [`WasmMetaInvalid`] (corrupted).

use std::path::PathBuf;

use ark_ext_metadata::extension_metadata_kdl_string;
use ark_scene::wasm_meta::read_extension_metadata;
use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext inspect`.
#[derive(Debug, Args)]
#[command(
    about = "Dump a wasm cartridge's metadata as KDL",
    long_about = "Read the `ark.metadata` custom section of a wasm\n\
                  cartridge and re-emit it as pretty-printed KDL on\n\
                  stdout. Useful for verifying an extension was built\n\
                  correctly before install.\n\
                  \n\
                  Examples:\n  \
                  ark ext inspect ./target/wasm32-wasip1/debug/my.wasm\n  \
                  ark ext inspect ~/Downloads/picker.wasm"
)]
pub struct InspectArgs {
    /// Path to the `.wasm` cartridge to inspect.
    pub path: PathBuf,
}

/// Dispatch handler for `ark ext inspect`.
///
/// Reads the file, decodes its `ark.metadata` section, and prints the
/// re-emitted KDL to stdout. `ctx` is unused but is accepted to stay
/// symmetric with every other subcommand's signature.
pub fn run(args: InspectArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let wasm_bytes = std::fs::read(&args.path).map_err(|e| CliError::Generic {
        reason: format!("failed to read `{}`: {e}", args.path.display()),
    })?;

    let path_label = args.path.display().to_string();
    let metadata =
        read_extension_metadata(&wasm_bytes, &path_label).map_err(|e| {
            CliError::Generic {
                reason: format!("ext/inspect: {e}"),
            }
        })?;

    let kdl = extension_metadata_kdl_string(&metadata).map_err(|e| CliError::Generic {
        reason: format!("ext/inspect: failed to re-emit metadata as KDL: {e}"),
    })?;

    print!("{kdl}");
    if !kdl.ends_with('\n') {
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Encode arbitrary text for embedding inside a WAT string literal.
    ///
    /// WAT strings reject raw newlines; every non-printable byte must
    /// use the `\hh` escape. Mirrors `wat_escape` in
    /// `ark_scene::wasm_meta::tests`.
    fn wat_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'\\' => out.push_str("\\\\"),
                b'"' => out.push_str("\\\""),
                0x20..=0x7e => out.push(b as char),
                _ => out.push_str(&format!("\\{b:02x}")),
            }
        }
        out
    }

    /// Build a synthetic wasm cartridge that embeds `ark.metadata`
    /// with the given KDL text. Mirrors the fixture pattern in
    /// `ark_scene::wasm_meta::tests`.
    fn make_wasm_with_metadata(kdl: &str) -> Vec<u8> {
        let wat_src = format!(
            r#"(module (@custom "ark.metadata" "{}"))"#,
            wat_escape(kdl)
        );
        wat::parse_str(&wat_src).expect("wat compiles")
    }

    fn sample_kdl() -> &'static str {
        r#"extension {
    name "demo"
    version "0.1.0"
    ark-range ">=0.1, <0.2"
    zellij-range ""
    config { }
    capabilities { }
}
"#
    }

    #[test]
    fn missing_file_surfaces_generic_error() {
        let tmp = TempDir::new().unwrap();
        let args = InspectArgs {
            path: tmp.path().join("nope.wasm"),
        };
        let ctx = Ctx::default();
        let err = run(args, &ctx).expect_err("missing file");
        let msg = format!("{err}");
        assert!(msg.contains("failed to read"), "{msg}");
    }

    #[test]
    fn wasm_without_metadata_section_surfaces_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bare.wasm");
        // Minimal empty wasm module — no `ark.metadata` section.
        let bytes = wat::parse_str("(module)").unwrap();
        std::fs::write(&path, bytes).unwrap();
        let args = InspectArgs { path };
        let ctx = Ctx::default();
        let err = run(args, &ctx).expect_err("no metadata section");
        let msg = format!("{err}");
        assert!(
            msg.contains("ext/inspect") || msg.contains("ark.metadata"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn valid_wasm_decodes_and_emits_kdl() {
        // Smoke test: the round-trip path (fixture KDL → wasm → inspect
        // → KDL out) should preserve the extension name at least.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("demo.wasm");
        let wasm = make_wasm_with_metadata(sample_kdl());
        std::fs::write(&path, &wasm).unwrap();
        let args = InspectArgs { path };
        let ctx = Ctx::default();
        // Just assert run() succeeds (actual stdout capture would
        // require a harness refactor — reserved for a later tier).
        run(args, &ctx).expect("inspect succeeds on valid cartridge");
    }
}
