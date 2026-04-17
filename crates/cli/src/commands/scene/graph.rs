//! `ark scene graph` — render attribution tree.
//!
//! T-12.5 (cavekit-scene R13). Shows extensions, plugins, reactions,
//! keybinds, intents — each leaf tagged with origin file:line.
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

/// Arguments for `ark scene graph`.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Path to a scene file. Graphs the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Output format: `text` (ASCII tree) or `json` (for scripts + future lsp).
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
}

/// Dispatch handler for `ark scene graph`.
///
/// # Migration note
///
/// The composition-walking logic (`load_composition`, `FragmentRole`,
/// `SceneSearchCtx`) depends on v2-only APIs not yet ported to ark-scene v3.
/// This stub prints a migration-in-progress message until those APIs land.
pub fn run(args: GraphArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let display = args
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<default>".to_string());
    eprintln!(
        "scene graph: {display} — pending v3 migration (composition / fragment APIs not yet ported)"
    );
    Err(CliError::Generic {
        reason: "ark scene graph is pending v3 migration (see T-12.5)".to_string(),
    })
}
