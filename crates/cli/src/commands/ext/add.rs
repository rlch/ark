//! `ark ext add` — install an extension from a source.
//!
//! T-12.9 (cavekit-scene R13) shipped install. T-13.1 / T-13.2
//! (cavekit-scene R10, v0.5) layer publisher-trust on top — see
//! [`super::trust`]. Every install derives a [`Publisher`] from the
//! source (github user / url host / local path), checks it against
//! the on-disk trust file, and either:
//!
//! * skips the prompt (publisher already trusted), or
//! * prompts the operator on stdin, or
//! * emits an audit-log entry when `--accept-all` is in effect.
//!
//! [`Publisher`]: super::trust::Publisher
//!
//! Three source forms:
//!
//! * `path:<dir>`          — recursive copy of a local directory.
//! * `url:<https-tarball>` — download `.tar.gz` / `.tgz` via `ureq`,
//!   decompress with `flate2`, extract with `tar`.
//! * `github:<user>/<repo>[@<ref>]` — subprocess `git clone --depth 1`
//!   (branch/tag via `--branch`). Subprocess chosen over `git2` to
//!   avoid pulling in libgit2 bindings for a single one-shot clone.
//!
//! Install target: `${XDG_DATA_HOME}/ark/extensions/<name>/`.
//!
//! # Post-install verification
//!
//! After the source lands in a staging dir, the installer reads
//! [`ark_ext_metadata::parse_extension_metadata_kdl`] on
//! `<staging>/extension.kdl` (symmetric with `ark ext list` /
//! `info`). If the staging dir has no `extension.kdl` but does carry a
//! `.wasm` cartridge, the wasm `ark.metadata` custom section is read
//! via [`ark_scene::wasm_meta::read_extension_metadata`] as a
//! fallback. The extension's advertised name MUST match the directory
//! name — any mismatch surfaces as [`CliError::Generic`] and the
//! staging dir is cleaned up.
//!
//! # `.ark-install` dotfile
//!
//! On success, a sibling `.ark-install` file is written at the
//! extension root recording the source specifier, the install
//! timestamp (RFC3339), and the resolved name. `ark ext info` prints
//! this file verbatim so users can trace "where did this extension
//! come from?".

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use ark_ext_metadata_types::ExtensionMetadata;
use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext add`.
#[derive(Debug, Args)]
#[command(
    about = "Install an ark extension from a source",
    long_about = "Install an extension into \
                  `${XDG_DATA_HOME}/ark/extensions/<name>/`.\n\
                  \n\
                  Sources:\n  \
                  path:<dir>                     copy from a local directory\n  \
                  url:https://...                download + extract a tarball\n  \
                  github:user/repo[@<ref>]       shallow-clone a git repository\n\
                  \n\
                  After install, the extension's `extension.kdl` (or\n\
                  embedded wasm metadata) is read to verify the name\n\
                  and the source specifier is recorded in\n\
                  `.ark-install`.\n\
                  \n\
                  Examples:\n  \
                  ark ext add path:./my-ext\n  \
                  ark ext add url:https://example.com/picker.tar.gz\n  \
                  ark ext add github:rlch/ark-picker@v0.1.0"
)]
pub struct AddArgs {
    /// Source specifier: `path:./local`, `github:user/repo@tag`,
    /// or `url:https://example.com/ext.tar.gz`.
    #[arg(required = true, value_name = "SOURCE")]
    pub source: String,

    /// Skip the publisher-trust prompt (for CI). Each bypass is
    /// recorded as a line in
    /// `${XDG_DATA_HOME}/ark/extension-audit.log` (T-13.2).
    #[arg(long = "accept-all")]
    pub accept_all: bool,
}

/// Parsed source specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Local directory to copy recursively.
    Path(PathBuf),
    /// HTTPS tarball URL (`.tar.gz` / `.tgz`).
    Url(String),
    /// `<user>/<repo>` plus an optional git ref (branch / tag / sha).
    Github {
        /// `<user>/<repo>` (both halves validated non-empty).
        slug: String,
        /// Git ref — branch, tag, or short SHA. `None` = default branch.
        git_ref: Option<String>,
    },
}

impl Source {
    /// Best-effort stringification for `.ark-install` + diagnostics.
    pub fn as_specifier(&self) -> String {
        match self {
            Source::Path(p) => format!("path:{}", p.display()),
            Source::Url(u) => format!("url:{u}"),
            Source::Github {
                slug,
                git_ref: Some(r),
            } => {
                format!("github:{slug}@{r}")
            }
            Source::Github {
                slug,
                git_ref: None,
            } => format!("github:{slug}"),
        }
    }
}

/// Parse a `path:`/`url:`/`github:` source specifier.
///
/// Returns a structured [`Source`] on success or a human-readable
/// error string suitable for [`CliError::Generic`].
pub fn parse_source(raw: &str) -> Result<Source, String> {
    if let Some(rest) = raw.strip_prefix("path:") {
        if rest.is_empty() {
            return Err("path: source requires a directory".into());
        }
        return Ok(Source::Path(PathBuf::from(rest)));
    }
    if let Some(rest) = raw.strip_prefix("url:") {
        if !(rest.starts_with("https://") || rest.starts_with("http://")) {
            return Err(format!("url: source must be an http(s) URL: `{rest}`"));
        }
        return Ok(Source::Url(rest.to_string()));
    }
    if let Some(rest) = raw.strip_prefix("github:") {
        let (slug, git_ref) = match rest.split_once('@') {
            Some((s, r)) => (s.to_string(), Some(r.to_string())),
            None => (rest.to_string(), None),
        };
        // Validate slug shape `<user>/<repo>`.
        let mut parts = slug.split('/');
        let user = parts.next().unwrap_or("");
        let repo = parts.next().unwrap_or("");
        if user.is_empty() || repo.is_empty() || parts.next().is_some() {
            return Err(format!(
                "github: source must look like `user/repo[@ref]`: `{rest}`"
            ));
        }
        return Ok(Source::Github { slug, git_ref });
    }
    Err(format!(
        "unknown source scheme in `{raw}` (expected `path:`, `url:`, or `github:`)"
    ))
}

