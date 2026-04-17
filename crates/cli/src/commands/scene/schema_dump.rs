//! `ark scene schema-dump` — emit scene-grammar schema from facet SHAPE.
//!
//! T-12.12 (cavekit-scene R13). Walks `SceneDoc::SHAPE` and emits the
//! full structural schema to stdout. Default format is KDL; `--format json`
//! produces a JSON alternative for non-KDL consumers.
//!
//! ## Migration status
//!
//! This command was migrated from ark-scene v2 to v3 at the Cargo.toml
//! level. The implementation requires the v2-only `schema` module
//! (`generate_schema_kdl`, `generate_schema_json`) that has not yet been
//! ported to the v3 crate. The `run` function is stubbed until that module
//! lands in v3.

use clap::{Args, ValueEnum};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Output format for the schema dump.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SchemaFormat {
    /// KDL format (default) — same shape as `scene.kdl-schema`.
    Kdl,
    /// JSON format — machine-friendly alternative.
    Json,
}

/// Arguments for `ark scene schema-dump`.
#[derive(Debug, Args)]
pub struct SchemaDumpArgs {
    /// Output format: `kdl` (default) or `json`.
    #[arg(long, default_value = "kdl")]
    pub format: SchemaFormat,
}

/// Dispatch handler for `ark scene schema-dump`.
///
/// # Migration note
///
/// The `schema::generate_schema_kdl` / `generate_schema_json` helpers
/// live in the v2-only `schema` module and have not yet been ported to
/// the v3 crate. This stub prints a migration-in-progress message until
/// that module lands.
pub fn run(args: SchemaDumpArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let fmt = match args.format {
        SchemaFormat::Kdl => "kdl",
        SchemaFormat::Json => "json",
    };
    eprintln!(
        "scene schema-dump --format {fmt} — pending v3 migration (schema module not yet ported)"
    );
    Err(CliError::Generic {
        reason: "ark scene schema-dump is pending v3 migration (see T-12.12)".to_string(),
    })
}
