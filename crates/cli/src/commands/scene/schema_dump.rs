//! `ark scene schema-dump` — emit scene-grammar schema from facet SHAPE.
//!
//! T-12.12 (cavekit-scene R13). Walks `SceneDoc::SHAPE` and emits the
//! full structural schema to stdout. Default format is KDL; `--format json`
//! produces a JSON alternative for non-KDL consumers.

use clap::{Args, ValueEnum};

use ark_scene::schema;

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

pub fn run(args: SchemaDumpArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let output = match args.format {
        SchemaFormat::Kdl => schema::generate_schema_kdl(),
        SchemaFormat::Json => schema::generate_schema_json(),
    };
    print!("{output}");
    Ok(())
}
