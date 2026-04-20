---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-16"
---

# Spec: Distribution

## Scope
Building, packaging, and shipping ark. Cargo workspace layout, `cargo-dist` release automation, homebrew formula, GitHub Releases, wasm plugin embedding, install flow + `ark doctor --fix` first-run experience.

## Requirements

### R1: Cargo workspace
**Description:** Monorepo layout with clear crate boundaries.
**Acceptance Criteria:**
- [ ] `Cargo.toml` workspace at repo root with `[workspace.package]` common fields (version, license = MIT, authors, edition = 2024)
- [ ] Crates under `crates/`:
  - `ark-types` — shared types (AgentId, AgentSpec, AgentEvent, etc.)
  - `ark-core` — supervisor, event bus, state dir, traits
  - `ark-engines-claude-code` — ClaudeCodeEngine
  - `ark-orchestrators-cavekit` — CavekitOrchestrator
  - `ark-orchestrators-claude-code` — ClaudeCodeOrchestrator
  - `ark-mux-zellij` — ZellijMux
  - `ark-pane` — pane subcommand binaries (consolidated into one via subcommand routing)
  - `ark-hook` — sidecar binary
  - `ark-cli` — top-level binary (`ark`)
  - `ark-plugin-status` — wasm crate (cdylib, wasm32-wasip1)
  - `ark-plugin-picker` — wasm crate (cdylib, wasm32-wasip1)
  - `ark-test-fixtures` — test helpers
- [ ] Binary crates (cli, hook, pane) compile to `target/release/ark`, `ark-hook`, etc.
- [ ] `ark pane` handled by `ark-cli` via subcommand routing — not a separate binary (avoids PATH pollution and extra install burden); internally routes to `ark-pane` crate's implementations. Alternative: separate `ark-pane` binary if clap ergonomics improve. Decision at impl time.
- [ ] Wasm crates build via `cargo build --target wasm32-wasip1 --release` (tracked in CI)
**Dependencies:** none

### R2: Release automation via cargo-dist
**Description:** GitHub Releases built from tags via cargo-dist.
**Acceptance Criteria:**
- [ ] `cargo dist init` produces `.github/workflows/release.yml` + `dist-workspace.toml`
- [ ] Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
- [ ] Published artifacts per release:
  - Prebuilt tarballs for each target containing `ark`, `ark-hook` (and embedded wasm inside `ark` binary — see R3)
  - SHA256 checksums (cargo-dist default)
- [ ] Tag-driven: push a `vX.Y.Z` tag → release builds + uploads automatically
- [ ] Homebrew formula auto-updated via `cargo-dist`'s brew integration to a `homebrew-ark` tap repo
**Dependencies:** R1

### R3: Wasm plugin embedding

