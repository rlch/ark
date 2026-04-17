//! `ark scene explain <ref>` — trace resolution of a specific ref.
//!
//! T-12.6 (cavekit-scene R13). Refs: `intent:<name>`,
//! `keybind:<chord>`, `plugin:<name>`, `reaction:<event-selector>`,
//! `ext:<name>`. Prints "defined at <file:line>; overridden by
//! <file:line>; final resolution: <origin>".
//!
//! ## Migration status
//!
//! This command was migrated from ark-scene v2 to v3 at the Cargo.toml
//! level. The implementation requires v2-only APIs (`extends::SceneSearchCtx`,
//! `merge::load_composition`, `merge::FragmentRole`) that have not yet been
//! ported to the v3 crate. The `run` function is stubbed until those APIs
//! land in v3.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene explain`.
#[derive(Debug, Args)]
#[command(
    about = "Trace resolution of a single ref across the composed scene",
    long_about = "Given a ref like `intent:<name>`, `keybind:<chord>`,\n\
                  `plugin:<name>`, `reaction:<selector>`, or `ext:<name>`,\n\
                  print every fragment that defined the ref plus the\n\
                  final merge-resolved origin.\n\
                  \n\
                  Examples:\n  \
                  ark scene explain intent:picker.show\n  \
                  ark scene explain keybind:'Alt p'\n  \
                  ark scene explain plugin:picker\n  \
                  ark scene explain reaction:Started\n  \
                  ark scene explain ext:aider-adapter"
)]
pub struct ExplainArgs {
    /// Ref to explain. Forms: `intent:<name>`, `keybind:<chord>`,
    /// `plugin:<name>`, `reaction:<selector>`, `ext:<name>`.
    #[arg(required = true, value_name = "REF")]
    pub reference: String,

    /// Path to a scene file. Uses the default scene when omitted.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

/// Parsed form of the user's ref argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    /// `intent:<name>` — dispatches an intent identifier.
    Intent(String),
    /// `keybind:<chord>` — key chord binding (R5 last-wins).
    Keybind(String),
    /// `plugin:<name>` — zellij wasm plugin declaration (R6).
    Plugin(String),
    /// `reaction:<selector>` — `on "<selector>"` reaction (R4).
    Reaction(String),
    /// `ext:<name>` — everything an extension contributed.
    Ext(String),
}

impl Ref {
    /// Short category label used in headers.
    fn category(&self) -> &'static str {
        match self {
            Ref::Intent(_) => "intent",
            Ref::Keybind(_) => "keybind",
            Ref::Plugin(_) => "plugin",
            Ref::Reaction(_) => "reaction",
            Ref::Ext(_) => "ext",
        }
    }

    /// The ref's payload (everything after the `<category>:` prefix).
    fn value(&self) -> &str {
        match self {
            Ref::Intent(v)
            | Ref::Keybind(v)
            | Ref::Plugin(v)
            | Ref::Reaction(v)
            | Ref::Ext(v) => v,
        }
    }
}

/// Parse a `<category>:<value>` ref specifier.
///
/// Whitespace inside `<value>` is preserved verbatim so chord refs like
/// `keybind:Alt p` work without shell-quoting tricks. Missing value or
/// unknown category produces a user-facing error string.
pub fn parse_ref(raw: &str) -> Result<Ref, String> {
    let (prefix, rest) = raw
        .split_once(':')
        .ok_or_else(|| format!(
            "missing ref prefix in `{raw}` (expected `intent:`, `keybind:`, \
             `plugin:`, `reaction:`, or `ext:`)"
        ))?;
    if rest.is_empty() {
        return Err(format!("empty ref value after `{prefix}:`"));
    }
    let value = rest.to_string();
    match prefix {
        "intent" => Ok(Ref::Intent(value)),
        "keybind" => Ok(Ref::Keybind(value)),
        "plugin" => Ok(Ref::Plugin(value)),
        "reaction" => Ok(Ref::Reaction(value)),
        "ext" => Ok(Ref::Ext(value)),
        other => Err(format!(
            "unknown ref category `{other}:` (expected `intent:`, `keybind:`, \
             `plugin:`, `reaction:`, or `ext:`)"
        )),
    }
}

/// Dispatch handler for `ark scene explain`.
///
/// # Migration note
///
/// The composition-walking logic (`load_composition`, `FragmentRole`,
/// `SceneSearchCtx`) depends on v2-only APIs not yet ported to ark-scene v3.
/// This stub validates the ref argument and prints a migration-in-progress
/// message until those APIs land.
pub fn run(args: ExplainArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let reference = parse_ref(&args.reference).map_err(|reason| CliError::Generic {
        reason: format!("scene/explain: {reason}"),
    })?;
    let _ = args.file;
    eprintln!(
        "scene explain {}:{} — pending v3 migration (composition / merge APIs not yet ported)",
        reference.category(),
        reference.value()
    );
    Err(CliError::Generic {
        reason: "ark scene explain is pending v3 migration (see T-12.6)".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_intent_ref() {
        assert_eq!(parse_ref("intent:picker.show").unwrap(), Ref::Intent("picker.show".into()));
    }

    #[test]
    fn parse_keybind_ref_preserves_whitespace() {
        assert_eq!(parse_ref("keybind:Alt p").unwrap(), Ref::Keybind("Alt p".into()));
    }

    #[test]
    fn parse_plugin_ref() {
        assert_eq!(parse_ref("plugin:picker").unwrap(), Ref::Plugin("picker".into()));
    }

    #[test]
    fn parse_reaction_ref() {
        assert_eq!(parse_ref("reaction:Started").unwrap(), Ref::Reaction("Started".into()));
    }

    #[test]
    fn parse_ext_ref() {
        assert_eq!(parse_ref("ext:aider").unwrap(), Ref::Ext("aider".into()));
    }

    #[test]
    fn parse_ref_rejects_missing_colon() {
        let err = parse_ref("pickerOnly").unwrap_err();
        assert!(err.contains("missing ref prefix"), "{err}");
    }

    #[test]
    fn parse_ref_rejects_empty_value() {
        let err = parse_ref("intent:").unwrap_err();
        assert!(err.contains("empty ref value"), "{err}");
    }

    #[test]
    fn parse_ref_rejects_unknown_category() {
        let err = parse_ref("engine:claude").unwrap_err();
        assert!(err.contains("unknown ref category"), "{err}");
    }
}
