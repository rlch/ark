# ark-plugin-protocol

Shared protocol surface between the ark plugin host (`ark-host`) and guest-side
plugin SDK (`ark-plugin-sdk`). Defines:

- Stable error-code enum (`PluginLoadError`) for every `error[plugin/*]`,
  `error[abi/*]`, `error[ark-kdl/*]` code referenced by cavekit-plugin-protocol
  requirements R3, R5, R6, R8, R9, R12, R14.
- Render-target classification (`Target::Terminal`, reserved `Gui`).
- Pipe/intent bus types (`Intent`, `IntentTarget`, `PipeMessage`,
  `PipeSource`, `BusError`).
- WIT contracts (under `wit/`, populated in Tier 1 of build-site-plugin-protocol.md).
- Postcard schemas for `ark-caps:v1` + `ark-meta:v1` custom sections
  (populated in Tier 1).

## Status

Tier 0 scaffold — see `context/plans/build-site-plugin-protocol.md` for the
full task graph. Runtime wiring lives in `ark-host`; guest-side codegen lives
in `ark-plugin-sdk`.

## Dependency policy

This crate MUST NOT depend (directly or transitively) on `arborium-sysroot`,
`facet-kdl`, or `facet-format` — they bloat the wasm guest artifact and were
the primary driver of the plugin-protocol rewrite. See cavekit-plugin-protocol.md
R12 regression test.
