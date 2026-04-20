//! T-PP-023 R14: `abi` must equal `ark_types::ARK_ABI_VERSION`.
use ark_plugin_sdk::Plugin;

#[derive(Plugin)]
#[plugin(
    name = "bad_abi",
    version = "0.1.0",
    abi = 9999,
)]
struct Bad;

fn main() {}