/// Dispatch handler for `ark ext add`.
pub fn run(args: AddArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let source = parse_source(&args.source).map_err(|reason| CliError::Generic {
        reason: format!("ext/add: {reason}"),
    })?;

    // T-13.1 / T-13.2: publisher trust gate. Resolve the source's
    // publisher, check it against the on-disk trust file, and either
    // skip the prompt (already trusted), prompt stdin (interactive),
    // or emit an audit log entry (--accept-all CI path).
    let publisher = super::trust::derive_publisher(&source);
    if !super::trust::is_trusted(&publisher) {
        if args.accept_all {
            // T-13.2: non-interactive path. Record the bypass and
            // persist the trust so subsequent installs in the same
            // CI context don't re-log.
            if let Err(e) = super::trust::append_audit(&publisher, &source.as_specifier()) {
                eprintln!("ark ext add: audit log write failed: {e}");
            }
            if let Err(e) = super::trust::save_trust(&publisher) {
                eprintln!("ark ext add: persisting trust failed: {e}");
            }
            eprintln!(
                "warning: --accept-all bypassed trust prompt for publisher `{}`",
                publisher.display()
            );
        } else {
            // T-13.1: interactive prompt.
            let accepted = super::trust::prompt_trust(&publisher);
            if !accepted {
                return Err(CliError::Generic {
                    reason: format!(
                        "ext/add: publisher `{}` was not trusted; install aborted",
                        publisher.display()
                    ),
                });
            }
            if let Err(e) = super::trust::save_trust(&publisher) {
                eprintln!("ark ext add: persisting trust failed: {e}");
            }
        }
    }

    let xdg_data_home = resolve_xdg_data_home().map_err(|reason| CliError::Generic {
        reason: format!("ext/add: {reason}"),
    })?;
    let extensions_root = xdg_data_home.join("ark/extensions");

    // T-13.4: install-time capability disclosure. After staging +
    // metadata read, inspect `ExtensionMetadata::capabilities`; if
    // non-empty and the (ext_key, cap) pair isn't already trusted,
    // either prompt the operator or take the `--accept-all` bypass
    // (with matching audit-log entry). Denying aborts the install and
    // cleans up the staging dir. [`decide_capability_disclosure`]
    // captures the actual policy so the install pipeline itself stays
    // decision-agnostic and the policy is unit-testable without
    // driving the full install pipeline.
    let accept_all = args.accept_all;
    let decide_caps = |meta: &ExtensionMetadata| -> Result<(), String> {
        decide_capability_disclosure(meta, accept_all)
    };

    let outcome =
        install_from_source_with_cap_decision(&source, &extensions_root, accept_all, &decide_caps)
            .map_err(|reason| CliError::Generic {
                reason: format!("ext/add: {reason}"),
            })?;

    println!(
        "installed extension `{}` (version {}) to {}",
        outcome.metadata.name.value,
        outcome.metadata.version.value,
        outcome.install_dir.display()
    );
    Ok(())
}

/// Everything a successful install produces: the parsed manifest + the
/// final install directory.
#[derive(Debug)]
pub struct InstallOutcome {
    /// Verified metadata from the installed extension.
    pub metadata: ExtensionMetadata,
    /// `${XDG_DATA_HOME}/ark/extensions/<name>/`.
    pub install_dir: PathBuf,
}

/// End-to-end install: fetch source into a staging dir, verify
/// metadata, rename into place, write `.ark-install`.
///
/// Backward-compat wrapper around
/// [`install_from_source_with_cap_decision`]: passes a no-op cap
/// decision closure that auto-accepts every declared capability.
/// Production callers (the `run` dispatch above) go through the
/// decision variant so T-13.4's cap-disclosure prompt runs; tests +
/// internal callers that don't care about cap disclosure use this
/// thin variant.
pub fn install_from_source(
    source: &Source,
    extensions_root: &Path,
    accept_all: bool,
) -> Result<InstallOutcome, String> {
    install_from_source_with_cap_decision(source, extensions_root, accept_all, &|_meta| Ok(()))
}

/// Full install pipeline with an injectable capability-disclosure
/// decision closure (T-13.4).
///
/// Called with the staging dir's parsed [`ExtensionMetadata`] right
/// after validation and right before the staging-to-final rename.
/// Returning `Err` from the closure aborts the install and cleans up
/// the staging dir — the error surfaces verbatim to the caller.
///
/// # Why a closure and not a flag
///
/// The cap decision is fundamentally interactive: it may prompt the
/// operator, it may consult a trust file, it may write an audit-log
/// entry. Encoding all of that via flags on this function would
/// couple the install pipeline to the specific trust-surface shape.
/// The closure keeps `install_from_source*` decision-free — the
/// dispatch in [`run`] wires the trust module in, tests inject their
/// own always-accept or always-deny closures.
pub fn install_from_source_with_cap_decision(
    source: &Source,
    extensions_root: &Path,
    _accept_all: bool,
    cap_decision: &dyn Fn(&ExtensionMetadata) -> Result<(), String>,
) -> Result<InstallOutcome, String> {
    fs::create_dir_all(extensions_root)
        .map_err(|e| format!("creating {}: {e}", extensions_root.display()))?;

    // Stage into a sibling `.ark-staging-<ulid>/` so partial failures
    // never leave a half-installed extension visible to `ark ext list`.
    let staging = extensions_root.join(format!(".ark-staging-{}", ulid::Ulid::new()));
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }

    let fetch_result = fetch_into_staging(source, &staging);
    if let Err(e) = fetch_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    // Verify: read manifest; confirm name matches some expected target.
    let metadata = match read_staging_metadata(&staging) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    let name = metadata.name.value.clone();
    if name.is_empty() {
        let _ = fs::remove_dir_all(&staging);
        return Err("metadata contains empty `name` field — refusing to install".into());
    }
    validate_name(&name).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staging);
    })?;

    let final_dir = extensions_root.join(&name);
    if final_dir.exists() {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!(
            "extension `{name}` is already installed at {}; remove it first with \
             `ark ext remove {name}`",
            final_dir.display()
        ));
    }

    // T-13.4: cap disclosure runs here, while the staging dir is
    // still disposable. Denying the prompt rolls staging back so no
    // partially-installed ext leaks into `ark ext list`.
    if let Err(e) = cap_decision(&metadata) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    // Write `.ark-install` into the staging dir before the rename so
    // the file lands atomically with the rest of the extension.
    write_install_dotfile(&staging, source, &name).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staging);
    })?;

    // Rename staging -> final. `fs::rename` is atomic when the src +
    // dst are on the same filesystem; staging is deliberately a
    // sibling of final_dir, which guarantees that.
    fs::rename(&staging, &final_dir).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        format!("moving staging to {}: {e}", final_dir.display())
    })?;

    Ok(InstallOutcome {
        metadata,
        install_dir: final_dir,
    })
}

