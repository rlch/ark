//! Extension search-path resolver (cavekit-scene R10, T-10.3).
//!
//! Walks the precedence-ordered list of locations where ark looks for an
//! extension by name:
//!
//! 1. `./.ark/extensions/<name>/` — project-local, vendored
//! 2. `${XDG_DATA_HOME}/ark/extensions/<name>/` — user-installed
//! 3. `/usr/share/ark/extensions/<name>/` (or any caller-supplied system
//!    path) — system-installed
//! 4. Built-in list — compiled into the `ark` binary
//!
//! First match wins.
//!
//! The [`resolve_extension_path`] function is **pure** — it takes the
//! environment (`cwd`, `xdg_data_home`, system dirs, built-in list) as
//! arguments rather than reading `std::env`. Callers (CLI, scene
//! compiler) inject the real environment; tests pass fixtures.
//!
//! Scene crate deliberately does NOT depend on this module — filesystem
//! search is a concern of the metadata / CLI layer, not the scene
//! compiler (which deals with an already-resolved ExtensionMetadata).

use std::path::{Path, PathBuf};

/// Where an extension was resolved from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionPath {
    /// Extension found on disk. Path points at the extension ROOT
    /// directory (the one containing `extension.kdl`), not at the
    /// manifest itself.
    File(PathBuf),

    /// Extension is a built-in compiled into the `ark` binary. The
    /// static string is the built-in name (same as the user-facing
    /// name; deduplicated against the input).
    BuiltIn(&'static str),
}

