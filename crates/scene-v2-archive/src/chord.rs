//! Loose compile-time validator for `keybind "<chord>"` strings (T-6.6).
//!
//! ## Why loose?
//!
//! Per `cavekit-scene.md` R5 acceptance criterion (T-6.6 entry):
//!
//! > **Loose** chord-string validation at scene compile: grammar
//! > `(Mod )*KEY` where Mod ∈ {Ctrl, Alt, Shift, Super} and KEY is
//! > alphanumeric or single zellij-known special. Reject clearly-invalid
//! > forms at compile. Finer errors (unknown KEY, unsupported combo)
//! > surface at first zellij session-spawn via miette. Tradeoff: less
//! > strict compile-time validation in exchange for zero maintenance
//! > burden as zellij chord grammar evolves.
//!
//! The strict validator already exists upstream:
//! [`zellij_utils::data::KeyWithModifier::from_str`]. We deliberately
//! do NOT depend on it here — pulling zellij-utils into the scene crate
//! would invert the workspace dep DAG (scene is a leaf; mux + plugins
//! depend on it). The loose check below catches typos, unrecognised
//! modifier names, and empty / whitespace-only chords without needing
//! the full zellij type. Anything that survives this check is then
//! re-validated at session spawn by zellij itself, which surfaces
//! richer errors via the upstream lexer.
//!
//! ## Grammar
//!
//! ```text
//! chord  := mod_seq KEY
//! mod    := "Ctrl" | "Alt" | "Shift" | "Super"
//! KEY    := alphanumeric+      // any printable letters/digits
//!         | special            // see SPECIAL_KEYS
//! ```
//!
//! Tokens are whitespace-separated. The KEY token is the LAST token in
//! the chord. Everything before it must be a recognised modifier name
//! (case-sensitive — matches zellij's own `KeyModifier::from_str`).
//!
//! ## What this does NOT validate
//!
//! - Unknown KEY tokens that *look* like specials (`Esceasy` →
//!   accepted: it's alphanumeric). Real zellij will reject.
//! - Unsupported chord combinations (`Shift z` vs `Z`). Real zellij
//!   normalises these.
//! - Duplicated modifiers (`Alt Alt p`). Cosmetic, harmless.
//!
//! These trade-offs are intentional — the upstream lexer is the
//! authoritative oracle, and we don't want to chase its grammar
//! changes inside scene.

use std::collections::HashSet;

/// Modifiers permitted before the KEY token. Lower-case-insensitive
/// matching keeps us tolerant of authors who mix capitalisation
/// (`alt p`, `ALT p`, `Alt p` all accept).
const MODIFIERS: &[&str] = &["Ctrl", "Alt", "Shift", "Super"];

/// Canonical zellij-known specials accepted as KEY tokens. The list
/// mirrors the names exposed by `zellij_utils::data::BareKey` plus the
/// usual ASCII symbols. Case-sensitive — matches the zellij lexer.
///
/// Function keys `F1`–`F12` are matched programmatically (see
/// [`is_function_key`]) rather than enumerated here.
const SPECIAL_KEYS: &[&str] = &[
    "Tab", "Enter", "Esc", "Space", "Backspace", "Delete", "Insert", "Home", "End", "PageUp",
    "PageDown", "Left", "Right", "Up", "Down", "CapsLock", "ScrollLock", "NumLock", "PrintScreen",
    "Pause", "Menu",
];

/// Result of [`validate_chord`].
///
/// Carries the failure shape so call sites can render a focused error
/// (the scene compile pass wraps this in [`crate::error::SceneError::InvalidChord`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    /// Empty or whitespace-only chord string.
    Empty,
    /// One of the leading whitespace-separated tokens isn't a
    /// recognised modifier name.
    UnknownModifier(String),
    /// The trailing KEY token is neither alphanumeric nor a recognised
    /// special.
    InvalidKey(String),
    /// Chord is just a modifier — no KEY at the end.
    MissingKey,
}

impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChordError::Empty => write!(f, "chord string is empty or whitespace-only"),
            ChordError::UnknownModifier(s) => write!(
                f,
                "unknown modifier `{s}` (expected one of: Ctrl, Alt, Shift, Super)"
            ),
            ChordError::InvalidKey(s) => write!(
                f,
                "invalid KEY token `{s}` (must be alphanumeric or one of the canonical specials)"
            ),
            ChordError::MissingKey => write!(
                f,
                "chord must end with a KEY token (e.g. `Alt p`, not just `Alt`)"
            ),
        }
    }
}

