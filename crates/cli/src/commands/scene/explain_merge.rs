//! `ark scene explain-merge` — trace scene composition per R11.
//!
//! T-12.11 (cavekit-scene R13). Prints which fragment each contribution
//! came from, and for plugins / keybinds, which fragment's value won
//! the merge.
//!
//! ## Migration status
//!
//! This command was migrated from ark-scene v2 to v3 at the Cargo.toml
//! level. The implementation requires v2-only APIs (`extends::SceneSearchCtx`,
//! `merge::load_composition`, `merge::merge_fragments`, `merge::FragmentRole`)
//! that have not yet been ported to the v3 crate. The `run` function is
//! stubbed until those APIs land in v3.

use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene explain-merge`.
#[derive(Debug, Args)]
pub struct ExplainMergeArgs {
    /// Path to a scene file. Explains composition of that scene.
    #[arg(value_name = "SCENE")]
    pub scene: PathBuf,
}

/// Dispatch handler for `ark scene explain-merge`.
///
/// # Migration note
///
/// The composition-walking and merge logic (`load_composition`,
/// `merge_fragments`, `FragmentRole`, `SceneSearchCtx`) depend on
/// v2-only APIs not yet ported to ark-scene v3. This stub prints a
/// migration-in-progress message until those APIs land.
pub fn run(args: ExplainMergeArgs, _ctx: &Ctx) -> Result<(), CliError> {
    eprintln!(
        "scene explain-merge: {} — pending v3 migration (composition / merge APIs not yet ported)",
        args.scene.display()
    );
    Err(CliError::Generic {
        reason: "ark scene explain-merge is pending v3 migration (see T-12.11)".to_string(),
    })
}
