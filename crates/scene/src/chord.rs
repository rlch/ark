//! Chord parsing (T-064 / R5.2).
//!
//! Parses the `chord` argument of a `bind "<chord>" { … }` node into a
//! typed [`Chord`] (set of modifiers + KEY token). Used by:
//!
//! - `compile::keybinds` (T-065) when lowering to zellij KDL.
//! - Scope validation passes that want to surface bad chords at parse
//!   time rather than at session spawn.
//!
//! # Why loose validation
//!
//! Per R5.2 the real authority is zellij's own
//! `KeyWithModifier::from_str`. We deliberately do not depend on
//! `zellij_utils` here — the scene crate is a workspace leaf, and
//! pulling zellij-utils would invert the DAG. The loose grammar below
//! catches empty strings, unknown modifier names, and punctuation in
//! the KEY slot; everything else is handed verbatim to zellij via the
//! rendered `bind "<chord>" { … }` node and surfaces richer errors at
//! session spawn if needed.
//!
//! # Grammar
//!
//! ```text
//! chord  := mod_seq KEY
//! mod    := "Ctrl" | "Alt" | "Shift" | "Super"   (case-insensitive)
//! KEY    := alphanumeric+                        (letters + digits)
//!         | canonical special                    (Tab, Enter, Esc, …)
//!         | F<N> with 1 <= N <= 12               (function keys)
//! ```
//!
//! Tokens are whitespace-separated. The KEY token is the LAST token;
//! every earlier token must be a recognised modifier.

use crate::error::SceneError;
use miette::{NamedSource, SourceSpan};

/// Chord modifier (R5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    /// `Ctrl` / `ctrl` / `CTRL`.
    Ctrl,
    /// `Alt` / `alt` / `ALT`.
    Alt,
    /// `Shift` / `shift` / `SHIFT`.
    Shift,
    /// `Super` / `super` / `SUPER`.
    Super,
}

impl Modifier {
    /// Canonical (TitleCase) rendering — the spelling emitted into
    /// zellij KDL output.
    pub fn as_str(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Super => "Super",
        }
    }

    /// Case-insensitive parse. Returns `None` when the token is not a
    /// recognised modifier name.
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" => Some(Modifier::Ctrl),
            "alt" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            "super" => Some(Modifier::Super),
            _ => None,
        }
    }
}

/// Parsed chord. Modifiers are stored in source order; the KEY token is
/// preserved verbatim (the loose validator only admits alphanumerics
/// + a small set of specials so round-tripping is safe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Modifiers applied to the key, in source order. May be empty for
    /// modifier-less chords (e.g. `"Enter"`, `"a"`).
    pub mods: Vec<Modifier>,

    /// The KEY token as authored (e.g. `"p"`, `"Enter"`, `"F4"`).
    pub key: String,
}

impl Chord {
    /// Render the chord back into the zellij `"<mod> <mod> <KEY>"`
    /// notation. Modifiers in canonical TitleCase; KEY unchanged. The
    /// rendered form is what `compile::keybinds` emits into the
    /// `bind "<chord>" { … }` node.
    pub fn as_zellij_string(&self) -> String {
        let mut out = String::new();
        for m in &self.mods {
            out.push_str(m.as_str());
            out.push(' ');
        }
        out.push_str(&self.key);
        out
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_zellij_string())
    }
}

/// Canonical zellij-known KEY specials. Case-sensitive — matches the
/// zellij lexer. Function keys `F1`–`F12` are handled programmatically
/// via [`function_key_shape`].
const SPECIAL_KEYS: &[&str] = &[
    "Tab", "Enter", "Esc", "Space", "Backspace", "Delete", "Insert", "Home", "End", "PageUp",
    "PageDown", "Left", "Right", "Up", "Down", "CapsLock", "ScrollLock", "NumLock", "PrintScreen",
    "Pause", "Menu",
];

