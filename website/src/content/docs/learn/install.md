---
title: Install
description: Install ark via Homebrew, cargo-binstall, or from source
---

## Homebrew (macOS + Linuxbrew)

```sh
brew install rlch/ark/ark
```

The formula is auto-updated on every release via cargo-dist.

## cargo binstall (fast, prebuilt binaries)

```sh
cargo binstall ark-cli
```

Fetches the prebuilt tarball for your platform from the GitHub Release. Falls back to `cargo install` if no prebuilt matches.

## cargo install (from source)

```sh
cargo install ark-cli
```

Builds from source. Requires a Rust toolchain. Slow but works on any target.

## Manual download

See [github.com/rlch/ark/releases](https://github.com/rlch/ark/releases) for tarballs (`ark-<version>-<target>.tar.xz`) and their SHA256 sums. Extract and drop `ark` + `ark-hook` into your `PATH`.

Standalone wasm plugins are also published as separate release assets:

```sh
V=0.1.0
curl -LO "https://github.com/rlch/ark/releases/download/v${V}/ark-status-v${V}.wasm"
curl -LO "https://github.com/rlch/ark/releases/download/v${V}/ark-picker-v${V}.wasm"
```

## First-run setup

After installing, run the doctor:

```sh
ark doctor --fix
```

This checks for `zellij`, `delta`, and the wasm plugins, and installs anything missing. It also prints the zellij KDL keybind snippet you need to add to your zellij config.

## Prerequisites

ark requires:
- **zellij** — the terminal multiplexer that ark uses as its UI layer. ark ships its own pinned zellij binary; set `ARK_USE_SYSTEM_ZELLIJ=1` to use a system-installed copy instead.
- **delta** — for diff rendering in the diff pane.
