//! T-PP-023 R9: plugin name must match `^[a-z][a-z0-9_]*$`. Uppercase
//! + hyphen is invalid on both counts.
use ark_plugin_sdk::Plugin;

#[derive(Plugin)]
#[plugin(
    name = "Invalid-Name",
    version = "0.1.0",
)]
struct Bad;

fn main() {}
