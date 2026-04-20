//! T-PP-023 R9: the `#[plugin(name = "...")]` attribute must agree
//! with the `world <name>` declaration in the WIT file.
//!
//! This fixture ships a sibling `wit_world_mismatch.wit` whose world
//! is `bar`, but declares `name = "foo"`. The macro must error.
use ark_plugin_sdk::Plugin;

#[derive(Plugin)]
#[plugin(
    name = "foo",
    version = "0.1.0",
    // Path is relative to CARGO_MANIFEST_DIR, which inside trybuild is
    // `$WORKSPACE/target/tests/trybuild/ark-plugin-sdk/` — so we hop up
    // four levels back to the workspace root and reach the real fixture.
    wit = "../../../../crates/ark-plugin-sdk/tests/compile-fail/wit_world_mismatch.wit",
)]
struct Bad;

fn main() {}
