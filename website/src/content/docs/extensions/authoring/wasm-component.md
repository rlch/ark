---
title: "WASM Component Extensions"
description: "WASI p2 and wasm-metadata"
---

WASM extensions run inside zellij's plugin runtime. They are sandboxed, portable, and distributed as a single `.wasm` file with an embedded `ark.metadata` custom section.

:::note
WASI p2 sandbox mode is deferred to v0.3+. In v0.1, WASM extensions use zellij's existing plugin runtime (WASI preview 1). The metadata format described here is frozen for 1.x.
:::

## How it works

A WASM extension is a zellij plugin that carries ark-specific metadata in a wasm [custom section](https://webassembly.github.io/spec/core/binary/modules.html#custom-section) named `ark.metadata`. ark reads this section at load time to discover the extension's intents, events, config schema, and capabilities. Protocol messages flow through the ark-bus zellij plugin via pipe.

## Metadata section

The `ark.metadata` section contains a UTF-8 KDL 2.0 document with a single `extension { ... }` root node:

```kdl
extension {
  name "status-lite"
  version "1.0.0"
  ark-range ">=1.0, <2.0"
  zellij-range ">=0.41"

  intent "status-lite.set_icon" {
    args-schema "{\"type\":\"object\",\"properties\":{\"icon\":{\"type\":\"string\"}},\"required\":[\"icon\"]}"
  }

  event "status-lite.icon_changed" {
    payload-schema "{\"type\":\"object\",\"properties\":{\"icon\":{\"type\":\"string\"}}}"
  }

  config {
    item "refresh_ms" {
      type "int"
      required #false
      default "1000"
    }
  }

  item "pipe"
  item "hook"
}
```

### Required fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Lowercase alphanumeric + `-`/`_`. Must match install directory name. |
| `version` | string | Semver of the extension. |
| `ark-range` | string | Supported ark protocol range. Empty string = no constraint. |
| `zellij-range` | string | Supported zellij range. Empty string = no constraint. |

### Optional fields

| Field | Type | Description |
|-------|------|-------------|
| `requires` | list of `item` | Other extensions this one depends on (`"name@range"` form). |
| `intents` | list of `intent` | Intents the extension contributes. |
| `events` | list of `event` | Events the extension emits. |
| `config` | block | Config schema for `use "ext" { config { ... } }`. |
| `capabilities` | list of `item` | Declared capabilities (see [Capabilities](/extensions/capabilities/)). |

## Size limits

| Bound | Max |
|-------|-----|
| Custom section payload | 64 KiB |
| Single field value | 4 096 bytes |
| `requires` entries | 64 |
| `intents` entries | 256 |
| `events` entries | 256 |
| `capabilities` entries | 32 |

Exceeding any limit produces `error[ext/metadata-invalid]`.

## Building in Rust

### Step 1: Generate metadata at build time

```rust
// build.rs
use ark_ext_metadata_types::{
    ExtensionManifest, ExtensionMetadata, IntentDecl,
    ConfigSchema, ConfigField, StringNode,
};
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = ExtensionManifest::new(ExtensionMetadata {
        name: StringNode::new("status-lite"),
        version: StringNode::new("1.0.0"),
        ark_range: StringNode::new(">=1.0, <2.0"),
        zellij_range: StringNode::new(">=0.41"),
        intents: vec![IntentDecl {
            name: "status-lite.set_icon".into(),
            args_schema: StringNode::new(r#"{"type":"object","properties":{"icon":{"type":"string"}}}"#),
        }],
        ..Default::default()
    });

    let kdl_bytes = facet_kdl::to_string(&manifest).expect("serialize");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap())
        .join("ark.metadata");
    fs::write(&out, kdl_bytes).expect("write");
}
```

### Step 2: Embed as a custom section

```rust
// src/lib.rs
#[link_section = "ark.metadata"]
#[used]
pub static ARK_METADATA: [u8; include_bytes!(
    concat!(env!("OUT_DIR"), "/ark.metadata")
).len()] = *include_bytes!(concat!(env!("OUT_DIR"), "/ark.metadata"));
```

The `#[used]` attribute prevents the compiler from stripping the static. `#[link_section = "ark.metadata"]` places it in a wasm custom section.

### Step 3: Compile to wasm

```bash
cargo build --target wasm32-wasip1 --release
```

The resulting `.wasm` file is a complete, self-contained extension.

## Inspecting metadata

```bash
ark ext inspect path/to/status-lite.wasm
```

This reads the `ark.metadata` section and prints the parsed manifest. No ark runtime required — the section is inspectable with any wasm-aware tool (`wasm-tools`, `wasmparser`, etc.).

## Version compatibility

The metadata format is frozen for 1.x:

- New optional fields may be added in a MINOR bump. Readers ignore unknown fields.
- Removing or renaming a field is a MAJOR change.
- The section name `ark.metadata` is frozen permanently.