> **AMENDED (2026-04-20)** by `cavekit-plugin-protocol.md` R12. Once the wasm-component plugin protocol lands, `ark-plugin-status` and `ark-plugin-picker` are rebuilt as ark-native wasm-component plugins (loaded by ark-host's own wasmtime, not by zellij). The embedding mechanism stays — the host still ships the plugins inside the `ark` binary so the install footprint stays single-artifact — but the build-time path changes (component-model build, no facet-kdl pulled into the guest, no `~/.config/zellij/plugins/` reconciliation), and runtime loading goes through plugin-protocol R8's three-phase loader instead of zellij's plugin runtime. This R3 stays as the embedding contract; the plugin-protocol kit owns the loader contract. Until the migration completes, the existing zellij-tile build path is the transitional implementation.

**Description:** Ship wasm plugins inside the `ark` binary.
**Acceptance Criteria:**
- [ ] `ark-cli` crate has a `build.rs` that:
  - Runs `cargo build --target wasm32-wasip1 --release -p ark-plugin-status`
  - Runs same for `ark-plugin-picker`
  - Copies resulting `.wasm` files to `$OUT_DIR/wasm/`
  - Embeds them via `include_bytes!("...")` in a module
- [ ] `ark doctor --fix` writes these bytes to `~/.config/zellij/plugins/ark-{status,picker}.wasm`
- [ ] **Size posture:** no hard byte budget (zellij-tile floor is ~500 KB raw / ~480 KB after wasm-opt; comparable plugins land 1-2 MB). Instead: CI regression check fails the PR if either plugin grows >25% vs main. Track absolute size in release notes.
- [ ] **Required size-reduction stack** (build defaults for both wasm crates):
  - `[profile.release] opt-level = "z", lto = "fat", codegen-units = 1, strip = true, panic = "abort"`
  - Postprocess via `wasm-opt -Oz --enable-bulk-memory` (saves ~15-20%)
  - `default-features = false` on every dep that supports it
  - Avoid `serde_json`, `chrono`, `regex`, `url`, `uuid`, `humantime` in plugins (collectively ~200-400 KB)
  - For picker fuzzy matching: use `nucleo-matcher` (leaf deps: `memchr` only), NOT `fuzzy-matcher` or full `nucleo` crate
- [ ] A standalone release asset also uploads the raw `.wasm` files (for users who want to install manually)
**Dependencies:** cavekit-plugin-status, cavekit-plugin-picker

### R4b: Bundled zellij
**Description:** Ark ships its own zellij binary to control the exact version users get — scene compilation depends on specific zellij KDL features (e.g., suppressed-pane API, `MessagePlugin` action, merge semantics) that shift between releases.
**Acceptance Criteria:**
- [ ] Prebuilt zellij binary bundled into each cargo-dist artifact at `share/ark/bin/zellij`. Version pin tracked in `Cargo.toml` at workspace level.
- [ ] Ark always launches the bundled binary by absolute path (no PATH lookup); this prevents accidental downgrade if user has an older zellij on PATH.
- [ ] `ARK_USE_SYSTEM_ZELLIJ=1` env var overrides to use system `zellij` from PATH — for dev iteration only, not a supported end-user workflow. When set, emit a debug log at spawn naming the resolved binary.
- [ ] `ark doctor` reports bundled-zellij version + checks for version skew against user's `~/.config/zellij/config.kdl` (warns if user config uses a feature newer than bundled zellij; zellij merges additively, so generally compatible).
- [ ] Homebrew formula depends on `zellij` *as well* (some users prefer the tap, and system zellij remains useful for `ark --no-scene` fallback paths); the bundled copy takes precedence at runtime.
**Dependencies:** R1, R2, cavekit-mux-zellij

### R4c: ACP crate pin
**Description:** Ark pins a specific version of the `agent-client-protocol` Rust crate; an ark release's ACP compatibility is whatever range that pin declares.
**Acceptance Criteria:**
- [ ] Workspace dependency `agent-client-protocol = "=X.Y.Z"` (equality pin, not range) so every build uses the identical ACP implementation. **ark v0.1.0 ships `agent-client-protocol = "=0.10.4"`** (T-ACP.1).
- [ ] Release notes for each ark version cite the bundled ACP crate version + supported `protocolVersion` range declared at `initialize`.
- [ ] `ark doctor` prints the bundled ACP version in its diagnostic header.
- [ ] Unstable ACP methods (from `meta.unstable.json`: `session/fork`, `nes/*`, `elicitation/*`, `document/did*`, etc.) are NOT shipped as v1 surface; gated behind capability negotiation per scene R17.
**Dependencies:** cavekit-scene R17

### R4: Homebrew and package managers
**Description:** Primary install paths.
**Acceptance Criteria:**
- [ ] Homebrew: `brew install rlch/ark/ark` via tap
- [ ] `cargo install ark-cli` (crate name TBD; might reserve `ark` but that may collide — if so use `ark-cli`)
- [ ] `cargo binstall` works automatically via cargo-dist artifacts
- [ ] No Windows package manager — Windows not a v1 target
- [ ] Linux: AUR package (community-driven, not maintained by us) acceptable; cargo-dist tarballs suffice for v1

## Install flow (`ark doctor --fix`)
First-run experience:
```
$ brew install rlch/ark/ark
$ ark doctor --fix

ark doctor
  ✓ zellij 0.44.2 (required ≥ 0.44)
  ✓ delta 0.17.0
  ✓ claude on PATH
  ✗ ~/.config/zellij/plugins/ark-status.wasm missing
    install? [y/N] y
    wrote ~/.config/zellij/plugins/ark-status.wasm (420 KB)
  ✗ ~/.config/zellij/plugins/ark-picker.wasm missing
    install? [y/N] y
    wrote ~/.config/zellij/plugins/ark-picker.wasm (712 KB)
  ⚠ ~/.config/ark/config.toml missing (defaults apply)
    create template? [y/N] y
    wrote ~/.config/ark/config.toml

Next steps:
  1. Add this to your zellij config (~/.config/zellij/config.kdl):

     layout {
         default_tab_template {
             pane size=1 borderless=true {
                 plugin location="file:~/.config/zellij/plugins/ark-status.wasm"
             }
             children
         }
     }

  2. Add a keybind for the picker (recommended Ctrl+g a):

     shared {
         bind "Ctrl g" "a" {
             LaunchOrFocusPlugin "file:~/.config/zellij/plugins/ark-picker.wasm" { floating true }
         }
     }

  3. Try: ark --scene cavekit --cwd .
```

## Out of Scope
- Windows installer — v2
- Docker image — not appropriate for a terminal tool
- Chocolatey / winget — deferred
- Snap / flatpak — not planning
- Auto-update from binary — users re-run `brew upgrade` / `cargo install --force`

## Cross-References
- cavekit-cli.md R5 — `ark doctor` details
- cavekit-plugin-status.md / cavekit-plugin-picker.md — wasm crates
- cavekit-config.md — template config written by doctor