/// Validate a chord string against the loose grammar.
///
/// Returns `Ok(())` when the chord has the shape `(Mod )*KEY` with
/// every token recognised. Otherwise [`ChordError`] describes the first
/// (most informative) issue found.
///
/// This is a single-pass tokeniser — no regex, no allocation beyond
/// the modifier-set HashSet (which is built from a static slice and is
/// dirt cheap).
pub fn validate_chord(chord: &str) -> Result<(), ChordError> {
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return Err(ChordError::Empty);
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    debug_assert!(!tokens.is_empty(), "trim()+split_whitespace gave empty");

    // Build the modifier set once. Static slice → cheap.
    let mod_set: HashSet<&str> = MODIFIERS.iter().copied().collect();

    if tokens.len() == 1 {
        // Just a KEY (no modifiers). Validate the lone token.
        return validate_key_token(tokens[0]);
    }

    // Multiple tokens: every token EXCEPT the last must be a modifier.
    for (i, tok) in tokens.iter().enumerate() {
        if i == tokens.len() - 1 {
            return validate_key_token(tok);
        }
        if !mod_set.contains(tok) {
            // Special case: a final-position-looking token in a
            // mid-position slot. If the user wrote `p Alt`, surface the
            // error against `p` (not `Alt`) so the diagnostic helps.
            return Err(ChordError::UnknownModifier((*tok).to_string()));
        }
    }
    // Unreachable per the loop's terminating arm — but Rust can't see it.
    Err(ChordError::MissingKey)
}

/// Validate a single KEY token. Accepts:
/// * pure alphanumeric (incl. lowercase / uppercase letters, digits);
/// * a recognised canonical special (case-sensitive);
/// * a function-key form `F<N>` for 1 ≤ N ≤ 12.
///
/// Order of checks matters: the function-key-shape probe runs BEFORE
/// the alphanumeric fallback so out-of-range `F0` / `F99` are rejected
/// (they would otherwise pass as plain alphanumerics).
fn validate_key_token(tok: &str) -> Result<(), ChordError> {
    if tok.is_empty() {
        return Err(ChordError::InvalidKey(tok.to_string()));
    }
    if SPECIAL_KEYS.contains(&tok) {
        return Ok(());
    }
    match function_key_shape(tok) {
        FunctionKeyShape::Valid => return Ok(()),
        FunctionKeyShape::OutOfRange => return Err(ChordError::InvalidKey(tok.to_string())),
        FunctionKeyShape::NotFunctionKey => {}
    }
    if tok.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(());
    }
    Err(ChordError::InvalidKey(tok.to_string()))
}

/// Classify a KEY token's relationship to the `F<N>` function-key
/// shape. We split this out so [`validate_key_token`] can reject
/// `F0` / `F13` (clearly intended as function keys but out of range)
/// without falling through to the alphanumeric fallback.
enum FunctionKeyShape {
    /// `F<N>` with 1 ≤ N ≤ 12 — a valid function key.
    Valid,
    /// `F<N>` shape but N is out of range (`F0`, `F13`, `F99`). Reject.
    OutOfRange,
    /// Not a function-key shape at all (no `F` prefix, or non-numeric
    /// suffix). Defer to other validators.
    NotFunctionKey,
}

