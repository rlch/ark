//! T-078: Build-site total-task extractor (cavekit-orchestrator-cavekit R4).
//!
//! Correlates an `impl-*.md` filename to the corresponding build-site file
//! under `context/plans/` and counts distinct `T-XXX` task rows. The count
//! becomes the authoritative `Progress.total` for the impl-tracking watcher,
//! falling back to the in-memory row count when no build-site file is present.
//!
//! # Filename correlation
//!
//! Given an `impl_filename` argument:
//!
//! - `impl-overview.md` → tries `build-site-overview.md` first, then falls
//!   back to `build-site.md` (the primary build site).
//! - `impl-{domain}.md` → tries `build-site-{domain}.md` first, then falls
//!   back to `build-site.md`.
//! - Anything unrecognised → falls back to `build-site.md`.
//!
//! Returns `None` when none of the candidate paths exist, when the file
//! cannot be read, or when no `T-XXX` rows are found. **Never panics.**
//!
//! # Row counting
//!
//! A build-site row is a markdown table row whose first cell (after the
//! leading pipe) starts with `T-` followed by one or more ASCII digits:
//!
//! ```text
//! | T-001 | Scaffold cargo workspace ... | M |
//! ```
//!
//! Text-level task references embedded inside other cells — common in
//! "Coverage Matrix" tables where a Tasks column lists `T-001, T-002` — are
//! NOT counted, because the first cell there is something else (e.g. `R1`).
//! Each unique `T-XXX` id is counted once even if duplicated across rows.
//!
//! # Caching note
//!
//! v1 re-reads the build-site file on every debounced re-parse. Build sites
//! change rarely and the file is small (hundreds of rows max), so the cost
//! is negligible. A per-`impl_filename` cache can be added later without
//! API churn.

use std::path::Path;

/// Extract the total task count from the build-site file that correlates
/// with `impl_filename`.
///
/// See the module-level docs for the correlation rule, counting semantics,
/// and failure modes. Returns `None` on any error — this function never
/// panics.
///
/// # Arguments
/// * `cwd` — the orchestrator's working directory. Build-site files are
///   resolved under `{cwd}/context/plans/`.
/// * `impl_filename` — the basename of the impl-tracking file (e.g.
///   `impl-auth.md`). Full paths are also accepted; only the final component
///   is used for correlation.
pub fn extract_build_site_total(cwd: &Path, impl_filename: &str) -> Option<u32> {
    let plans_dir = cwd.join("context").join("plans");

    // Normalize: accept either a bare filename or a path; we only care about
    // the final component.
    let basename = Path::new(impl_filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(impl_filename);

    for candidate in candidates_for(basename) {
        let path = plans_dir.join(&candidate);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let count = count_task_rows(&contents);
            if count > 0 {
                return Some(count);
            }
        }
    }

    None
}

/// Build the ordered candidate list of build-site filenames to try for a
/// given impl filename. The first existing + non-empty file wins.
fn candidates_for(impl_basename: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    if let Some(domain) = extract_domain(impl_basename) {
        if domain == "overview" {
            // overview special-case: explicit variant first, then the bare
            // primary build-site.
            out.push("build-site-overview.md".to_string());
            out.push("build-site.md".to_string());
        } else {
            out.push(format!("build-site-{domain}.md"));
            // Fallback to the primary when the domain-specific file is absent.
            out.push("build-site.md".to_string());
        }
    } else {
        // Malformed/unrecognised impl filename — fall back to primary.
        out.push("build-site.md".to_string());
    }
    out
}

/// Extract the `{domain}` segment from an `impl-{domain}.md` basename.
/// Returns `None` if the shape doesn't match (e.g. `impl.md`, `foo.md`).
fn extract_domain(basename: &str) -> Option<&str> {
    let stem = basename.strip_suffix(".md")?;
    stem.strip_prefix("impl-")
}

/// Count distinct `T-XXX` ids appearing as the leading cell of a markdown
/// table row in `contents`.
///
/// Only the first cell is inspected — cells referencing `T-XXX` anywhere
/// else in the table (e.g. a Coverage Matrix "Tasks" column listing
/// `T-001, T-002`) are ignored. Returns 0 when no qualifying rows exist.
fn count_task_rows(contents: &str) -> u32 {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if !line.starts_with('|') {
            continue;
        }
        // Strip the leading pipe and take everything up to the first `|`
        // — that's the first cell. Trailing pipes on the row don't matter
        // here; we only need the first-cell contents.
        let rest = &line[1..];
        let Some(end) = rest.find('|') else { continue };
        let first = rest[..end].trim();
        if let Some(task_id) = parse_task_id(first) {
            seen.insert(task_id);
        }
    }
    seen.len() as u32
}