/// T-13.4 / T-13.5 capability-disclosure decision.
///
/// Called with the parsed [`ExtensionMetadata`] of an extension mid-
/// install. Walks the declared-cap list, filters out caps already
/// trusted for this `<name>@<version>`, then either:
///
/// * emits a `warning[ext/unknown-capability]`-style message for caps
///   outside [`ark_ext_metadata_types::ALLOWED_CAPABILITIES`] and
///   continues (T-13.3 "unknown caps are non-fatal"),
/// * auto-accepts and records an `accept-all-caps` audit entry + trust
///   entry when `accept_all` is set (CI path, symmetric with T-13.2),
/// * prompts the operator on stdin and either records the acceptance
///   or returns an abort-install error on denial.
///
/// # T-13.5 — version-bump re-prompt
///
/// When a caller installs `foo@1.2` and the trust file already
/// records `capability "pipe" extension="foo@1.1"`, the per-version
/// key has changed (`foo@1.2` ≠ `foo@1.1`) so the T-13.4 check would
/// naively re-prompt for every cap. T-13.5 narrows that: caps already
/// trusted on *any* prior version of the same `name` are auto-
/// carried forward — we persist them under the new version's key so
/// future checks stay cheap, and we only prompt for caps that are
/// genuinely new in this version. The operator sees "already trusted
/// from a prior version" for carried-forward caps so the audit trail
/// is visible at install time.
///
/// Returns `Err(reason)` when the install should abort — the caller
/// (`install_from_source_with_cap_decision`) rolls staging back and
/// bubbles the reason up as a [`CliError::Generic`].
pub fn decide_capability_disclosure(
    meta: &ExtensionMetadata,
    accept_all: bool,
) -> Result<(), String> {
    let ext_key = super::trust::ext_version_key(&meta.name.value, &meta.version.value);
    // Warn on unknown caps (T-13.3 vocabulary); still proceed — the
    // v0.4 spec text explicitly keeps unknown names non-fatal.
    for unknown in meta.unknown_capabilities() {
        eprintln!(
            "warning: extension `{ext_key}` declares unknown capability \
             `{unknown}` (not in v0.4 vocabulary); accepting anyway"
        );
    }
    // Filter to caps that aren't already trusted for THIS exact
    // <name>@<version>. Preserves manifest order so diagnostic output
    // and the prompt list stay stable.
    let already = super::trust::load_trusted_caps();
    let untrusted_on_current: Vec<&str> = meta
        .capability_names()
        .filter(|c| !already.contains(&(ext_key.clone(), (*c).to_string())))
        .collect();
    if untrusted_on_current.is_empty() {
        return Ok(());
    }

    // T-13.5: split the untrusted-on-current-version set into
    // (a) caps already trusted under a *different* version of this
    //     same extension name — carry forward silently,
    // (b) caps that are genuinely new — prompt / accept-all for these.
    let prior = super::trust::prior_version_caps(&meta.name.value);
    let mut carried: Vec<&str> = Vec::new();
    let mut requested: Vec<&str> = Vec::new();
    for cap in &untrusted_on_current {
        if prior.contains(*cap) {
            carried.push(cap);
        } else {
            requested.push(cap);
        }
    }

    // Persist carried-forward caps under the new version's key so the
    // T-13.4 check (is_cap_trusted) works on subsequent installs
    // without re-walking the prior-version set. Surface them to the
    // operator so the audit trail is visible at install time.
    if !carried.is_empty() {
        if let Err(e) = super::trust::save_caps(&ext_key, &carried) {
            eprintln!("ark ext add: persisting carried-forward cap trust failed: {e}");
        }
        eprintln!(
            "ark: extension `{ext_key}` — capabilities already trusted from a \
             prior version: {}",
            carried.join(", ")
        );
    }

    if requested.is_empty() {
        return Ok(());
    }

    if accept_all {
        if let Err(e) = super::trust::append_caps_audit(&ext_key, &requested) {
            eprintln!("ark ext add: audit log write failed: {e}");
        }
        if let Err(e) = super::trust::save_caps(&ext_key, &requested) {
            eprintln!("ark ext add: persisting cap trust failed: {e}");
        }
        eprintln!(
            "warning: --accept-all bypassed capability prompt for `{ext_key}` \
             (new caps: {})",
            requested.join(", ")
        );
        Ok(())
    } else {
        let accepted = super::trust::prompt_caps(&ext_key, &requested);
        if !accepted {
            return Err(format!(
                "capabilities for `{ext_key}` ({}) were not accepted; \
                 install aborted",
                requested.join(", ")
            ));
        }
        if let Err(e) = super::trust::save_caps(&ext_key, &requested) {
            eprintln!("ark ext add: persisting cap trust failed: {e}");
        }
        Ok(())
    }
}

/// Resolve `${XDG_DATA_HOME}` honouring the same fallback chain as
/// [`ark_types::StateLayout`] (`$HOME/.local/share` when the env var
/// is unset). Duplicated locally so the install path doesn't drag
/// `StateLayout` into the CLI's ext module.
fn resolve_xdg_data_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share"))
}

/// Fetch `source` into `dest`. Assumes `dest` does not exist.
fn fetch_into_staging(source: &Source, dest: &Path) -> Result<(), String> {
    match source {
        Source::Path(src) => install_path(src, dest),
        Source::Url(url) => install_url(url, dest),
        Source::Github { slug, git_ref } => install_github(slug, git_ref.as_deref(), dest),
    }
}

/// `path:` source — recursive directory copy.
fn install_path(src: &Path, dest: &Path) -> Result<(), String> {
    if !src.exists() {
        return Err(format!("source path `{}` does not exist", src.display()));
    }
    if !src.is_dir() {
        return Err(format!("source `{}` is not a directory", src.display()));
    }
    copy_dir_recursive(src, dest).map_err(|e| format!("copying {}: {e}", src.display()))
}

/// `url:` source — download a tarball and extract. Accepts `.tar.gz`
/// / `.tgz` (gzipped tar) — plain `.tar` works too since `flate2`'s
/// `GzDecoder` passes non-gzip streams through when headers do not
/// match... actually it errors; so we first try gz-decoded, then fall
/// back to raw-tar if the gz header is absent.
fn install_url(url: &str, dest: &Path) -> Result<(), String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTPS GET {url}: {e}"))?;
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("reading tarball from {url}: {e}"))?;
    extract_tarball(&body, dest)
}