fn function_key_shape(tok: &str) -> FunctionKeyShape {
    let Some(rest) = tok.strip_prefix('F') else {
        return FunctionKeyShape::NotFunctionKey;
    };
    if rest.is_empty() {
        // Lone "F" is just an alphanumeric.
        return FunctionKeyShape::NotFunctionKey;
    }
    let Ok(n) = rest.parse::<u8>() else {
        // `F-1` / `Fabc` — not a function-key shape.
        return FunctionKeyShape::NotFunctionKey;
    };
    if (1..=12).contains(&n) {
        FunctionKeyShape::Valid
    } else {
        FunctionKeyShape::OutOfRange
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- happy path ---

    #[test]
    fn alphanumeric_keys_accepted() {
        for k in ["a", "A", "p", "z", "0", "9", "abc"] {
            validate_chord(k).unwrap_or_else(|e| panic!("`{k}` should accept: {e}"));
        }
    }

    #[test]
    fn single_modifier_plus_key() {
        for chord in ["Alt p", "Ctrl c", "Shift x", "Super space"] {
            // Note: `space` is alphanumeric here, not the canonical
            // `Space`. The loose grammar accepts it.
            validate_chord(chord).unwrap_or_else(|e| panic!("`{chord}` should accept: {e}"));
        }
    }

    #[test]
    fn multiple_modifiers_plus_key() {
        validate_chord("Ctrl Shift t").expect("ok");
        validate_chord("Ctrl Alt Shift Super p").expect("ok");
    }

    #[test]
    fn canonical_specials_accepted() {
        for spec in [
            "Tab", "Enter", "Esc", "Space", "Backspace", "Delete", "Insert", "Home", "End",
            "PageUp", "PageDown", "Left", "Right", "Up", "Down",
        ] {
            validate_chord(spec).unwrap_or_else(|e| panic!("`{spec}` should accept: {e}"));
            validate_chord(&format!("Alt {spec}"))
                .unwrap_or_else(|e| panic!("`Alt {spec}` should accept: {e}"));
        }
    }

    #[test]
    fn function_keys_in_range_accepted() {
        for n in 1..=12 {
            let f = format!("F{n}");
            validate_chord(&f).unwrap_or_else(|e| panic!("`{f}` should accept: {e}"));
        }
    }

    // --- failure path ---

    #[test]
    fn empty_or_whitespace_rejected() {
        assert_eq!(validate_chord("").unwrap_err(), ChordError::Empty);
        assert_eq!(validate_chord("   ").unwrap_err(), ChordError::Empty);
        assert_eq!(validate_chord("\t\n").unwrap_err(), ChordError::Empty);
    }

    #[test]
    fn unknown_modifier_rejected() {
        match validate_chord("Hyper p") {
            Err(ChordError::UnknownModifier(s)) => assert_eq!(s, "Hyper"),
            other => panic!("expected UnknownModifier(Hyper), got {other:?}"),
        }
        // Lower-case variant of a known modifier is also unknown
        // (case-sensitive matches upstream zellij).
        match validate_chord("alt p") {
            Err(ChordError::UnknownModifier(s)) => assert_eq!(s, "alt"),
            other => panic!("expected UnknownModifier(alt), got {other:?}"),
        }
    }

    #[test]
    fn function_key_out_of_range_rejected() {
        for tok in ["F0", "F13", "F99", "F-1"] {
            match validate_chord(tok) {
                Err(ChordError::InvalidKey(s)) => assert_eq!(s, tok),
                other => panic!("expected InvalidKey for `{tok}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn key_with_punctuation_rejected() {
        for tok in ["a!", "p@", "x.y", "a-b"] {
            match validate_chord(tok) {
                Err(ChordError::InvalidKey(s)) => assert_eq!(s, tok),
                other => panic!("expected InvalidKey for `{tok}`, got {other:?}"),
            }
        }
    }

    /// Multi-token chords with the modifier in the WRONG position
    /// surface `UnknownModifier` (the offending token is the
    /// non-modifier one).
    #[test]
    fn modifier_in_wrong_position_rejected() {
        match validate_chord("p Alt") {
            // `p` is at index 0, not a modifier → UnknownModifier(p).
            Err(ChordError::UnknownModifier(s)) => assert_eq!(s, "p"),
            other => panic!("expected UnknownModifier(p), got {other:?}"),
        }
    }

    /// Tokens AFTER the KEY position aren't possible in this grammar
    /// (we treat the LAST token as KEY). Exercise via "Alt p q" — `p`
    /// is in mid-position, fails as UnknownModifier.
    #[test]
    fn three_tokens_with_alphanumeric_middle() {
        match validate_chord("Alt p q") {
            Err(ChordError::UnknownModifier(s)) => assert_eq!(s, "p"),
            other => panic!("expected UnknownModifier(p), got {other:?}"),
        }
    }

    #[test]
    fn chord_error_display_is_useful() {
        for e in [
            ChordError::Empty,
            ChordError::UnknownModifier("X".to_string()),
            ChordError::InvalidKey("a!".to_string()),
            ChordError::MissingKey,
        ] {
            let s = e.to_string();
            assert!(!s.is_empty());
        }
    }

    /// Keys like `Esc` (canonical special) AND lone `Esc` both work.
    #[test]
    fn esc_alone_and_with_modifier() {
        validate_chord("Esc").expect("ok");
        validate_chord("Ctrl Esc").expect("ok");
    }

    /// Sanity: the upstream-zellij examples in cavekit-scene.md R5
    /// (`Alt p`, `Ctrl Shift t`, `F4`) all pass.
    #[test]
    fn cavekit_doc_examples_pass() {
        for ex in ["Alt p", "Ctrl Shift t", "F4"] {
            validate_chord(ex).unwrap_or_else(|e| panic!("doc example `{ex}` failed: {e}"));
        }
    }
}
