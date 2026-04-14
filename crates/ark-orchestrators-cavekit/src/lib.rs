//! CavekitOrchestrator — detect-only scaffold for T-075.
//!
//! Implements cavekit-orchestrator-cavekit.md R1 (detection). The remaining
//! requirements (R2–R9: engine/run/watchers/review-tab spawn/etc.) land in
//! later tasks (T-076 onwards); for this task we expose only the `detect`
//! function.
//!
//! ## Detection heuristics
//!
//! `detect(cwd)` returns `true` when the directory looks cavekit-managed.
//! Any of the following is sufficient:
//!
//! 1. `cwd/context/sites/*.md` contains at least one file.
//! 2. `cwd/context/plans/*.md` contains at least one file AND at least one
//!    of those markdown files contains the string `"build-site"` or
//!    `"Tier "` (heuristic to separate cavekit build sites from generic
//!    plan docs).
//! 3. `cwd/.cavekit/config` exists (regular file).
//! 4. `cwd/context/kits/cavekit-*.md` contains at least one file.
//!
//! All I/O errors (permission denied, missing intermediate paths, unreadable
//! files) are swallowed and cause `detect` to return `false`. We do not
//! panic.

use std::fs;
use std::path::Path;

/// Return `true` when `cwd` matches any of the cavekit detection heuristics.
pub fn detect(cwd: &Path) -> bool {
    // Rule 3: .cavekit/config — cheapest, check first.
    if is_file(&cwd.join(".cavekit").join("config")) {
        return true;
    }

    // Rule 1: context/sites/*.md
    if any_md_file(&cwd.join("context").join("sites")) {
        return true;
    }

    // Rule 4: context/kits/cavekit-*.md
    if any_cavekit_kit(&cwd.join("context").join("kits")) {
        return true;
    }

    // Rule 2: context/plans/*.md containing "build-site" or "Tier "
    if any_plan_with_buildsite_marker(&cwd.join("context").join("plans")) {
        return true;
    }

    false
}

fn is_file(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Any `*.md` regular file in `dir`. Errors → `false`.
fn any_md_file(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(meta) = fs::metadata(&path) {
                if meta.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// Any `cavekit-*.md` regular file in `dir`. Errors → `false`.
fn any_cavekit_kit(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("cavekit-") {
            continue;
        }
        if let Ok(meta) = fs::metadata(&path) {
            if meta.is_file() {
                return true;
            }
        }
    }
    false
}

/// Any `*.md` in `dir` whose contents contain either `"build-site"` or
/// `"Tier "`. Errors → `false`.
fn any_plan_with_buildsite_marker(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if fs::metadata(&path).map(|m| !m.is_file()).unwrap_or(true) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if contents.contains("build-site") || contents.contains("Tier ") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_tempdir_returns_false() {
        let dir = TempDir::new().unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn plans_with_buildsite_marker_matches() {
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(
            plans.join("build-site.md"),
            "# Build Site\n\nTier 0 — Foundations\n",
        )
        .unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn plans_with_tier_but_no_buildsite_text_matches() {
        // "Tier " alone is sufficient.
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(plans.join("plan.md"), "# Plan\n\nTier 0 foundation.\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn plans_with_generic_markdown_does_not_match() {
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(plans.join("notes.md"), "just some notes\n").unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn cavekit_config_file_matches() {
        let dir = TempDir::new().unwrap();
        let cav = dir.path().join(".cavekit");
        fs::create_dir_all(&cav).unwrap();
        fs::write(cav.join("config"), "caveman_mode=on\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn cavekit_config_directory_does_not_match() {
        // `.cavekit/config` must be a regular file.
        let dir = TempDir::new().unwrap();
        let cav = dir.path().join(".cavekit").join("config");
        fs::create_dir_all(&cav).unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn cavekit_kit_file_matches() {
        let dir = TempDir::new().unwrap();
        let kits = dir.path().join("context").join("kits");
        fs::create_dir_all(&kits).unwrap();
        fs::write(kits.join("cavekit-foo.md"), "# foo\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn non_cavekit_kit_does_not_match() {
        let dir = TempDir::new().unwrap();
        let kits = dir.path().join("context").join("kits");
        fs::create_dir_all(&kits).unwrap();
        fs::write(kits.join("other-foo.md"), "# foo\n").unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn sites_directory_with_md_matches() {
        let dir = TempDir::new().unwrap();
        let sites = dir.path().join("context").join("sites");
        fs::create_dir_all(&sites).unwrap();
        fs::write(sites.join("my-site.md"), "# site\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn sites_directory_empty_does_not_match() {
        let dir = TempDir::new().unwrap();
        let sites = dir.path().join("context").join("sites");
        fs::create_dir_all(&sites).unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn does_not_panic_on_missing_intermediate_paths() {
        // No `context` dir at all — every read_dir returns Err.
        let dir = TempDir::new().unwrap();
        assert!(!detect(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_returns_false_without_panic() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        // Make plans unreadable.
        let mut perms = fs::metadata(&plans).unwrap().permissions();
        perms.set_mode(0o000);
        // On macOS, setting 0o000 on a directory owned by root may still allow
        // the owning user to read. Best-effort only.
        let applied = fs::set_permissions(&plans, perms).is_ok();

        let result = detect(dir.path());

        // Restore so tempdir cleanup works.
        let mut restore = fs::metadata(&plans).unwrap().permissions();
        restore.set_mode(0o755);
        let _ = fs::set_permissions(&plans, restore);

        if applied {
            assert!(!result, "expected false on unreadable dir, got true");
        }
    }
}