/// Parse a chord source string into a typed [`Chord`] (T-064).
///
/// On failure returns [`SceneError::InvalidChord`] with a
/// human-readable `reason`. The `src`/`span` fields are stubbed for
/// library callers; scene-level validators that want caret-aware
/// errors should wrap with their own `NamedSource`.
#[allow(clippy::result_large_err)]
pub fn parse_chord(src: &str) -> Result<Chord, SceneError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err(invalid_chord(src, "chord string is empty or whitespace-only"));
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    debug_assert!(!tokens.is_empty(), "trim+split yielded empty");

    if tokens.len() == 1 {
        let key = validate_key_token(tokens[0]).map_err(|e| invalid_chord(src, &e))?;
        return Ok(Chord {
            mods: Vec::new(),
            key,
        });
    }

    let mut mods: Vec<Modifier> = Vec::with_capacity(tokens.len() - 1);
    for (i, tok) in tokens.iter().enumerate() {
        if i == tokens.len() - 1 {
            let key = validate_key_token(tok).map_err(|e| invalid_chord(src, &e))?;
            return Ok(Chord { mods, key });
        }
        match Modifier::parse(tok) {
            Some(m) => mods.push(m),
            None => {
                return Err(invalid_chord(
                    src,
                    &format!("unknown modifier `{tok}` (expected one of: Ctrl, Alt, Shift, Super)"),
                ));
            }
        }
    }
    // Unreachable — loop always returns on the final iteration.
    Err(invalid_chord(src, "chord must end with a KEY token"))
}

/// Validate a single KEY token and return it owned.
///
/// Accepts:
/// * pure alphanumeric (`a`, `P`, `9`, `abc`);
/// * a canonical special (see [`SPECIAL_KEYS`]) — case-sensitive;
/// * `F<N>` with 1 <= N <= 12.
fn validate_key_token(tok: &str) -> Result<String, String> {
    if tok.is_empty() {
        return Err("KEY token is empty".to_string());
    }
    if SPECIAL_KEYS.contains(&tok) {
        return Ok(tok.to_string());
    }
    match function_key_shape(tok) {
        FunctionKeyShape::Valid => return Ok(tok.to_string()),
        FunctionKeyShape::OutOfRange => {
            return Err(format!(
                "KEY `{tok}` looks like a function key but the number is out of range 1..=12"
            ));
        }
        FunctionKeyShape::NotFunctionKey => {}
    }
    if tok.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(tok.to_string());
    }
    Err(format!(
        "invalid KEY token `{tok}` (must be alphanumeric, a canonical special, or `F1`-`F12`)"
    ))
}

/// Classify a KEY token against the `F<N>` function-key pattern.
enum FunctionKeyShape {
    Valid,
    OutOfRange,
    NotFunctionKey,
}

fn function_key_shape(tok: &str) -> FunctionKeyShape {
    let Some(rest) = tok.strip_prefix('F') else {
        return FunctionKeyShape::NotFunctionKey;
    };
    if rest.is_empty() {
        return FunctionKeyShape::NotFunctionKey;
    }
    let Ok(n) = rest.parse::<u8>() else {
        return FunctionKeyShape::NotFunctionKey;
    };
    if (1..=12).contains(&n) {
        FunctionKeyShape::Valid
    } else {
        FunctionKeyShape::OutOfRange
    }
}

