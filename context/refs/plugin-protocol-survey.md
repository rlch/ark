# Plugin Protocol Survey (2026-04-20)

## Cluster 4: Capability Declaration Mechanics

The chicken-and-egg: a wasm plugin needs to tell the host what it requires
*before* the host instantiates it, but you cannot call an exported function
until you instantiate, and you do not want to hand out capabilities you have
not yet decided to grant. Six approaches solve this; they trade verification
purity, toolchain weight, encoding complexity, and drift risk against each
other. This cluster goes deeper into each, then returns a verdict for ark.

---

### 4.1 Custom section approach (out-of-band-in-band)

The plugin embeds a named custom section in its `.wasm` binary. The host
parses the binary as bytes (no instantiation) and decodes the section payload.

**Bytes-on-disk encoding.** Surveying real-world choices:

| Project           | Section name             | Encoding          |
| ----------------- | ------------------------ | ----------------- |
| wit-bindgen / wit-component | `component-type:<world>` | Custom binary (the encoded component type, leb128-prefixed, see `wit_component::metadata::encode`) |
| wasmCloud (pre-1.x)         | `jwt`                    | Signed JWT (JSON inside JWS) |
| wasmCloud capability claims | `wasmCloud`              | JWT with custom `wascap` claim |
| wasm-bindgen                | `__wasm_bindgen_unstable` | JSON, schema-versioned |
| Cosmonic / wash             | `wasmcloud`              | JWT |

For a fresh project, the realistic choices are:

- **postcard** — fastest to encode/decode, smallest bytes, but humans cannot
  read the section with `wasm-objdump -j <name> -x`. Schema evolution requires
  discipline (postcard is not self-describing).
- **CBOR** (e.g. `ciborium`) — self-describing, dump-friendly via `cbor diag`,
  schema evolution natural, ~2× the size of postcard.
- **JSON** — debuggable by hand with `wasm-tools dump`, but bloats the binary
  and loses Rust-native type fidelity (no enums-as-tagged-unions without
  effort). Used by wasm-bindgen historically.
- **FlatBuffers / Cap'n Proto** — overkill for a manifest of <1 KiB.
- **Custom binary** — what wit-component does. Justified only if you are
  carrying types of arbitrary depth, which a flat capability list is not.

**Schema versioning of the payload itself.** Three patterns:

1. *Magic + version prefix.* First 4 bytes = `b"ARK1"`, next byte = version
   tag. Decoder branches on the tag. wasm-bindgen does this with its
   `SCHEMA_VERSION` constant baked into the section.
2. *Self-describing format (CBOR).* The payload includes a `version` field
   and the decoder switches on it. Simpler but no early-reject on garbage.
3. *Sectioned by version.* Plugin embeds `ark-caps:v1` and (later) also
   `ark-caps:v2`. Old hosts read v1, new hosts prefer v2. Larger binary,
   trivial fallback.

The wasm spec gives you (3) free of charge — section names are arbitrary
strings — so changing names is the cleanest forward-compat story.

**What if the host cannot decode the section?**

- *Older host, newer plugin* — the host sees an unknown version tag (or an
  unknown section name) and refuses to load: `error: plugin "X" declares
  ark-caps schema v3, this ark binary supports up to v2; upgrade ark`.
- *Newer host, older plugin* — host falls back to the older decoder, which
  it should keep around for at least one major version.
- *Section missing entirely* — refuse to load. A plugin without a manifest
  is indistinguishable from a malicious binary that wants to import `wasi:fs`
  without telling you.

**Performance — startup scan vs lazy.** A custom section is *cheap to find*
because `wasmparser::Parser` is event-driven and you can short-circuit the
moment you see your section. Numbers from a typical 200 KiB wasm plugin:
parsing the section table to locate one custom section is sub-millisecond.
For 50 plugins on disk that is ~50 ms total at startup — fine. Lazy scanning
buys you nothing measurable, and worse, defers the user-facing capability
prompt to first-use.

```rust
use wasmparser::{Parser, Payload};

fn read_capability_section(bytes: &[u8]) -> Option<Vec<u8>> {
    for payload in Parser::new(0).parse_all(bytes) {
        if let Ok(Payload::CustomSection(s)) = payload {
            if s.name() == "ark-caps:v1" {
                return Some(s.data().to_vec());
            }
        }
    }
    None
}
```

This runs against `&[u8]` — no `Engine`, no `Module::new`, no JIT.

---

### 4.2 Two-phase instantiate

Idea: instantiate with empty/stubbed imports, call `caps()`, decide grants,
re-instantiate with real imports.

**Is it possible with wasmtime?** Yes, two ways:

1. `Linker::define_unknown_imports_as_traps(&module)` walks the module's
   imports and stubs each with a function that traps on call. This lets
   instantiation succeed even if you have not implemented a single import,
   provided the plugin does not actually call any during initialisation.
2. `Linker::define_unknown_imports_as_default_values(&module)` returns
   zero/null instead of trapping. Safer for plugins that touch globals at
   startup but mask real bugs.

```rust
let mut linker = wasmtime::Linker::new(&engine);
linker.define_unknown_imports_as_traps(&module)?;
let pre_instance = linker.instantiate(&mut store, &module)?;
let caps_fn = pre_instance.get_typed_func::<(), (u32, u32)>(&mut store, "ark_capabilities")?;
let (ptr, len) = caps_fn.call(&mut store, ())?;
// ... read caps from linear memory ...
drop(pre_instance); drop(store);

// Phase 2: real linker with grants
let mut store2 = Store::new(&engine, host_state);
let real_linker = build_linker_with_grants(&engine, &grants)?;
let instance = real_linker.instantiate(&mut store2, &module)?;
```

**Memory cost.** Two `Store`s, two `Instance`s. Each instance owns its linear
memory (default 64 KiB minimum, often grown). For a typical plugin this is
2-4 MiB doubled = 4-8 MiB transient. Free the first store before the second
and steady-state cost is single-instance.

**Type-only introspection without running anything.** The wasmtime
`Module::imports()` and `Component::component_type().imports()` APIs give
you the full import list, with types, *without* an `Instance`. So if the
capability list IS the import list (see 4.3), there is no need for a phase-1
instantiation at all. Phase-1 only buys something if the plugin wants to
*compute* its capability declaration at load time (e.g. probe the
environment), which is itself suspicious — capability declarations should be
static.

**Known wrinkle.** `define_unknown_imports_as_traps` has been
[reported][wt-10663] to conflict with already-defined imports unless you
enable `linker.allow_shadowing(true)`. Worth knowing if you mix it with
WASI defaults.

[wt-10663]: https://github.com/bytecodealliance/wasmtime/issues/10663

---

### 4.3 Imports list as the manifest (Spin / component-model native)

The plugin's import list IS its capability list. The host enumerates imports
and refuses to load if any unmet-and-not-granted entry exists.

**Library functions.**

- Core wasm: `wasmtime::Module::imports() -> impl ExactSizeIterator<Item = ImportType>`.
  Each `ImportType` carries `module()`, `name()`, and `ty()` (`ExternType`).
  Cheap, no instantiation, just `Module::new(&engine, &bytes)`.
- Component model: `Component::component_type().imports(&engine)` returns
  `(name, ComponentItem)` pairs where `ComponentItem` distinguishes
  function/instance/type/component imports — much richer than core wasm.

```rust
let module = Module::new(&engine, &wasm_bytes)?;
for imp in module.imports() {
    let key = format!("{}::{}", imp.module(), imp.name());
    if !grants.contains(&key) {
        bail!("plugin requires {} which is not granted", key);
    }
}
```

**Validating import names against a well-known set.** Plain strings are a
weak typing layer. Two ways to harden:

1. *Namespace prefixing.* All host-provided imports live under `ark:cap/*`
   modules. Anything under another module name is rejected unconditionally.
2. *Capability registry.* The host owns a `HashSet<&'static str>` (or a
   `HashMap<&'static str, CapabilityKind>`). Every import is matched
   exactly. Unknown imports = refuse to load with `unknown capability
   "ark:cap/fs.read" — supported capabilities: …`. Typos surface as load
   errors, never as silent traps at runtime.

The Spin model leans on this: every `wasi:http/outgoing-handler` import
maps to `allowed_outbound_hosts` checks at runtime, so the import set is
the universe of capabilities and the manifest tightens the granted subset.

**Subtlety.** A clever plugin can declare an import it never calls. Capability
declarations should still be enforced at load time (refuse to instantiate)
not lazily, so a plugin asking for `fs.write` without a grant fails *before*
any code runs.

---

### 4.4 Component model `world` declaration

A WIT `world` declares imports and exports at the type level:

```wit
package ark:plugin@0.1.0;

world editor-extension {
    import ark:cap/fs.{read-file};
    import ark:cap/term.{spawn-pane};
    export activate: func();
}
```

Once compiled to a component, the host introspects it:

```rust
use wasmtime::component::{Component, types::ComponentItem};

let component = Component::from_file(&engine, "plugin.wasm")?;
let ty = component.component_type();
for (name, item) in ty.imports(&engine) {
    match item {
        ComponentItem::ComponentInstance(inst) => {
            for (fn_name, _) in inst.exports(&engine) {
                check_grant(name, fn_name)?;
            }
        }
        _ => check_grant(name, "")?,
    }
}
```

**Trade-off.** The component model is heavy machinery. You get:

- A real type system over imports (records, variants, resources, lists).
- WIT as a single source of truth — bindgen outputs match introspection.
- Adapter ecosystem (preview1-to-preview2 etc.).

You pay:

- The whole `wit-component` toolchain in your build pipeline.
- Larger binaries (component wrappers add bytes).
- A moving target — preview2 is stable, preview3 still in flight.
- Plugin authors must learn WIT.

Because the world's imports ARE the capability set, this is a *strict
superset* of approach 4.3 with type checking thrown in. If you are
already going components, this is the answer.

---

### 4.5 OCI annotation / sidecar manifest

Capabilities live outside the wasm — in an OCI image annotation, a TOML
sibling, or in `ark.kdl` itself.

Spin's `spin.toml` is the canonical example:

```toml
[component.email]
source = "email.wasm"
allowed_outbound_hosts = ["https://api.sendgrid.com"]
key_value_stores = ["default"]
```

The `.wasm` does not declare these. The runtime grants them based on the
manifest, and outbound calls are gated at the host-function level.

**Trade-offs.**

- *Pro:* The plugin author does not need a rebuild to widen capabilities.
  Operators can audit one file (the manifest) for the whole deployment.
- *Pro:* Encoding is whatever the manifest format already uses. No custom
  section, no schema versioning of binary payloads.
- *Con:* Drift. If the wasm tries to call an import the manifest forgot to
  whitelist, you get a runtime trap, not a load-time error. (Spin mitigates
  by checking `allowed_outbound_hosts` against the actual host string at
  call time, but the *capability* itself — "this component imports
  `wasi:http/outgoing-handler`" — is still in the wasm; the manifest only
  scopes it.)
- *Con:* Two sources of truth that have to stay in sync. Easy for a sloppy
  plugin author to ship a manifest that under-grants and a wasm that
  over-imports — you discover this the first time the user runs the plugin.

A hybrid is common: import set is the *technical* capability surface
(enforced at load), manifest is the *policy scope* (enforced at call).

---

### 4.6 WIT-less custom interface (export-driven)

Plugin exports a single safe function:

```rust
// in the plugin
#[no_mangle]
pub extern "C" fn ark_capabilities() -> *const u8 {
    // returns ptr to a postcard-encoded Vec<Capability> in linear memory
    static MANIFEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/manifest.bin"));
    MANIFEST.as_ptr()
}
#[no_mangle]
pub extern "C" fn ark_capabilities_len() -> u32 { MANIFEST_LEN as u32 }
```

Host instantiates with **no granted capabilities** (linker stubs everything
unknown, see 4.2), calls `ark_capabilities`, decodes, decides, then
re-instantiates with grants OR — and this is the load-bearing question —
modifies the existing instance.

**Can you mutate an instance's imports after instantiation?** No. wasmtime
imports are bound at `Linker::instantiate` time and the instance carries
function pointers into its `Store`. There is no `instance.update_import()`.
You must re-instantiate with a new linker. The first instance can be
dropped.

**Can the linker itself be mutated mid-program?** Yes,
`Linker::func_new`/`func_wrap` add definitions, but they affect *future*
instantiations, not the live instance.

**Why use this instead of 4.1 (custom section)?** You would not. The custom
section gives you the same data with zero runtime cost and zero risk of the
plugin lying during phase-1 by computing fake caps. Approach 4.6 is strictly
worse than 4.1 for static caps. It is only interesting if capabilities
genuinely depend on plugin runtime state — which is a code smell.

---

### Critical questions, answered

**Q1: Which approach lets the host validate capabilities without ever
running plugin code?**

Three: 4.1 (custom section), 4.3 (imports list), 4.4 (component world).
All operate on the wasm bytes via parser/type APIs without an `Instance`.
4.5 (sidecar) also qualifies if you treat the manifest as authoritative.
Only 4.2 and 4.6 require running anything, and both are avoidable.

**Q2: If the host MUST instantiate to query, can capability-gated host
functions be added/removed mid-instance, or do we need a fresh instance?**

You need a fresh instance. The wasmtime `Instance` snapshot binds imports
at construction. The `Linker` is mutable across instantiations but not
within one. Two-phase = two stores = two instances; the phase-1 store is
disposable.

**Q3: For ark — user grants in `ark.kdl`, plugin wants in code, host
enforces — what's the cleanest mechanism that fails loud on mismatch and
quietly succeeds on match?**

Combine 4.3 (import list = wants) with 4.1 (custom section = friendly
declaration + version + human-readable summary). The import list is the
truth (you cannot lie about what symbols you reference); the custom section
adds version-tagged metadata (display name, reason strings, intended use)
that the host can show to the user during the grant prompt.

---

### Verdict for ark

**Recommended mechanism:** *Hybrid 4.3 + 4.1*.

1. Plugin imports live under a tightly namespaced module, e.g.
   `ark:cap/fs.read`, `ark:cap/term.spawn`, `ark:cap/net.outbound`. The
   host owns a static `HashMap<&'static str, Capability>` registry. Any
   import outside this namespace = refuse to load.
2. Plugin embeds a custom section `ark-caps:v1` with a postcard-encoded
   `CapabilityManifest { version, plugin_name, requested: Vec<CapRequest>,
   reason_strings: HashMap<CapId, String> }`. This is parsed via
   `wasmparser` against `&[u8]` — no `Module::new`, no JIT.
3. Host validates: import list and section MUST agree. If they disagree,
   refuse — the plugin's manifest lies about what it actually links.
4. Host loads `ark.kdl`, intersects user grants with requested caps. Any
   requested-but-not-granted = refuse to load with a clear message.
5. Linker is built with only the granted host functions. Ungranted imports
   are NOT stubbed as traps — they are absent, so instantiation fails
   loudly with `unknown import` rather than at first call.

**Why this combo:**

- *Authoritative on what the code can do:* the import list — you cannot
  reference a symbol you did not import.
- *Friendly to humans:* the custom section carries display name and reason
  strings for the consent prompt.
- *Cross-checked:* import-vs-section disagreement is a build-time bug
  surfaced at load.
- *Zero plugin-code execution before the grant decision:* both checks are
  static against bytes / `Module` types.
- *Component-model upgrade path:* if ark later goes full components, 4.3
  becomes 4.4 with type richness, and the custom section becomes a WIT
  annotation — the policy code barely changes.

**Failure mode on capability mismatch.** Load-time refusal. The plugin
binary is rejected before any wasm code executes; no `Store` is created,
no linear memory is allocated for it, no host function is wired.

**Load-time CLI error a user sees on cap mismatch:**

```
ark: refused to load plugin "git-status-bar"
  declared capabilities (ark-caps:v1):
    ark:cap/fs.read       — "watch repo for HEAD changes"
    ark:cap/term.spawn    — "run git status-porcelain"
    ark:cap/net.outbound  — "fetch upstream summary"   (NOT GRANTED)

  granted in ark.kdl:
    ark:cap/fs.read
    ark:cap/term.spawn

  fix: add `cap "ark:cap/net.outbound"` under `plugin "git-status-bar"`
       in ark.kdl, or rebuild the plugin without that capability.
```

And on import-vs-section drift:

```
ark: refused to load plugin "git-status-bar"
  declared in ark-caps:v1: [fs.read, term.spawn]
  imported by wasm:        [fs.read, term.spawn, net.outbound]
  drift: net.outbound is imported but not declared.
  this plugin's manifest does not match its code. report to the author.
```

Both errors point to one file the user can edit (`ark.kdl`) or one party
they can blame (the plugin author). No silent traps, no first-use
surprises, no capability creep.

---

