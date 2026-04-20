//! T-PP-023 R9: version must parse as semver 2.0.0.
use ark_plugin_sdk::Plugin;

#[derive(Plugin)]
#[plugin(
    name = "bad_ver",
    version = "not-a-version",
)]
struct Bad;

fn main() {}
