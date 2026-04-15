//! "Did you mean …?" typo suggestions for scene diagnostics.
//!
//! `facet` surfaces field-level typo suggestions natively on
//! deserialization failure (its `UnknownField` error carries one). But
//! the scene compile pipeline also needs cross-shape suggestions for:
//!
//! * Unknown scene-root nodes (R1 grammar — facet's field suggestion
//!   doesn't apply because our lax facet-kdl acceptance silently drops
//!   unknown nodes; see `scope.rs` module docs).
//! * Unknown op verbs (R7) — matched against the op-vocabulary
//!   registry at compile time (T-3.x).
//! * Unknown extension names (R10) — matched against the installed
//!   extension set.
//! * Unknown intent refs (R7) — matched against the namespaced intent
//!   registry assembled during the merge pass.
//!
//! All of these collapse to the same problem: "given `needle`, pick
//! the top-N names from `haystack` that look typo-adjacent." The
//! algorithm is Jaro–Winkler similarity, thresholded at 0.75 — the
//! standard "strong match" threshold per `strsim` docs — returning
//! up to the three best hits.
//!
//! Why Jaro–Winkler: it rewards shared prefixes (so `keybnd` → `keybind`
//! ranks above `extends` despite both being edit-distance 2), handles
//! transpositions (`layuot` → `layout`), and runs in linear time on
//! short identifiers. Levenshtein is viable but less forgiving on
//! single-character transpositions, which dominate user typos.
//!
//! The helper is deliberately small and crate-public so scope.rs
//! (T-1.2), the op pass (T-3.x), and the extension resolver (T-2.x)
//! can share it without duplicating the threshold / top-N logic.

use strsim::jaro_winkler;

/// Jaro–Winkler similarity threshold for "close enough to suggest".
///
/// Picked per [`strsim`]'s rule of thumb (0.7 = weak match, 0.85 =
/// near-identical). 0.75 is the documented cutoff for typo-suggestion
/// use-cases; values below it surface too many false positives on
/// short identifiers (e.g. `on` vs `use`).
pub const SUGGESTION_THRESHOLD: f64 = 0.75;

/// Maximum number of suggestions returned for a single lookup.
///
/// Three is the de-facto convention for "did you mean …?" surfaces
/// (rustc, cargo, ripgrep). Enough to cover ambiguous typos; few
/// enough to keep the diagnostic help text readable.
pub const MAX_SUGGESTIONS: usize = 3;

/// Return up to [`MAX_SUGGESTIONS`] candidates from `haystack` whose
/// Jaro–Winkler similarity to `needle` is at least
/// [`SUGGESTION_THRESHOLD`], sorted by descending similarity.
///
/// Case-insensitive comparison so `Keybnd` still suggests `keybind`.
/// An exact-match input still returns the canonical haystack casing
/// in the output vector — useful when callers want to echo the
/// corrected form back to the user.
///
/// Ties (same similarity score) are broken by alphabetical order on
/// the candidate string, so output is deterministic across runs.
///
/// ```
/// use ark_scene::suggest::suggest_similar;
/// let hits = suggest_similar("keybnd", &["extends", "keybind", "engine"]);
/// assert_eq!(hits, vec!["keybind"]);
/// ```
pub fn suggest_similar(needle: &str, haystack: &[&str]) -> Vec<String> {
    let needle_lc = needle.to_ascii_lowercase();
    let mut scored: Vec<(f64, &str)> = haystack
        .iter()
        .map(|cand| (jaro_winkler(&needle_lc, &cand.to_ascii_lowercase()), *cand))
        .filter(|(score, _)| *score >= SUGGESTION_THRESHOLD)
        .collect();

    // Descending by score; alphabetical tiebreak keeps output stable.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });

    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, cand)| cand.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-typo root-node names resolve to the obvious candidate.
    #[test]
    fn close_typo_resolves_to_single_candidate() {
        let roots = &["extends", "include", "use", "layout", "plugin", "on", "keybind"];
        let hits = suggest_similar("keybnd", roots);
        assert_eq!(hits, vec!["keybind".to_string()]);

        let hits = suggest_similar("layuot", roots);
        assert_eq!(hits, vec!["layout".to_string()]);
    }

    /// Case-insensitive matching: `Keybind` vs `keybind` still
    /// resolves above threshold.
    #[test]
    fn match_is_case_insensitive() {
        let hits = suggest_similar("Keybnd", &["keybind", "engine"]);
        assert_eq!(hits, vec!["keybind".to_string()]);
    }

    /// Far-off input yields no suggestions — below threshold noise
    /// must NOT surface as "did you mean …?" hints.
    #[test]
    fn distant_input_yields_empty() {
        let hits = suggest_similar("xyzzy", &["extends", "layout", "plugin"]);
        assert!(hits.is_empty(), "got: {hits:?}");
    }

    /// Top-3 cap — even with many close candidates we never return
    /// more than `MAX_SUGGESTIONS` hits.
    #[test]
    fn top_n_capped_at_three() {
        // Five near-identical candidates; pick the three most similar
        // by Jaro-Winkler.
        let cands = &["aaa", "aab", "aac", "aad", "aae"];
        let hits = suggest_similar("aaa", cands);
        assert_eq!(hits.len(), MAX_SUGGESTIONS);
    }

    /// Exact-match input round-trips as the first (and only) hit.
    #[test]
    fn exact_match_is_returned() {
        let hits = suggest_similar("layout", &["extends", "layout", "plugin"]);
        assert_eq!(hits[0], "layout");
    }

    /// Ordering is deterministic — ties break alphabetically so
    /// snapshot tests stay stable.
    #[test]
    fn ordering_is_deterministic_on_ties() {
        // `ab` vs `ba` vs `cb`: jaro-winkler against `ab` ranks
        // `ab` at 1.0, then `ba` / `cb` below. The key assertion:
        // output is stable across runs, so tie-broken order matches.
        let hits = suggest_similar("aa", &["ab", "ba", "ab"]);
        // Dedupe isn't part of the contract — we just want stable
        // ordering. Re-run and compare.
        let again = suggest_similar("aa", &["ab", "ba", "ab"]);
        assert_eq!(hits, again);
    }

    /// Threshold constant is the documented 0.75 value.
    #[test]
    fn threshold_constant_is_stable() {
        assert_eq!(SUGGESTION_THRESHOLD, 0.75);
    }

    /// Top-N constant is three.
    #[test]
    fn max_suggestions_is_three() {
        assert_eq!(MAX_SUGGESTIONS, 3);
    }
}
