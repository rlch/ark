---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
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
**Description:** Ship wasm plugins inside the `ark` binary.
**Acceptance Criteria:**
- [ ] `ark-cli` crate has a `build.rs` that:
  - Runs `cargo build --target wasm32-wasip1 --release -p ark-plugin-status`
  - Runs same for `ark-plugin-picker`
  - Copies resulting `.wasm` files to `$OUT_DIR/wasm/`
  - Embeds them via `include_bytes!("...")` in a module
- [ ] `ark doctor --fix` writes these bytes to `~/.config/zellij/plugins/ark-{status,picker}.wasm`
- [ ] Size budgets: ark-status < 500 KB, ark-picker < 800 KB (enforced by test asserting bytes.len())
- [ ] A standalone release asset also uploads the raw `.wasm` files (for users who want to install manually)
**Dependencies:** cavekit-plugin-status, cavekit-plugin-picker

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

  3. Try: ark spawn --orchestrator cavekit --cwd . -- claude --resume
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