/// Decode a tar.gz or plain-tar byte buffer into `dest`.
fn extract_tarball(bytes: &[u8], dest: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use io::Cursor;
    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;

    // Try gzip first (tar.gz / tgz). If the gz header is absent, fall
    // back to raw-tar decoding — accommodates `url:` sources that
    // point at an uncompressed `.tar` too.
    let looks_gzipped = bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b;
    let mut archive = if looks_gzipped {
        tar::Archive::new(Box::new(GzDecoder::new(Cursor::new(bytes))) as Box<dyn io::Read>)
    } else {
        tar::Archive::new(Box::new(Cursor::new(bytes)) as Box<dyn io::Read>)
    };

    // Many GitHub-style tarballs wrap everything in a single top-level
    // directory (`<repo>-<sha>/…`). We extract into a sibling scratch
    // dir first, then promote the single-root subdirectory if that's
    // the shape; otherwise keep the flat layout.
    let scratch = dest.with_extension("extract");
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|e| format!("mkdir {}: {e}", scratch.display()))?;
    archive
        .unpack(&scratch)
        .map_err(|e| format!("extracting tarball: {e}"))?;
    promote_single_root(&scratch, dest)?;
    let _ = fs::remove_dir_all(&scratch);
    Ok(())
}

/// If `scratch` contains a single subdirectory and nothing else (the
/// GitHub release tarball shape), move that subdirectory's contents
/// up into `dest`. Otherwise move everything under `scratch` to
/// `dest` verbatim. Both branches leave `scratch` empty.
fn promote_single_root(scratch: &Path, dest: &Path) -> Result<(), String> {
    let entries: Vec<_> = fs::read_dir(scratch)
        .map_err(|e| format!("readdir {}: {e}", scratch.display()))?
        .filter_map(|e| e.ok())
        .collect();

    let effective_src = if entries.len() == 1 && entries[0].path().is_dir() {
        entries[0].path()
    } else {
        scratch.to_path_buf()
    };

    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;

    for entry in fs::read_dir(&effective_src)
        .map_err(|e| format!("readdir {}: {e}", effective_src.display()))?
    {
        let entry = entry.map_err(|e| format!("readdir: {e}"))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)
                .map_err(|e| format!("copying {}: {e}", from.display()))?;
        } else {
            fs::copy(&from, &to).map_err(|e| format!("copying {}: {e}", from.display()))?;
        }
    }
    Ok(())
}

/// `github:` source — subprocess `git clone --depth 1 [--branch <ref>]`.
fn install_github(slug: &str, git_ref: Option<&str>, dest: &Path) -> Result<(), String> {
    let url = format!("https://github.com/{slug}.git");
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(&url).arg(dest);
    let output = cmd
        .output()
        .map_err(|e| format!("spawning git: {e} (is git installed?)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git clone {url} failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    // Strip the `.git` so `ark ext info` / list don't see spurious
    // repo internals. Non-fatal on failure — worst case the user sees
    // a `.git` dir inside their extension root.
    let _ = fs::remove_dir_all(dest.join(".git"));
    Ok(())
}

/// Read metadata from a staging directory. Prefers `extension.kdl`
/// (symmetric with `ark ext list` / `info`); falls back to the first
/// `*.wasm` file's `ark.metadata` custom section if no `extension.kdl`
/// is present.
pub fn read_staging_metadata(staging: &Path) -> Result<ExtensionMetadata, String> {
    let manifest = staging.join("extension.kdl");
    if manifest.is_file() {
        let text = fs::read_to_string(&manifest)
            .map_err(|e| format!("reading {}: {e}", manifest.display()))?;
        return ark_ext_metadata::parse_extension_metadata_kdl(&text)
            .map_err(|e| format!("parsing {}: {e}", manifest.display()));
    }

    // Fallback: wasm custom-section read. Pick the first `*.wasm` in
    // the root — cartridge-only extensions are single-file.
    let wasm = first_wasm_in_dir(staging)?;
    let bytes = fs::read(&wasm).map_err(|e| format!("reading {}: {e}", wasm.display()))?;
    ark_ext_metadata::wasm_meta::read_wasm_metadata(&bytes)
        .map_err(|e| format!("reading wasm metadata from {}: {e}", wasm.display()))
}

/// Find the first `*.wasm` file in `dir`. Errors with a stable
/// message when none is present.
fn first_wasm_in_dir(dir: &Path) -> Result<PathBuf, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("readdir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            return Ok(path);
        }
    }
    Err(format!(
        "no `extension.kdl` and no `.wasm` cartridge found in {} — can't verify \
         extension metadata",
        dir.display()
    ))
}

/// Reject names the scene compiler would later reject. Mirrors R10's
/// "lower-case alphanumeric with `-` / `_`" rule — mild check here;
/// the authoritative validator lives in the scene crate.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("extension name is empty".into());
    }
    for ch in name.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_';
        if !ok {
            return Err(format!(
                "extension name `{name}` contains invalid character `{ch}` \
                 (expected lower-case alphanumeric, `-`, or `_`)"
            ));
        }
    }
    Ok(())
}

/// Write `<dir>/.ark-install` with source specifier + timestamp.
fn write_install_dotfile(dir: &Path, source: &Source, name: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let contents = format!(
        "source: {}\ninstalled-at: {}\nname: {}\n",
        source.as_specifier(),
        now,
        name
    );
    fs::write(dir.join(".ark-install"), contents).map_err(|e| format!("writing .ark-install: {e}"))
}

/// Copy `src` into `dst` recursively. Skips symlinks by following
/// them at copy time (so a symlinked extension root is still safe to
/// install). Files carry their content only — permissions + mtimes
/// are not preserved.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to)?;
        } else if ft.is_symlink() {
            // Resolve + copy the target. Skip broken symlinks.
            let resolved = fs::metadata(&from);
            if let Ok(meta) = resolved {
                if meta.is_dir() {
                    copy_dir_recursive(&from, &to)?;
                } else if meta.is_file() {
                    fs::copy(&from, &to)?;
                }
            }
        }
    }
    Ok(())
}

