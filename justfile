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
# T-131: prints byte + human-readable size of each artifact afterwards so
# regressions are visible without a second command. Use `just wasm-opt`
# after this to apply the binaryen `wasm-opt -Oz` postprocess.
wasm:
    cargo build --target wasm32-wasip1 --release \
        -p ark-plugin-status -p ark-plugin-picker
    @echo "--- wasm artifact sizes (cavekit-distribution R3 / T-131) ---"
    @ls -l target/wasm32-wasip1/release/ark_plugin_status.wasm \
           target/wasm32-wasip1/release/ark_plugin_picker.wasm | \
        awk '{printf "  %-50s %10d bytes\n", $NF, $5}'
    @ls -lh target/wasm32-wasip1/release/ark_plugin_status.wasm \
            target/wasm32-wasip1/release/ark_plugin_picker.wasm | \
        awk '{printf "  %-50s %s\n", $NF, $5}'

# Same as `wasm` (kept for compatibility; used to be a separate size-summary
# target, now `wasm` itself reports sizes).
release-wasm: wasm

# T-131: binaryen `wasm-opt -Oz --enable-bulk-memory` postprocess on both
# plugin artifacts. Rewrites the .wasm files in-place and prints
# before/after sizes. No-op with a friendly warning if wasm-opt isn't on
# PATH (install via `brew install binaryen` / package manager equivalent).
wasm-opt: wasm
    @command -v wasm-opt >/dev/null 2>&1 || { \
        echo "wasm-opt not on PATH — install binaryen to shrink further"; \
        exit 0; \
    }
    @for artifact in \
        target/wasm32-wasip1/release/ark_plugin_status.wasm \
        target/wasm32-wasip1/release/ark_plugin_picker.wasm; do \
        before=$(stat -f%z "$artifact" 2>/dev/null || stat -c%s "$artifact"); \
        wasm-opt -Oz --enable-bulk-memory "$artifact" -o "$artifact.opt"; \
        mv "$artifact.opt" "$artifact"; \
        after=$(stat -f%z "$artifact" 2>/dev/null || stat -c%s "$artifact"); \
        saved=$((before - after)); \
        echo "wasm-opt -Oz: $artifact: $before → $after bytes (-$saved)"; \
    done

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
