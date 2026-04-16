---
created: "2026-04-16"
last_edited: "2026-04-16"
status: frozen — v1.0
---

# Wasm Extension Metadata — v1.0 (frozen)

> Companion to `intent-api-v1.md`. This document describes the on-wire
> shape of the `ark.metadata` custom section that every wasm-component
> ark extension MUST carry. Once v1.0 is cut, this layout is frozen
> for 1.x under the rules in §"Version compatibility contract" below.
>
> Sources of truth:
> - Schema types: `crates/ark-ext-metadata-types/src/lib.rs`.
> - Reader: `crates/scene/src/wasm_meta.rs`.
> - Section name constant: `ark_scene::wasm_meta::ARK_METADATA_SECTION`.

## Wire layout

Every ark extension distributed as a wasm component has exactly one
wasm [custom section][custom-section] named **`ark.metadata`** whose
payload is UTF-8 KDL 2.0 matching the `ExtensionMetadata` struct
below.

[custom-section]: https://webassembly.github.io/spec/core/binary/modules.html#custom-section

```
custom section
    name = "ark.metadata"
    data = UTF-8 KDL (1 document, 1 root `extension { … }` node)
```

Section discovery is done via `wasmparser::Parser::parse_all`, walking
custom sections and picking the first whose name equals
`ark.metadata`. Non-matching custom sections (`name`, `producers`,
etc.) are ignored.

**Duplicate-section policy:** if a cartridge somehow ships two
`ark.metadata` sections (misconfigured build), the reader picks the
first and ignores the rest. This is documented rather than declared
an error — cartridges with duplicates are malformed but we prefer
permissive read + strict write.

## Size limits

| Bound                              | Value       | Why                                 |
|------------------------------------|-------------|-------------------------------------|
| Custom-section payload, max bytes  | 65 536      | Keeps `ark ext inspect` fast — any extension whose metadata exceeds 64 KiB is likely misusing the manifest (embedding README text, etc.) and should move that out of band. |
| Single field (string node), max   | 4 096       | Per-field cap on values the manifest embeds (e.g. intent schemas). Consistent with CEL expression length bound (R8). |
| `requires` list length             | 64          | A single extension shouldn't transitively require more than 64 peers; deeper ladders indicate a packaging mistake. |
| `intents` list length              | 256         | Loose bound — practical extensions ship <32; the cap exists to keep the scene compile-time symbol table sized reasonably. |
| `events` list length               | 256         | Same rationale as `intents`. |
| `capabilities` list length         | 32          | Well above the 6-value v0.4 vocabulary; room for post-v0.4 additions without a schema bump. |

The wasm spec itself imposes no per-section cap; these numbers are
ark-level policy. They are checked by the metadata reader + the
`ark ext inspect` / `ark scene check --v1-strict` path. Exceeding
them surfaces as `error[ext/metadata-invalid]`.

## KDL 2.0 reference

