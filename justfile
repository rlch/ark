# ark — convenience targets.
#
# Prefer these over hand-rolled `cargo` invocations so wasm plugin
# artifacts land where `ark-cli`'s build.rs expects them (see
# `crates/cli/build.rs` and `context/kits/cavekit-distribution.md` R3).

# Default: list available recipes.
default:
    @just --list

# Build both wasm plugins into target/wasm32-wasip1/release/.
# `cargo build -p ark-cli` will discover + embed them automatically.
wasm:
    cargo build --target wasm32-wasip1 --release \
        -p ark-plugin-status -p ark-plugin-picker

# Same as `wasm`, plus a one-line size summary per artifact.
release-wasm: wasm
    @echo "--- wasm artifact sizes ---"
    @ls -lh target/wasm32-wasip1/release/ark_plugin_status.wasm \
             target/wasm32-wasip1/release/ark_plugin_picker.wasm

# Build the full workspace (no wasm — run `just wasm` first for
# a real plugin embed, or set ARK_BUILD_WASM=1 for inline build).
build:
    cargo build --workspace

# Build ark-cli with inline wasm compilation opted in. Isolated in
# $OUT_DIR/wasm-target so the workspace `target/` stays untouched.
build-with-wasm:
    ARK_BUILD_WASM=1 cargo build -p ark-cli

# Run the full test suite single-threaded (matches CI gating).
test:
    cargo test --workspace -- --test-threads=1

# Run the end-to-end suite (opt-in via ARK_E2E=1).
e2e:
    ARK_E2E=1 cargo test -p ark-cli --test e2e -- --test-threads=1

# Format all code in-place.
fmt:
    cargo fmt --all

# Format check (CI).
fmt-check:
    cargo fmt --all -- --check