/// Return the canonical `T-XXX` form if `cell` is exactly a `T-` prefix
/// followed by one or more ASCII digits; otherwise `None`.
///
/// Rejects cells with extra content (e.g. `T-001, T-002`, `T-001 (partial)`)
/// so that Coverage Matrix rows and free-text references are never counted.
fn parse_task_id(cell: &str) -> Option<String> {
    let rest = cell.strip_prefix("T-")?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("T-{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_plans(tmp: &TempDir, name: &str, contents: &str) {
        let dir = tmp.path().join("context").join("plans");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn primary_build_site_counted() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site.md",
            "# Site\n\n| Task | Title | Effort |\n|--|--|--|\n\
             | T-001 | a | S |\n\
             | T-002 | b | M |\n\
             | T-003 | c | L |\n\
             | T-004 | d | S |\n\
             | T-005 | e | M |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, Some(5));
    }

    #[test]
    fn domain_build_site_counted() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site-foo.md",
            "| T-010 | thing | S |\n\
             | T-011 | other | M |\n\
             | T-012 | last | L |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-foo.md");
        assert_eq!(n, Some(3));
    }

    #[test]
    fn domain_missing_falls_back_to_primary() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site.md",
            "| T-100 | x | S |\n| T-101 | y | S |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-missing-domain.md");
        assert_eq!(n, Some(2));
    }

    #[test]
    fn overview_prefers_explicit_then_bare() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site-overview.md",
            "| T-001 | a | S |\n| T-002 | b | S |\n",
        );
        // Primary build-site also exists but with different count — explicit
        // variant must win.
        write_plans(
            &tmp,
            "build-site.md",
            "| T-500 | z | S |\n| T-501 | y | S |\n| T-502 | w | S |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, Some(2));
    }

    #[test]
    fn no_build_site_returns_none() {
        let tmp = TempDir::new().unwrap();
        // Don't create context/plans/ at all.
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, None);
    }

    #[test]
    fn malformed_file_does_not_panic_and_yields_none() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site.md",
            "not a table at all\n\
             some prose here\n\
             | not-a-task-id | still no | table |\n\
             | header | header | header |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, None);
    }

    #[test]
    fn duplicate_task_ids_counted_once() {
        let tmp = TempDir::new().unwrap();
        write_plans(
            &tmp,
            "build-site.md",
            "| T-001 | first appearance | S |\n\
             | T-002 | b | S |\n\
             | T-001 | second appearance (dup) | S |\n\
             | T-003 | c | S |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, Some(3));
    }

    #[test]
    fn coverage_matrix_rows_are_not_counted() {
        let tmp = TempDir::new().unwrap();
        // Two real task rows, plus a Coverage-Matrix-style block whose first
        // cells are R-IDs and whose Tasks column references T-IDs inline. The
        // text references MUST NOT count.
        write_plans(
            &tmp,
            "build-site.md",
            "## Tier 0\n\n\
             | Task | Title | Effort |\n\
             |--|--|--|\n\
             | T-001 | one | S |\n\
             | T-002 | two | M |\n\n\
             ## Coverage Matrix\n\n\
             | Req | Criterion | Tasks |\n\
             |--|--|--|\n\
             | R1 | something | T-001, T-002, T-003 |\n\
             | R2 | otherthing | T-004 |\n\
             | R3 | third | covered by T-005 and T-006 |\n",
        );
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        // Only T-001 and T-002 appear as leading cells.
        assert_eq!(n, Some(2));
    }

    #[test]
    fn full_path_impl_filename_normalizes() {
        let tmp = TempDir::new().unwrap();
        write_plans(&tmp, "build-site-bar.md", "| T-001 | a | S |\n");
        // Pass a path that includes directories — only the basename matters.
        let n = extract_build_site_total(tmp.path(), "/abs/path/context/impl/impl-bar.md");
        assert_eq!(n, Some(1));
    }

    #[test]
    fn empty_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        write_plans(&tmp, "build-site.md", "");
        let n = extract_build_site_total(tmp.path(), "impl-overview.md");
        assert_eq!(n, None);
    }

    #[test]
    fn parse_task_id_rejects_extra_text() {
        assert_eq!(parse_task_id("T-001"), Some("T-001".to_string()));
        assert_eq!(parse_task_id("T-001, T-002"), None);
        assert_eq!(parse_task_id("T-001 (partial)"), None);
        assert_eq!(parse_task_id("T-"), None);
        assert_eq!(parse_task_id("T-abc"), None);
        assert_eq!(parse_task_id("R-001"), None);
    }

    #[test]
    fn candidates_for_shapes() {
        assert_eq!(
            candidates_for("impl-overview.md"),
            vec!["build-site-overview.md", "build-site.md"]
        );
        assert_eq!(
            candidates_for("impl-auth.md"),
            vec!["build-site-auth.md", "build-site.md"]
        );
        assert_eq!(candidates_for("garbage.md"), vec!["build-site.md"]);
    }
}