The metadata KDL document targets **KDL 2.0** (see
[kdl.dev](https://kdl.dev)). Relevant features used:

- **Node names** are bare identifiers (`name`, `version`, `intent`,
  `event`, `item`). Kebab-case is used for multi-word names
  (`ark-range`, `zellij-range`, `payload-schema`, `args-schema`).
- **Positional arguments** carry each string-typed field: `name "demo"`,
  `version "0.1.0"`. Parsed as the first positional of the node.
- **Properties** carry the one typed non-string field: `required #true`
  / `required #false` on config fields. KDL 2.0 booleans use the
  `#true` / `#false` sigil.
- **Children** are `{ … }` blocks. Most list fields use a sequence of
  `item "value"` children (facet-kdl 0.42 hard-codes the sequence
  item name — see §Known limitations).
- **Comments** are single-line `//` or multi-line `/* */`.

Extensions SHOULD NOT embed type annotations (`(u8)42`) or slashdash
(`/-`) comments in the manifest — both are legal KDL 2.0 but reduce
readability and offer no benefit here.

## Schema: `ExtensionMetadata`

Top-level document wraps one `extension { … }` root. The wrapper is
`ExtensionManifest`; its only field is the body. All fields below are
fields of `ExtensionMetadata` unless otherwise noted.

| Field              | KDL shape                                   | Type                   | Req | Notes                                                                     |
|--------------------|---------------------------------------------|------------------------|-----|---------------------------------------------------------------------------|
| `name`             | `name "<str>"`                              | string                 | yes | Lowercase alphanumeric + `-` / `_`. Must match install-dir name.          |
| `version`          | `version "<semver>"`                        | string                 | yes | Semver of the extension itself.                                           |
| `ark_range`        | `ark-range "<semver-range>"`                | string                 | yes | Supported ark-protocol range. Empty string = no constraint.               |
| `zellij_range`     | `zellij-range "<semver-range>"`             | string                 | yes | Supported zellij range. Empty string = no constraint.                     |
| `requires`         | `item "<name>@<range>"*`                    | list<string>           | no  | Other extensions this one depends on. Cycles rejected at resolve.         |
| `intents`          | `intent "<name>" { … }*`                    | list<IntentDecl>       | no  | One per contributed intent.                                               |
| `events`           | `event "<name>" { … }*`                     | list<EventDecl>        | no  | User-events the extension emits.                                          |
| `config`           | `config { … }`                              | ConfigSchema           | no  | Declarative schema for user `use "<ext>" { config { … } }` blocks.        |
| `capabilities`     | `item "<cap>"*`                             | list<string>           | no  | v0.4 declared-caps surface.                                               |

### `IntentDecl`

```kdl
intent "<fully-qualified-name>" {
    args-schema "<json-schema-as-string>"
}
```

- `name` — first positional; MUST be `<ext-name>.<intent>` form.
- `args_schema` — JSON-Schema document carried as a UTF-8 string.
  String form (not structured) because facet 0.42 has no blanket SHAPE
  impl for `serde_json::Value`; foreign-language bindings treat this
  field as `{ "type": "string", "format": "json-schema" }`.

### `EventDecl`

```kdl
event "<fully-qualified-name>" {
    payload-schema "<json-schema-as-string>"
}
```

Same shape as `IntentDecl`; `name` goes to the UserEvent name.

### `ConfigSchema`

```kdl
config {
    item "<field-name>" {
        type "<type>"
        required #true
        default "<str>"?
    }*
}
```

`type` MUST be one of `"string"`, `"int"`, `"bool"`, `"path"`, `"url"`,
`"duration"`. Anything else is `error[ext/bad-config]` at manifest-load.

### `capabilities`

Declared-capability vocabulary (v0.4, T-13.3):

| Value        | Declares                                                 |
|--------------|----------------------------------------------------------|
| `exec`       | Spawns subprocesses.                                     |
| `fs-read`    | Reads files outside the ext's install dir.               |
| `fs-write`   | Writes files outside the ext's install dir.              |
| `pipe`       | Emits pipe messages to other zellij panes/plugins.       |
| `network`    | Opens outbound TCP/UDP/HTTP sockets.                     |
| `hook`       | Registers scene reactions ([[hooks]] analog).            |

Unknown values are parsed (per compat rule #3) but surface as
`warning[ext/unknown-capability]` at inspection time. `--v1-strict`
upgrades that to an error.

## Canonical example

```kdl
extension {
    name "status-lite"
    version "1.0.0"
    ark-range ">=1.0, <2.0"
    zellij-range ">=0.41"

    // Peers this ext depends on at runtime.
    item "shared-ui-kit@^1.2"

    // Intents this extension contributes (namespaced).
    intent "status-lite.set_icon" {
        args-schema "{\"type\":\"object\",\"properties\":{\"icon\":{\"type\":\"string\"}},\"required\":[\"icon\"]}"
    }

    // User-events this extension emits.
    event "status-lite.icon_changed" {
        payload-schema "{\"type\":\"object\",\"properties\":{\"icon\":{\"type\":\"string\"}},\"required\":[\"icon\"]}"
    }

    // Config knobs available under `use "status-lite" { config { … } }`.
    config {
        item "refresh_ms" {
            type "int"
            required #false
            default "1000"
        }
    }

    // v0.4 declared-caps. Shown in `ark ext inspect`.
    item "pipe"
    item "hook"
}
```

## Emitting the section: proc-macro helper

Extension authors do not hand-write the KDL — they build an
`ExtensionMetadata` value via Rust and let `ark-ext-metadata` (companion
helper crate, separate from `ark-ext-metadata-types`) emit the KDL bytes
at build time.

### Build-time emission

```rust
// build.rs in a wasm extension crate
use ark_ext_metadata_types::{
    ExtensionManifest, ExtensionMetadata, IntentDecl, EventDecl,
    ConfigSchema, ConfigField, StringNode,
};
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = ExtensionManifest::new(ExtensionMetadata {
        name: StringNode::new("status-lite"),
        version: StringNode::new("1.0.0"),
        ark_range: StringNode::new(">=1.0, <2.0"),
        zellij_range: StringNode::new(">=0.41"),
        requires: vec![StringNode::new("shared-ui-kit@^1.2")],
        intents: vec![IntentDecl {
            name: "status-lite.set_icon".into(),
            args_schema: StringNode::new(
                r#"{"type":"object","properties":{"icon":{"type":"string"}},"required":["icon"]}"#,
            ),
        }],
        events: vec![EventDecl {
            name: "status-lite.icon_changed".into(),
            payload_schema: StringNode::new(
                r#"{"type":"object","properties":{"icon":{"type":"string"}},"required":["icon"]}"#,
            ),
        }],
        config: ConfigSchema {
            fields: vec![ConfigField {
                name: "refresh_ms".into(),
                type_name: StringNode::new("int"),
                required: false,
                default: Some(StringNode::new("1000")),
            }],
        },
        capabilities: vec![
            StringNode::new("pipe"),
            StringNode::new("hook"),
        ],
    });

    let kdl_bytes = facet_kdl::to_string(&manifest)
        .expect("serialize ExtensionMetadata");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("ark.metadata");
    fs::write(&out, kdl_bytes).expect("write ark.metadata");
}
```

### Link-section embedding

```rust
// src/lib.rs of the wasm extension crate
#[link_section = "ark.metadata"]
#[used]
pub static ARK_METADATA: [u8; include_bytes!(concat!(env!("OUT_DIR"), "/ark.metadata")).len()] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/ark.metadata"));
```

The `#[used]` attribute prevents `rustc` from GC'ing the static when
nothing in the crate references it. `#[link_section = "ark.metadata"]`
is what the linker translates into a wasm custom section.

### Proc-macro shorthand

For extension authors that don't want a `build.rs`, the
`#[ark_extension(…)]` proc-macro (shipped by `ark-ext-metadata`,
post-v1 roadmap) takes the same fields as attributes and expands to
the build.rs + static above. Schematic usage:

```rust
use ark_ext_metadata::ark_extension;

#[ark_extension(
    name = "status-lite",
    version = "1.0.0",
    ark_range = ">=1.0, <2.0",
    intents(("status-lite.set_icon", args_schema = include_str!("schemas/set_icon.json"))),
    capabilities("pipe", "hook"),
)]
pub fn activate(ctx: &mut ExtContext) { /* … */ }
```

The proc-macro is NOT part of the v1 freeze — only the on-wire KDL
layout and the `ExtensionMetadata` struct are. Changes to the
proc-macro's Rust surface are free to happen across 1.x MINOR bumps.

## Reader contract

`ark_scene::wasm_meta::read_extension_metadata(wasm_bytes, path)`
returns one of:

| Outcome                    | Error variant                                       | Diagnostic code          |
|----------------------------|-----------------------------------------------------|--------------------------|
| Success                    | `Ok(ExtensionMetadata)`                             | —                        |
| No `ark.metadata` section  | `Err(SceneError::WasmMetaMissing { path })`         | `ext/metadata-missing`   |
| Section bytes invalid      | `Err(SceneError::WasmMetaInvalid { path, message})` | `ext/metadata-invalid`   |

"Invalid" covers three cases, all mapped to the same code:

1. Wasm-level parse failure (malformed cartridge).
2. Section bytes are not valid UTF-8.
3. Bytes are valid UTF-8 but do not parse as the `ExtensionManifest`
   KDL shape.

The reader does **NOT** validate:

- Semver ranges (deferred to the use-resolver, T-10.4).
- Dependency closure (deferred to the use-resolver).
- Capability-list vocabulary (deferred to `ark ext inspect` /
  `ark scene check --v1-strict`).

These are separate concerns with their own error codes
(`ext/version-mismatch`, `ext/cycle`, `ext/unknown-capability`).

## Version compatibility contract

Mirrors the intent-API contract (see `intent-api-v1.md`). For 1.x:

1. **New optional fields** may be added to `ExtensionMetadata` in a
   MINOR bump. Receivers MUST ignore unknown fields (R16 rule #3).
   This means an older ark reading a newer extension's metadata sees
   the v1 fields and ignores the rest — no hard failure.
2. **Renaming or removing** a field is a MAJOR change.
3. **Changing a field's KDL shape** (property → child, string → int,
   required → optional) is a MAJOR change.
4. **Enum vocabularies** (`type` in `ConfigField`, capability names)
   may grow via MINOR; shrinking is MAJOR.
5. **The custom-section name** `ark.metadata` is frozen permanently.
   A post-v1 format revision would ship in a new section name
   (e.g. `ark.metadata.v2`) so readers can route by section name —
   older ark reading a v2-only cartridge surfaces
   `error[ext/metadata-missing]` with a v1 section lookup, which is
   the correct (fail-closed) behaviour.

## Known limitations (v1)

Documented so they don't come back as bugs:

- **facet-kdl 0.42 hard-codes sequence item names as `item`.**
  That's why `requires`, `capabilities`, and `config.fields` render
  as `item "…"` rather than the more natural `require "…"` /
  `cap "…"` forms. Post-facet-kdl-per-field-rename this can be
  tightened in a future MINOR bump — both shapes will remain legal
  at read-time.
- **JSON Schemas embedded as strings.** Ideally `args_schema` /
  `payload_schema` would be structured KDL. Because facet 0.42 has no
  blanket SHAPE impl for `serde_json::Value`, v1 carries them as
  strings. Upgrading to structured is a MAJOR change (reader shape
  change) and is thus a 2.0 candidate.
- **No multi-language i18n.** All descriptive strings (future
  `description` field, error messages) are English-only. v1 does not
  attempt localisation.

## Reference surface

| Artifact                                           | Purpose                                 |
|----------------------------------------------------|-----------------------------------------|
| `crates/ark-ext-metadata-types/src/lib.rs`         | `ExtensionManifest`, `ExtensionMetadata`, `IntentDecl`, `EventDecl`, `ConfigSchema`, `ConfigField`, `StringNode`, `ALLOWED_CAPABILITIES`. |
| `crates/scene/src/wasm_meta.rs`                    | Reader: `read_extension_metadata`, `ARK_METADATA_SECTION`. |
| `crates/scene/src/use_resolution.rs`               | Downstream validation (version ranges, cycles). |
| `context/refs/intent-api-v1.md`                    | Companion: frozen intent surface. |
| `context/kits/cavekit-scene.md` R10, R16           | Extension system + wire stability rules. |

## Rationale

Choosing wasm custom sections over a sibling `.kdl` file or an RPC
handshake:

- **Co-located with code.** A cartridge is one artifact — `ark ext
  install` downloads one file, validates one signature, ships one
  thing to disk. Sidecar metadata files double the surface for
  corruption + version drift.
- **Inspectable without a runtime.** `wasm-tools strings my.wasm` or
  any wasm-aware tool can pull the section out for debugging. No
  ark binary required.
- **Standard.** Custom sections are the canonical wasm mechanism for
  tool-specific metadata. Every toolchain (wasmtime, wasmparser,
  wasm-component-ld) handles them the same way.

The 64-KiB section cap echoes VSCode's extension-manifest practice —
enough room for comprehensive schemas, small enough that a runaway
manifest forces refactoring rather than silently ballooning the
cartridge.