/// Construct a [`SceneError::InvalidChord`] for `src` with `reason`.
/// The span targets the full chord string so miette renders the caret
/// under the whole token.
fn invalid_chord(src: &str, reason: &str) -> SceneError {
    SceneError::InvalidChord {
        chord: src.to_string(),
        reason: reason.to_string(),
        src: NamedSource::new("<chord>", src.to_string()),
        span: SourceSpan::new(0.into(), src.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- happy path ---

    #[test]
    fn alphanumeric_keys_accepted() {
        for k in ["a", "A", "p", "z", "0", "9", "abc"] {
            let c = parse_chord(k).unwrap_or_else(|e| panic!("`{k}` should accept: {e:?}"));
            assert!(c.mods.is_empty());
            assert_eq!(c.key, k);
        }
    }

    #[test]
    fn single_modifier_plus_key() {
        let c = parse_chord("Alt p").unwrap();
        assert_eq!(c.mods, vec![Modifier::Alt]);
        assert_eq!(c.key, "p");

        let c = parse_chord("Ctrl c").unwrap();
        assert_eq!(c.mods, vec![Modifier::Ctrl]);
        assert_eq!(c.key, "c");
    }

    #[test]
    fn multiple_modifiers_plus_key() {
        let c = parse_chord("Ctrl Alt x").unwrap();
        assert_eq!(c.mods, vec![Modifier::Ctrl, Modifier::Alt]);
        assert_eq!(c.key, "x");

        let c = parse_chord("Ctrl Alt Shift Super p").unwrap();
        assert_eq!(
            c.mods,
            vec![
                Modifier::Ctrl,
                Modifier::Alt,
                Modifier::Shift,
                Modifier::Super
            ]
        );
    }

    #[test]
    fn case_insensitive_modifiers() {
        for chord in ["alt q", "ALT q", "Alt q", "AlT q"] {
            let c = parse_chord(chord).unwrap_or_else(|e| panic!("`{chord}`: {e:?}"));
            assert_eq!(c.mods, vec![Modifier::Alt]);
        }
    }

    #[test]
    fn canonical_specials_accepted() {
        for spec in ["Tab", "Enter", "Esc", "Space", "F1", "F12"] {
            parse_chord(spec).unwrap_or_else(|e| panic!("`{spec}`: {e:?}"));
        }
        parse_chord("Ctrl Enter").unwrap();
    }

    // --- failure path ---

    #[test]
    fn empty_chord_is_error() {
        let err = parse_chord("").unwrap_err();
        assert!(matches!(err, SceneError::InvalidChord { .. }));
    }

    #[test]
    fn whitespace_only_chord_is_error() {
        let err = parse_chord("   \t \n").unwrap_err();
        assert!(matches!(err, SceneError::InvalidChord { .. }));
    }

    #[test]
    fn unknown_modifier_rejected() {
        let err = parse_chord("Hyper p").unwrap_err();
        match err {
            SceneError::InvalidChord { reason, .. } => assert!(reason.contains("Hyper")),
            other => panic!("expected InvalidChord, got {other:?}"),
        }
    }

    #[test]
    fn modifier_in_key_slot_rejected() {
        // "p Alt" — last is KEY (Alt); first `p` isn't a modifier.
        let err = parse_chord("p Alt").unwrap_err();
        match err {
            SceneError::InvalidChord { reason, .. } => assert!(reason.contains("p")),
            other => panic!("expected InvalidChord, got {other:?}"),
        }
    }

    #[test]
    fn function_key_out_of_range_rejected() {
        for tok in ["F0", "F13", "F99"] {
            let err = parse_chord(tok).unwrap_err();
            match err {
                SceneError::InvalidChord { reason, .. } => {
                    assert!(reason.contains("out of range"), "tok `{tok}`: {reason}")
                }
                other => panic!("expected InvalidChord for `{tok}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn key_with_punctuation_rejected() {
        for tok in ["a!", "p@", "x.y", "a-b"] {
            let err = parse_chord(tok).unwrap_err();
            assert!(matches!(err, SceneError::InvalidChord { .. }), "tok `{tok}`");
        }
    }

    // --- rendering ---

    #[test]
    fn chord_renders_roundtrip() {
        let c = parse_chord("Alt p").unwrap();
        assert_eq!(c.as_zellij_string(), "Alt p");
        let c = parse_chord("Ctrl Alt x").unwrap();
        assert_eq!(c.as_zellij_string(), "Ctrl Alt x");
        let c = parse_chord("Enter").unwrap();
        assert_eq!(c.as_zellij_string(), "Enter");
    }
}