/// Resolve an extension name to the first matching location in the
/// precedence order described in [the module docs](self).
///
/// Pure function — takes the full search environment as arguments:
///
/// * `name` — extension name (the string the user writes in `use
///   "<name>"`).
/// * `cwd` — session CWD. Project-local search rooted here as
///   `<cwd>/.ark/extensions/<name>/`.
/// * `xdg_data_home` — `${XDG_DATA_HOME}`. When `None`, the
///   user-installed tier is skipped (caller decides whether to fall
///   back to `$HOME/.local/share` — this resolver does NOT guess).
/// * `system_dirs` — list of system directories to search in order.
///   Typically `[Path::new("/usr/share/ark/extensions")]`, but callers
///   can pass multiple (XDG `system_data_dirs`, homebrew prefix, etc.).
/// * `builtin` — built-in extension names compiled into the binary.
///   Ark's CLI populates this from a `const`.
///
/// Returns `Some(ExtensionPath::File(root))` if a directory was found on
/// one of the three filesystem tiers, `Some(ExtensionPath::BuiltIn(n))`
/// if the name is in the built-in list, or `None` otherwise.
///
/// An extension directory is considered to exist when both the
/// directory itself AND its `extension.kdl` manifest file are present —
/// empty directories (or stray directories containing unrelated
/// artifacts) do NOT shadow later tiers.
pub fn resolve_extension_path(
    name: &str,
    cwd: &Path,
    xdg_data_home: Option<&Path>,
    system_dirs: &[&Path],
    builtin: &[&'static str],
) -> Option<ExtensionPath> {
    // 1. Project-local
    let project = cwd.join(".ark/extensions").join(name);
    if is_extension_dir(&project) {
        return Some(ExtensionPath::File(project));
    }

    // 2. User-installed
    if let Some(xdg) = xdg_data_home {
        let user = xdg.join("ark/extensions").join(name);
        if is_extension_dir(&user) {
            return Some(ExtensionPath::File(user));
        }
    }

    // 3. System-installed
    for sys in system_dirs {
        let p = sys.join(name);
        if is_extension_dir(&p) {
            return Some(ExtensionPath::File(p));
        }
    }

    // 4. Built-in list
    for &b in builtin {
        if b == name {
            return Some(ExtensionPath::BuiltIn(b));
        }
    }

    None
}

/// Returns `true` iff `path` is a directory containing an
/// `extension.kdl` manifest. Used by [`resolve_extension_path`] to
/// distinguish a real extension directory from an unrelated stray
/// directory that happens to share the name.
fn is_extension_dir(path: &Path) -> bool {
    path.is_dir() && path.join("extension.kdl").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create `<root>/<rel>/extension.kdl` so [`is_extension_dir`]
    /// treats `<root>/<rel>` as a valid extension root.
    fn make_ext(root: &Path, rel: &str) {
        let dir = root.join(rel);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extension.kdl"), b"extension \"placeholder\"").unwrap();
    }

    #[test]
    fn project_local_wins_over_user_and_system() {
        let cwd = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        let sys = TempDir::new().unwrap();

        make_ext(cwd.path(), ".ark/extensions/demo");
        make_ext(xdg.path(), "ark/extensions/demo");
        make_ext(sys.path(), "demo");

        let got = resolve_extension_path(
            "demo",
            cwd.path(),
            Some(xdg.path()),
            &[sys.path()],
            &[],
        )
        .unwrap();
        match got {
            ExtensionPath::File(p) => {
                assert!(
                    p.starts_with(cwd.path()),
                    "expected project-local path, got {}",
                    p.display()
                );
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn user_wins_over_system_and_builtin() {
        let cwd = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        let sys = TempDir::new().unwrap();

        make_ext(xdg.path(), "ark/extensions/demo");
        make_ext(sys.path(), "demo");

        let got = resolve_extension_path(
            "demo",
            cwd.path(),
            Some(xdg.path()),
            &[sys.path()],
            &["demo"],
        )
        .unwrap();
        match got {
            ExtensionPath::File(p) => assert!(p.starts_with(xdg.path())),
            other => panic!("expected user-install File, got {other:?}"),
        }
    }

    #[test]
    fn system_wins_over_builtin_when_user_missing() {
        let cwd = TempDir::new().unwrap();
        let sys = TempDir::new().unwrap();

        make_ext(sys.path(), "demo");

        let got = resolve_extension_path("demo", cwd.path(), None, &[sys.path()], &["demo"])
            .unwrap();
        match got {
            ExtensionPath::File(p) => assert!(p.starts_with(sys.path())),
            other => panic!("expected system-install File, got {other:?}"),
        }
    }

    #[test]
    fn builtin_wins_when_no_filesystem_match() {
        let cwd = TempDir::new().unwrap();
        let got = resolve_extension_path("status", cwd.path(), None, &[], &["status", "picker"])
            .unwrap();
        assert_eq!(got, ExtensionPath::BuiltIn("status"));
    }

    #[test]
    fn missing_extension_returns_none() {
        let cwd = TempDir::new().unwrap();
        let got = resolve_extension_path("nope", cwd.path(), None, &[], &[]);
        assert!(got.is_none());
    }

    #[test]
    fn directory_without_manifest_does_not_shadow_later_tiers() {
        let cwd = TempDir::new().unwrap();
        // Create the directory but NOT the manifest — should not
        // match the project-local tier.
        fs::create_dir_all(cwd.path().join(".ark/extensions/demo")).unwrap();

        let sys = TempDir::new().unwrap();
        make_ext(sys.path(), "demo");

        let got =
            resolve_extension_path("demo", cwd.path(), None, &[sys.path()], &[]).unwrap();
        match got {
            ExtensionPath::File(p) => assert!(
                p.starts_with(sys.path()),
                "expected fallback to system tier, got {}",
                p.display()
            ),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn xdg_data_home_none_skips_user_tier() {
        let cwd = TempDir::new().unwrap();
        let sys = TempDir::new().unwrap();
        make_ext(sys.path(), "demo");

        let got = resolve_extension_path("demo", cwd.path(), None, &[sys.path()], &[]).unwrap();
        match got {
            ExtensionPath::File(p) => assert!(p.starts_with(sys.path())),
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn multiple_system_dirs_searched_in_order() {
        let cwd = TempDir::new().unwrap();
        let sys_a = TempDir::new().unwrap();
        let sys_b = TempDir::new().unwrap();

        // Only second system dir has the extension.
        make_ext(sys_b.path(), "demo");

        let got = resolve_extension_path(
            "demo",
            cwd.path(),
            None,
            &[sys_a.path(), sys_b.path()],
            &[],
        )
        .unwrap();
        match got {
            ExtensionPath::File(p) => assert!(p.starts_with(sys_b.path())),
            other => panic!("expected File, got {other:?}"),
        }
    }
}