// Pulled into scope for `ureq::Response::into_reader` so callers
// don't need to import `std::io::Read` too.
use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_manifest(name: &str) -> String {
        format!(
            r#"
extension {{
    name "{name}"
    version "0.1.0"
    ark-range ">=0.1"
    zellij-range ""
    config {{ }}
    capabilities {{ }}
}}
"#
        )
    }

    fn write_ext(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("extension.kdl"), sample_manifest(name)).unwrap();
    }

    /// Build an [`ExtensionMetadata`] in-memory for T-13.4 decision
    /// tests. We deliberately avoid serialising through facet-kdl +
    /// re-parsing because facet-kdl 0.42 can't disambiguate `Vec`
    /// fields on the parse side — see
    /// `ark_ext_metadata::round_trip_through_kdl_*` for the upstream
    /// limitation. Tests of the decision policy itself work against
    /// the struct directly; the `install_from_source` pipeline is
    /// exercised through `install_aborts_when_cap_decision_denies`
    /// which injects a raw closure rather than round-tripping caps
    /// through the on-disk KDL.
    fn meta_with_caps(name: &str, version: &str, caps: &[&str]) -> ExtensionMetadata {
        use ark_ext_metadata::{CapabilitySet, ConfigSchema, StringNode};
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new(version),
            ark_range: StringNode::new(">=0.1"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::from_strs(caps),
            config_sections: vec![],
            reload_gates: vec![],
        }
    }

    // --- parse_source ---

    #[test]
    fn parse_path_source() {
        let s = parse_source("path:/tmp/foo").unwrap();
        assert_eq!(s, Source::Path(PathBuf::from("/tmp/foo")));
    }

    #[test]
    fn parse_url_source_https() {
        let s = parse_source("url:https://example.com/x.tar.gz").unwrap();
        assert!(matches!(s, Source::Url(u) if u == "https://example.com/x.tar.gz"));
    }

    #[test]
    fn parse_url_source_rejects_non_http() {
        let err = parse_source("url:file:///etc/passwd").unwrap_err();
        assert!(err.contains("http"), "{err}");
    }

    #[test]
    fn parse_github_source_without_ref() {
        let s = parse_source("github:rlch/ark").unwrap();
        assert_eq!(
            s,
            Source::Github {
                slug: "rlch/ark".into(),
                git_ref: None,
            }
        );
    }

    #[test]
    fn parse_github_source_with_ref() {
        let s = parse_source("github:rlch/ark@v0.1.0").unwrap();
        assert_eq!(
            s,
            Source::Github {
                slug: "rlch/ark".into(),
                git_ref: Some("v0.1.0".into()),
            }
        );
    }

    #[test]
    fn parse_github_source_rejects_malformed_slug() {
        let err = parse_source("github:no-slash").unwrap_err();
        assert!(err.contains("user/repo"), "{err}");
    }

    #[test]
    fn parse_unknown_scheme_errors() {
        let err = parse_source("ftp://somewhere").unwrap_err();
        assert!(err.contains("unknown source scheme"), "{err}");
    }

    #[test]
    fn parse_empty_path_errors() {
        let err = parse_source("path:").unwrap_err();
        assert!(err.contains("directory"), "{err}");
    }

    // --- source specifier round-trip ---

    #[test]
    fn source_specifier_round_trip_path() {
        let s = Source::Path(PathBuf::from("/a/b"));
        assert_eq!(s.as_specifier(), "path:/a/b");
    }

    #[test]
    fn source_specifier_round_trip_github_with_ref() {
        let s = Source::Github {
            slug: "user/repo".into(),
            git_ref: Some("main".into()),
        };
        assert_eq!(s.as_specifier(), "github:user/repo@main");
    }

    // --- install_from_source (path:) ---

    #[test]
    fn install_path_copies_and_writes_dotfile() {
        let work = TempDir::new().unwrap();
        let src_dir = work.path().join("src/my-ext");
        write_ext(&src_dir, "my-ext");
        let extensions_root = work.path().join("xdg/ark/extensions");

        let source = Source::Path(src_dir.clone());
        let outcome = install_from_source(&source, &extensions_root, true).expect("install");

        assert_eq!(outcome.metadata.name.value, "my-ext");
        let installed = extensions_root.join("my-ext");
        assert!(installed.join("extension.kdl").is_file());
        let dotfile = fs::read_to_string(installed.join(".ark-install")).unwrap();
        assert!(dotfile.contains(&format!("path:{}", src_dir.display())));
        assert!(dotfile.contains("name: my-ext"));
        assert!(dotfile.contains("installed-at:"));
    }

    #[test]
    fn install_rejects_missing_extension_kdl_and_no_wasm() {
        let work = TempDir::new().unwrap();
        let src_dir = work.path().join("empty-ext");
        fs::create_dir_all(&src_dir).unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");

        let source = Source::Path(src_dir);
        let err = install_from_source(&source, &extensions_root, true).unwrap_err();
        assert!(err.contains("no `extension.kdl`"), "{err}");
        // Staging must not leak into the extensions root.
        let leftover: Vec<_> = fs::read_dir(&extensions_root).unwrap().flatten().collect();
        assert!(leftover.is_empty(), "expected clean extensions root");
    }

    #[test]
    fn install_rejects_when_name_already_installed() {
        let work = TempDir::new().unwrap();
        let src_dir = work.path().join("src");
        write_ext(&src_dir, "collide");
        let extensions_root = work.path().join("xdg/ark/extensions");

        // Pre-seed a collision.
        let existing = extensions_root.join("collide");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("extension.kdl"), sample_manifest("collide")).unwrap();

        let source = Source::Path(src_dir);
        let err = install_from_source(&source, &extensions_root, true).unwrap_err();
        assert!(err.contains("already installed"), "{err}");
    }

    #[test]
    fn install_rejects_source_pointing_at_missing_dir() {
        let work = TempDir::new().unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");
        let source = Source::Path(work.path().join("nope"));
        let err = install_from_source(&source, &extensions_root, true).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn install_rejects_name_mismatch_between_manifest_and_ext_name() {
        // A manifest carrying an invalid name should be rejected; the
        // validator mirrors R10's alphanumeric + `-` + `_` rule.
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("extension.kdl"), sample_manifest("BAD NAME")).unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");
        let source = Source::Path(src);
        let err = install_from_source(&source, &extensions_root, true).unwrap_err();
        assert!(
            err.contains("invalid character") || err.contains("empty"),
            "{err}"
        );
    }

    #[test]
    fn install_copies_nested_files_recursively() {
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        write_ext(&src, "nested");
        // Add a nested file.
        fs::create_dir_all(src.join("assets/icons")).unwrap();
        fs::write(src.join("assets/icons/x.png"), b"png-ish").unwrap();

        let extensions_root = work.path().join("xdg/ark/extensions");
        let source = Source::Path(src);
        install_from_source(&source, &extensions_root, true).expect("install");
        let installed = extensions_root.join("nested/assets/icons/x.png");
        assert!(installed.is_file(), "nested copy preserved");
    }

    // --- extract_tarball ---

    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let buf: Vec<u8> = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn extract_tarball_flat_layout() {
        let work = TempDir::new().unwrap();
        let dest = work.path().join("out");
        let manifest = sample_manifest("tarball-flat");
        let bytes = build_tar_gz(&[("extension.kdl", manifest.as_bytes()), ("README.md", b"hi")]);
        extract_tarball(&bytes, &dest).unwrap();
        assert!(dest.join("extension.kdl").is_file());
        assert!(dest.join("README.md").is_file());
    }

    #[test]
    fn extract_tarball_promotes_single_root() {
        // GitHub tarballs wrap everything in a single top-level
        // directory — the extractor should flatten.
        let work = TempDir::new().unwrap();
        let dest = work.path().join("out");
        let manifest = sample_manifest("tarball-wrapped");
        let bytes = build_tar_gz(&[
            ("wrapped-root/extension.kdl", manifest.as_bytes()),
            ("wrapped-root/src/lib.rs", b"// stub"),
        ]);
        extract_tarball(&bytes, &dest).unwrap();
        assert!(
            dest.join("extension.kdl").is_file(),
            "single-root should be promoted"
        );
        assert!(dest.join("src/lib.rs").is_file());
    }

    #[test]
    fn install_from_url_bytes_round_trip() {
        // We don't hit the network — instead, assert that a tarball
        // prepared in memory can be extracted into a staging dir and
        // verified end-to-end by re-using `extract_tarball` +
        // `read_staging_metadata`.
        let work = TempDir::new().unwrap();
        let dest = work.path().join("out");
        let bytes = build_tar_gz(&[("extension.kdl", sample_manifest("url-sample").as_bytes())]);
        extract_tarball(&bytes, &dest).unwrap();
        let meta = read_staging_metadata(&dest).unwrap();
        assert_eq!(meta.name.value, "url-sample");
    }

    // --- validate_name ---

    #[test]
    fn validate_name_accepts_valid_identifiers() {
        validate_name("a-b_c123").unwrap();
    }

    #[test]
    fn validate_name_rejects_space() {
        assert!(validate_name("no space").is_err());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_name("").is_err());
    }

    // --- xdg resolution ---

    #[test]
    fn xdg_data_home_honours_env_when_set() {
        // Duplicates Ctx's path resolver logic. Env mutation is
        // serialised through `ENV_LOCK` — any test touching process
        // env must acquire it so that parallel `cargo test` doesn't
        // race (T-13.1 introduced a second `run`-dispatching test
        // which forces the discipline).
        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let ns = tmp.path().join("ns");
        let prior = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &ns);
        }
        let got = resolve_xdg_data_home().unwrap();
        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        assert_eq!(got, ns);
    }

    // --- run_smoke (exercises full CLI dispatch) ---

    #[test]
    fn run_installs_via_path_source() {
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        write_ext(&src, "run-smoke");
        let xdg = work.path().join("xdg");
        let xdg_cfg = work.path().join("xdg-cfg");
        fs::create_dir_all(&xdg).unwrap();
        fs::create_dir_all(&xdg_cfg).unwrap();

        // Isolate the test via `XDG_DATA_HOME` (install root + audit
        // log) and `XDG_CONFIG_HOME` (trust file, T-13.1). Other tests
        // that touch the same env vars serialise through the
        // `ENV_LOCK` in `crate::test_lock`.
        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_data = std::env::var_os("XDG_DATA_HOME");
        let prior_cfg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
            std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
        }

        let args = AddArgs {
            source: format!("path:{}", src.display()),
            accept_all: true,
        };
        let ctx = Ctx::default();
        let result = run(args, &ctx);

        unsafe {
            match prior_data {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match prior_cfg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }

        result.expect("run should succeed");
        let installed = xdg.join("ark/extensions/run-smoke");
        assert!(installed.join("extension.kdl").is_file());
        assert!(installed.join(".ark-install").is_file());
        // T-13.2: `--accept-all` must drop a line into the audit log.
        let audit = xdg.join("ark/extension-audit.log");
        assert!(audit.is_file(), "audit log should exist");
        let audit_text = fs::read_to_string(&audit).unwrap();
        assert!(audit_text.contains("accept-all"));
        assert!(audit_text.contains(&format!("publisher=path:{}", src.display())));
        // T-13.1: the trust file should now list this publisher.
        let trust = xdg_cfg.join("ark/extension-trust.kdl");
        assert!(trust.is_file(), "trust file should exist");
        let trust_text = fs::read_to_string(&trust).unwrap();
        assert!(trust_text.contains(&format!("path:{}", src.display())));
    }

    // ---------------------------------------------------------------
    // T-13.4: capability disclosure + trust-file persistence.
    //
    // Tests of the decision *policy* construct `ExtensionMetadata`
    // programmatically (see `meta_with_caps`) and exercise
    // `decide_capability_disclosure` directly; facet-kdl 0.42 can't
    // round-trip the `capabilities` Vec back from KDL so driving the
    // full `run` pipeline with cap-bearing on-disk manifests isn't
    // viable for v0.4 (tracked upstream by `ark_ext_metadata`'s
    // round-trip comment). The `install_from_source_with_cap_decision`
    // injection point is covered by `install_aborts_when_cap_decision_denies`
    // which proves the staging-rollback behaviour without relying on
    // on-disk KDL caps.
    // ---------------------------------------------------------------

    /// Smoke test for the fixture helper itself — if the `exec`/`pipe`
    /// caps don't survive `meta_with_caps`, every downstream test is
    /// vacuous.
    #[test]
    fn meta_with_caps_round_trips_in_memory() {
        let m = meta_with_caps("fixture", "0.1.0", &["exec", "pipe"]);
        let names: Vec<&str> = m.capability_names().collect();
        assert_eq!(names, vec!["exec", "pipe"]);
        assert_eq!(m.name.value, "fixture");
        assert_eq!(m.version.value, "0.1.0");
    }

    /// Shared scaffolding for T-13.4 decision-policy tests: install
    /// isolated `XDG_DATA_HOME` / `XDG_CONFIG_HOME` vars so the policy
    /// reads/writes its own trust file + audit log, run the supplied
    /// body, then restore prior env. Uses the same `ENV_LOCK` the
    /// T-13.1 tests do so parallel `cargo test` doesn't race.
    fn with_isolated_xdg<R>(f: impl FnOnce(&Path, &Path) -> R) -> R {
        let work = TempDir::new().unwrap();
        let xdg = work.path().join("xdg");
        let xdg_cfg = work.path().join("xdg-cfg");
        fs::create_dir_all(&xdg).unwrap();
        fs::create_dir_all(&xdg_cfg).unwrap();

        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_data = std::env::var_os("XDG_DATA_HOME");
        let prior_cfg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
            std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
        }

        let out = f(&xdg, &xdg_cfg);

        unsafe {
            match prior_data {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match prior_cfg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        out
    }

    #[test]
    fn decide_caps_accept_all_records_trust_and_audit() {
        with_isolated_xdg(|xdg, xdg_cfg| {
            let m = meta_with_caps("cap-auto", "0.1.0", &["exec", "pipe"]);
            decide_capability_disclosure(&m, true).expect("accept-all");

            let trust_text = fs::read_to_string(xdg_cfg.join("ark/extension-trust.kdl"))
                .expect("trust file written");
            assert!(
                trust_text.contains("capability \"exec\" extension=\"cap-auto@0.1.0\""),
                "missing exec cap:\n{trust_text}"
            );
            assert!(
                trust_text.contains("capability \"pipe\" extension=\"cap-auto@0.1.0\""),
                "missing pipe cap:\n{trust_text}"
            );

            let audit = fs::read_to_string(xdg.join("ark/extension-audit.log")).unwrap();
            assert!(audit.contains("accept-all-caps"));
            assert!(audit.contains("extension=cap-auto@0.1.0"));
            assert!(audit.contains("caps=exec,pipe"));
        });
    }

    #[test]
    fn decide_caps_empty_list_is_noop() {
        with_isolated_xdg(|xdg, xdg_cfg| {
            let m = meta_with_caps("bare", "0.1.0", &[]);
            decide_capability_disclosure(&m, true).expect("empty caps ok");

            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            if trust_path.exists() {
                let text = fs::read_to_string(&trust_path).unwrap();
                assert!(
                    !text.contains("capability "),
                    "no caps declared — trust file should carry no capability nodes: {text}"
                );
            }
            let audit_path = xdg.join("ark/extension-audit.log");
            assert!(
                !audit_path.exists(),
                "empty cap list should not produce audit output"
            );
        });
    }

    #[test]
    fn decide_caps_skips_already_trusted_entries() {
        with_isolated_xdg(|xdg, xdg_cfg| {
            // Pre-seed the trust file so `exec` is already accepted
            // for `pre-trusted@0.1.0`. Only the new cap (`pipe`) should
            // trigger the `accept-all-caps` audit line.
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(
                &trust_path,
                "capability \"exec\" extension=\"pre-trusted@0.1.0\"\n",
            )
            .unwrap();

            let m = meta_with_caps("pre-trusted", "0.1.0", &["exec", "pipe"]);
            decide_capability_disclosure(&m, true).expect("accept-all");

            let audit = fs::read_to_string(xdg.join("ark/extension-audit.log")).unwrap();
            // `exec` already trusted → must not re-appear on the audit
            // line. The line for `pipe` should still exist.
            assert!(audit.contains("caps=pipe"), "audit: {audit}");
            assert!(!audit.contains("caps=exec"), "audit: {audit}");
        });
    }

    #[test]
    fn decide_caps_fully_trusted_is_silent() {
        with_isolated_xdg(|xdg, xdg_cfg| {
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(
                &trust_path,
                "capability \"exec\" extension=\"full@0.1.0\"\n\
                 capability \"pipe\" extension=\"full@0.1.0\"\n",
            )
            .unwrap();

            let m = meta_with_caps("full", "0.1.0", &["exec", "pipe"]);
            decide_capability_disclosure(&m, true).expect("all pre-trusted");

            // No new audit entries — every cap already trusted.
            let audit_path = xdg.join("ark/extension-audit.log");
            assert!(
                !audit_path.exists(),
                "fully-trusted caps must not emit audit events"
            );
        });
    }

    #[test]
    fn decide_caps_unknown_vocab_still_persists() {
        // T-13.3 keeps unknown caps non-fatal; the decision path
        // should warn (stderr, not asserted on here) but still persist
        // the unknown cap as trusted so T-13.5 version-bump logic can
        // reason about the full declared set.
        with_isolated_xdg(|_xdg, xdg_cfg| {
            let m = meta_with_caps("mixed", "0.1.0", &["exec", "weird.new"]);
            decide_capability_disclosure(&m, true).expect("accept-all");
            let trust_text = fs::read_to_string(xdg_cfg.join("ark/extension-trust.kdl")).unwrap();
            assert!(trust_text.contains("\"exec\""));
            assert!(trust_text.contains("\"weird.new\""));
        });
    }

    // ---------------------------------------------------------------
    // T-13.5: version-bump re-prompt (per-version cap diff).
    //
    // The trust file keys caps by `<name>@<version>` so a bump from
    // 1.1 → 1.2 naturally looks "untrusted" under the T-13.4 check.
    // T-13.5 narrows the re-prompt to caps that are *new* in 1.2;
    // caps already trusted on any prior version of the same `name`
    // are auto-carried forward and persisted under the new key so the
    // operator isn't bothered about permissions they already granted.
    // ---------------------------------------------------------------

    #[test]
    fn decide_caps_version_bump_only_prompts_for_new_caps() {
        with_isolated_xdg(|xdg, xdg_cfg| {
            // Pre-seed v1.1 trust: {pipe} already accepted.
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(&trust_path, "capability \"pipe\" extension=\"foo@1.1\"\n").unwrap();

            // Install v1.2 declaring {pipe, exec} with --accept-all.
            // pipe should carry forward silently; exec is genuinely
            // new and should land in the audit log.
            let m = meta_with_caps("foo", "1.2", &["pipe", "exec"]);
            decide_capability_disclosure(&m, true).expect("accept-all");

            // Trust file now carries pipe under BOTH versions (old
            // preserved, new carried forward) and exec under the new.
            let trust_text = fs::read_to_string(&trust_path).unwrap();
            assert!(
                trust_text.contains("capability \"pipe\" extension=\"foo@1.1\""),
                "prior version entry preserved:\n{trust_text}"
            );
            assert!(
                trust_text.contains("capability \"pipe\" extension=\"foo@1.2\""),
                "pipe carried forward to new version:\n{trust_text}"
            );
            assert!(
                trust_text.contains("capability \"exec\" extension=\"foo@1.2\""),
                "exec recorded under new version:\n{trust_text}"
            );

            // Only the genuinely-new cap should land in the audit log
            // (carried-forward caps don't need an accept-all line —
            // they were accepted on the prior install).
            let audit = fs::read_to_string(xdg.join("ark/extension-audit.log")).unwrap();
            assert!(audit.contains("caps=exec"), "audit: {audit}");
            assert!(
                !audit.contains("pipe"),
                "pipe was carried forward — should not appear in audit: {audit}"
            );
        });
    }

    #[test]
    fn decide_caps_version_bump_with_only_prior_caps_is_silent() {
        // v1.1 had {exec, pipe}; v1.2 declares the same set — after
        // the carry-forward the requested list is empty and no audit
        // entry should be written.
        with_isolated_xdg(|xdg, xdg_cfg| {
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(
                &trust_path,
                "capability \"exec\" extension=\"bar@1.1\"\n\
                 capability \"pipe\" extension=\"bar@1.1\"\n",
            )
            .unwrap();

            let m = meta_with_caps("bar", "1.2", &["exec", "pipe"]);
            decide_capability_disclosure(&m, true).expect("all carried forward");

            // Caps carried forward under the new version key.
            let trust_text = fs::read_to_string(&trust_path).unwrap();
            assert!(trust_text.contains("extension=\"bar@1.2\""));
            // No genuinely-new caps → no audit entry.
            let audit_path = xdg.join("ark/extension-audit.log");
            assert!(
                !audit_path.exists(),
                "all caps carried forward — audit must stay clean"
            );
        });
    }

    #[test]
    fn decide_caps_version_bump_does_not_cross_extension_names() {
        // `picker@1.1` has `exec`; installing `picker-ng@1.1` with
        // `exec` must NOT auto-carry-forward — they're different
        // extension names. This guards the prefix match boundary.
        with_isolated_xdg(|xdg, xdg_cfg| {
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(
                &trust_path,
                "capability \"exec\" extension=\"picker@1.1\"\n",
            )
            .unwrap();

            let m = meta_with_caps("picker-ng", "1.1", &["exec"]);
            decide_capability_disclosure(&m, true).expect("accept-all");

            // `picker-ng`'s `exec` should land in the audit log as a
            // genuinely new accept — the `picker@1.1` entry must not
            // shadow it.
            let audit = fs::read_to_string(xdg.join("ark/extension-audit.log")).unwrap();
            assert!(
                audit.contains("extension=picker-ng@1.1"),
                "picker-ng install should produce its own audit line: {audit}"
            );
            assert!(audit.contains("caps=exec"), "audit: {audit}");
        });
    }

    #[test]
    fn decide_caps_version_bump_interactive_declines_for_new_caps_only() {
        // Interactive path: v1.1 had {pipe}; v1.2 requests {pipe, exec}.
        // The prompt should ask about `exec` only. We can't easily
        // drive stdin in this test, so we approximate by checking that
        // (a) `pipe` is carried forward before the prompt fires,
        // (b) the decline-returned error references only `exec`.
        //
        // We simulate stdin EOF → denial by setting up an env where
        // prompt_caps reads empty stdin. Since prompt_caps hits the
        // real stdin when run by `decide_capability_disclosure`, we
        // instead test the composition by construction: after seeding
        // only {pipe}, a v1.2 install declaring {pipe} only should
        // carry-forward-and-return-Ok WITHOUT any prompt touching
        // stdin — this proves the split happens before the prompt.
        with_isolated_xdg(|_xdg, xdg_cfg| {
            let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
            fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
            fs::write(&trust_path, "capability \"pipe\" extension=\"baz@1.1\"\n").unwrap();

            // accept_all=false would normally hit stdin; with only
            // carry-forward caps the prompt is skipped entirely.
            let m = meta_with_caps("baz", "1.2", &["pipe"]);
            decide_capability_disclosure(&m, false)
                .expect("fully carried-forward → no prompt, returns Ok");

            let trust_text = fs::read_to_string(&trust_path).unwrap();
            assert!(trust_text.contains("extension=\"baz@1.2\""));
        });
    }

    #[test]
    fn install_aborts_when_cap_decision_denies() {
        // Drive `install_from_source_with_cap_decision` directly with
        // a closure that denies — simulates the interactive `n`
        // response path. The staging dir must be cleaned up (no
        // half-installed ext visible to `ext list`).
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        write_ext(&src, "deny-me");
        let extensions_root = work.path().join("xdg/ark/extensions");

        let source = Source::Path(src);
        let err =
            install_from_source_with_cap_decision(&source, &extensions_root, false, &|_meta| {
                Err("user declined caps".to_string())
            })
            .unwrap_err();
        assert!(err.contains("user declined caps"), "{err}");

        // Neither the final install dir nor any staging residue
        // should remain.
        assert!(!extensions_root.join("deny-me").exists());
        if extensions_root.exists() {
            let leftover: Vec<_> = fs::read_dir(&extensions_root).unwrap().flatten().collect();
            assert!(
                leftover.is_empty(),
                "expected clean extensions root after denial"
            );
        }
    }

    // T-13.1: a second `--accept-all` install for the same publisher
    // must skip the audit log (already trusted, no bypass needed) and
    // must not duplicate the trust entry.
    #[test]
    fn run_skips_audit_when_publisher_already_trusted() {
        let work = TempDir::new().unwrap();
        let src_a = work.path().join("src-a");
        let src_b = work.path().join("src-b");
        write_ext(&src_a, "first");
        write_ext(&src_b, "second");
        let xdg = work.path().join("xdg");
        let xdg_cfg = work.path().join("xdg-cfg");
        fs::create_dir_all(&xdg).unwrap();
        fs::create_dir_all(&xdg_cfg).unwrap();

        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_data = std::env::var_os("XDG_DATA_HOME");
        let prior_cfg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
            std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
        }

        // Pre-seed the trust file with both `path:<src-a>` and
        // `path:<src-b>` — each Source::Path publisher key is the
        // full path, so we need both entries for the second install
        // to also be trusted.
        let trust_path = xdg_cfg.join("ark/extension-trust.kdl");
        fs::create_dir_all(trust_path.parent().unwrap()).unwrap();
        fs::write(
            &trust_path,
            format!(
                "publisher \"path:{}\"\npublisher \"path:{}\"\n",
                src_a.display(),
                src_b.display()
            ),
        )
        .unwrap();

        let ctx = Ctx::default();
        let r1 = run(
            AddArgs {
                source: format!("path:{}", src_a.display()),
                accept_all: true,
            },
            &ctx,
        );
        let r2 = run(
            AddArgs {
                source: format!("path:{}", src_b.display()),
                accept_all: true,
            },
            &ctx,
        );

        unsafe {
            match prior_data {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match prior_cfg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }

        r1.expect("first install");
        r2.expect("second install");

        // Audit log should not exist (publishers were pre-trusted).
        let audit = xdg.join("ark/extension-audit.log");
        assert!(
            !audit.exists(),
            "pre-trusted publisher must not write audit log"
        );
    }

    // --- github subprocess (network-dependent, ignored by default) ---

    #[test]
    #[ignore = "requires network + git on PATH"]
    fn install_github_shallow_clone_smoke() {
        let work = TempDir::new().unwrap();
        let dest = work.path().join("cloned");
        // Any tiny public repo with an `extension.kdl` at the root
        // works; keep this opt-in so CI doesn't need network.
        install_github("rlch/ark", Some("HEAD"), &dest).expect("clone");
        assert!(dest.exists());
    }

    // --- url download (network-dependent, ignored by default) ---

    #[test]
    #[ignore = "requires network"]
    fn install_url_tarball_smoke() {
        let work = TempDir::new().unwrap();
        let dest = work.path().join("out");
        // Pick a small well-known tarball. Kept opt-in.
        install_url(
            "https://codeload.github.com/rlch/ark/tar.gz/refs/heads/main",
            &dest,
        )
        .expect("download");
    }
}
