//! "Did you mean?" suggestion utility using Jaro-Winkler similarity.
//!
//! Pure utility module — no AST/IR dependencies. Consumed by scope errors
//! (T-013), op errors (T-052), view errors (T-031), and extension errors
//! (T-093) to surface typo-aware help text in miette diagnostics.

/// Returns up to `max` candidates sorted by descending Jaro-Winkler similarity,
/// filtered by `threshold`.
///
/// Default usage: `suggest(unknown_verb, &known_verbs, 0.75, 3)`.
pub fn suggest(input: &str, candidates: &[&str], threshold: f64, max: usize) -> Vec<String> {
    let mut scored: Vec<(f64, &str)> = candidates
        .iter()
        .filter_map(|&c| {
            let sim = strsim::jaro_winkler(input, c);
            if sim >= threshold {
                Some((sim, c))
            } else {
                None
            }
        })
        .collect();

    // Sort descending by similarity, stable-tie-break by candidate name.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });

    scored.into_iter().take(max).map(|(_, c)| c.to_owned()).collect()
}

/// Formats suggestions as `; did you mean: \`a\`, \`b\`, \`c\`?` for appending
/// to error help text. Returns an empty string when `suggestions` is empty.
pub fn format_suggestions(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        return String::new();
    }

    let joined = suggestions
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ");

    format!("; did you mean: {joined}?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_match() {
        let result = suggest("focsu", &["focus", "close", "rename"], 0.75, 3);
        assert_eq!(result, vec!["focus"]);
    }

    #[test]
    fn threshold_filters_distant() {
        let result = suggest("xyz", &["focus", "close"], 0.75, 3);
        assert!(result.is_empty());
    }

    #[test]
    fn max_limits_results() {
        let candidates = &[
            "focus", "focux", "focusx", "focs", "focas", "focos", "focis",
        ];
        let result = suggest("focus", candidates, 0.75, 2);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn format_suggestions_empty() {
        assert_eq!(format_suggestions(&[]), "");
    }

    #[test]
    fn format_suggestions_single() {
        let suggestions = vec!["focus".to_owned()];
        assert_eq!(format_suggestions(&suggestions), "; did you mean: `focus`?");
    }

    #[test]
    fn format_suggestions_multiple() {
        let suggestions = vec!["focus".to_owned(), "close".to_owned()];
        assert_eq!(
            format_suggestions(&suggestions),
            "; did you mean: `focus`, `close`?"
        );
    }
}