Sources:
- [wasmtime::Module — imports() docs](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html)
- [wasmtime::component::Component — component_type()](https://docs.rs/wasmtime/latest/wasmtime/component/struct.Component.html)
- [wasmtime::Linker — define_unknown_imports_as_traps](https://docs.wasmtime.dev/api/wasmtime/struct.Linker.html)
- [wasmtime issue #10663 — define_unknown_imports_as_traps shadowing](https://github.com/bytecodealliance/wasmtime/issues/10663)
- [wasmparser — event-driven wasm parser](https://github.com/bytecodealliance/wasmparser)
- [wit_component::metadata — component-type custom section encoding](https://bytecodealliance.github.io/wrpc/wit_component/metadata/index.html)
- [wit-component crate](https://crates.io/crates/wit-component)
- [Spin — allowed_outbound_hosts in component manifest](https://developer.fermyon.com/spin/v2/http-outbound)
- [Spin Writing Apps — capability manifest](https://spinframework.dev/v2/writing-apps)
- [wasmCloud capability concepts](https://wasmcloud.com/docs/concepts/capabilities/)
- [wasmCloud provider-archive — signed JWT claims](https://github.com/wasmCloud/provider-archive)
- [Component Model linker introspection](https://docs.rs/wasmtime/latest/wasmtime/component/struct.Linker.html)
- [wasm-tools — wasmparser repo](https://github.com/bytecodealliance/wasm-tools)
- [Wasmtime API access to custom sections — issue #10218](https://github.com/bytecodealliance/wasmtime/issues/10218)

---

## Cluster 2: Multi-Target Render Abstraction

How do plugin/extension systems keep one extension model viable across multiple
render targets (terminal vs GUI, desktop vs mobile, native vs web, Chrome vs
Firefox)? Surveyed seven systems for the exact manifest field that classifies
which targets a plugin runs on, what plugins emit (data vs widgets vs pixels),
how mismatch is refused, and whether "works in both, degrades" is a first-class
case. Compiled to inform ark's terminal-or-GUI extension classification.

---

### 1. Tauri (Rust core, multiple frontend + platform targets)

**Frontend story**: Tauri's "frontend" is whatever you point its WebView at —
Tauri does not abstract rendering itself. The Rust core hosts an OS WebView
(WebView2 / WKWebView / WebKitGTK). All UI plugins ship as a Cargo crate
*plus* an optional NPM package that supplies JS bindings to invoke the
Rust commands. There is no "alternate frontend" — the WebView is the only one.

**Multi-target manifest**: per-platform structure is *implicit in the project
layout*, not in a single manifest field:

- `desktop.rs` and `mobile.rs` source files in the plugin crate
- optional `android/` (Gradle library project) and `ios/` (Swift package)
  sibling directories
- `tauri plugin init --android --ios` scaffolds the platform dirs

**Where platform IS declared explicitly**: in *capability files* (the security
ACL), not the plugin manifest. Each capability JSON/TOML in
`src-tauri/capabilities/` may carry a `platforms` array:

```json
{
  "identifier": "desktop-capability",
  "windows": ["main"],
  "platforms": ["linux", "macOS", "windows"],
  "permissions": ["global-shortcut:default"]
}
```

Allowed values: `linux`, `macOS`, `windows`, `iOS`, `android`. Default = all.
This is the *exact* mechanism for "this plugin's API is only available on
desktop" — global-shortcut goes in a desktop capability, nfc/barcode-scanner
in a mobile one. Refusal is silent: the permission isn't granted on a
non-listed platform, and any frontend `invoke()` of the gated command throws
"command not allowed".

**What plugins emit**: nothing visual. Plugins expose Rust `#[command]`
functions invoked over Tauri's IPC bridge from the frontend JS. The frontend
chooses how to render; the plugin is data/effect only.

**Refuse-to-load UX**: plugins themselves never refuse — capabilities just
gate which IPC commands the WebView can call on which platform. Mismatch =
runtime "denied" error, not install-time refusal.

**Both-but-degrades?** Yes, expressed by writing matching `desktop.rs` and
`mobile.rs` impls with conditional compilation; the *plugin* is single-target
but the *implementation* per-target. No first-class "fallback" notion in the
manifest.

---

### 2. Lapce + Floem (Rust core, native GPU GUI only)

**Frontend story**: Lapce only has a Floem (wgpu-backed) frontend. There is no
terminal mode of Lapce itself — the editor is GUI-only. The "built-in
terminal" is a Floem widget hosting a PTY, not a separate render target.
So Lapce dodges the multi-target problem by having one target.

**Multi-target manifest**: Plugins (called *volts*) are WASI modules with a
`volt.toml`. The known fields:

```toml
name = "lapce-rust"
version = "0.0.1"
description = "Rust language support"
author = "lapce"
display-name = "Rust"
icon = "rust.svg"
repository = "https://github.com/lapce/lapce-rust"
wasm = "bin/plugin.wasm"

[activation]
language = ["rust"]
workspace-contains = ["**/Cargo.toml"]

[config]
"volt.serverPath" = { default = "", description = "..." }
```

There is **no** target/render/kind field. Every volt is a WASI compute module
that cannot render anything directly. Activation is by language or by
workspace contents — i.e., "load me when the user opens a Rust file" — not
by render capability.

**What plugins emit**: LSP-shaped data through the `lapce-proxy` process. The
proxy mediates between WASI plugins and the Floem frontend. Plugins return
JSON; Floem widgets render it. There is *no* GPUI/Floem handle exposed to
plugins — the WASI sandbox forbids it.

**Refuse-to-load UX**: not applicable; every volt is renderable because no
volt renders. The decision "can this volt run here" is purely "is it a
matching language/file?".

**Both-but-degrades?** N/A — single render target.

**Lesson for ark**: Lapce shows the *brutal-simplicity* end of the spectrum.
By forbidding plugins from touching the GUI at all, the manifest needs zero
target fields. The cost: extensions cannot add panels, status items, or
custom views without core changes. (There is an open issue stream asking for
exactly this.)

---

### 3. Zed + GPUI (Rust core, native GPU GUI only — proposed visual API)

**Frontend story**: Zed is GPU-only (GPUI on wgpu). Like Lapce, no terminal
target.

**Multi-target manifest**: `extension.toml` declares *what kind of thing the
extension provides*, not what render target it needs. Because there's only
one target, the question is "what subsystem do you plug into?" — themes,
languages, debuggers, snippets, slash-commands, MCP servers, icon themes.
Each is registered by a *typed sub-table*:

```toml
id = "my-extension"
name = "My Extension"
version = "0.0.1"
schema_version = 1
authors = ["..."]
description = "..."
repository = "..."

[slash_commands.echo]
description = "echoes the provided input"
requires_argument = true

[slash_commands.pick-one]
description = "pick one of three options"
requires_argument = true

[language_servers.my-lsp]
name = "My LSP"
languages = ["My Language"]
```

The presence of `[slash_commands.*]` *is* the declaration that the extension
provides slash commands. There is no boolean "needs-gui = true" — every
extension implicitly needs Zed.

**What plugins emit**: WIT-typed values across a Wasm boundary. For a slash
command: `SlashCommandOutput { text: String, sections: Vec<SlashCommandOutputSection> }`.
The host renders. **Extensions never receive a GPUI handle.**

**Visual extensions proposal** (Discussion #53403): explicit RFC to expand
the WIT to include a *declarative component tree* — `container`, `text`,
`button`, `list` — that the host renders into GPUI. Stated rationale:

1. Security — no arbitrary HTML/CSS, no XSS, no native pointer escape.
2. Performance — host enforces frame budgets and can disable slow extensions.
3. Consistency — extensions inherit the user's theme automatically.
4. Stability — VS Code's webview model produces "laggy, crashing extensions
   and inconsistent UI"; Zed explicitly rejects that path.

So Zed is converging on: **plugins emit *abstract widget trees*; host
materializes them into native GPUI nodes**. This is the data-vs-pixels
pattern at full strength — even when the extension's purpose IS UI, it
hands the host a *recipe*, not a handle.

**Refuse-to-load UX**: schema validation at load time — `extension.toml`
must declare `schema_version = 1`; unknown sub-tables are tolerated;
malformed Wasm fails to instantiate with an inline error in the extensions
panel.

**Both-but-degrades?** N/A — single target.

---

### 4. VS Code (`extensionKind` + `capabilities.virtualWorkspaces` + web/desktop split)

**Frontend story**: VS Code runs in three render contexts — desktop Electron,
remote (extension host on remote machine, UI on local), and web (VS Code
for the Web in a browser). Plus virtual workspaces (no FS, e.g., GitHub
repos browsed read-only). The single extension model has to span all of these.

**Multi-target manifest**: three orthogonal declarations in `package.json`:

#### a. `extensionKind` — where the extension *runs*

```json
"extensionKind": ["ui"]               // must run on the UI host (local)
"extensionKind": ["workspace"]        // must run where the workspace is
"extensionKind": ["ui", "workspace"]  // prefers UI; falls back to workspace
```

Semantics:
- `["workspace"]` — needs FS / process / shell access where the code lives.
  Most extensions. In remote dev, runs on the remote.
- `["ui"]` — needs local OS bits (clipboard, native modules, low latency to
  monitor). Runs on the user's machine. **In VS Code for the Web with
  Codespaces, a `ui`-only extension cannot load** unless it is also a
  *web* extension.
- `["ui", "workspace"]` — order is preference. Both fine; UI preferred.

#### b. `capabilities.virtualWorkspaces` — does it work without a real FS?

```json
"capabilities": {
  "virtualWorkspaces": {
    "supported": false,
    "description": "Debugging is not possible in virtual workspaces."
  }
}
```

Accepted shapes:
- `true` — fully supported (default if omitted)
- `false` — refuse to activate; show description in UI
- `"limited"` — works partially; description shown as warning

#### c. Web extension manifest split — does it run in a browser worker?

`browser` field in `package.json` (vs `main` for Node). Presence = web-capable.
Without a `browser` entry, the extension is desktop-only.

**What plugins emit**: TypeScript/JS objects via the VS Code extension API.
For UI: TreeView providers (declarative tree of items host renders), CodeLens
providers, Webview panels (sandboxed iframe — the "escape hatch" that Zed
critiques). Most extensions emit *data into typed providers*; webviews are
the exception where the extension ships HTML/CSS/JS.

**Refuse-to-load UX**: triple-layered.
1. *Resolved-but-greyed-out* in the Extensions panel with the reason
   ("This extension is not supported in virtual workspaces") and a button
   linking to the description.
2. *Activation events suppressed* — `onLanguage:rust` won't fire if the
   extension's `extensionKind` doesn't match the current host.
3. *Install-time block* in VS Code for the Web: extensions without a
   `browser` entry are not installable from the marketplace.

**Both-but-degrades?** Yes, formal: `"limited"` for `virtualWorkspaces`
with a `description` is the canonical "works but with caveats" channel.
Also `extensionKind: ["ui", "workspace"]` is "either host is fine".

**Lesson for ark**: VS Code's three-axis classification (host location ×
filesystem realness × browser-vs-Node) is the most evolved we found. Every
axis has a `false` / `"limited"` / `true` ladder with a human-readable
`description` shown to the user when a refusal happens. This is the gold
standard for graceful refusal UX.

---

### 5. iOS `UIRequiredDeviceCapabilities` (Info.plist)

**Frontend story**: not a plugin system — but the canonical "this app needs
hardware feature X, App Store will refuse to install on devices without it"
pattern. Worth surveying because ark's terminal-vs-GUI is morally the same
dichotomy.

**Multi-target manifest**: `UIRequiredDeviceCapabilities` in `Info.plist`,
two equivalent shapes:

```xml
<!-- Array form: every key listed is required -->
<key>UIRequiredDeviceCapabilities</key>
<array>
    <string>arm64</string>
    <string>metal</string>
</array>

<!-- Dictionary form: per-key true/false -->
<key>UIRequiredDeviceCapabilities</key>
<dict>
    <key>metal</key>           <true/>
    <key>telephony</key>       <false/>  <!-- must NOT be present -->
</dict>
```

Allowed keys (~30 total): `armv7`, `arm64`, `metal`, `gps`, `gyroscope`,
`magnetometer`, `front-facing-camera`, `auto-focus-camera`, `bluetooth-le`,
`nfc`, `wifi`, `microphone`, `accelerometer`, `sms`, `telephony`, etc.

**Refuse-to-load UX**: App Store refuses install on non-matching devices —
the user sees "This app is not compatible with this device" and *cannot
download it at all*. Hard refusal at distribution time, before any code
runs.

**Constraint that bit Apple developers**: a published version cannot *add*
capability requirements that would exclude devices the previous version
supported. Apple enforces this at App Store Connect submission.

**Both-but-degrades?** No. `UIRequiredDeviceCapabilities` is binary.
For "use this if present": apps must check at runtime via the relevant
framework (e.g., `CMMotionManager.isGyroAvailable`) and adapt. Apple
deliberately separates *required* (manifest, install-time) from
*optional* (runtime probe).

**Lesson for ark**: install-time hard refusal is the right move when the
extension would crash or be useless. Runtime probe is for graceful
degradation. The two are *separate* mechanisms.

---

### 6. Android `<uses-feature>` (AndroidManifest.xml)

**Multi-target manifest**: same idea as iOS, finer-grained, with an explicit
"required" boolean per feature and a special-cased `glEsVersion` numeric:

```xml
<uses-feature
    android:name="android.hardware.camera"
    android:required="true" />

<uses-feature
    android:glEsVersion="0x00030001"
    android:required="true" />

<uses-feature
    android:name="android.hardware.bluetooth_le"
    android:required="false" />  <!-- nice-to-have, runtime check -->
```

Key axes:
- `android:name` — feature ID (~150 valid strings)
- `android:required` — `true` (Play Store filters device list) or `false`
  (informational; app should runtime-probe)
- `android:glEsVersion` — single packed hex int; if specified multiple
  times, the *highest* wins (so an OpenGL 3.1 game implicitly works on
  3.2 devices); default if omitted = OpenGL ES 1.0

**Refuse-to-load UX**: Play Store hides the listing from incompatible
devices; sideloaded APKs install but the app can read the feature list
and degrade. So the *same manifest* drives store-filtering AND runtime
adaptation.

**Both-but-degrades?** Yes, first-class — `required="false"` means "I
prefer this feature, but I'm fine without it; I'll runtime-check". The
`PackageManager.hasSystemFeature(...)` API is the runtime side of the same
declaration. This is the cleanest "declare-once, dual-purpose" model in
the survey.

**Lesson for ark**: a per-capability `required: bool` is the right knob
for "GPU widgets are optional, terminal is required". The same field
populates both install-time gating and runtime feature-detection.

---

### 7. WebExtensions `browser_specific_settings` + per-permission gating

**Frontend story**: the same extension package targets Chrome, Firefox,
Edge, Safari — four different browsers with overlapping but not identical
APIs. No common host; each browser ships its own implementation of the
WebExtensions spec.

**Multi-target manifest**: there is **no** field that says "Chrome-only" or
"Firefox-only" directly. Cross-browser declaration is achieved by:

#### a. `browser_specific_settings` — host-specific overrides

```json
"browser_specific_settings": {
  "gecko": {
    "id": "my-ext@example.com",
    "strict_min_version": "109.0"
  },
  "gecko_android": {},
  "safari": {
    "strict_min_version": "16.4"
  }
}
```

Sub-keys: `gecko` (Firefox desktop), `gecko_android` (Firefox Android),
`safari`. **Chrome ignores this key entirely.** So:

- "Firefox-only" effect: include `gecko` with required `id`, distribute
  only to AMO. Chrome would silently ignore the manifest noise.
- "Chrome-only" effect: omit `browser_specific_settings`; distribute only
  to the Chrome Web Store. There is no negative declaration.

#### b. Permission strings — per-API gating

```json
"permissions": ["tabs", "storage", "nativeMessaging"]
```

Each browser implements the union of WebExtensions APIs it supports.
Requesting `nativeMessaging` on a browser that doesn't support it yields
a load error in that browser only. There is no "permission requires
browser X" declaration — the browser's *implementation* is the gate.

**Refuse-to-load UX**: browser-specific. Firefox refuses install if
`strict_min_version` exceeds the running version. Chrome silently no-ops
unknown permissions. Safari shows a generic "extension cannot be loaded".
Distribution channels (AMO, CWS) do additional pre-publish validation.

**Both-but-degrades?** Implicitly, by writing the JS to feature-detect
(`if (browser.menus) { ... }`). The manifest does not model graceful
degradation; the *code* does. Cross-browser portability is mostly the
extension author's responsibility, with `browser_specific_settings` as a
narrow per-host config escape hatch.

**Lesson for ark**: this is the *anti-pattern* for ark's case. Targeting
multiple hosts via "lowest common denominator API + per-host overrides +
silent ignore" works for browsers (because there's a spec everyone
mostly implements) but would be terrible for terminal-vs-GUI ark, where
the API surfaces are genuinely different. ark's case is closer to
iOS/Android's "declare hardware needs explicitly" than to WebExtensions'
"hope the host implements it".

---

## Cross-cutting Synthesis

### Granularity: how big is the classification enum?

| System | Axis count | Values |
|---|---|---|
| Tauri | 1 (capability `platforms`) | linux/macOS/windows/iOS/android (5) |
| Lapce | 0 | n/a — single target |
| Zed | 0 | n/a — single target; provides-what enum is large but orthogonal |
| VS Code | 3 | extensionKind {ui, workspace}; virtualWorkspaces tri-state; web vs node entry |
| iOS | 1 | ~30 capability strings, all binary required |
| Android | 1 | ~150 feature strings + per-entry `required` boolean |
| WebExtensions | 1 (host) | gecko/gecko_android/safari sub-keys + version ranges |

Pattern: when you have **one render target**, you don't need a target enum
at all (Lapce, Zed). When you have **a small fixed set of render targets**
(desktop OSes, mobile OSes, browsers), a string enum suffices (Tauri,
WebExtensions). When you have **orthogonal axes** (host location ×
filesystem realness × runtime), you split into multiple boolean/tri-state
fields each with their own description (VS Code).

### What plugins emit

| System | Emits |
|---|---|
| Tauri | IPC commands (data only); WebView is owned by the app, not the plugin |
| Lapce | LSP-shaped JSON via WASI |
| Zed | WIT-typed structs via Wasm; proposed: declarative component trees |
| VS Code | TypeScript objects into typed providers + (escape hatch) webview HTML |
| WebExtensions | DOM manipulations + browser API calls |

The data-vs-pixels axis is real. The systems that grant plugins *raw render
access* (VS Code webviews, WebExtensions content scripts) regret it
publicly — Zed cites VS Code webview pain as the reason for a declarative
component model. The trend, especially for Rust-native editors, is **plugins
emit abstract widget trees; host materializes**.

### Refuse-to-load UX patterns observed

1. **Hard install-time block with description shown** (iOS, Android Play
   Store, VS Code Web for desktop-only extensions). Extension never
   appears in the user's library on incompatible hosts.
2. **Soft install but greyed activation** (VS Code on a virtual
   workspace). Extension visible, marked unavailable with a one-line reason
   linking to docs.
3. **Silent runtime denial** (Tauri capability mismatch, WebExtensions
   unknown permission). Plugin loads, calls fail. Worst UX — looks like a
   bug to the user.
4. **Schema validation at load** (Zed, Lapce). Manifest typo = inline
   error in the extensions panel.

The pattern that shows up repeatedly in well-loved systems: **the same
manifest field that drives install-time refusal also drives the
human-readable explanation shown on refusal**. iOS's empty error vs
Android's Play Store filter explanation vs VS Code's `description` string
— developers prefer the ones with explanations.

### Works-in-both-degrades-gracefully

Three models:

- **None** (iOS, Tauri, Lapce, Zed). Capability is binary; either you
  qualify or you don't. Runtime adaptation is a separate code-level
  concern.
- **Manifest-tri-state** (VS Code's `virtualWorkspaces: "limited"` with
  description; WebExtensions' optional `permissions`). The manifest
  *names* the degradation and explains it, but the host still loads
  the extension; the extension's code probes and adapts.
- **Per-capability required-flag** (Android's `<uses-feature
  required="false">`). One declaration drives both store-filter AND
  runtime probe. Cleanest dual-purpose model.

---

## Verdict for ark

ark is closer to **iOS/Android** (single user, hardware-classed feature
needs) than to **WebExtensions** (per-host API negotiation). The
terminal-vs-GUI choice is a *target capability*, not an API dialect.

### Recommended classification scheme

A small typed enum on the extension manifest, plus a per-capability
required-flag for the cross-cutting "needs GPU" concern. Concretely:

```kdl
extension "claude-code" {
    runs-on "terminal" "gui"   // both — degrades implementation per host
    requires "tty"             // hard requirement when in terminal mode
    prefers  "gpu-canvas"      // soft — uses if present
}

extension "timeline-viz" {
    runs-on "gui"              // GUI-only
    requires "gpu-canvas"      // hard — refuse to load otherwise
}

extension "lsp-bridge" {
    runs-on "terminal" "gui"
    // no requires/prefers — pure data, host-agnostic
}
```

Where:

- `runs-on` is a closed enum of **3 values**: `terminal`, `gui`,
  `headless`. (The third covers daemons / background workers / test
  fixtures.) Multi-value = "I have a render impl for each listed host".
- `requires` is a list of **capability strings** (small closed set, ~5
  initially: `tty`, `gpu-canvas`, `clipboard`, `process-spawn`,
  `network`). Hard. Mismatch = refuse-to-load with the missing
  capability shown in the UI.
- `prefers` is the same vocabulary, soft. Extension code can probe at
  runtime via a host-provided `host.has_capability(name)` API. The
  manifest declaration drives docs/discovery; the runtime probe drives
  branching code.

### Why this shape

1. **Three host values, not five**: ark only ships terminal (today) and
   GUI (later). `headless` covers the daemon-y bits that need neither.
   A 3-value enum stays comprehensible; iOS-style 30-key dictionaries
   are overkill for a project with one developer.

2. **Multi-target via list, not boolean**: VS Code's
   `extensionKind: ["ui", "workspace"]` is the proven shape for "I'm
   fine with either host". Same idiom, smaller vocabulary.

3. **Capability strings, not types**: a typed `Capability` enum in Rust
   would force every new capability to ship as a core release. Strings
   let extensions list capabilities the host doesn't yet implement
   (manifest validation warns); the host can stabilize the closed set
   over a couple of releases.

4. **Required vs preferred split borrowed from Android**: this is the
   one mechanism that handles the "works in both, degrades" case
   without ambiguity. `requires` = install-time gate; `prefers` =
   doc-only declaration that pairs with a runtime probe.

5. **Plugins emit abstract widget trees, not GPUI/ratatui handles**: take
   Zed's verdict directly. The host owns rendering. A `claude-code`
   extension declaring a `Conversation` widget gets rendered by the
   terminal frontend as ANSI lines and by the (future) GUI frontend as
   a native panel. Same WIT-typed output, two materializers.

6. **Refusal UX = VS Code's pattern**: when a manifest declares
   `requires "gpu-canvas"` and ark is in terminal mode, the extension
   appears in the extensions list with a greyed badge and the line
   "Requires gpu-canvas (not available in terminal mode)". No silent
   denials.

### What to avoid

- A single `kind = "terminal" | "gui" | "both"` string field is too
  coarse. It conflates *render target* (terminal vs GUI) with
  *capability needs* (does it want GPU? a TTY? clipboard?). Splitting
  these matches every well-evolved system in the survey.
- Granting plugins direct render handles (zellij Pane, Floem View). Zed's
  rationale applies in full to ark; the terminal-vs-GUI portability is
  literally what's at stake.
- WebExtensions' "lowest-common-denominator + per-host overrides + silent
  ignore" model. ark's terminal and GUI surfaces are too different to
  share a flat permission namespace.

---

Sources:
- [Tauri — Plugin Development](https://v2.tauri.app/develop/plugins/)
- [Tauri — Capabilities for Different Windows and Platforms](https://v2.tauri.app/learn/security/capabilities-for-windows-and-platforms/)
- [Tauri — Capability reference](https://v2.tauri.app/reference/acl/capability/)
- [Tauri — Permissions](https://v2.tauri.app/security/permissions/)
- [Lapce — Architecture docs](https://docs.lapce.dev/development/architecture)
- [Lapce — main repo](https://github.com/lapce/lapce)
- [Floem — native Rust UI library](https://github.com/lapce/floem)
- [Lapce volt.toml example (lapce-rust)](https://github.com/lapce/lapce-rust/blob/master/volt.toml)
- [Zed — Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)
- [Zed — Slash Command Extensions](https://zed.dev/docs/extensions/slash-commands)
- [Zed — Life of a Zed Extension (Rust, WIT, Wasm)](https://zed.dev/blog/zed-decoded-extensions)
- [Zed — RFC: Visual Extension API (#53403)](https://github.com/zed-industries/zed/discussions/53403)
- [Zed — GPUI WASM (#8203)](https://github.com/zed-industries/zed/discussions/8203)
- [VS Code — Virtual Workspaces guide](https://code.visualstudio.com/api/extension-guides/virtual-workspaces)
- [VS Code — Supporting Remote Development & Codespaces (extensionKind)](https://code.visualstudio.com/api/advanced-topics/remote-extensions)
- [VS Code — Extension Host architecture](https://code.visualstudio.com/api/advanced-topics/extension-host)
- [VS Code Wiki — Virtual Workspaces](https://github.com/microsoft/vscode/wiki/Virtual-Workspaces)
- [Apple — UIRequiredDeviceCapabilities](https://developer.apple.com/documentation/bundleresources/information-property-list/uirequireddevicecapabilities)
- [Android — `<uses-feature>` element](https://developer.android.com/guide/topics/manifest/uses-feature-element)
- [MDN — browser_specific_settings](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json/browser_specific_settings)
- [MDN — manifest.json permissions](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json/permissions)
- [MDN — manifest.json overview](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json)

---

## Cluster 5: Wasm Plugin Lifecycle Hooks

Goal: define the smallest, most idempotent set of lifecycle hooks an ark wasm plugin should export, and decide whether they collapse into one `activate(reason)` or stay as separate `on_install / on_load / on_update` entry points. Evidence is drawn from VS Code, Chrome WebExtensions (MV3), the WASI 0.2 Component Model, Spin, wasmCloud, IntelliJ Platform, and Zellij.

### 5.1 VS Code: `activate(context)` + `deactivate()`

Signature: `export function activate(context: vscode.ExtensionContext): Promise<API> | API` and an optional `export function deactivate(): Promise<void> | void`. The host invokes `activate` exactly once per process when any of the extension's `activationEvents` fires (Manifest V3 of the extension manifest now generates implicit activation events from `contributes`). The host does **not** pass a "reason" enum to `activate`. There is no `onInstalled`-style hook in VS Code at all — the only signal an extension gets that it is on a fresh version is `context.extension.packageJSON.version` vs whatever the extension itself stashed in `context.globalState`.

State the host sets up before the call:

- `context.subscriptions: Disposable[]` — every contribution registered through this array is auto-disposed on `deactivate()`. This is the actual lifetime contract; missing this is the #1 source of memory leaks across the ecosystem.
- `context.globalState` / `context.workspaceState` — KV stores backed by a SQLite file (`~/Library/Application Support/Code/User/globalStorage/state.vscdb` on macOS). They survive uninstall, which is a long-standing complaint (microsoft/vscode#119022).
- `context.extensionMode: ExtensionMode` — `Production | Development | Test`. This is the closest VS Code gets to a "reason" enum and it's about the host environment, not why the extension was activated.
- `context.extension`, `context.storageUri`, `context.extensionUri`, `context.secrets`, etc.

`deactivate()` is sync **or** async (the host awaits the returned promise with a 5s timeout in modern versions). Critical bug pattern: `globalState.update()` calls fired from `deactivate()` get cancelled before they flush (microsoft/vscode#144118). Practical guidance from VS Code's own issue tracker: do all persistence work in `activate`, treat `deactivate` as best-effort cleanup of in-process resources only.

Idempotency: VS Code makes no idempotency promises. `activate` can be called again after a soft "Reload Window" or after the extension host crashes, but with a fresh process — the prior call's globals are gone. Within a single host process there is exactly one `activate` and at most one `deactivate`.

Update lifecycle: VS Code reloads the extension host process on update. The new version's `activate` runs from scratch in a new V8 isolate. There is **no migration hook**; extensions roll their own by reading `globalState.get('schemaVersion')` and bumping it inside `activate`.

### 5.2 Chrome WebExtensions (MV3): `chrome.runtime.onInstalled` + ephemeral SW

The lifecycle here is the most aggressive idempotency-forcing model in the survey. The background context is a service worker that the browser kills after **30 seconds** of no events / no extension API calls. There is no symmetric `onSuspend` hook in Chrome MV3 — by the time the SW could run cleanup code, it's already being torn down. The MDN-documented `runtime.onSuspend` and `runtime.onSuspendCanceled` exist on Firefox MV2 but are explicitly absent from Chrome MV3.

`chrome.runtime.onInstalled` fires with `details.reason: chrome.runtime.OnInstalledReason` where the enum is exactly:

| reason | When |
| --- | --- |
| `install` | Extension was just installed for the first time |
| `update` | Extension was updated to a new version. `details.previousVersion` is set |
| `chrome_update` | Browser was updated. The extension itself didn't change |
| `shared_module_update` | A shared module the extension depends on was updated. `details.id` is the module's extension id |

The contract Chrome enforces:

1. The handler MUST be registered at top level of the SW script, not inside an async callback. Otherwise the SW restart misses the event.
2. Any state used at runtime must be reconstructable from `chrome.storage.{local,sync,session,managed}`. In-memory globals are wiped every 30s of idle.
3. Long-running work uses `chrome.alarms` (not `setTimeout`), because the SW dies before the timer fires.

Why no `deactivate`: Chrome explicitly designed away the symmetric pair. The ecosystem learned that "cleanup hooks" on aggressively-cycled workers are unreliable — extensions were leaking, and developers were assuming `onSuspend` would always run when in fact it ran 70% of the time at best. Removing the hook **forced** every extension to be crash-safe, which is the only correct posture when the runtime is allowed to kill you at any moment.

This is the cleanest design lesson for ark: if the host can kill the guest at will, do not give the guest a goodbye hook. Make it design for sudden death.

### 5.3 WASI 0.2 / Component Model: lifecycle is the host's problem

The Component Model itself **does not define lifecycle hooks**. The MVP explainer's "lifecycle" is purely about instance creation: instantiate, optionally call a start function, drop. The semantics that matter for plugin authors are:

- **Command vs Reactor**: a Component Model component (lowering down to Core Wasm) is either a Command (exports `_start`, runs to completion, exits) or a Reactor (exports `_initialize`, then services calls into its other exports for the lifetime of the instance). Wasmtime's `wasmtime` CLI distinguishes them; reactors are what plugin systems actually want.
- **`_initialize`**: an optional Core Wasm export. If present, the host must call it exactly once, before any other export. Wasmtime's component bindgen invokes it as part of `instantiate_async`, so most embedders never see it. It is *not* a lifecycle hook in the VS Code sense — it's just a "runtime is ready, allocate your tables now" signal.
- **`cabi_realloc`**: Canonical ABI export; the host calls into it whenever it needs to allocate guest-side memory to pass strings/lists across the boundary. It is not a lifecycle hook; it is an allocator. Mentioning it as part of lifecycle is a category error that shows up in some plugin-system designs.
- **Resource destructors**: a `resource` declared in WIT can carry a `dtor` function. The Canonical ABI guarantees that when the **last handle** to a resource is dropped (on either side), the destructor runs. This is real lifetime tracking, but it is per-resource, not per-instance. Async dtors are now allowed under the async proposal.
- **`post-return`**: a `canon lift` option that runs after the host has finished reading return values, so the guest can free the buffers it returned. Per-call, not per-instance.

Net: at the Component Model layer, the host gets an instance with `_initialize`, calls exports until done, then drops the instance and any live resources run their destructors. Anything resembling "install / update / load" is invented at the layer above by the embedding host (Spin, wasmCloud, ark).

### 5.4 Spin: per-request instances, no session lifecycle

Spin's component model is "instance per request": the HTTP trigger receives a request, the host instantiates the component, calls the `wasi:http/incoming-handler` export, then drops the instance. Cold-start is fast enough (sub-millisecond on cached components) that this model is viable. There is **no session-long lifecycle** for a Spin component — no `on_install`, no `on_load`, no `on_update` exposed to the component.

Between requests to the same component, in-memory state does not survive — the instance is gone. State lives in `wasi:keyvalue`, the bundled SQLite store, or external services. This is the same lesson as Chrome's MV3 SW, applied to a different motivation (multi-tenant serverless density rather than battery life).

Implication for ark: a request-scoped instance model would dodge the entire lifecycle question, but it costs us long-lived state inside the guest (subscriptions, in-flight async work, render caches). For an IDE plugin we likely need long-lived instances and therefore must take a position on lifecycle hooks.

### 5.5 wasmCloud: orchestrator owns lifecycle, actor owns nothing

wasmCloud actors (now "components" in the post-Component-Model nomenclature) are managed by the wasmCloud host (`wasmcloud-host`, formerly `wasmcloud-otp`). The actor exposes capability-defined exports (e.g. `wasmcloud:httpserver/handler.handle-request`) and *no lifecycle hooks*. Start/stop is an operator-driven command (`wash start component`, `wash stop component`); it spawns or kills instances inside the host. The actor itself has no `on_start` to react to.

Hot reload: `wash dev` watches the source and replaces the running component. From the actor's perspective this is indistinguishable from being killed and re-started — no migration hook, no "you are about to be unloaded" callback. Long-running work is structured around the host calling exports; if the actor is replaced mid-call the in-flight call fails and the caller sees a transport error.

Lesson: the wasmCloud team explicitly chose not to give actors lifecycle hooks because they wanted operators to be able to restart any actor at any time without coordinating with the actor's code. This is the same posture as Chrome SW, dressed up for the cloud.

### 5.6 IntelliJ Platform: rich lifecycle, manual disposal hierarchy, dynamic load events

IntelliJ is the *opposite* extreme: a deep tree of lifecycle hooks, each with explicit ordering guarantees.

- `com.intellij.postStartupActivity` — runs on a background thread after the project opens, on the indexing-allowed pool. `DumbAware` implementations may run before indexes are ready.
- `com.intellij.backgroundPostStartupActivity` — runs even later, after `postStartupActivity` and after the initial project frame is shown. Used for non-blocking warmup (e.g. fetching remote config).
- `com.intellij.applicationConfigurable` and friends — instantiated lazily on first access, not at startup.
- `Disposable` — the platform's lifetime primitive. `Disposer.register(parent, child)` builds a tree; `Disposer.dispose(parent)` cascades. The plugin's project-scoped service is a typical parent; `context.subscriptions` in VS Code is the same idea with a flat list instead of a tree.
- `DynamicPluginListener` — application-level listener, fires `beforePluginLoaded`, `pluginLoaded`, `beforePluginUnload`, `pluginUnloaded`. Other plugins subscribe to **observe** their dependencies' lifecycles. This is unique in the survey and matters for ark: a "claude-code-subagent" extension wants to know when "claude-code" was unloaded so it can put up a "host extension is gone" placeholder.

Idempotency: `postStartupActivity.runActivity` runs once per project open. On dynamic plugin reload the platform tears down via `Disposer.dispose(plugin)` and re-instantiates from scratch. Plugins that registered via `Disposer` get their cleanup for free; plugins that hold global JVM state outside the disposable tree leak across reloads — a recurring source of ClassLoader leaks.

Update lifecycle: dynamic plugins reload in-place. Non-dynamic plugins force an IDE restart, which is essentially a process restart and skips the lifecycle hook problem entirely.

### 5.7 Zellij: `load(config) / update(event) / render(rows, cols) / pipe(msg)` — no teardown

Zellij is the closest precedent to ark in stack and posture (Rust host, Wasm guests over a stable IDL, ratatui-style render loop). The plugin trait:

```rust
pub trait ZellijPlugin {
    fn load(&mut self, configuration: BTreeMap<String, String>);
    fn update(&mut self, event: Event) -> bool;       // true => please re-render
    fn render(&mut self, rows: usize, cols: usize);
    fn pipe(&mut self, pipe_message: PipeMessage) -> bool;
}
```

Host call discipline:

- `load` is called exactly once after `instantiate`, with the user's KDL `plugin {}` block flattened to a `BTreeMap<String,String>`. This is the correct place to `subscribe([Event::Key, Event::Resize, ...])`.
- `update` is called for every event the plugin subscribed to. Returning `true` schedules a `render`; `false` is the no-op fast path.
- `render` is called when zellij decides the pane needs to repaint (post-update, post-resize, on demand). Plugins write to stdout; zellij captures and composites.
- `pipe` is the inter-plugin RPC: another plugin or `zellij pipe` from the CLI delivers a payload. Same `bool => render` convention.

There is **no teardown hook**. When the plugin's pane closes, zellij drops the wasm instance. The plugin gets no chance to flush state. The Zellij team's explicit reasoning: pane closure is user-initiated and synchronous, the user does not want their `q` keypress to block on a plugin's async cleanup, and a plugin that needs to persist state should write it on every relevant `update` rather than batching to the end. Same lesson as Chrome SW, applied to a TUI multiplexer.

Wire format: protobuf-over-host-functions since v0.38, transparent to authors using `zellij-tile`.

### 5.8 Cross-system idempotency table

| System | "install" hook | "load" hook | "update" hook | "unload" hook | Reason enum | State across restart |
| --- | --- | --- | --- | --- | --- | --- |
| VS Code | none | `activate(ctx)` | none (use globalState version) | `deactivate()` best-effort | none (mode is env, not reason) | `globalState` SQLite |
| Chrome MV3 | `onInstalled(install)` | SW wakes on event | `onInstalled(update)` | none | yes (4 values) | `chrome.storage.*` |
| Component Model | none | `_initialize` (mechanical) | none | resource `dtor`, `post-return` | none | host-defined |
| Spin | none | per-request instantiate | none | per-request drop | none | `wasi:keyvalue` |
| wasmCloud | none (operator) | none | none (operator) | none | none | external |
| IntelliJ | none | `postStartupActivity` | none (DynamicPluginListener observes) | `Disposable` tree | none on activity, yes on Listener events | platform-managed |
| Zellij | none | `load(cfg)` | `update(event)` per event | none | none | none (plugin's job) |
| **ark v0.2 proposal** | (see verdict) | (see verdict) | (see verdict) | (see verdict) | (see verdict) | scene KV + extension storage |

The pattern is overwhelming: production systems either have a single load hook with no install/update distinction (Zellij, Spin, wasmCloud, Component Model itself) or they pair `activate` with a separate `onInstalled(reason)` event (VS Code+Chrome). Nobody in this set ships a unified `activate(reason: enum)` with reasons spanning install/update/load. The closest is Chrome's `onInstalled`, which is install-specific and decoupled from load.

### 5.9 Failure modes the surveyed systems converge on

1. **Sudden death is the only safe assumption.** Every system that has been bitten (Chrome, Spin, wasmCloud, Zellij) eventually removed or never added a "you are about to die" hook because in practice it doesn't fire reliably. VS Code keeps `deactivate()` but documents it as best-effort.
2. **Persistence belongs in `load`/`activate`, not in `deactivate`.** VS Code's `globalState.update()` cancellation bug is the canonical example; the ecosystem's workaround is "write through on every change, treat deactivate as a no-op."
3. **Migration hooks lose to "compare versions in activate."** No system in the survey has a successful `migrate(oldVersion -> newVersion)` hook. They all reduce to "read your schema-version key in activate, run migrations there, rewrite the key."
4. **Cross-plugin observation matters.** IntelliJ's `DynamicPluginListener` is the only mechanism in the survey that lets plugin B watch plugin A's lifecycle. ark's claude-code-subagent / claude-code dependency proves we need this.
5. **A "reason" enum is most useful when it carries data.** Chrome's `OnInstalledReason::Update { previous_version }` is more valuable than the bare reason; without `previous_version` the extension can't migrate. Bare-string reasons get ignored.

### Verdict for ark

**Adopt a small, sudden-death-safe lifecycle that mirrors Zellij's posture, with one compromise borrowed from Chrome (`on-install` for first-run + version bumps).** Reject the unified `activate(reason)` design — the survey shows it is a synthetic abstraction nobody actually runs in production. Keep install detection on a separate hook so the hot path (`load`) never has to branch on reason.

#### Hook signatures (WIT-flavored sketch)

```wit
interface ark-plugin {
    use ark:host/types.{config-map, install-event, scene-snapshot, render-rect, event, pipe-message, plugin-error};

    /// Called once per plugin **version** seen by this host installation.
    /// Fires before load() on the activation that triggered it.
    /// Plugin uses this for migrations and one-shot side effects (writing
    /// default config, registering global hotkeys with the host, etc.).
    /// Idempotent: host persists "last-seen-version" in scene KV; if the
    /// plugin crashes mid-on-install the host re-runs it on next load.
    on-install: func(ev: install-event) -> result<_, plugin-error>;

    /// Called once per instance after instantiate, before any update/render.
    /// Receives the resolved config (KDL plugin{} block flattened) and a
    /// snapshot of the parts of the scene the plugin declared interest in
    /// at manifest-time. Subscribe to events here.
    load: func(cfg: config-map, scene: scene-snapshot) -> result<_, plugin-error>;

    /// Per-event entry point. Return value: did anything render-relevant
    /// change? Cheap no-ops return false.
    update: func(ev: event) -> result<bool, plugin-error>;

    /// Called when host wants pixels. Plugin writes to a host-provided
    /// framebuffer / stdout (TBD; see render kit).
    render: func(rect: render-rect) -> result<_, plugin-error>;

    /// Inter-extension RPC. Same render-bool convention as update.
    pipe: func(msg: pipe-message) -> result<bool, plugin-error>;
}
```

No `deactivate` / `on-unload`. The host drops the instance; resource destructors handle host-allocated handles via Component Model semantics. Plugins that need to persist must write through on every `update` — same rule as Chrome MV3 and Zellij.

#### `install-event` shape (the one place a "reason" enum is justified)

```wit
variant install-event {
    /// First time this plugin id has been seen on this host.
    fresh-install(version),
    /// Plugin id was already known; version differs from last-seen.
    upgrade(upgrade-info),
    /// Host (ark) version changed; plugin code is unchanged.
    /// Mostly informational; used by plugins that pin to host capabilities.
    host-upgrade(host-upgrade-info),
    /// A plugin this plugin depends on (declared in manifest) was upgraded.
    /// Mirrors Chrome's shared_module_update.
    dependency-upgrade(dependency-info),
}

record version { semver: string }
record upgrade-info { from: version, to: version }
record host-upgrade-info { from: string, to: string }
record dependency-info { plugin-id: string, from: version, to: version }
```

This is the *only* hook that takes a reason, and the reason carries the data needed to act on it (previous version, dependency id). All four variants line up with `chrome.runtime.OnInstalledReason` — the one production design that has actually shipped a reason enum at scale.

#### Why not a unified `activate(reason)`?

- It forces the hot-path entry point to branch on a discriminator that is `Load` 99.9% of the time.
- It makes the invariant "load runs after on-install completed" awkward to state — they're the same function, but the plugin must know to re-do load-style work after the install branch.
- No system in the survey ships this design. Zellij, Spin, wasmCloud, IntelliJ's `postStartupActivity`, and the Component Model itself all keep load separate from any install/update event.
- It conflates a per-instance call (`load`, runs on every spawn) with a per-host-installation call (`on-install`, runs once per version per host). They have different idempotency stories and different host-side state.

#### Idempotency contract the host enforces

| Hook | Called per | Host pre-state | Re-entrancy on crash | Plugin's safe assumption |
| --- | --- | --- | --- | --- |
| `on-install` | unique (plugin-id, version, ark-host-uuid) tuple, advisory only | "last-seen-version" key in scene KV; bumped *after* hook returns ok | yes — if plugin crashes before returning ok, host re-runs on next `load` | "I may run again. Make my writes idempotent (UPSERT, mkdir -p)." |
| `load` | every instance spawn | scene snapshot built; subscriptions table cleared | yes — on crash host respawns, re-runs `load` from scratch | "Nothing in memory survives across calls. Read state from host KV." |
| `update` | every subscribed event | up-to-date scene state | yes — failed updates can be replayed; events are idempotent at the source | "I may see the same logical event twice. Dedupe via event-id if it matters." |
| `render` | host repaint trigger | render rect computed | no — host skips this frame on error | "I may be killed mid-frame. Don't mutate persistent state from render." |
| `pipe` | per inbound message | sender authenticated by host | yes — sender will see error and may retry | "Sender retries on transport error. Make handlers idempotent or assign request-ids." |

#### How the host handles hook failures

| Hook | Failure policy | Rationale |
| --- | --- | --- |
| `on-install` | Log + retry on next `load`; if fails 3 times consecutively, mark plugin disabled and surface in TUI | Install hooks are rare and one-shot; aggressive retry is fine. Disabling protects the user from a broken plugin holding back the rest of the scene. |
| `load` | Log + unload the plugin instance immediately. Do not retry until user/scene re-triggers it. Surface error in plugin-status pane. | A plugin that can't load is broken; silent retry loops hide bugs. Unloading frees the wasm store. Mirrors Zellij's behavior. |
| `update` | Log + skip this event; keep the plugin alive. After N consecutive failures (config: default 16), unload the instance and surface error. | Per-event failure is often transient (bad payload, race). Don't kill the plugin on the first one. Threshold prevents a stuck plugin from spamming logs forever. |
| `render` | Log + paint a "plugin error" placeholder in the rect; keep the plugin alive. Rate-limit logs. | Render errors are very common during dev; killing the plugin makes the iteration loop painful. Placeholder gives the user a visible signal without losing the pane. |
| `pipe` | Return the plugin-error to the sender as a typed transport error; keep the plugin alive. | The sender is in the best position to decide retry vs. give-up. Don't make pipe errors plugin-fatal. |
| `panic` (any hook traps the wasm store) | Unload the instance, log the trap, surface in plugin-status. Do not auto-restart unless the scene declares a restart policy. | A trap means the wasm store is poisoned; we cannot continue using it. Auto-restart should be opt-in per plugin (mirrors systemd's `Restart=` rather than VS Code's "extension host crashed, reload?" modal). |

The defaults intentionally lean toward "keep the plugin alive, surface the error, let the user decide" rather than "fail fast, kill on first error." For an IDE the cost of a flapping plugin is much lower than the cost of a plugin that vanishes when the user is mid-edit.

#### Cross-plugin observation (lifted from IntelliJ)

ark adds one host-side event stream that plugins can subscribe to via `update`:

```
Event::PluginLifecycle { plugin_id, kind: PluginLifecycleKind }
PluginLifecycleKind = Loaded | Unloaded | Crashed | Upgraded { from, to }
```

This is the IntelliJ `DynamicPluginListener` analog and is what `claude-code-subagent` needs to know when `claude-code` goes away. No new hook is required; it rides the existing `update` channel. The host emits these events synchronously after the corresponding hook returns ok (or after the unload/crash settles).

#### Summary of the contract

- Five hooks: `on-install`, `load`, `update`, `render`, `pipe`. No `deactivate`.
- Exactly one hook (`on-install`) carries a reason enum, and the enum mirrors Chrome's `OnInstalledReason` because that is the one production design that worked.
- Sudden-death-safe: every hook is re-entrant; the host re-runs `load` on respawn; `on-install` is retried until it succeeds or hits the failure threshold.
- Hook failures default to "keep the plugin alive, surface the error, log." Unload only on `load` failure or wasm trap.
- Cross-plugin lifecycle observation rides on `update` events, not a separate hook.

This collapses to the smallest set the surveyed systems empirically converge on, with one targeted addition (`on-install` with a Chrome-style reason) for the migration use case Zellij conspicuously lacks and which an IDE plugin ecosystem will need within the first few releases.

---

Sources (Cluster 5):

- [VS Code — Extension Anatomy (activate / deactivate)](https://code.visualstudio.com/api/get-started/extension-anatomy)
- [VS Code — Activation Events](https://code.visualstudio.com/api/references/activation-events)
- [VS Code — ExtensionContext + ExtensionMode](https://code.visualstudio.com/api/references/vscode-api)
- [microsoft/vscode#144118 — globalState.update cancelled in deactivate](https://github.com/microsoft/vscode/issues/144118)
- [microsoft/vscode#119022 — uninstall does not clear globalState](https://github.com/microsoft/vscode/issues/119022)
- [Chrome — Extension service worker lifecycle](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle)
- [Chrome — Handle events with service workers](https://developer.chrome.com/docs/extensions/get-started/tutorial/service-worker-events)
- [MDN — runtime.onInstalled](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/runtime/onInstalled)
- [MDN — runtime.OnInstalledReason](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/API/runtime/OnInstalledReason)
- [WebAssembly Component Model — Explainer](https://github.com/WebAssembly/component-model/blob/main/design/mvp/Explainer.md)
- [Component Model — resource destructors and post-return](https://component-model.bytecodealliance.org/)
- [Dylibso — WASI Command and Reactor Modules (`_initialize` / `_start`)](https://dylibso.com/blog/wasi-command-reactor/)
- [bytecodealliance/wasi-rs#87 — `cabi_realloc`](https://github.com/bytecodealliance/wasi-rs/issues/87)
- [Spin — HTTP Trigger (instance-per-request)](https://developer.fermyon.com/spin/v2/http-trigger)
- [Fermyon Serverless Guide — Statelessness](https://www.fermyon.com/serverless-guide/statelessness)
- [wasmCloud — components and actor lifecycle](https://wasmcloud.com/blog/webassembly_components_and_wasmcloud_actors_a_glimpse_of_the_future)
- [wasmCloud#103 — does not properly stop actors](https://github.com/wasmCloud/wasmCloud/issues/103)
- [IntelliJ Platform — Disposer and Disposable](https://plugins.jetbrains.com/docs/intellij/disposers.html)
- [IntelliJ Platform — Dynamic Plugins (DynamicPluginListener)](https://plugins.jetbrains.com/docs/intellij/dynamic-plugins.html)
- [Zellij — Plugin Lifecycle](https://zellij.dev/documentation/plugin-lifecycle.html)
- [Zellij — Plugin Pipes](https://zellij.dev/documentation/plugin-pipes.html)
- [Zellij — Developing a Rust plugin (ZellijPlugin trait)](https://zellij.dev/tutorials/developing-a-rust-plugin/)

---

## Cluster 1: Editor Wasm Hosts (Zed, Lapce, Helix)

Three editors, three different bets on how to host third-party code. Zed shipped a wasm component-model host and runs the largest extension marketplace today. Lapce shipped a wasm "WASI Preview 1 + JSON-RPC" plugin proxy and stalled. Helix tried wasm three times, gave up, and is incubating a Scheme runtime instead. Below: what each system actually does, with sources, then five lift/avoid bullets for ark.

---

### 1. Zed — wasm component model on wasmtime

**Wasm runtime.** `wasmtime` (workspace dep), full async + component model + epoch interruption.
- `crates/extension_host/Cargo.toml` lists `wasmtime.workspace = true` and `wasmtime-wasi.workspace = true`. Source: [`zed/crates/extension_host/Cargo.toml`](https://github.com/zed-industries/zed/blob/main/crates/extension_host/Cargo.toml).
- Engine config from `crates/extension_host/src/wasm_host.rs`:
  ```rust
  let mut config = wasmtime::Config::new();
  config.wasm_component_model(true);
  config.async_support(true);
  config.enable_incremental_compilation(cache_store()).unwrap();
  config.epoch_interruption(true);
  ```
  Source: [`crates/extension_host/src/wasm_host.rs`](https://github.com/zed-industries/zed/blob/main/crates/extension_host/src/wasm_host.rs).
- Epoch ticks every 100ms; per-store deadline forces every extension to yield via `store.epoch_deadline_async_yield_and_update(1)`. The mechanism is fairness, not absolute time — "this implementation will still let it happily take forever, it just has to let other extensions have a turn." Source: [PR #24986](https://github.com/zed-industries/zed/pull/24986) (Feb 2025).

**Plugin ABI — WIT-defined, not raw exports.** Extensions are wasm components. The interface lives in versioned WIT files under `crates/extension_api/wit/since_v0.6.0/extension.wit`. Source: [`extension.wit`](https://raw.githubusercontent.com/zed-industries/zed/main/crates/extension_api/wit/since_v0.6.0/extension.wit).

Required exports include:
- `init-extension: func()` — extension constructor; `register_extension!` emits `#[unsafe(export_name = "init-extension")] pub extern "C" fn __init_extension()`.
- `language-server-command(language-server-id, worktree)` — return command + args + env to spawn an LSP.
- `language-server-initialization-options(...)`, `language-server-workspace-configuration(...)`.
- `labels-for-completions(language-server-id, completions)`, `labels-for-symbols(...)` — custom completion/symbol rendering as `CodeLabel`.
- `complete-slash-command-argument(command, args)`, `run-slash-command(command, args, worktree?)`.
- `context-server-command(...)`, `context-server-configuration(...)` — for MCP servers.
- `get-dap-binary(...)`, `dap-config-to-scenario(...)`, `dap-locator-create-scenario(...)` — for debug adapters.
- `suggest-docs-packages(provider-name)`, `index-docs(provider-name, package-name, database)`.

Imported interfaces (host functions): `context-server`, `dap`, `github`, `http-client`, `platform`, `process`, `nodejs`, `lsp`, `slash-command`. The host crate links these via `wasmtime-wasi` plus generated bindings.

**Loading lifecycle.** Manifest-driven discovery, then component instantiation:
1. Discover under `~/Library/Application Support/Zed/extensions` (macOS) / `$XDG_DATA_HOME/zed/extensions` (Linux). Source: [zed.dev/docs/extensions/installing-extensions](https://zed.dev/docs/extensions/installing-extensions).
2. Read `extension.toml` → parse `ExtensionManifest` (see below).
3. Compile + cache the wasm component, instantiate, call exported `init-extension`.
4. On feature use (open file in language X, run slash command, etc.), invoke the matching export. Each call gets a fresh epoch deadline; long calls yield to the executor.
5. Update via the marketplace; auto-install via the `auto_install_extensions` setting.

**Identity.** `extension.toml` manifest with stable `id` field. Source: [`extension_manifest.rs`](https://github.com/zed-industries/zed/blob/main/crates/extension/src/extension_manifest.rs):
```rust
pub struct ExtensionManifest {
    pub id: Arc<str>,
    pub name: String,
    pub version: Arc<str>,
    pub schema_version: SchemaVersion,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub authors: Vec<String>,
    pub lib: LibManifestEntry,
    pub themes: Vec<PathBuf>,
    pub icon_themes: Vec<PathBuf>,
    pub languages: Vec<PathBuf>,
    pub grammars: BTreeMap<Arc<str>, GrammarManifestEntry>,
    pub language_servers: BTreeMap<LanguageServerName, LanguageServerManifestEntry>,
    pub context_servers: BTreeMap<Arc<str>, ContextServerManifestEntry>,
    pub agent_servers: BTreeMap<Arc<str>, AgentServerManifestEntry>,
    pub slash_commands: BTreeMap<Arc<str>, SlashCommandManifestEntry>,
    pub snippets: Option<ExtensionSnippets>,
    pub capabilities: Vec<ExtensionCapability>,
    pub debug_adapters: BTreeMap<Arc<str>, DebugAdapterManifestEntry>,
    pub debug_locators: BTreeMap<Arc<str>, DebugLocatorManifestEntry>,
    pub language_model_providers: BTreeMap<Arc<str>, LanguageModelProviderManifestEntry>,
}
```
The `id` is the registry key and "cannot be changed after your extension has been published" (zed.dev docs).

**Capability model.** Declarative — `capabilities = [...]` array in `extension.toml`. The host stores `granted_capabilities: Vec<ExtensionCapability>` on `WasmHost` and gates host functions against it. The variant we have evidence for is `ProcessExec`, checked as `capability.allows(desired_command, desired_args)`. The extension must declare in advance which executables it will spawn; the host refuses anything else. There is no runtime prompt — install-time inspection is the trust boundary.

**View rendering.** Extensions do *not* own pixels. They emit either:
- Configuration data (LSP commands, settings JSON, theme JSON, grammar references, snippet definitions).
- `CodeLabel` records (text + spans) which the host renders into the autocomplete list / outline.
- Slash-command output as structured `SlashCommandOutput` consumed by the agent panel.

The Rhai proposal ([Discussion #40049](https://github.com/zed-industries/zed/discussions/40049), Feb 2025+) explicitly calls out that current extensions can't render UI: "Phase 3 expand to webview, richer events, and script packs" — i.e. webview is *not* in the current ABI. This is a deliberate constraint.

**Render-target portability.** Zed is single-frontend (GPUI, native). Extensions don't declare targets; there's only one. Themes ship JSON usable by GPUI's color/typography system.

**Distribution.** Git-submodule registry. The [zed-industries/extensions](https://github.com/zed-industries/extensions) repo's `extensions.toml` lists every published extension as `[id] submodule = "..." version = "x.y.z"`:
```toml
[1984-theme]
submodule = "extensions/1984-theme"
version = "0.1.3"
```
Each extension is a git submodule; updating means `git submodule update --remote` + bump version + PR. The Zed backend builds wasm artifacts per submodule commit and serves them to the in-app gallery. No OCI, no npm — git tags + submodules.

---

### 2. Lapce — wasmtime + WASI Preview 1 + JSON-RPC over stdio

**Wasm runtime.** `wasmtime` with WASI Preview 1 (not the component model). Source: [`lapce-proxy/src/plugin/wasi.rs`](https://github.com/lapce/lapce/blob/master/lapce-proxy/src/plugin/wasi.rs):
```rust
let engine = wasmtime::Engine::default();
let module = wasmtime::Module::from_file(&engine, wasm_path)?;
let mut linker = wasmtime::Linker::new(&engine);
wasmtime_wasi::add_to_linker(&mut linker, |s| s)?;
HttpState::new()?.add_to_linker(&mut linker, ...)?;
```
HTTP gets in via [`lapce-wasi-experimental-http`](https://lib.rs/crates/lapce-wasi-experimental-http) — a fork of the Bytecode Alliance's experimental WASI HTTP shim.

**Plugin ABI — single export, JSON-RPC over WASI stdio.**
- The plugin must export `handle_rpc`. The host calls it whenever a message arrives:
  ```rust
  instance.get_func(&mut store, "handle_rpc")...
  ```
- Two host functions registered via `linker.func_wrap`:
  - `lapce::host_handle_rpc` — drained from plugin stdout, parsed as JSON-RPC, dispatched.
  - `lapce::host_handle_stderr` — captured for plugin logs.
- Wire format: full LSP-style JSON-RPC. Plugin writes `Content-Length: N\r\n\r\n{...}` to stdout (a WASI pipe); host writes the same to stdin. Source: [`psp.rs`](https://github.com/lapce/lapce/blob/master/lapce-proxy/src/plugin/psp.rs).

The Plugin Server Protocol (PSP) reuses LSP message types verbatim. `PluginServerRpc` is an enum over requests/notifications; `PluginServerRpcHandler` correlates async responses through a `server_pending` map; `PluginHostHandler` checks `ServerCapabilities` (e.g. `Completion::METHOD => self.server_capabilities.completion_provider.is_some()`) before routing. So a Lapce plugin behaves like an LSP server bundled inside a wasm sandbox with extra hooks.

**Loading lifecycle.**
- `load_all_volts(plugin_rpc, extra_paths, disabled)` walks the plugin dir, reads `volt.toml`, registers each as `unactivated_volts`.
- Activation is conditional on `[activation]` rules (file language, `workspace-contains` glob).
- On activation, `start_volt(workspace, configurations, plugin_rpc, meta)` builds a wasmtime store with WASI ctx, sets env vars `VOLT_OS`, `VOLT_ARCH`, `VOLT_LIBC`, opens a preopen dir at the plugin's directory, instantiates, and spawns a thread running the plugin's RPC loop.
- Updates: `volts publish` to plugins.lapce.dev (cargo-installable CLI). The host pulls registry index, downloads new wasm, restarts the plugin. Source: [docs.lapce.dev plugin development](https://docs.lapce.dev/development/plugin-development).

**Identity.** `volt.toml` manifest. Live example from [`sharpSteff/lapce-csharp-plugin/volt.toml`](https://github.com/sharpSteff/lapce-csharp-plugin/blob/master/volt.toml):
```toml
name = "csharp"
version = "2.0.0"
author = "sharpSteff"
display-name = "C#"
description = "C# for lapce using csharp-ls"
wasm = "target/wasm32-wasi/release/lapce-plugin-csharp.wasm"
icon = "logo.png"
repository = "https://github.com/sharpSteff/lapce-csharp-plugin.git"

[activation]
language = ["csharp"]
workspace-contains = ["*/*.csx", "*/*.cs", "*/*.sln"]

[config."csharp.solution"]
default = ""
description = "Path to the solution file"
```
`VoltMetadata` mirrors this: `name, version, display_name, author, description, icon, repository, wasm, color_themes, icon_themes, dir, activation, config`.

**Capability model.** Implicit and coarse:
- Filesystem: WASI preopen scoped to the plugin directory only.
- Network: HTTP via the experimental shim with `allowed_hosts: Some(vec!["insecure:allow-all"...])` and `max_concurrent_requests: Some(100)` — i.e. *no host allowlist enforced by default*. This is the cautionary tale.
- Process spawn: not exposed to the wasm side; the *real* language server runs in a host child process the plugin asks the host to spawn via RPC. So plugins can't fork; they just describe what they want.
- No declarative `capabilities = [...]` like Zed.

**View rendering.** Pure data + RPC. Plugins return LSP responses (completions, diagnostics, hovers) and that's it. Lapce's UI (Floem-based) renders them. Plugins cannot draw.

**Render-target portability.** Lapce ships a single GUI; no terminal frontend. Plugins don't declare targets.

**Distribution.** Centralized: [plugins.lapce.dev](https://plugins.lapce.dev) (registry indexed at [`lapce.github.io/volts2`](https://github.com/lapce/lapce.github.io/blob/master/volts2)). GitHub OAuth for publisher identity. `cargo install volts; volts publish` from the plugin dir uploads bare wasm + manifest. No bundle, no OCI.

---

### 3. Helix — wasm tried 3x, abandoned; betting on Steel (Scheme)

**Wasm runtime.** None in tree. Multiple attempts:
- [PR #2949](https://github.com/helix-editor/helix/pull/2949) (gavynriebau, 2022) — abandoned.
- Two more iterations referenced by maintainer archseer (Jul 2023): "we had three separate attempts at integrating WASM that didn't lead anywhere."
- [Issue #122](https://github.com/helix-editor/helix/issues/122) — long-running tracking issue.

The current decision (re-confirmed in [Discussion #3806](https://github.com/helix-editor/helix/discussions/3806) through 2024 and [Discussion #14457](https://github.com/helix-editor/helix/discussions/14457) "Collected directions for Helix's future"): **Scheme via Steel**, not wasm. From archseer: "the language will remain scheme, but the implementation will make it possible to easily swap languages if for example we choose to implement our own scheme." Steel can `dlopen` cdylibs for native perf escape hatch.

**Why wasm was rejected (every reason is a warning sign for ark).**
1. *No standard ABI.* "WASM lacks a standardized ABI interface, making it impractical without targeting a specific implementation rather than the standard itself" — Discussion #580. The component model fixes this *now*, but didn't when Helix evaluated.
2. *Distribution is binary.* "Binary format requires pre-compilation; no simple git URL installation like Neovim." Helix wanted git-clone-and-go.
3. *Async/event-driven story weak.* "Initial design focused on command callbacks, limiting event-driven plugins."
4. *Implementation size.* "Concern about how large wasm implementations are" — embedding wasmtime adds megabytes to the binary.
5. *Stability.* From [Discussion #13945](https://github.com/helix-editor/helix/discussions/13945) (Jul 2025): "current plan is to embed scheme, because WASM component models are not stable enough yet."

**Steel ABI.**
- `helix.scm` defines exposed commands (`@doc` annotations carry help text).
- `init.scm` runs at startup with the editor `cx` in scope.
- Sample from [PR #8675](https://github.com/helix-editor/helix/pull/8675):
  ```scheme
  (provide shell)
  ;;@doc
  ;; Specialized shell — also be able to override the existing definition.
  (define (shell cx . args)
    (define expanded
      (map (lambda (x) (if (equal? x "%") (current-path cx) x)) args))
    (helix.run-shell-command cx expanded helix.PromptEvent::Validate))
  ```
- Naming: kebab-case, constructor-style `(position row col)` instead of `Position::new(row, col)`. Higher-level abstractions, no exposed internal structs.

**PR status as of 2026.** [#8675](https://github.com/helix-editor/helix/pull/8675) is still draft, ~3 years old. Author mattwparas: "Certainly functional to use and write plugins with. I've been daily driving on it for a long time now." Marked "Ongoing experiment that does not require reviewing and won't be merged in its current state." So Helix has *no merged plugin system at all* in 2026.

**Active counter-prototype.** [Discussion #13945](https://github.com/helix-editor/helix/discussions/13945) (Jul 2025) by denieldiniz proposes Lua + WASM dual runtime: "Lua is perfect for simple tasks... performance and security of the WASM runtime fits well for more demanding use cases." Not adopted; no roadmap commitment. The dual-runtime idea is community noise, not direction.

**What Helix already loads dynamically (the realistic comparison).** Tree-sitter grammars as cdylibs via `libloading`. So Helix accepts unsafe native code for parsers but refuses it for plugins — an interesting line.

**Distribution / capability / view rendering / target portability:** all N/A. There is no plugin system to evaluate.

---

### Side-by-side summary

| Dimension | Zed | Lapce | Helix |
|---|---|---|---|
| Runtime | wasmtime + component model + epoch interrupts | wasmtime + WASI Preview 1 | none (Steel scheme planned) |
| ABI | WIT, ~20 exports, ~9 imported interfaces | one export `handle_rpc`, JSON-RPC framing | Scheme functions in `helix.scm`/`init.scm` |
| Identity | `extension.toml` `id` (immutable) | `volt.toml` `name` | filename / `(provide ...)` |
| Capabilities | declarative `capabilities = [ProcessExec(...)]` | implicit; HTTP defaults to `allow-all` | n/a |
| UI | data-only (CodeLabel, settings); no pixels | data-only (LSP responses) | n/a |
| Distribution | git submodule registry, version-pinned, in-app gallery | central registry plugins.lapce.dev, `volts publish` | n/a |
| Frontends | single (GPUI) | single (Floem) | single (TUI) |
| Concurrency safety | epoch yield every 100ms | thread per plugin, blocking I/O | n/a |
| Health | active, growing | stale (Lapce dev slowed in 2024-25) | unmerged after 3 years |

---

### Verdict for ark

**Lift this:**

1. **Wasmtime + component model + WIT.** Zed's choice has aged best; the component model gives you cross-language plugins (any source → wasm), strong typing, and forward-compatible ABIs via versioned WIT files (`since_v0.6.0/extension.wit`). Lapce's raw `handle_rpc` export forces every plugin into JSON-RPC ceremony; the component model removes that floor.
2. **Epoch-based interruption, not fuel.** Zed's `epoch_deadline_async_yield_and_update(1)` with a 100ms ticker is the correct primitive for a reactive editor: it doesn't kill plugins, it just preempts them. Pair with `wasm_component_model(true) + async_support(true)` exactly as `crates/extension_host/src/wasm_host.rs` does.
3. **Declarative install-time capabilities, not runtime prompts.** Zed's `capabilities = [ProcessExec(...)]` array stored on the host as `granted_capabilities` and checked at every host call is the right shape: no permission popups (terrible TUI UX), trust boundary at install time, easy to audit a plugin by reading its TOML. Lapce's `allowed_hosts: ["insecure:allow-all"]` is the negative example. Aligns directly with this survey's Cluster 4 verdict (4.3 + 4.1 hybrid).
4. **Manifest-versioned, immutable plugin ID.** Both Zed (`id` "cannot be changed after your extension has been published") and Lapce (`name` in volt.toml) put identity in the manifest, not the filename. ark's KDL plugin descriptors should do the same: stable string ID, separate `version`, separate human `display-name`. Filename-based identity gets confusing the moment a plugin is renamed or vendored.
5. **Data-only UI for v1; defer pixel-pushing.** Both shipping systems forbid plugins from drawing. Zed's Rhai proposal (Discussion #40049) explicitly defers webview to "Phase 3" because the team still hasn't decided how to scope it safely. ark should make plugins emit *intents* (open pane with content X, render label with spans Y, run command Z) and let the ark TUI actually draw. This also automatically solves render-target portability — plugins can't accidentally depend on a frontend.

**Avoid this:**

1. **Don't ship without a registry plan from day one.** Helix's three failed wasm attempts and Lapce's stalled plugin ecosystem both correlate with "we'll figure out distribution later." Zed succeeded because the git-submodule registry shipped *with* the host. ark should pick a shape (git submodules à la Zed; OCI artifacts; signed tarballs) before merging the host.
2. **Don't reuse LSP JSON-RPC as your plugin protocol.** Lapce's PSP is LSP wire format with extra methods. It works, but every plugin is now stuck thinking in LSP's request/response/notification shape, including plugins that have nothing to do with language tooling (themes, debuggers, panels). The component model gives you typed function calls; use them.
3. **Don't promise UI rendering by extensions in v1.** Zed has been asked for it since [Issue #5269](https://github.com/zed-industries/zed/issues/5269) (Jul 2022) and four years later still hasn't shipped it. The Rhai discussion (#40049) is the *third* swing at it. Either commit to a real view DSL (heavy spec lift) or refuse pixel access entirely. Half-measures rot.
4. **Don't expose internal structs across the wasm boundary.** Helix's Steel design explicitly avoided this ("constructors like `(position row col)` rather than `Position::new`"). Zed's WIT does the same — every type that crosses the boundary is a `record` or `resource`, not a Rust struct. The moment plugin authors couple to your internal types, refactoring the host breaks every plugin.
5. **Don't do "WASI Preview 1 + stdio pipes" like Lapce.** It's the path of least resistance and it's a dead end: no typed interfaces, async story is "spawn a thread and block on read", no component composition. WASI Preview 2 + components is the supported path going forward; even Lapce will likely have to migrate.

---

Sources:
- [Zed extension_host Cargo.toml](https://github.com/zed-industries/zed/blob/main/crates/extension_host/Cargo.toml)
- [Zed wasm_host.rs](https://github.com/zed-industries/zed/blob/main/crates/extension_host/src/wasm_host.rs)
- [Zed extension.wit](https://raw.githubusercontent.com/zed-industries/zed/main/crates/extension_api/wit/since_v0.6.0/extension.wit)
- [Zed extension_manifest.rs](https://github.com/zed-industries/zed/blob/main/crates/extension/src/extension_manifest.rs)
- [Zed PR #24986 (epoch interruption)](https://github.com/zed-industries/zed/pull/24986)
- [Zed Issue #5269 (plugin interface)](https://github.com/zed-industries/zed/issues/5269)
- [Zed Discussion #40049 (Rhai proposal)](https://github.com/zed-industries/zed/discussions/40049)
- [Zed extensions registry](https://github.com/zed-industries/extensions)
- [Zed install docs](https://zed.dev/docs/extensions/installing-extensions)
- [Lapce wasi.rs](https://github.com/lapce/lapce/blob/master/lapce-proxy/src/plugin/wasi.rs)
- [Lapce psp.rs](https://github.com/lapce/lapce/blob/master/lapce-proxy/src/plugin/psp.rs)
- [Lapce csharp plugin volt.toml](https://github.com/sharpSteff/lapce-csharp-plugin/blob/master/volt.toml)
- [Lapce plugin development docs](https://docs.lapce.dev/development/plugin-development)
- [lapce-wasi-experimental-http](https://lib.rs/crates/lapce-wasi-experimental-http)
- [Helix Discussion #580 (wasm pre-RFC)](https://github.com/helix-editor/helix/discussions/580)
- [Helix Discussion #3806 (plugin system)](https://github.com/helix-editor/helix/discussions/3806)
- [Helix Discussion #13945 (Lua+WASM prototype, Jul 2025)](https://github.com/helix-editor/helix/discussions/13945)
- [Helix Discussion #14457 (collected future directions)](https://github.com/helix-editor/helix/discussions/14457)
- [Helix PR #8675 (Steel)](https://github.com/helix-editor/helix/pull/8675)
- [Helix Issue #122 (wasm plugins)](https://github.com/helix-editor/helix/issues/122)
- [Helix PR #2949 (early wasm prototype)](https://github.com/helix-editor/helix/pull/2949)

---

## Cluster 3: wasmtime Embedder Patterns

Research target: how a Rust host (the ark IDE process) embeds wasmtime to load
many ark plugins as wasm components, with per-plugin capability gating. All
references resolved against wasmtime's published `docs.wasmtime.dev` API surface
(crate version is the current published `wasmtime` on docs.rs as of 2026-04).

> Notation: code blocks are minimal compilable sketches — `?` operators assume
> `anyhow::Result`, `use wasmtime::*;` and `use wasmtime::component::*;` are in
> scope where relevant.

---

### 3.1 Engine + Store + Instance lifecycle

The three layers, top down:

| Layer    | Lifetime                            | Shared across threads?         | Cost to create                   |
|----------|-------------------------------------|--------------------------------|----------------------------------|
| `Engine` | process-lifetime                    | yes (`Clone`, `Send + Sync`)   | medium (compiles config, mmap pools) |
| `Store<T>` | per "request" / per plugin instance | NO — one thread at a time      | cheap                            |
| `Instance` | lives inside one `Store`          | NO — bound to the store        | the work `instantiate` does      |

The wasmtime docs are explicit on lifecycle intent: `Engine` is "a global
compilation and runtime environment" with "typically one Engine per process."
A `Store<T>` is "a short-lived object" that "should correspond roughly to the
lifetime of a 'main instance'." Critically, **wasmtime has no GC of instances
within a store** — once you instantiate inside a `Store`, the only way to free
that instance's resources is to drop the whole `Store`. That fact dictates the
isolation model.

**Per-plugin isolation model for ark.**

The right shape for a plugin host that wants to load/unload plugins
independently is:

- **One `Engine` per ark process** (cloned/shared by `Arc` semantics — `Engine`
  is itself ref-counted internally and is `Clone + Send + Sync`).
- **One `Store<PluginCtx>` per plugin instance.** Dropping a misbehaving plugin
  = dropping its `Store`. All linear memories, tables, host resources in its
  `ResourceTable`, and accumulated WASI state go with it.
- **One `Instance` per `Store`** for normal plugins. Multi-instance-per-store is
  for orchestration components; ark plugins are leaf code.

Skeleton:

```rust
use std::sync::Arc;
use wasmtime::{Config, Engine, Store};
use wasmtime::component::{Component, Linker, Instance};

pub struct PluginRuntime {
    engine: Engine,                     // shared, cloneable
    linker: Arc<Linker<PluginCtx>>,     // host-fn registry, built once
}

pub struct PluginCtx {
    wasi:  wasmtime_wasi::p2::WasiCtx,
    table: wasmtime::component::ResourceTable,
    caps:  PluginCapabilities,          // ark-specific gate set
    id:    PluginId,
}

pub struct LoadedPlugin {
    store:    Store<PluginCtx>,
    instance: Instance,
}

impl PluginRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        cfg.async_support(true);            // see 3.7
        cfg.epoch_interruption(true);       // see 3.7
        let engine = Engine::new(&cfg)?;
        let linker = Linker::<PluginCtx>::new(&engine);
        // host fns + WASI added once, see 3.2
        Ok(Self { engine, linker: Arc::new(linker) })
    }

    pub async fn load(
        &self,
        component: &Component,
        ctx: PluginCtx,
    ) -> anyhow::Result<LoadedPlugin> {
        let mut store = Store::new(&self.engine, ctx);
        let instance = self.linker
            .instantiate_async(&mut store, component)
            .await?;
        Ok(LoadedPlugin { store, instance })
    }
}
```

Two performance patterns wasmtime expects you to follow:

1. **`Component::serialize` once on install, `Component::deserialize` on each
   process start.** Cranelift compilation is the slow step; AOT artifacts skip
   it. `unsafe fn deserialize` because the bytes must come from a trusted file
   that ark itself wrote.
2. **`Linker::instantiate_pre(&component) -> InstancePre<T>` once per plugin
   binary, then `pre.instantiate(&mut store)` per instance.** `InstancePre`
   resolves all imports / typechecks against the linker; only allocation +
   start-fn run remain at instantiate time. Worth caching per loaded plugin.

```rust
let pre: wasmtime::component::InstancePre<PluginCtx> =
    self.linker.instantiate_pre(&component)?;
// ... later, cheaply:
let instance = pre.instantiate_async(&mut store).await?;
```

For ark this means: one `InstancePre` cached alongside each `Component` in the
plugin registry; `Store + Instance` torn up and down freely.

Instance count cap: `Store::new` defaults to 10,000 instances/memories/tables
combined per store. For ark's "one instance per store" model we are nowhere
near this; mention only because limit-tuning lives on `Store::limiter`.

---

### 3.2 Host imports — Linker patterns (component model vs core wasm)

Wasmtime ships **two `Linker` types**:

- `wasmtime::Linker<T>` — core wasm modules, name = `(module, name)` strings.
- `wasmtime::component::Linker<T>` — components, names are WIT interface
  identifiers like `wasi:cli/stdout@0.2.0`. Has semver-aware resolution.

ark plugins are components (gives us WIT, resources, async, semver) so the
component `Linker` is the only one that matters for host wiring.

#### Define a host function

The ergonomic path is `LinkerInstance::func_wrap`:

```rust
pub fn func_wrap<F, P, R>(&mut self, name: &str, f: F) -> Result<()>
where F: Fn(StoreContextMut<'_, T>, P) -> Result<R> + Send + Sync + 'static,
      P: ComponentNamedList + Lift,
      R: ComponentNamedList + Lower;
```

Wired in two namespacing styles:

```rust
let mut linker: Linker<PluginCtx> = Linker::new(&engine);

// Root namespace (rare for components)
linker.root().func_wrap("ark-now-ms", |_caller, ()| {
    Ok((std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64,))
})?;

// Named instance — matches WIT `interface clock { ... }` import
linker.instance("ark:host/clock@0.1.0")?
    .func_wrap("now-ms", |mut caller, ()| {
        let ctx = caller.data_mut();
        Ok((ctx.clock.now_ms(),))
    })?;
```

Async variant: `func_wrap_async` (requires `Config::async_support(true)`).
Concurrent variant: `func_wrap_concurrent` (component-model-async feature, lets
multiple guest invocations interleave on the host). For a TUI host the plain
`func_wrap_async` returning `BoxFuture` is the right tool.

#### Per-plugin different host fns (capability-gated)

The `Linker` itself is shared. **wasmtime does not let you swap hosts per
instance once the linker is built**, because instantiation type-checks against
the linker's table of names. There are two viable approaches:

**A. One Linker per capability profile** (recommended for ark).

```rust
struct LinkerSet {
    base:        Arc<Linker<PluginCtx>>,  // wasi + ark:host/log
    with_fs:     Arc<Linker<PluginCtx>>,  // + ark:host/fs
    with_pty:    Arc<Linker<PluginCtx>>,  // + ark:host/pty
    with_fs_pty: Arc<Linker<PluginCtx>>,
    // ... 2^N variants, but N is tiny
}
```

Capabilities for ark are coarse (fs, net, pty, ai-stream, surface-write),
and the host wires the correct linker variant for each plugin at load time.
Concrete + cheap.

**B. One Linker, per-call gate inside the host fn body.**

```rust
linker.instance("ark:host/fs@0.1.0")?
    .func_wrap("read", |mut caller, (path,): (String,)| {
        let ctx = caller.data_mut();
        if !ctx.caps.fs_read.allows(&path) {
            return Err(anyhow::anyhow!("capability denied: fs.read {path}"));
        }
        Ok((std::fs::read(&path)?,))
    })?;
```

The function is *defined* for every plugin but *fails* for plugins without the
cap. Wasmtime treats this as an honest error, not a trap, so the guest can
recover. Downside: the import is *visible* — a plugin can `use` fs and only
discover at first call that it's denied. Bad for discovery.

**C. `define_unknown_imports_as_traps`** (component-model-only):

```rust
let mut linker = base_linker.clone();
linker.define_unknown_imports_as_traps(&component)?;
```

Lets a plugin be instantiated even if it imports things the linker doesn't
have — those imports trap if called. Useful for forward-compat (host added a
new interface a plugin doesn't yet declare) but **the wrong tool for cap
gating** because traps unwind the entire instance.

**ark verdict for 3.2:** approach A (linker variants) for hard gates, with B
inside host-fn bodies for fine-grained per-resource checks (e.g., fs path
allowlist). This dovetails with cluster 4's "imports list IS the manifest" —
the linker variant determines which imports type-check; the per-call gate
narrows that to specific resources.

---

### 3.3 wasmtime-wasi `WasiCtxBuilder` — granular per-instance gating

This is where wasmtime's capability story actually shines, because the WASI
context is **constructed per-Store**, not per-Linker. Every plugin gets its
own `WasiCtx` with exactly the doors you opened.

Full API surface (from `wasmtime_wasi::p2::WasiCtxBuilder`, the WASIp2 /
component-model builder):

| Category    | Methods                                                               |
|-------------|-----------------------------------------------------------------------|
| Construction| `new()`, `build() -> WasiCtx`, `build_p1() -> WasiP1Ctx`              |
| Stdin       | `stdin(impl StdinStream)`, `inherit_stdin()`                          |
| Stdout/err  | `stdout(...)`, `stderr(...)`, `inherit_stdout()`, `inherit_stderr()`, `inherit_stdio()` |
| Args        | `arg(s)`, `args(&[s])`, `inherit_args()`                              |
| Env         | `env(k, v)`, `envs(&[(k, v)])`, `inherit_env()`                       |
| FS          | `preopened_dir(host_path, guest_path, DirPerms, FilePerms) -> Result<&mut Self>` |
| Net         | `allow_tcp(bool)` (default ON), `allow_udp(bool)` (default ON), `allow_ip_name_lookup(bool)` (default off), `inherit_network()`, `socket_addr_check(F)` |
| RNG         | `secure_random(impl Rng)`, `insecure_random(...)`, `insecure_random_seed(u128)`, `max_random_size(u64)` |
| Clocks      | `wall_clock(impl HostWallClock)`, `monotonic_clock(impl HostMonotonicClock)` |
| Misc        | `allow_blocking_current_thread(bool)`                                 |

> Defaults at `WasiCtxBuilder::new()`: stdin closed, stdout/stderr muted, no
> envs, no args, no preopens, **TCP and UDP allowed**, ip-name-lookup denied.
> Note that net is on by default — surprising, and important for ark's threat
> model. We must call `allow_tcp(false).allow_udp(false)` on every plugin
> that hasn't been granted net.

Per-plugin construction:

```rust
use wasmtime_wasi::p2::{WasiCtxBuilder, DirPerms, FilePerms};

fn build_wasi(caps: &PluginCapabilities) -> anyhow::Result<wasmtime_wasi::p2::WasiCtx> {
    let mut b = WasiCtxBuilder::new();

    // Stdio: ark wires plugin stdio to the plugin pane, never inherits host.
    b.stdout(caps.pane_stdout.clone())
     .stderr(caps.pane_stderr.clone());

    // Env: only what the manifest declared.
    for (k, v) in &caps.env {
        b.env(k, v);
    }

    // Filesystem: preopen each granted dir individually.
    for grant in &caps.fs_grants {
        let dperm = if grant.write { DirPerms::all() } else { DirPerms::READ };
        let fperm = if grant.write { FilePerms::all() } else { FilePerms::READ };
        b.preopened_dir(&grant.host_path, &grant.guest_path, dperm, fperm)?;
    }

    // Net: closed unless explicitly granted.
    b.allow_tcp(caps.net_tcp.is_allowed())
     .allow_udp(caps.net_udp.is_allowed())
     .allow_ip_name_lookup(caps.net_dns);

    // Optional: per-address allow/deny (called for every socket op).
    if let Some(allow) = caps.net_allowlist.clone() {
        b.socket_addr_check(move |addr, _use| {
            let allow = allow.clone();
            Box::pin(async move { allow.contains(&addr) })
        });
    }

    Ok(b.build())
}
```

Then, inside `PluginCtx`:

```rust
impl wasmtime_wasi::p2::WasiView for PluginCtx {
    fn ctx(&mut self) -> &mut wasmtime_wasi::p2::WasiCtx { &mut self.wasi }
    fn table(&mut self) -> &mut wasmtime::component::ResourceTable { &mut self.table }
}

// Host setup
wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
```

`add_to_linker_async` registers all the `wasi:*` interfaces in one call; the
gates are enforced inside the WASI implementation when guests call
`wasi:filesystem/preopens.get-directories`, `wasi:sockets/tcp.bind`, etc.

**Plugin discovery of what's wired.** The honest answer: the plugin discovers
through the WASI APIs themselves. `wasi:filesystem/preopens.get-directories`
returns the empty list if no preopens. `wasi:sockets/tcp.start-bind` returns
`error-code::access-denied` when `allow_tcp(false)`. For ark-specific imports,
we should add a thin `ark:host/caps.list() -> list<string>` interface so a
plugin can introspect ark's grant set in one call.

---

### 3.4 Component-model resource types — typed handles for Pane / View / Stack

Resources are wasmtime's answer to "host owns a fat object, guest gets a typed
opaque handle". This is the right tool for ark's `Pane`, `View`, `Stack`, etc.

#### WIT side

```wit
// ark:surface/types@0.1.0
package ark:surface@0.1.0;

interface types {
    resource pane {
        title: func() -> string;
        write: func(text: string);
        focus: func();
        // host-only constructor — no `constructor()` line, so plugins receive
        // panes from other host calls, not by `pane::new()`.
    }

    resource view {
        kind: func() -> string;
        attach-pane: func(p: borrow<pane>);
    }
}

interface surface {
    use types.{pane, view};
    open-pane: func(title: string) -> pane;     // returns owned handle
    list-panes: func() -> list<borrow<pane>>;   // returns borrowed handles
}
```

`borrow<T>` vs owned `T`: borrow does not transfer ownership ("you can poke
this for the duration of the call"). Owned `T` means the guest now holds the
handle and is responsible for it (must `drop` it from the guest, or via
`ResourceAny::resource_drop` from the host for exported guest resources).

#### Host bindings via `bindgen!`

```rust
wasmtime::component::bindgen!({
    path: "wit",
    world: "ark:host/plugin",
    async: true,
    with: {
        "ark:surface/types/pane": HostPane,
        "ark:surface/types/view": HostView,
    },
    trappable_imports: true,
});
```

`with:` ascribes a Rust type for each WIT resource — the host's "real"
representation. wasmtime stores them in a `ResourceTable` and hands the guest
a `Resource<HostPane>` (a `u32` + phantom marker).

#### Host trait impl

The `bindgen!` macro generates a `HostPane` trait that ark must implement on
its `PluginCtx`:

```rust
use wasmtime::component::Resource;

pub struct HostPane {
    id: PaneId,
    // pointers back into ark's Surface
}

impl ark::surface::types::HostPane for PluginCtx {
    async fn title(
        &mut self,
        h: Resource<HostPane>,
    ) -> wasmtime::Result<String> {
        let pane = self.table.get(&h)?;
        Ok(self.ark.surface.title(pane.id))
    }

    async fn write(
        &mut self,
        h: Resource<HostPane>,
        text: String,
    ) -> wasmtime::Result<()> {
        let pane = self.table.get(&h)?;
        if !self.caps.can_write_pane(pane.id) {
            return Err(anyhow::anyhow!("denied: pane.write"));
        }
        self.ark.surface.write(pane.id, &text);
        Ok(())
    }

    async fn focus(&mut self, h: Resource<HostPane>) -> wasmtime::Result<()> {
        let pane = self.table.get(&h)?;
        self.ark.surface.focus(pane.id);
        Ok(())
    }

    // Required when guests can drop owned handles.
    async fn drop(&mut self, h: Resource<HostPane>) -> wasmtime::Result<()> {
        let _pane = self.table.delete(h)?;
        // pane stays alive in ark — handle just goes away.
        Ok(())
    }
}

// Top-level interface that hands out resources:
impl ark::surface::surface::Host for PluginCtx {
    async fn open_pane(
        &mut self,
        title: String,
    ) -> wasmtime::Result<Resource<HostPane>> {
        if !self.caps.can_open_pane() {
            return Err(anyhow::anyhow!("denied: surface.open-pane"));
        }
        let id = self.ark.surface.open(title);
        Ok(self.table.push(HostPane { id })?)
    }

    async fn list_panes(&mut self) -> wasmtime::Result<Vec<Resource<HostPane>>> {
        Ok(self.ark.surface.iter()
           .map(|id| self.table.push(HostPane { id }).unwrap())
           .collect())
    }
}
```

Key invariants:

- `ResourceTable::push` returns a fresh `Resource<T>`. Pushing the *same* host
  object twice yields *two* different guest handles.
- `ResourceTable::delete` consumes and returns the host value. The `drop`
  impl above demonstrates the pattern; ark itself can keep the underlying
  `PaneId` alive.
- Guest-exported resources flow back as `ResourceAny` and require explicit
  `resource_drop` from the host. Likely irrelevant for ark — plugins
  consume host resources, not the other way around.

cargo-component / wit-bindgen guest side mirrors this: the guest gets a
`pane` opaque type with method calls, no idea of the underlying integer.

---

### 3.5 Custom-section reading at install time (`wasmparser::Parser`)

Cluster 4.1 already covers the read pattern; here we add the
performance/embedder details that matter for the loader.

`wasmparser` is the right tool — it's the same parsing core wasmtime uses, but
exposed as a streaming, allocation-light, no-side-effects parser.

#### Streaming read for files (memory-mapped or on disk)

```rust
use std::fs::File;
use std::io::{BufReader, Read};
use wasmparser::{Parser, Payload, Chunk};

pub fn read_ark_meta(path: &std::path::Path) -> anyhow::Result<Option<Vec<u8>>> {
    let mut file   = BufReader::new(File::open(path)?);
    let mut parser = Parser::new(0);
    let mut buf    = Vec::with_capacity(64 * 1024);

    loop {
        // Parse what we have; refill on NeedMoreData.
        let (payload, used) = match parser.parse(&buf, false)? {
            Chunk::NeedMoreData(_) => {
                let mut chunk = [0u8; 64 * 1024];
                let n = file.read(&mut chunk)?;
                if n == 0 { return Ok(None); }       // EOF, no section
                buf.extend_from_slice(&chunk[..n]);
                continue;
            }
            Chunk::Parsed { payload, consumed: u } => (payload, u),
        };

        match payload {
            Payload::CustomSection(reader) if reader.name() == "ark_meta" => {
                return Ok(Some(reader.data().to_vec()));
            }
            Payload::End(_) => return Ok(None),
            _ => {}
        }

        buf.drain(..used);
    }
}
```

For wasm already in memory, the `parse_all` iterator simplifies it to:

```rust
pub fn ark_meta_from_bytes(bytes: &[u8]) -> anyhow::Result<Option<&[u8]>> {
    for payload in Parser::new(0).parse_all(bytes) {
        if let Payload::CustomSection(r) = payload? {
            if r.name() == "ark_meta" {
                return Ok(Some(r.data()));
            }
        }
    }
    Ok(None)
}
```

`CustomSectionReader` exposes `name() -> &str`, `data() -> &[u8]`,
`data_offset() -> usize`, `range() -> Range<usize>`. We only need `name` to
filter and `data` to extract the payload (which would be e.g. CBOR or
postcard).

**Performance.** wasmparser is event-driven and allocates ~nothing for sections
it skips — it just advances the cursor. A 5–10 MiB plugin is read in a few
hundred microseconds. Important for ark: this is two orders of magnitude
cheaper than `Component::new`, which has to invoke Cranelift on every code
section. Always introspect first.

#### Component-aware traversal

For component binaries, wasmparser additionally yields `ComponentSection`,
`ComponentImportSection`, `ComponentExportSection`, etc. We can build a
"manifest derivation" pass that walks a freshly-installed component, lists its
imports (`ark:host/fs`, `wasi:sockets/...`, etc.), and presents them to the
user before granting capabilities. Cheaper than asking the human to trust the
plugin's claimed manifest.

---

### 3.6 Two-phase loading — introspect-then-instantiate

Wasmtime explicitly supports the two phases ark wants:

**Phase 1: typecheck and inspect without running anything.**

```rust
let component = Component::from_file(&engine, &path)?;   // compiles bytes
let ty: wasmtime::component::types::Component = component.component_type();

for (name, item) in ty.imports(&engine) {
    println!("import: {name} :: {item:?}");
    // item is types::ComponentItem — Func, Resource, Instance, etc.
}
for (name, item) in ty.exports(&engine) {
    println!("export: {name} :: {item:?}");
}
```

`Component::component_type()` returns a `types::Component` which gives us
`imports(&Engine)` and `exports(&Engine)` iterators of
`(name: &str, types::ComponentItem)`. We can introspect:

- Which WIT interfaces the component imports (host caps it asks for).
- Which it exports (the world it claims to implement — does it actually
  export `ark:plugin/lifecycle.activate()`, etc.?).
- Function signatures, resource types, record fields, all reachable.

Caveat from the docs: "the precise type of imports and exports of a component
change when it is instantiated with respect to resources." The pre-
instantiation view is sufficient for *interface identity* checks ("does it
import `ark:host/fs`?") but not for handle equality across stores.

**Phase 2: gated instantiation.**

```rust
// Decide based on imports + ark_meta whether to grant.
let caps = derive_caps_from(&ty, &ark_meta, &user_grants)?;

// Pick the linker variant matching `caps`.
let linker = self.linker_set.for_caps(&caps);

// Pre-instantiate against the chosen linker (does typecheck).
let pre = linker.instantiate_pre(&component)?;

// Build a fresh PluginCtx + Store and finally run.
let ctx       = PluginCtx::new(caps, /* … */)?;
let mut store = Store::new(&self.engine, ctx);
let instance  = pre.instantiate_async(&mut store).await?;
```

`instantiate_pre` is when imports are checked against the linker. If the
plugin imports `ark:host/fs` and we picked the no-fs linker, it errors here
— before any guest code runs. That error is recoverable; we can prompt the
user, swap linkers, retry.

**Order summary.**

1. `wasmparser::Parser::parse_all` over raw bytes → extract `ark_meta`,
   declared world, declared caps.
2. `Component::new` (or `Component::deserialize` for cached AOT) → produces
   a typed `Component`.
3. `component.component_type().imports(...)` → audit imports vs ark_meta.
4. User grant decision.
5. `Linker.instantiate_pre(&component)` → import resolution check.
6. `Store::new`; `pre.instantiate_async(&mut store)` → actually run.

Steps 1–4 cost nothing per-instance and can be cached. 5 is amortized per
plugin binary. 6 is the per-session cost.

---

### 3.7 Async vs sync wasm in a TUI host

Wasmtime supports both. The relevant axis is whether **host calls and
instantiation are `.await`-able** so the IDE's tokio runtime stays unblocked.

| Mode          | Enable                                                                 | Suspension mechanism                                                | Cost                                     |
|---------------|------------------------------------------------------------------------|---------------------------------------------------------------------|------------------------------------------|
| Sync wasm     | (default)                                                              | none — host fns block the calling thread                            | zero overhead                            |
| Async wasm    | `Config::async_support(true)`                                          | Wasmtime-managed fibers; guest can suspend at any host-fn await pt  | small per-instance stack overhead, ~few % runtime |
| Async + epoch | `Config::epoch_interruption(true)` + `Store::epoch_deadline_async_yield_and_update(N)` | guest yields at backedges/prologues when global epoch counter passes deadline | ~10% slowdown                            |
| Async + fuel  | `Config::consume_fuel(true)` + `Store::fuel_async_yield_interval(N)`   | guest yields after consuming N units of fuel                        | ~2–3× more overhead than epochs (but deterministic) |

**For ark.**

- `async_support(true)` is mandatory — every plugin host fn that touches
  ark's reactive surface (read pane, write to a tokio channel) needs `await`.
- `epoch_interruption(true)` is strongly preferred over fuel. ark doesn't need
  determinism; ark needs "kill any plugin that holds the runtime hostage."
- Wire one wall-clock tick (e.g. `tokio::time::interval(50ms)`) that calls
  `engine.increment_epoch()`. Each plugin store calls
  `store.set_epoch_deadline(N)` and
  `store.epoch_deadline_async_yield_and_update(N)` so it cooperatively yields
  to tokio every N ticks (~100ms is sane for IDE responsiveness).

```rust
// One per process
let engine_clone = engine.clone();
tokio::spawn(async move {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
    loop {
        tick.tick().await;
        engine_clone.increment_epoch();
    }
});

// Per plugin store, at load time:
store.set_epoch_deadline(2);                            // 100 ms before yield
store.epoch_deadline_async_yield_and_update(2);
```

For "panic button" hard-stop, `store.epoch_deadline_trap()` instead of
`_async_yield_and_update` — the guest traps and the instance dies on next
deadline. Combine with a watchdog that flips a per-plugin flag after N
consecutive yields without progress.

The Zed discussion (zed-industries/zed#24515) is worth noting: they hit
real-world async-wasm latency issues running multiple components in a single
runtime. The takeaway is to not over-share — keep one tokio task per plugin
where possible.

---

### 3.8 Hot reload

The plain truth from wasmtime issue #3017 and the runtime architecture: there
is **no first-class hot-reload of an instance with state preserved**. wasmtime
doesn't snapshot linear memory + execution state at arbitrary points, and even
if you serialized memory you can't resume across a code change (offsets,
vtables, RTTI all shift).

What actually works:

- **Drop and rebuild.** Drop the old `Store`, recompile the new `Component`,
  build a new `Store`, instantiate. Free, instantaneous from a wasm POV — but
  the plugin's transient state (caches, open file descriptors, in-progress
  ops) is lost.
- **Plugin-cooperative reload.** Define `ark:plugin/lifecycle` with
  `serialize-state() -> list<u8>` and `restore-state(bytes: list<u8>)`. Old
  instance dumps state to the host; host drops it; new component is loaded;
  new instance receives the bytes. Plugin is responsible for being able to
  read its own old format (or refuse). This is the only reload path that
  actually preserves anything meaningful.
- **External state.** Push as much state as possible to ark itself — the
  plugin holds `Resource<HostXyz>` handles, not raw owned state. After reload,
  the new instance asks the host for fresh handles, and "state" magically
  survives because ark always held it.

For a TUI/IDE plugin (a pane renderer, a command provider), the third option
is pragmatic and overwhelmingly cheaper than the second.

#### Reload skeleton

```rust
pub async fn reload(
    runtime: &PluginRuntime,
    slot: &mut LoadedPlugin,
    new_component: &Component,
    new_caps: PluginCapabilities,
) -> anyhow::Result<()> {
    // 1. Ask the old instance to checkpoint (optional, plugin-cooperative).
    let snapshot = call_serialize_state(slot).await.ok();

    // 2. Drop the old store entirely. Frees memory, drops Resource handles,
    //    fires WASI close ops on preopened dirs.
    let plugin_id = slot.store.data().id;
    // (in real code, take the slot by Option<_> or replace via mem::replace)

    // 3. Build a fresh ctx + store + instance.
    let ctx = PluginCtx::new(plugin_id, new_caps, /* … */)?;
    let mut store = Store::new(&runtime.engine, ctx);
    let pre = runtime.cached_pre(new_component)?;
    let instance = pre.instantiate_async(&mut store).await?;
    *slot = LoadedPlugin { store, instance };

    // 4. Hand the snapshot back if any.
    if let Some(s) = snapshot {
        call_restore_state(slot, &s).await?;
    }
    Ok(())
}
```

The pooling allocator (`PoolingAllocationConfig`) makes step 3 effectively
free for memory: it reuses the slot the old instance occupied, preserving
mmap caches and TLB locality.

---

### Verdict for ark — concrete API choices

**Use:**

- One process-wide `Engine` built from a `Config` with:
  - `wasm_component_model(true)` — components only.
  - `async_support(true)` — every host call returns a future.
  - `epoch_interruption(true)` — cooperative yield + watchdog kill.
  - `allocation_strategy(InstanceAllocationStrategy::Pooling(...))` after
    profiling — gives reload-without-cost.
- `wasmtime::component::Component` (never raw `Module`) — components are
  ark's plugin format.
- `wasmtime::component::Linker<PluginCtx>` per **capability profile**, all
  built up-front and cached. Use approach A from 3.2.
- `wasmtime_wasi::p2::WasiCtx` per Store, built freshly per plugin from the
  manifest's grants. Default-deny: `allow_tcp(false).allow_udp(false)` until
  proven otherwise.
- `Resource<T>` for `Pane`, `View`, `Stack`, `Buffer`, `Command`, `Subagent`
  — anything ark owns and lends to plugins. Use `ResourceTable` and a single
  `bindgen!` invocation per WIT world.
- `wasmparser::Parser::parse_all` for the install-time `ark_meta`
  introspection pass. Never `Component::new` an unsigned/unaudited binary.
- `Component::component_type()` for the "what does this plugin import?" UX
  prompt before granting caps.
- `Linker::instantiate_pre` cached per plugin binary; `pre.instantiate_async`
  per session.
- `Store::set_epoch_deadline` + `epoch_deadline_async_yield_and_update` per
  store; one tokio task increments the epoch every ~50 ms.
- AOT cache: `Component::serialize` on install, `Component::deserialize` on
  process startup, behind a content-hash key.

**Avoid:**

- `wasmtime::Linker` (core wasm) — components only.
- Fuel-based interruption — slower than epochs without ark needing
  determinism.
- `define_unknown_imports_as_traps` for capability gating — wrong granularity.
- Sharing a `Store` across plugins or threads — instance leaks + isolation
  violations.
- Trying to snapshot/resume linear memory across a reload — broken in the
  general case; build a `lifecycle.serialize-state` interface instead.
- Multi-instance-per-store — the wasmtime architectural pattern is one
  instance per store for leaf plugins.

**Host plugin loader skeleton (consolidated):**

```rust
use std::sync::Arc;
use wasmtime::{Config, Engine, Store};
use wasmtime::component::{Component, Linker, Instance, InstancePre, ResourceTable};
use wasmtime_wasi::p2::{WasiCtx, WasiCtxBuilder, WasiView};

pub struct PluginRuntime {
    engine:  Engine,
    linkers: LinkerSet,                   // one per capability profile
    cache:   moka::sync::Cache<PluginHash, Arc<CachedPlugin>>,
}

struct CachedPlugin {
    component:   Component,
    pre_by_caps: dashmap::DashMap<CapsKey, InstancePre<PluginCtx>>,
    meta:        ArkMeta,
}

pub struct PluginCtx {
    wasi:  WasiCtx,
    table: ResourceTable,
    caps:  PluginCapabilities,
    ark:   ArkHandle,                     // back-reference to IDE state
    id:    PluginId,
}

impl WasiView for PluginCtx {
    fn ctx(&mut self)   -> &mut WasiCtx       { &mut self.wasi }
    fn table(&mut self) -> &mut ResourceTable { &mut self.table }
}

impl PluginRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true)
           .async_support(true)
           .epoch_interruption(true);
        let engine = Engine::new(&cfg)?;

        // Spawn the epoch ticker.
        let e = engine.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_millis(50));
            loop { t.tick().await; e.increment_epoch(); }
        });

        let linkers = LinkerSet::build(&engine)?;
        Ok(Self { engine, linkers, cache: Default::default() })
    }

    /// Phase 1 — introspect bytes, decide whether to compile.
    pub fn inspect(&self, wasm: &[u8]) -> anyhow::Result<PluginManifest> {
        let meta = ark_meta_from_bytes(wasm)?
            .ok_or_else(|| anyhow::anyhow!("missing ark_meta section"))?;
        Ok(PluginManifest::parse(&meta)?)
    }

    /// Phase 2 — compile + introspect imports.
    pub fn compile(&self, wasm: &[u8]) -> anyhow::Result<Arc<CachedPlugin>> {
        let component = Component::new(&self.engine, wasm)?;
        let meta      = ArkMeta::parse(ark_meta_from_bytes(wasm)?.unwrap())?;
        let cached = Arc::new(CachedPlugin {
            component, pre_by_caps: Default::default(), meta,
        });
        Ok(cached)
    }

    /// Phase 3 — instantiate against a granted cap set.
    pub async fn instantiate(
        &self,
        cached: &CachedPlugin,
        caps:   PluginCapabilities,
    ) -> anyhow::Result<LoadedPlugin> {
        let key = caps.key();
        let pre = cached.pre_by_caps.entry(key.clone()).or_try_insert_with(|| {
            let linker = self.linkers.for_caps(&caps);
            linker.instantiate_pre(&cached.component)
        })?.clone();

        let wasi = build_wasi(&caps)?;
        let ctx  = PluginCtx {
            wasi, table: ResourceTable::new(),
            caps, ark: ArkHandle::current(),
            id: PluginId::new(),
        };
        let mut store = Store::new(&self.engine, ctx);
        store.set_epoch_deadline(2);                              // ~100ms
        store.epoch_deadline_async_yield_and_update(2);
        let instance = pre.instantiate_async(&mut store).await?;
        Ok(LoadedPlugin { store, instance })
    }
}
```

That is the loader. Plugin lifecycle (`activate`, `deactivate`,
`serialize-state`, etc.) lives one layer above as typed bindings generated by
`bindgen!` against the `ark:plugin` world.

---

#### Sources (Cluster 3)

- [wasmtime::Engine](https://docs.wasmtime.dev/api/wasmtime/struct.Engine.html)
- [wasmtime::Store](https://docs.rs/wasmtime/latest/wasmtime/struct.Store.html)
- [wasmtime::component::Linker](https://docs.wasmtime.dev/api/wasmtime/component/struct.Linker.html)
- [wasmtime::component::LinkerInstance](https://docs.wasmtime.dev/api/wasmtime/component/struct.LinkerInstance.html)
- [wasmtime::component::Component](https://docs.rs/wasmtime/latest/wasmtime/component/struct.Component.html)
- [wasmtime::component::InstancePre](https://docs.rs/wasmtime/latest/wasmtime/component/struct.InstancePre.html)
- [wasmtime::component::bindgen! examples — imported resources](https://docs.wasmtime.dev/api/wasmtime/component/bindgen_examples/_4_imported_resources/index.html)
- [wasmtime::component::bindgen! examples — exported resources](https://docs.wasmtime.dev/api/wasmtime/component/bindgen_examples/_6_exported_resources/index.html)
- [wasmtime::component::bindgen! examples — async](https://docs.wasmtime.dev/api/wasmtime/component/bindgen_examples/_7_async/index.html)
- [wasmtime_wasi::WasiCtxBuilder](https://docs.wasmtime.dev/api/wasmtime_wasi/struct.WasiCtxBuilder.html)
- [Capabilities-Based Security with WASI (Marco Kuoni)](https://marcokuoni.ch/blog/15_capabilities_based_security/)
- [wasmtime — Interrupting Execution (epochs/fuel)](https://docs.wasmtime.dev/examples-interrupting-wasm.html)
- [wasmtime — Fast Instantiation](https://docs.wasmtime.dev/examples-fast-instantiation.html)
- [Component Model book — Rust resources](https://component-model.bytecodealliance.org/language-support/using-wit-resources/rust.html)
- [Building Native Plugin Systems with WebAssembly Components (Sy Brand)](https://tartanllama.xyz/posts/wasm-plugins/)
- [wasmparser — Parser](https://docs.rs/wasmparser/latest/wasmparser/struct.Parser.html)
- [wasmparser — Payload (CustomSection)](https://docs.rs/wasmparser/latest/wasmparser/enum.Payload.html)
- [wasmtime issue #3017 — Save WASM state and resume](https://github.com/bytecodealliance/wasmtime/issues/3017)
- [wasmtime PR #3699 — Epoch-based interruption](https://github.com/bytecodealliance/wasmtime/pull/3699)
- [Zed discussion #24515 — async wasm component perf](https://github.com/zed-industries/zed/discussions/24515)
- [WASI and the Component Model: Current Status (eunomia, 2025)](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)


---

## Cluster 6: Extension Target Classification

How extension/plugin systems classify plugins by required render target or device
feature, and refuse to install/load when the host can't satisfy them. Survey
informs ark's decision: how should a `claude-code` extension or a hypothetical
GPU-only viz extension declare what host shapes it can run on (zellij-backed
terminal IDE today, GUI ark tomorrow), and how should the host reject the
mismatch.

---

### 1. VS Code `extensionKind` + `capabilities`

VS Code has the most directly analogous problem: one extension manifest, two
different host shapes (UI host = local Electron renderer; workspace host =
remote machine over SSH / WSL / Codespaces / web). It solves it with an
**ordered preference array**, plus a separate `capabilities` block for axes
that aren't about *where* the extension runs but about *what hostile workspace
state* it tolerates.

**Manifest fields** (all in `package.json`):

```jsonc
{
  "engines": {
    "vscode": "^1.85.0"               // host-version compat (different axis)
  },
  "main":    "./out/extension.js",     // node entry — desktop host only
  "browser": "./out/web-extension.js", // web-worker entry — web host only
  "extensionKind": ["ui", "workspace"],
  "capabilities": {
    "virtualWorkspaces": {
      "supported": "limited",
      "description": "Cross-file references not available on virtual file systems."
    },
    "untrustedWorkspaces": {
      "supported": "limited",
      "description": "Linter rules from the workspace are ignored in Restricted Mode.",
      "restrictedConfigurations": ["mylinter.rulesPath"]
    }
  }
}
```

**`extensionKind`** — preference array, evaluated in order:
- `["ui"]` — must run in the UI extension host (local). Cannot read remote
  workspace files, cannot spawn workspace-side processes. Examples: themes,
  keymaps, snippets.
- `["workspace"]` — must run in the workspace extension host (wherever the
  workspace lives). Required for anything that reads workspace files or runs
  `child_process.spawn`. Default if unspecified.
- `["ui", "workspace"]` — prefers UI but works in either; VS Code picks UI on
  local workspaces (saves a remote install) and workspace on remote workspaces.
- `["workspace", "ui"]` — inverse preference.

**`browser` field** — declares this extension has a separate web build (runs
in a web worker, no Node APIs, only `vscode.workspace.fs` for file access).
If absent, the extension is desktop-only and the web marketplace UI labels it
"This extension is not available in vscode.dev". The presence of `browser`
makes it a "web extension"; absence + `extensionKind: workspace` makes it a
"desktop-only workspace extension."

**`capabilities.virtualWorkspaces`** — does this extension work when the
workspace is a virtual file system (GitHub repo browsed without clone, S3
bucket, etc.)? Three values:
- `true` — works fine.
- `{ supported: false, description: "..." }` — disabled with a tooltip.
- `{ supported: "limited", description: "..." }` — runs but degraded; the
  Extensions view shows the description as a warning.

**`capabilities.untrustedWorkspaces`** — same three-state pattern for
Restricted Mode (workspace not yet marked trusted). `restrictedConfigurations`
lists settings that the host should *force-ignore* from workspace settings even
when the extension runs in limited mode — i.e., per-setting graceful
degradation rather than per-extension.

**`engines.vscode`** is a separate axis (host *version* compat, not *kind*).
The VS Code marketplace refuses to install an extension whose
`engines.vscode` semver range excludes the running editor; if an older
compatible version exists it auto-substitutes.

**Enforcement / UX**:
- Marketplace filters incompatible extensions out of search results when the
  user is in a Codespace / web context.
- For installed-but-incompatible extensions: the Extensions view shows them
  greyed out with a "This extension cannot run in [context] because [desc]"
  tooltip. The `Developer: Show Running Extensions` command surfaces the same
  decision. There is also a setting `remote.extensionKind` users can override
  per-extension to force a kind.
- `engines.vscode` mismatches: the install button is replaced with "Cannot
  install: requires VS Code ≥ X.Y.Z."

**Key design choice**: VS Code separates *where the extension can run*
(`extensionKind`) from *which workspace shapes it tolerates*
(`capabilities.virtualWorkspaces`, `capabilities.untrustedWorkspaces`). A
preference *array* (not a single value) lets a manifest say "I'd rather be UI
but workspace is fine" — the host picks the first viable entry.

Sources: [Supporting Remote Development](https://code.visualstudio.com/api/advanced-topics/remote-extensions),
[Web Extensions](https://code.visualstudio.com/api/extension-guides/web-extensions),
[Virtual Workspaces guide](https://code.visualstudio.com/api/extension-guides/virtual-workspaces),
[Workspace Trust guide](https://code.visualstudio.com/api/extension-guides/workspace-trust).

---

### 2. iOS `UIRequiredDeviceCapabilities`

Single-axis, install-time-enforced, fine-grained hardware feature gate baked
into the binary's `Info.plist`. The App Store reads it, refuses to ship the
binary to a device missing any required capability.

**Manifest** (`Info.plist`):

```xml
<!-- Array form: presence = required -->
<key>UIRequiredDeviceCapabilities</key>
<array>
    <string>arm64</string>
    <string>metal</string>
    <string>bluetooth-le</string>
</array>

<!-- Dictionary form: explicit true/false -->
<key>UIRequiredDeviceCapabilities</key>
<dict>
    <key>metal</key>          <true/>   <!-- required -->
    <key>telephony</key>      <false/>  <!-- prohibited (won't install on phones) -->
</dict>
```

**Vocabulary** (the canonical key strings — all lowercase, dash-separated):
`armv7`, `arm64`, `accelerometer`, `gyroscope`, `magnetometer`,
`bluetooth-le`, `gps`, `wifi`, `telephony`, `microphone`, `camera-flash`,
`front-facing-camera`, `still-camera`, `video-camera`, `auto-focus-camera`,
`opengles-1`, `opengles-2`, `opengles-3`, `metal`, `healthkit`, `nfc`,
`peer-peer`, `sms`, `location-services`, `arkit`, `armv7-perf-mon`,
`apple-pay`, `water-resistant`.

**Enforcement**: App Store install-time. Once the binary is on the device it
won't be re-checked — capabilities are an install gate, not a runtime gate.
There's no per-feature "graceful degradation" mode; each entry is hard-required
or hard-prohibited. For graceful degradation, apps simply *don't* declare the
capability and instead use runtime checks (`UIDevice.current`, framework
availability, `if #available`).

**UX on rejection**: at the point of install (or update), the App Store shows
"This app is not compatible with this iPhone." No store listing in search
results from an incompatible device. Apple's review team will reject submissions
that declare unused capabilities (false positives starve devices of apps for no
reason — see the recurring Flutter issue where `armv7` was wrongly inherited).

**Capability upgrades**: if the developer adds a new capability in v2 that
v1's installed users don't have, the device simply stops receiving updates;
v1 keeps running silently. No proactive "your phone won't get future updates"
prompt.

Sources: [UIRequiredDeviceCapabilities reference](https://developer.apple.com/documentation/bundleresources/information-property-list/uirequireddevicecapabilities),
[Required Device Capabilities support page](https://developer.apple.com/support/required-device-capabilities/),
[QA1397 Understanding the UIRequiredDeviceCapabilities key](https://developer.apple.com/library/archive/qa/qa1397/_index.html).

---

### 3. Android `<uses-feature>`

Same shape as iOS but with an explicit per-feature `required` boolean — the
"graceful degradation" axis Apple omits. Distinguishes hardware
(`android.hardware.*`) from software (`android.software.*`) namespaces, and
Google Play (not the OS) enforces filtering.

**Manifest** (`AndroidManifest.xml`):

```xml
<!-- Hard-required: app vanishes from devices without GPS -->
<uses-feature android:name="android.hardware.location.gps"
              android:required="true" />

<!-- Optional: app installs everywhere, code must hasSystemFeature() before use -->
<uses-feature android:name="android.hardware.camera"
              android:required="false" />

<!-- Software feature with a version: app needs Vulkan >= 1.0.3 -->
<uses-feature android:name="android.hardware.vulkan.version"
              android:version="0x400003"
              android:required="true" />
```

**Vocabulary** (selected — there are ~80 standard names):
- Hardware: `android.hardware.camera`, `.camera.front`, `.camera.autofocus`,
  `.camera.flash`, `.bluetooth`, `.bluetooth_le`, `.wifi`, `.wifi.direct`,
  `.nfc`, `.usb.host`, `.location`, `.location.gps`, `.location.network`,
  `.sensor.accelerometer`, `.sensor.compass`, `.sensor.gyroscope`,
  `.touchscreen`, `.faketouch`, `.microphone`, `.audio.output`,
  `.audio.pro`, `.type.automotive`, `.type.watch`, `.type.pc`,
  `.vulkan.version`, `.vulkan.level`, `.opengles.aep`.
- Software: `android.software.leanback`, `.live_tv`, `.app_widgets`,
  `.home_screen`, `.sip`, `.sip.voip`, `.device_admin`, `.managed_users`,
  `.print`, `.midi`, `.input_methods`.

**`android:required` behavior**:
- `true` (default) — Play Store filters the app out of the device's listing
  entirely. The app does not appear in search results from an incompatible
  device.
- `false` — Play Store shows it everywhere; the app's code is responsible
  for `PackageManager.hasSystemFeature(...)` before exercising the capability,
  and falling back when absent. This is the canonical "graceful degradation"
  pattern.

**Implicit feature inference**: Permissions like `CAMERA`, `RECORD_AUDIO`,
`BLUETOOTH`, `ACCESS_FINE_LOCATION` *imply* a required `<uses-feature>` even
without explicit declaration. To opt out you must add the feature with
`required="false"` to suppress the inferred filter — common gotcha for apps
that want to support cameraless Chromebooks.

**Critical caveat**: `<uses-feature>` is *not* enforced by the OS at install
time. Sideloaded APKs install fine on devices missing required features; only
the Play Store filters. Runtime `hasSystemFeature()` checks are mandatory if
correctness matters. (Compare iOS where the OS itself enforces install
gating.)

**Capability upgrades**: if v2 adds `android:required="true"` for a new
feature, v1 users on incompatible devices simply stop seeing updates in Play
Store. Existing install keeps working. There's no eviction.

Sources: [`<uses-feature>` reference](https://developer.android.com/guide/topics/manifest/uses-feature-element),
[Device compatibility overview](https://developer.android.com/guide/practices/compatibility),
[Increase your app's availability across device types](https://android-developers.googleblog.com/2023/12/increase-your-apps-availability-across-device-types.html).

---

### 4. WebExtensions `browser_specific_settings` + `minimum_chrome_version`

No unified cross-browser version field. Each browser owns its own namespace
and ignores the others'. Genuinely cross-browser extensions ship a *single*
manifest with all three keys.

**Manifest** (`manifest.json`):

```jsonc
{
  "manifest_version": 3,

  // Chromium / Edge / Opera read this; Firefox & Safari ignore it.
  "minimum_chrome_version": "126",

  // Firefox + Safari read this; Chromium ignores it.
  "browser_specific_settings": {
    "gecko": {
      "id": "myext@example.com",
      "strict_min_version": "115.0",
      "strict_max_version": "*"
    },
    "gecko_android": {
      "strict_min_version": "120.0"
    },
    "safari": {
      "strict_min_version": "16.4"
    }
  }
}
```

**Enforcement** (per browser):
- **Chrome Web Store** — replaces the Install button with a **"Not compatible"**
  message when the user's Chrome version is below `minimum_chrome_version`. New
  installs blocked at the store. *Existing installs* on a downgraded Chrome
  silently stop receiving updates — no in-browser dialog, no eviction. Docs
  explicitly warn "this happens silently, exercise caution."
- **Firefox AMO** — refuses install if `strict_min_version` exceeds running
  Firefox; AMO listings hide the install button on incompatible browsers.
- **Safari** — Mac App Store handles distribution and version filtering.

**API surface differences** are *not* declared in the manifest; extensions are
expected to feature-detect at runtime (e.g., `if ('storage' in browser) ...`).
Manifest V3 vs V2 is its own complication: Chrome dropped V2 in mid-2024,
Firefox supports both, Safari supports V3 only. The `manifest_version` field
itself is the cross-browser compat axis here.

**Capability upgrades**: bump `minimum_chrome_version` in v2 → v1 users on old
Chrome get pinned to v1; Web Store stops pushing v2 to them. Firefox: same via
`strict_min_version`. No user notification.

Sources: [Chrome `minimum_chrome_version`](https://developer.chrome.com/docs/extensions/reference/manifest/minimum-chrome-version),
[MDN `browser_specific_settings`](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json/browser_specific_settings),
[Firefox MV3 migration guide](https://extensionworkshop.com/documentation/develop/manifest-v3-migration-guide/).

---

### 5. Tauri plugin `platforms` + conditional crate split

Tauri v2 plugins target desktop (linux/macOS/windows) + mobile (iOS/android)
from one crate, but the *implementation* is conditionally compiled and
*capability JSON* gates which OS gets which permission.

**Crate-level (Cargo.toml)** — the consumer app declares the plugin only on
applicable targets:

```toml
[target.'cfg(any(target_os = "android", target_os = "ios"))'.dependencies]
tauri-plugin-biometric = "2"

[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]
tauri-plugin-cli = "2"
```

**Plugin source layout** — desktop and mobile diverge by Rust `cfg`:

```
src/
├── lib.rs       // re-exports + cfg(mobile) / cfg(desktop) branches
├── desktop.rs   // pure-Rust impl for windows/macos/linux
└── mobile.rs    // sends commands over JNI/Swift bridge
```

**Capability JSON** — runtime permission grants per-platform:

```jsonc
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "desktop-capability",
  "windows": ["main"],
  "platforms": ["linux", "macOS", "windows"],
  "permissions": ["global-shortcut:allow-register"]
}
```

The `platforms` array gates whether this *capability* (= a bundle of permission
grants) is loaded at all on this OS. Tauri's documented behavior: a capability
with non-matching `platforms` is *silently inert* on the wrong OS — it doesn't
fail to load, it just doesn't grant anything. The plugin itself, if added as a
non-conditional dependency, will compile but its commands will return errors at
invocation time when the OS-specific impl is absent.

**No marketplace** — Tauri has no central plugin store, so there's no
install-time UI for "this plugin is for mobile only." Discovery and gating
happen at the Cargo dependency layer.

Sources: [Tauri Mobile Plugin Development](https://v2.tauri.app/develop/plugins/develop-mobile/),
[Tauri Capabilities](https://v2.tauri.app/security/capabilities/),
[Conditional capabilities discussion #10400](https://github.com/tauri-apps/tauri/discussions/10400).

---

### 6. Rust `[target.'cfg(...)'.dependencies]`

The closest existing precedent for ark, since ark plugins (whether dylib or
WASM-compiled-from-Rust) live in the Cargo ecosystem. Cargo supports
arbitrary `cfg(...)` predicates as table headers in Cargo.toml; the predicate
grammar matches `#[cfg(...)]` source attributes.

```toml
[target.'cfg(unix)'.dependencies]
nix = "0.27"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"

[target.'cfg(any(target_os = "ios", target_os = "android"))'.dependencies]
ndk = "0.8"

[target.'cfg(all(unix, target_pointer_width = "64"))'.dependencies]
io-uring = "0.6"
```

Cargo supports `not`, `any`, `all`, and the standard predicates: `target_arch`,
`target_os`, `target_family`, `target_env`, `target_endian`,
`target_pointer_width`, `target_vendor`, plus features and arbitrary
`--cfg` flags.

**Adapting this to plugin manifests**: the predicate grammar is rich, machine-
parseable, well-trodden territory for Rustaceans, and reusable beyond OS
("renderer = terminal", "multiplexer = zellij") via custom cfg names. The
*downside* is that Cargo `cfg` is evaluated *at compile time* against fixed
host attributes; ark needs evaluation *at plugin-load time* against a dynamic
host description. The grammar transfers; the evaluator is bespoke.

**Resolver v2 nuance**: features enabled on platform-specific deps for
non-active targets are now ignored, preventing feature-unification leaks. The
analogue for ark: don't let a "needs GPU" plugin's transitive features
accidentally flip a "terminal-friendly" plugin into requiring GPU.

Source: [Cargo Manifest – Platform-specific dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies),
[Conditional compilation reference](https://doc.rust-lang.org/reference/conditional-compilation.html).

---

### 7. Helix per-platform commands

Helix has no plugin system, but its built-in commands occasionally diverge
per-OS (clipboard backends, shell open commands). The pattern in
`helix-term/src/commands.rs` is plain `#[cfg(target_os = "...")]` on
function bodies and dispatch arms — a single binary that picks the right
implementation at compile time. There's no manifest, no runtime negotiation,
because Helix ships one binary per target.

The lesson for ark: when the host *itself* is the same binary on every
target, per-OS divergence happens at compile time. Plugins don't have that
luxury — they're loaded into a host whose capabilities aren't known until
runtime. **Helix's model doesn't transfer.** It's a counterexample illustrating
what we *can't* do.

Source: [helix-term/src/commands.rs](https://github.com/helix-editor/helix/blob/master/helix-term/src/commands.rs).

---

### Comparison table

| System              | Axes declared                                | Granularity      | Enforcement point              | Graceful-degrade flag | Upgrade-incompat behavior |
|---------------------|----------------------------------------------|------------------|-------------------------------|----------------------|---------------------------|
| VS Code             | host-kind (ui/workspace) + caps + version    | preference array + 3-state per cap | Marketplace + extension host | `"limited"` per cap  | Pinned to last compatible |
| iOS                 | hardware features (~25 keys)                 | per-feature      | App Store + OS install         | None (binary)        | Update silently withheld  |
| Android             | hw + sw features (~80 keys, namespaced)      | per-feature      | Play Store only (not OS)       | `required="false"`   | Update silently withheld  |
| WebExtensions       | per-browser min-version                      | per-browser semver | Each store independently     | None (feature-detect at runtime) | Pinned, silent      |
| Tauri               | OS list per capability + cfg-gated dep       | per-capability + per-dep | Cargo + runtime         | Silent inert         | N/A (no store)            |
| Cargo               | arbitrary `cfg()` predicate per dep table    | rich grammar     | Compile time                   | Optional dep + feature flag | N/A                  |
| Helix               | none (single binary per target)              | n/a              | Compile time                   | n/a                  | n/a                       |

---

### Cross-cutting design lessons

**1. Granularity divides into two camps.**

- *Coarse "host-kind" enum* (VS Code's `extensionKind`): ≤4 values, ordered
  preference, picked by the host. Good when the axes are few and stable.
- *Fine "feature bag"* (iOS / Android): per-capability key strings, app
  declares the set, host checks each. Good when the matrix is wide and
  evolving (camera autofocus vs camera flash vs front camera).

The two are not mutually exclusive — VS Code uses both: `extensionKind`
(coarse) plus `capabilities.virtualWorkspaces` and
`capabilities.untrustedWorkspaces` (per-feature, three-state).

**2. Three-state ("yes / no / limited") beats binary.**

VS Code (`true | false | "limited"`) and Android (`required=true |
required=false`, where false = "I'll degrade") both encode the "I work, just
worse" mode explicitly. iOS doesn't, and the workaround is undeclared
capabilities + runtime checks — which sacrifices the install-time gate
entirely. The three-state pattern is strictly more expressive at trivial
schema cost.

**3. Where you enforce shapes the UX.**

- *Store/marketplace enforcement* (Chrome, Apple, Google, VS Code Marketplace):
  user sees "Not compatible" before clicking install. Best UX, requires a
  central distribution point.
- *Host-side enforcement at load* (Tauri, all sideloaded cases): user
  installed it, then sees an error or silent no-op. Worse UX, but the only
  option without a marketplace.
- *No enforcement* (sideloaded Android APKs, Cargo cfg deps): the developer
  is responsible for runtime checks. Worst UX, lowest friction for the
  ecosystem.

**4. Upgrade-incompat is universally handled by silent pinning.**

Every surveyed system pins users to the last-compatible version when an
update raises requirements. Nobody evicts. Nobody proactively warns. The
implication for ark: if v2 of a plugin adds a hard `gpu` requirement, terminal
hosts should keep running v1, with a passive "update available but not
compatible" hint at most.

**5. Implicit inference is a footgun.**

Android's permission-implies-feature inference is a recurring source of bugs
(apps mysteriously hidden from Chromebooks because requesting `CAMERA`
permission auto-required a back camera). Be explicit in the manifest; never
infer requirements from other declared facts.

---

### Verdict for ark

ark has at least three orthogonal axes that a plugin might constrain:

1. **Renderer** — does the plugin paint via the terminal (cells/ANSI) or via
   GPU primitives (vertex buffers, shaders)?
2. **Multiplexer** — does the plugin require a zellij session it can drive
   (e.g. `claude-code` opens a pane, attaches to PTY) or is it
   multiplexer-agnostic (pure data extension, agent runner with no UI)?
3. **Host shell** — does it need a real OS terminal underneath (TTY, real fd
   inheritance) vs. an in-process pty emulator?

The user's framing collapses these to renderer × multiplexer for v1 — and
that's the right cut. GPU is the only axis that hard-excludes zellij. Almost
everything else can be made to work in both shells. So follow VS Code's
playbook: a coarse, ordered preference enum (the analogue of `extensionKind`)
plus a per-feature `capabilities` block (the analogue of
`virtualWorkspaces`/`untrustedWorkspaces`).

#### Proposed manifest fields

```kdl
extension "claude-code" version="0.2.0" {
    // Coarse host-shape preference. Ordered. Host picks first viable.
    targets "terminal" "gui"

    // Fine-grained capability requirements. Each entry: name + required.
    requires {
        multiplexer "zellij" required=true   // hard-requires PTY-pane host
        gpu version=">=2"     required=false // optional accelerated render path
    }

    // Engine version compat (separate axis, like engines.vscode).
    ark ">=0.2.0,<0.4.0"
}
```

#### Proposed `Target` enum

Trinary, matching the user's framing exactly:

```rust
/// Render targets a plugin can run in. Stored as an ordered preference list
/// in the manifest (`targets "terminal" "gui"`), evaluated head-first by the
/// host loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// Terminal-only: paints cells, requires a TTY-shaped surface.
    /// Runs on zellij-backed hosts and on GUI ark's embedded terminal pane.
    Terminal,

    /// GUI-only: requires GPU surface + native event loop.
    /// Refuses to load on zellij-backed hosts.
    Gui,

    /// Headless: no rendering surface needed (data extension, background
    /// agent, lint provider). Runs on every host shape.
    Headless,
}
```

Three values, not two, because **headless is a real third class** — a
`claude-code-subagent` extension that just owns a tokio task and emits events
to the orchestrator should declare `targets "headless"` and the host should
load it on any shape including future "ark daemon" deployments without a UI
at all.

#### Capability matrix

Borrow the Android trinary `required=true|false` per cap. Schema:

```rust
pub struct Capability {
    pub name: CapName,           // enum, e.g. Multiplexer, Gpu, Tty, Network
    pub version: Option<VersionReq>,  // semver constraint, optional
    pub required: bool,          // true = host must satisfy or refuse load;
                                 // false = plugin will degrade gracefully
}
```

Initial `CapName` set:
- `multiplexer` (value: `"zellij" | "tmux" | "ark-native"`)
- `gpu` (version: shader-model bucket, e.g. `>=2`)
- `tty` (real fd vs emulated pty)
- `network` (host can outbound)
- `filesystem` (workspace-side fs access)

#### Mismatch error message

Modelled on VS Code's tooltip + Chrome's "Not compatible" copy. Shown in the
ark extension picker UI at install time, and in `ark ext list` for
already-installed-but-disabled plugins:

```
ark: cannot load extension `cosmic-cube-viz` (v0.4.1)

  Reason:    requires render target `gui`, current host is `terminal` (zellij)
  Required:  targets [gui]
  Provided:  targets [terminal, headless]

  This extension is GPU-only. To use it, run ark in GUI mode:
      ark --gui

  To suppress this notice for this extension:
      ark ext disable cosmic-cube-viz
```

For the optional/limited case (`required=false` cap missing), surface a
warning rather than refuse:

```
ark: loaded extension `claude-code` (v0.2.0) with reduced capabilities

  Missing optional: gpu (>=2)
  Effect: code-block syntax highlighting will use terminal palette only.
```

#### Graceful degradation in v1

**Yes — but only for the `requires` block, not for the top-level `targets`
enum.**

- `targets` is a hard gate. A plugin declaring `targets "gui"` running on a
  terminal host: refuse load, no exceptions. The plugin's render path doesn't
  exist for terminal cells; there's nothing to degrade *to*. This matches
  VS Code's `extensionKind` (a `["ui"]` extension simply will not run in a
  workspace host — no "limited UI mode").
- Per-capability `required=false` is the degradation knob. Plugin declares
  what it can do without each optional cap; host loads anyway and the plugin
  branches on `host.has_capability("gpu")` at runtime. This is the
  Android `required="false"` + `hasSystemFeature()` pattern.

The reason to ship degradation in v1 (not defer): the user's own example —
`claude-code` working on both zellij and GUI ark — is *exactly* a graceful
degradation case at the `gpu` cap level. Defer the knob and you can't even
express that example correctly.

#### Upgrade-incompat handling

Follow the universal pattern: silent pinning. If `claude-code` v0.3 adds
`requires { gpu required=true }`, terminal hosts keep running v0.2 and the
extension picker shows a dim "v0.3 available (incompatible: requires gpu)"
note next to it. No eviction, no proactive prompt — matches VS Code,
Chrome, Apple, Google.

#### Worked example: `claude-code` extension

```kdl
extension "claude-code" version="0.2.0" {
    targets "terminal" "gui"  // works on both, no preference
    requires {
        multiplexer required=false  // can use a host pane if available,
                                    // else renders inline in own surface
        network     required=true   // hard-requires outbound to claude.ai
        tty         required=false  // graceful: real TTY for raw mode if
                                    // available, falls back to line-buffered
    }
    ark ">=0.2.0"
}
```

Loads on zellij ark (terminal target, multiplexer present, network present,
real tty). Loads on GUI ark (gui target, multiplexer absent → degraded to
inline surface, network present, emulated pty). Refuses to load on a
hypothetical airgapped ark daemon (network unavailable, required=true).
That's the full v1 contract.

Sources:
- [Supporting Remote Development and GitHub Codespaces](https://code.visualstudio.com/api/advanced-topics/remote-extensions)
- [VS Code Web Extensions](https://code.visualstudio.com/api/extension-guides/web-extensions)
- [VS Code Virtual Workspaces guide](https://code.visualstudio.com/api/extension-guides/virtual-workspaces)
- [VS Code Workspace Trust guide](https://code.visualstudio.com/api/extension-guides/workspace-trust)
- [UIRequiredDeviceCapabilities reference](https://developer.apple.com/documentation/bundleresources/information-property-list/uirequireddevicecapabilities)
- [Apple Required Device Capabilities support page](https://developer.apple.com/support/required-device-capabilities/)
- [Android `<uses-feature>` reference](https://developer.android.com/guide/topics/manifest/uses-feature-element)
- [Android Device compatibility overview](https://developer.android.com/guide/practices/compatibility)
- [Chrome `minimum_chrome_version`](https://developer.chrome.com/docs/extensions/reference/manifest/minimum-chrome-version)
- [MDN `browser_specific_settings`](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/manifest.json/browser_specific_settings)
- [Tauri Mobile Plugin Development](https://v2.tauri.app/develop/plugins/develop-mobile/)
- [Tauri Capabilities](https://v2.tauri.app/security/capabilities/)
- [Cargo platform-specific dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies)
- [Rust conditional compilation reference](https://doc.rust-lang.org/reference/conditional-compilation.html)
- [helix-term/src/commands.rs](https://github.com/helix-editor/helix/blob/master/helix-term/src/commands.rs)
