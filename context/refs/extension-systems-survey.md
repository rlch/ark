# Extension Systems Survey (2026-04-20)

## Cluster C: Krew, Cargo, Helix, TPM, Neovim

Survey of five extension/plugin systems from CLI-tool ecosystems and editor LSP loading,
focused on what plugins ship as, how (or whether) they declare metadata, and how
install/lifecycle works. Compiled to inform ark's extension model decision.

---

### 1. Krew (kubectl plugin manager)

**Ships as**: tar.gz / zip archive at a URL, listed in a central plugin index repo
(`krew-index`), with a yaml manifest per plugin checked into that index.

**Manifest** (`Plugin` kind, `krew.googlecontainertools.github.com/v1alpha2`):
- `metadata.name` (must match filename)
- `spec.version` (semver, leading `v` required)
- `spec.shortDescription`, `spec.description`, optional `spec.caveats`
- `spec.platforms[]` — each entry has:
  - `selector.matchExpressions` over `os` (darwin/linux/windows) and `arch`
  - `uri` (archive URL) + `sha256` (mandatory integrity check)
  - `files[]` (which paths inside the archive to extract; default = all)
  - `bin` (path to executable inside the archive)

**Install flow**:
1. User runs `kubectl krew update` (refreshes the index repo via git pull)
2. `kubectl krew search <term>`
3. `kubectl krew install <name>` — krew picks the matching `platforms[]` entry,
   downloads, verifies sha256, extracts listed files, symlinks `kubectl-<name>`
   into `$KREW_ROOT/bin`.
4. Plugin is invoked transparently as `kubectl <name>` — kubectl finds
   `kubectl-<name>` on PATH.

**Lifecycle hooks**: none beyond install/upgrade/uninstall. No post-install scripts;
`spec.caveats` is *just text shown to the user* (e.g. "add this to your shell rc").
This is deliberate — Krew refuses to run arbitrary install scripts as a security stance.

**Verbs vs lifecycle**: zero plugin-supplied verbs at the manager level. Every plugin
exposes its own CLI surface as a kubectl subcommand. Krew itself owns the lifecycle
verbs (update/search/install/upgrade/uninstall/list/info).

**Key observation**: manifest exists primarily for *distribution integrity*
(sha256, platform matching, version pinning) and *discovery* (the central index),
not for runtime capability declaration. The plugin binary is a black box once installed.

---

### 2. Cargo subcommands (`cargo-X` binaries on PATH)

**Ships as**: a bare executable named `cargo-<command>`, anywhere in `$PATH` (cargo
prefers `$CARGO_HOME/bin`). No archive, no install metadata, no registry entry
required (though most are published as crates and installed via `cargo install`).

**Manifest**: **none whatsoever.** Discovery is purely filename-based — cargo
scans PATH directories for any executable matching `cargo-*` and exposes them as
`cargo X`. `cargo --list` enumerates them.

**Install flow**: completely user-driven. Typical paths:
- `cargo install cargo-edit` (uses crates.io as a *binary distribution channel*)
- `brew install cargo-watch`
- manually drop a binary into `~/.cargo/bin`

There is no "cargo plugin install" verb — cargo defers entirely to the host OS's
binary management.

**Version compat**: **none.** A cargo subcommand is forbidden from linking to
cargo-the-library (unstable). The contract is the *CLI* of `cargo metadata` and a
few well-defined env vars (`CARGO_MANIFEST_DIR`, etc.). This stable CLI surface is
what makes the no-manifest model viable: the contract is in the protocol, not in
declared capabilities.

**Capability declaration**: zero. A subcommand could do anything. Discovery is
"if it's named right, it's invokable" — capability negotiation happens at
runtime when the subcommand calls `cargo metadata` (or doesn't).

**Argv convention**: cargo invokes `cargo-foo foo <args>` (note the doubled command
name) so that subcommands can be invoked either as `cargo foo` or as standalone
`cargo-foo`.

**Tradeoff**: dead simple, ergonomic for both users and authors, but cargo cannot
know in advance what a subcommand does, what cargo version it expects, or whether
it's even still maintained. The community absorbs this by (a) the stable
`cargo metadata` contract and (b) crates.io serving as a de facto registry with
README/version metadata that lives one level up from cargo itself.

---

### 3. Helix language server loading

**Ships as**: just an LSP binary (rust-analyzer, gopls, pyright, …). Helix does
*not* package or ship LSPs — the user installs them out-of-band (cargo install,
brew, npm, pip, system pkg manager).

**Configuration**: `languages.toml` in Helix's config dir defines two tables:
- `[language-server.<name>]` — `command`, `args`, `config` (init options sent over
  LSP), `timeout`, `environment`, `required-root-patterns`
- `[[language]]` entries — list which `language-servers` to attach, with
  per-feature filters (`only-features` / `except-features`)

**Manifest**: **none for the LSP itself.** All metadata lives in the *user's*
`languages.toml`. The LSP binary advertises capabilities at runtime via the LSP
`initialize` handshake — that's the manifest, expressed as a protocol message
rather than a static file.

**Install flow**: completely manual on the LSP side; Helix ships defaults for
common LSPs in its built-in `languages.toml`, so users only have to install the
binary. No plugin manager exists.

**Capability negotiation**: handled by LSP itself (`ServerCapabilities` in the
`initialize` response). Helix gates features on what the server declares it
supports. This is the cleanest example in the survey of "no manifest because the
runtime protocol is the manifest."

**Hooks**: none. `required-root-patterns` is the closest thing — purely a
"should I even start this server" gate, not a hook.

**Verbs vs lifecycle**: lifecycle is implicit (start when a matching file opens,
stop when the workspace closes). Users don't run `helix lsp install x`.

---

### 4. Tmux Plugin Manager (TPM)

**Ships as**: a git repository (GitHub `user/repo`, BitBucket, or arbitrary git
URL) containing one or more `*.tmux` executable scripts and any supporting files
(usually a `scripts/` dir).

**Manifest**: **none.** Plugin authors provide no metadata file. The convention is
that any `*.tmux` file at the repo root is an entry-point script that TPM will
execute when sourcing the plugin.

**User declaration** (in `~/.tmux.conf`):
```
set -g @plugin 'tmux-plugins/tmux-sensible'
set -g @plugin 'github_user/repo_name'
run '~/.tmux/plugins/tpm/tpm'
```
The user-side line is the closest thing to a manifest, but it lives in the
user's config, not the plugin.

**Install flow**:
- prefix + I — TPM reads `@plugin` lines, `git clone`s each into
  `~/.tmux/plugins/<name>`, then sources every `*.tmux` file in each.
- prefix + U — `git pull` for each plugin.
- prefix + alt + u — removes plugin directories no longer referenced in conf.

**Hooks**: the `*.tmux` script itself is the only hook — it runs once at plugin
load time (not separated into install/configure/load phases). If a plugin needs
to compile native code, it does so by writing to `*.tmux` and shelling out (no
declared "build" step like lazy.nvim has). This is implicit-hook design: every
plugin runs the same single phase.

**Verbs vs lifecycle**: TPM exposes only install/update/clean (as keybinds).
Plugins themselves expose tmux key bindings, status-bar segments, etc. by
mutating the tmux session from inside their `.tmux` script — there's no
"tpm run <verb>" surface.

**Tradeoff**: zero metadata is paid for in two ways:
1. No version constraints (tmux-version, plugin-version compat).
2. No declared dependencies between plugins. Plugin authors solve this by
   either bundling deps or documenting in README.

---

### 5. Neovim package managers (lazy.nvim, packer)

**Ships as**: a git repository, again. Plugin author provides Lua/VimL files in
conventional directories (`lua/`, `plugin/`, `ftplugin/`, `autoload/`,
`colors/`, etc.) that Neovim's runtime path picks up.

**Manifest** (in the plugin itself): **none.** Authors do not write a manifest.
Neovim's `runtimepath` convention *is* the manifest — file location implies
load semantics.

**Spec** (in the *user's* config, not the plugin): lazy.nvim takes a Lua table
spec per plugin. Key fields:
- Source: short URL like `"folke/tokyonight.nvim"`, or `dir`/`url`
- `init` — runs at startup unconditionally (set globals)
- `opts` — config table merged with parent specs, passed to `setup()`
- `config` — runs when the plugin actually loads
- `build` — runs on install/update (e.g. `make`, `:TSUpdate`)
- `dependencies` — other plugins to load first
- `lazy` — boolean to defer loading
- `event`, `cmd`, `ft`, `keys` — declarative triggers for lazy-loading

**Install flow**: lazy.nvim reads the user's spec table, `git clone`s missing
plugins to `~/.local/share/nvim/lazy/<name>`, runs each plugin's `build` hook,
adds the path to `runtimepath`, and sets up the lazy-load triggers. Subsequent
loads are governed by the triggers.

**Critical inversion vs Krew/cargo**: the *user* owns the manifest, not the
*plugin author*. lazy.nvim has no opinion about what's inside the plugin — it
just clones, runs build, and adds to runtimepath. The plugin author can ship
zero metadata and still be loadable.

This works because Neovim's runtime *itself* defines a strong convention
(`runtimepath` directories, `plugin/*.lua` autoloading, `:autocmd` events for
lazy triggers). The package manager is a thin wrapper over git + a config DSL.

**Hooks**: build/init/config are real lifecycle hooks but they live *in the
user's spec*, not in a plugin manifest. Authors can document recommended specs
in their README; users copy-paste.

**Verbs vs lifecycle**: lazy.nvim has lifecycle (`:Lazy install/update/sync/
clean/check`); plugins themselves expose Neovim user commands (`:Telescope`,
`:NvimTreeToggle`) registered at load time. No verb declaration in any spec.

---

## Cross-cutting comparison

| System | Plugin manifest? | Author ships | User declares | Build hook | Install hooks | Capability decl |
|--------|------------------|--------------|---------------|------------|---------------|-----------------|
| Krew | yaml (in central index) | tarball + sha | nothing (just `install x`) | no | no (caveats text only) | none (CLI black box) |
| Cargo subcmd | none | bare binary | nothing | no | no | none (CLI contract) |
| Helix LSP | none | binary | `[language-server.X]` config | no | no | runtime via LSP `initialize` |
| TPM | none | git repo + `*.tmux` | `set -g @plugin` | no (sourced in `.tmux`) | no (`.tmux` runs every load) | none |
| lazy.nvim | none | git repo (conventional dirs) | Lua spec table | yes (in user spec) | init/config (in user spec) | none (runtimepath convention) |

**Manifest location pattern**: of the five, only Krew has a true plugin-author
manifest, and that manifest is hosted in a *central index repo* (not in the
plugin's own source). The other four push declaration either to (a) a runtime
protocol (Helix → LSP `initialize`), (b) filesystem convention (cargo, TPM,
nvim's runtimepath), or (c) the user's own config (lazy.nvim spec).

---

## Critical question: is no-manifest viable for ark?

**Strongest no-manifest case: cargo subcommands.** The reason it works:
- The cargo CLI is the contract (stable `cargo metadata` interface).
- Distribution is offloaded entirely to crates.io + OS package managers.
- Discovery is filesystem (PATH).
- Cargo never needs to know what a subcommand does because it never calls into
  it programmatically — it just `exec`s.

**Strongest pro-manifest case: Krew.** The reason it needs one:
- Cross-platform binary distribution requires platform selectors + sha256.
- A central index needs metadata to render search results.
- Without sha256, a kubectl plugin manager would be a malware vector.

**Where ark sits**: ark wants extensions that:
- Run *inside* the ark process (wasm), not as separate binaries → unlike cargo,
  there is no OS-level binary contract to lean on.
- Need to declare *which surfaces* they extend (panes, status bar, key tables) →
  unlike cargo or TPM, ark's host has structured extension points, so the
  plugin must somehow tell ark which ones it implements.
- Are cross-platform via wasm → no per-platform binary tarball selection
  needed (unlike Krew).

The Helix model — "no manifest, the runtime protocol declares capabilities" —
is the closest fit *if* ark's wasm extension API is rich enough to be
self-describing at load time (extension exports a `register()` function that
declares its surfaces). Then a manifest is redundant with what the wasm module
already exports.

The lazy.nvim model — "user owns the spec, plugin ships zero metadata" — is
viable for v1 if ark expects users to write KDL config declaring each
extension. But it pushes work onto users that authors could do once.

The cargo model — "filename convention is the manifest" — fails because wasm
modules don't have a natural CLI verb namespace the way `cargo-X` does.

---

## Lessons for ark (5 bullets, honest)

1. **Manifests pay rent only when distribution or platform-matching demands it.**
   Krew's yaml exists because `(os, arch)` × sha256 × tarball selection cannot
   be expressed at runtime. ark ships wasm — single artifact, no platform
   matrix — so the strongest reason to need a manifest is already absent.

2. **Helix's "protocol is the manifest" is the most ark-shaped pattern.** A
   wasm extension that exports a `register()` -> `ExtensionSpec` function gives
   ark capability info at load time without a separate file, and the spec
   stays in sync with the code by construction (no drift between manifest.yaml
   and what the binary actually does — a chronic Krew/lazy.nvim pain point).

3. **TPM and cargo both demonstrate that "no manifest" is socially viable when
   the convention is strong enough.** TPM's `*.tmux` filename and cargo's
   `cargo-X` filename do all the work. ark could mimic this with a wasm export
   convention (`__ark_register`), making the wasm module self-declaring.

4. **lazy.nvim's user-side spec table is the strongest argument AGAINST a
   plugin-side manifest** — it puts lifecycle (build/init/config/lazy triggers)
   in the *user's* config where it composes with the user's other choices.
   ark's KDL config is already heading this way; a plugin-side manifest would
   duplicate or conflict with whatever the user writes in their `ark.kdl`.

5. **The honest pattern that argues against manifests**: every system surveyed
   except Krew avoids them, and Krew only has one because it ships native
   binaries to a central index. For ark v1 — wasm-only, no central registry,
   user-declared extensions in KDL — a manifest is **complexity without
   payoff**. Defer it until ark has (a) a registry that needs to render search
   results, or (b) third-party extensions with semver compat constraints
   ark needs to enforce before load. Until then, mimic Helix: let the wasm
   module's exported `register()` function be the manifest, and let the user's
   KDL config be the spec.

---

## Cluster B: Wasm Component Model, Spin, wasmtime/wasmCloud

Survey of WebAssembly-native extension systems with focus on metadata
declaration, distribution, and the critical question of whether anyone embeds
declarative metadata in a wasm custom section without dragging a parser into
the guest.

Sources fetched 2026-04-20:
- component-model.bytecodealliance.org (book + WIT page + Rust language support)
- spinframework.dev/v3/manifest-reference (formerly developer.fermyon.com)
- docs.rs/wasmparser, docs.rs/wasmtime-wasi, wasmtime Component API rustdoc
- raw wit-bindgen source (`crates/rust/src/lib.rs`) for exact section naming
- wasmCloud OCI/wash docs (partial — several pages 404'd, supplemented with
  README + prior knowledge of the nkeys/JWT scheme; mark as approximate)

---

### 1. WebAssembly Component Model (canonical)

**Metadata declaration.** WIT (Wasm Interface Type) text files define `world`s
(roots) and `interface`s (groups of typed functions/resources). A WIT package
groups `.wit` files in a directory. WIT is *contract only* — no behaviour,
only types. Authors write `wit/world.wit`; bindings generators consume it.

**Embedding.** The encoded WIT for a component's world is serialized into a
wasm **custom section**. The section name (per wit-bindgen Rust backend,
verified against `crates/rust/src/lib.rs`):

```
component-type:wit-bindgen:<version>:<pkg>:<world>:<suffix>[opts]
```

Emitted via `#[unsafe(link_section = "...")]` on a `pub static` byte array,
gated by `#[cfg(target_arch = "wasm32")]`. **The bytes are baked at codegen
time as a static blob — no parser ships in the guest.** The host (or
`wasm-tools component new`) reads this section to lift the core module into
a proper component.

**Consumption.** `wasm-tools component wit foo.wasm` extracts the WIT back
out; hosts using the wasmtime `Component` API call `component.component_type()`,
`.imports()`, `.exports()` to enumerate the interface **before instantiation**
(rustdoc confirms `get_export_index` lets you skip string lookups at runtime).
Introspection happens at *load* time; the data itself is baked at *build*.

**Build pipeline.** Modern flow (`cargo-component` is now de-emphasized in
favour of stock toolchain):
1. `cargo build --target=wasm32-wasip2 --release` — produces a core module
   that already contains the `component-type:` custom section thanks to the
   `wit_bindgen::generate!()` macro.
2. (If targeting wasip1) `wasm-tools component new` wraps the core module
   into a real component, consuming the custom section to synthesize the
   component type. With wasip2 this is implicit.

Artifacts: a single `target/wasm32-wasip2/release/foo.wasm`. ~16KB release,
~3.3MB debug per the official Rust tutorial.

**Capability model.** The component model has **no ambient authority** —
imports are the only way out. A component's `world` lists `import`s; the
host must satisfy each. WASI capabilities (filesystem, sockets, clocks,
env, stdio) are themselves WIT interfaces (`wasi:filesystem`,
`wasi:sockets`, `wasi:clocks`). Refusing to wire them = sandboxing.
wasmtime-wasi exposes a `WasiCtxBuilder` with `preopened_dir()`,
`allow_tcp(bool)`, `allow_udp(bool)`, `inherit_stdio()`, `env(k,v)` —
granular per-instance gating.

**Distribution unit.** A single `.wasm` component file. The component model
has a `package` concept at the WIT level (interface namespacing), but the
*shipped* artifact is one binary. OCI is the de facto registry format —
`wkg` (warg/wasm package tools) pushes/pulls components as OCI artifacts
with `application/wasm` media type.

---

### 2. Spin (Fermyon / spinframework.dev)

**Metadata declaration.** **Sidecar `spin.toml`**, not embedded. v2 schema:

```toml
spin_manifest_version = 2

[application]
name = "my-app"
version = "0.1.0"

[[trigger.http]]
route = "/api/..."
component = "api"

[component.api]
source = "target/wasm32-wasip1/release/api.wasm"
allowed_outbound_hosts = ["https://api.example.com"]
files = ["static/**"]
key_value_stores = ["default"]
sqlite_databases = ["main"]
environment = { LOG_LEVEL = "info" }

[component.api.build]
command = "cargo build --target wasm32-wasip1 --release"
```

**Stages.** Host (the `spin` runtime) parses `spin.toml` at *install/load*
time. Build vs runtime split is clean: TOML is purely runtime config; the
wasm itself carries no Spin-specific metadata.

**Build CLI.** `spin build` runs each component's `[component.X.build]`
command (typically `cargo build`). `spin up` loads the manifest + wasm
artifacts. `spin registry push` ships the whole app (manifest + wasms) as
an **OCI artifact** to a registry.

**Capability model.** Declarative gating in TOML — `allowed_outbound_hosts`
is the network capability list, `files` whitelists filesystem preopens,
`key_value_stores`/`sqlite_databases` enumerate which named state backends
this component may touch. The runtime translates these into wasmtime-wasi
`WasiCtxBuilder` calls + Spin-specific runtime configs.

**Distribution.** OCI artifact bundling manifest + components, or git/disk
for development. The unit is the *application* (multi-component), not the
single `.wasm`.

---

### 3. wasmtime (embedder API)

Not an extension system per se, but the substrate. Two relevant facts for
ark:

- `wasmtime::component::Component::component_type()` returns a typed view
  with `.imports()` / `.exports()` iterators — a host can fully introspect
  what a component needs and provides **before instantiating**, by parsing
  only the type section. No execution, no allocations beyond type info.
- `wasmparser::Parser` (used internally) exposes `CustomSectionReader` —
  you can stream a wasm file, filter for a specific custom section name
  (e.g. `ark-meta`), extract bytes, and stop. Cheap enough to run at
  install time on every plugin.

WASI gating is per-store via `WasiCtxBuilder` — covered above.

---

### 4. wasmCloud

**Metadata declaration.** Two layers:
- **Embedded JWT claims** in a custom section (historically named `jwt`)
  of the wasm. Generated by `wash claims sign` (the new `wash` may have
  folded this into `wash build`). The JWT contains: subject (component
  public key), issuer (signer key), capability claim list (e.g.
  `wasmcloud:httpserver`, `wasmcloud:keyvalue`), human-readable name,
  version, revision, and expiration. Verified at runtime by the wasmCloud
  host.
- **Wadm manifests** (YAML) for *deployment* topology — separate from
  per-component identity.

  Note: the wasmCloud doc pages on this 404'd at survey time; details from
  README + prior knowledge of the nkeys/JWT scheme they have used since
  0.x. Treat as approximate — verify if ark adopts this pattern.

**Build CLI.** `wash build` compiles + signs. `wash push` ships to OCI
registry. `wash app deploy` applies a wadm manifest to a lattice.

**Capability model.** Claims-based: the JWT lists which capability provider
contracts (e.g. `wasmcloud:httpserver`) the component is *permitted* to
bind to. Host enforces at link time. Plus the standard component-model
imports graph underneath.

**Distribution.** OCI artifact. Single signed `.wasm` per component.

---

### 5. Critical question — embedding metadata without a guest parser

**Yes, this is solved and standard practice.** Two production patterns:

**Pattern A — wit-bindgen / component model (proven, ubiquitous).**
The `wit_bindgen::generate!()` macro emits, at *codegen* time:

```rust
#[cfg(target_arch = "wasm32")]
#[unsafe(link_section = "component-type:wit-bindgen:0.X.0:my-pkg:my-world:encoded")]
#[doc(hidden)]
pub static __WIT_BINDGEN_COMPONENT_TYPE: [u8; N] = *b"\x00\x61\x73\x6d...";
```

The bytes are the binary-encoded component type, computed by the proc
macro on the host at build. The guest never parses anything — it carries
an opaque blob. The host (wasmtime / wasm-tools / any embedder) reads the
section by name with `wasmparser::Parser` and decodes it.

**Pattern B — wasmCloud JWT (proven, narrower).**
`wash claims sign` writes a JWT (compact base64 string) into a custom
section. JSON claims are decoded host-side by `nkeys` + standard JWT libs.
Same principle: data baked at build, opaque to guest, parsed by host.

**Implication for ark.** The `#[link_section = "..."]` + static byte array
trick is the established mechanism. It works on `wasm32-wasip1` today with
zero guest dependencies beyond the `link_section` attribute, which is in
stable rustc. ark could:

1. Define a section name (e.g. `ark-meta`).
2. Provide a tiny proc-macro or `build.rs` helper that takes a Rust struct
   describing the extension (name, version, capabilities, views, lifecycle
   hooks), serializes to bytes (postcard / CBOR / JSON), and emits the
   `link_section` static.
3. Host uses `wasmparser` to scan custom sections by name at install time,
   deserialize, and store in a registry. Zero parser bloat in the guest.

This pattern handles *all four* of ark's needs (name/version/capabilities/
views/lifecycle) without adopting the rest of the component model
machinery (WIT files, wit-bindgen world generation, type-driven binding
ceremony).

---

### Lessons for ark (Cluster B, 5 bullets)

1. **Adopt the custom-section + static-bytes pattern, skip full WIT.** ark
   only needs declarative metadata — name/version/caps/views/lifecycle.
   wit-bindgen's whole apparatus is for *typed function-signature binding*,
   which ark doesn't need (host calls extensions through a small fixed
   ABI, not arbitrary world-defined functions). Lift the
   `#[link_section]` + static-bytes idea, leave WIT/world-generation
   alone. **Wasm component model = worth borrowing the trick, not worth
   adopting wholesale.**

2. **Use `wasmparser::CustomSectionReader` at install time.** It's the
   same crate wasmtime already pulls in transitively. One pass over the
   wasm file, filter by section name, deserialize a small payload
   (postcard is ~1KB of code on host, zero on guest). Reject plugins
   missing the section. This becomes ark's "validate manifest before
   load" step — and pairs cleanly with the Helix-style runtime
   `register()` discussed in Cluster C: section = static identity,
   `register()` = dynamic surface registration.

3. **Keep manifest sidecar for human-edited config, embed for static
   identity.** Spin proves the sidecar TOML model works for *deployment*
   config (which hosts to call, which KV store to bind). wasmCloud + the
   component model prove embedded sections work for *identity/
   capabilities* that should travel with the binary. ark wants the
   embedded approach for the immutable extension contract; if per-install
   config is needed later, add a sidecar KDL — don't conflate the two.

4. **Capabilities should be declarative-in-section, enforced at host
   wiring time.** Mirror WASI's "no ambient authority" stance: the
   section declares what the extension *wants* (`fs:read /workspace`,
   `tui:render`, `kv:default`); the host decides at instantiate time
   which imports to satisfy. Don't try to embed runtime-checked
   permission strings inside the wasm logic. Spin's
   `allowed_outbound_hosts` is the model — except ark moves that list
   from sidecar TOML *into* the embedded section so plugins can't lie
   about what they need vs what they ship.

5. **Ship a single `.wasm`, push to OCI later.** Every system surveyed
   converged on `.wasm` as the unit and OCI as the registry. For v0.2 a
   single file on disk + the embedded section is sufficient. When ark
   needs distribution, `oci-distribution` crate + `application/wasm`
   media type plus an annotation surfacing the `ark-meta` section bytes
   (so registry browsers can show metadata without pulling the whole
   wasm) gets parity with Spin/wasmCloud without adopting their
   runtimes.

---

## Cluster D: Zellij Plugin Loading (deep dive)

Sources fetched 2026-04-20:
- https://zellij.dev/documentation/plugins.html (and plugin-loading, plugin-aliases, plugin-pipes sub-pages)
- https://github.com/zellij-org/zellij/tree/main/default-plugins
- https://github.com/zellij-org/zellij/blob/main/zellij-tile/src/lib.rs
- https://github.com/zellij-org/zellij/blob/main/zellij-utils/src/plugin_api/ (protobuf)
- https://github.com/zellij-org/rust-plugin-example
- https://docs.rs/zellij-tile

This matters because **ark plugins ARE zellij plugins** — same `wasm32-wasip1` target, same
`zellij-tile` crate, loaded into the same zellij runtime that ark sits on top of. Whatever
ark adds is layered on; the constraints below are non-negotiable from zellij's side.

---

### 1. Plugin Loading & Discovery

Zellij does **not** scan a plugin directory at startup. There is no
`~/.config/zellij/plugins/` auto-discovery in the "find every .wasm and load it" sense.
Plugins are loaded **on demand**, identified by a URL-style location string. Four schemes
are recognized:

| Scheme       | Example                              | Resolved by                          |
| ------------ | ------------------------------------ | ------------------------------------ |
| `file:`      | `file:/abs/path/to/plugin.wasm`      | filesystem read                      |
| `http(s):`   | `https://example.com/foo.wasm`       | HTTP fetch (cached on disk)          |
| `zellij:`    | `zellij:tab-bar`                     | embedded in zellij binary at compile |
| bare alias   | `tab-bar`, `filepicker`              | resolved via `plugins {}` config     |

Triggers that cause a plugin to load:

1. **Layout files** (`.kdl`) reference plugins inside a `pane` block.
2. **CLI** — `zellij action launch-or-focus-plugin <url>`.
3. **Keybindings** — bound to a `LaunchOrFocusPlugin` action.
4. **`load_plugins {}` config block** — startup-loaded background plugins.
5. **Other plugins** — `MessageToPlugin` with `new_plugin_args` can spawn one.

Cached web plugins live under `$ZELLIJ_CACHE_DIR` (XDG cache). Compiled `.wasm` modules
are cached as serialized wasmer artifacts there too — that's how zellij avoids
re-compiling on every load. There is *no* manifest sitting next to the `.wasm` anywhere.

---

### 2. Plugin Identification — How Zellij Knows What a `.wasm` IS

**Zellij treats every `.wasm` as opaque.** Identification is purely:

- **Path / URL** — the location string is the identity.
- **Runtime exports** — zellij asserts the wasm exports the four required functions
  (`load`, `update`, `render`, `pipe`) plus `plugin_version`. If any are missing, load
  fails.
- **Runtime permission requests** — capabilities are negotiated *after* load via
  `RequestPluginPermissions` calls from inside the running plugin.

There is **no sidecar metadata file**. No `plugin.toml`, no `manifest.json`, no header
section read by zellij. The `Cargo.toml` of `default-plugins/strider/` is a normal Rust
crate manifest; zellij never reads it. Once compiled, all that matters is the `.wasm`
file's ABI surface.

The `plugins {}` config block is the **only** place a human-readable name (alias) gets
attached to a plugin URL — and that's a user-side affordance, not embedded metadata.

```kdl
plugins {
    tab-bar    location="zellij:tab-bar"
    filepicker location="zellij:strider" { cwd "/" }
    my-thing   location="file:/home/x/plugin.wasm" { theme "dark" }
}
```

The `{ … }` block under each alias is `user_configuration` — an arbitrary
`BTreeMap<String,String>` handed to the plugin's `load()` at startup. That's the closest
thing zellij has to a manifest, and it lives in user config, not next to the binary.

The default layout (`default.kdl`) references bundled plugins by their alias only:

```kdl
pane size=1 borderless=true { plugin location="tab-bar" }
pane size=1 borderless=true { plugin location="status-bar" }
```

Zellij's built-in `plugins {}` defaults map `tab-bar` → `zellij:tab-bar` etc. Users can
override; the layout doesn't care which scheme resolves the alias.

---

### 3. Plugin ABI — Required Exports & Provided Imports

**Trait the plugin implements (`zellij-tile`):**

```rust
pub trait ZellijPlugin: Default {
    fn load(&mut self, configuration: BTreeMap<String, String>) {}
    fn update(&mut self, event: Event) -> bool { false }   // -> render?
    fn pipe(&mut self, pipe_message: PipeMessage) -> bool { false }
    fn render(&mut self, rows: usize, cols: usize) {}
}
```

**Wasm exports zellij requires** (generated by `register_plugin!(MyState)`):

| Export           | Purpose                                                      |
| ---------------- | ------------------------------------------------------------ |
| `load`           | first call after instantiation; receives configuration map   |
| `update`         | dispatch a subscribed `Event`                                |
| `render`         | draw to ANSI buffer at given rows×cols                       |
| `pipe`           | receive a `PipeMessage` (CLI / keybinding / other plugin)    |
| `plugin_version` | returns the `zellij-tile` crate version it was built against |

The macro also wires panic handling and protobuf (de)serialization to/from stdin/stdout
inside the wasm module. The host writes protobuf-encoded messages to the plugin's WASI
stdin and reads results from stdout — that's the whole cross-boundary protocol.

**Host imports** (provided by zellij to every plugin) — a flat namespace of host
functions, all gated through the permission system. Examples: `host_subscribe`,
`host_unsubscribe`, `host_run_command`, `host_open_file`, `host_open_terminal`,
`host_post_message_to_plugin` (pipe send), `host_request_permission`,
`host_set_selectable`, `host_resize_pane`, `host_focus_pane`, etc. Effectively zellij
exposes ~211 commands (the `CommandName` enum in `plugin_command.proto`). Every host call
is a protobuf payload across stdin/stdout.

---

### 4. Permissions — Runtime, Interactive, Persisted per-URL

Permissions in zellij are **declared at runtime, not in a manifest**. Inside `load()`,
the plugin calls:

```rust
request_permission(&[
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::WriteToStdin,
]);
```

What happens:

1. Zellij pops a **modal in the plugin's pane** asking the user to grant the requested
   set ("`<plugin-url>` is requesting: ReadApplicationState, ChangeApplicationState.
   Allow?").
2. User answers yes / no. Decision is persisted to `$ZELLIJ_CACHE_DIR/permissions.kdl`,
   keyed by **plugin URL**.
3. On future loads of the same URL, the cached decision is honored — no prompt.
4. Calls to host functions that require a permission the plugin didn't request (or was
   denied) silently fail / return errors.

The full `PermissionType` enum (17 variants, from `plugin_permission.proto`):

```
ReadApplicationState              ChangeApplicationState
OpenFiles                         RunCommands
OpenTerminalsOrPlugins            WriteToStdin
WebAccess                         ReadCliPipes
MessageAndLaunchOtherPlugins      Reconfigure
FullHdAccess                      StartWebServer
InterceptInput                    ReadPaneContents
RunActionsAsUser                  WriteToClipboard
ReadSessionEnvironmentVariables
```

Note: permissions are coarse and capability-flavoured (verbs, not scoped objects).
`OpenFiles` is "can open any file", not "can open files matching pattern X".

---

### 5. Plugin Pipes — Inter-plugin Messaging

Worth knowing because ark's "intent" design overlaps with this.

A **pipe** is a unidirectional named channel carrying a `PipeMessage`:

```rust
pub struct PipeMessage {
    pub source: PipeSource,        // Cli(id) | Plugin(id) | Keybind
    pub name: String,              // user-chosen or random UUID
    pub payload: Option<String>,   // arbitrary text (often JSON)
    pub args: BTreeMap<String, String>,
    pub is_private: bool,          // targeted vs broadcast
}
```

Three sources:
- **CLI** — `zellij pipe --plugin <url> --name foo --args k=v <<<"payload"`. Has
  backpressure: STDIN buffer only releases after target plugin renders or explicitly
  unblocks.
- **Other plugin** — `pipe_message_to_plugin(MessageToPlugin { plugin_url, name,
  payload, … })`.
- **Keybind** — bound to `MessageToPlugin` action.

Two delivery modes:
- **Targeted** (`plugin_url` set) — only that plugin receives.
- **Broadcast** (no destination) — every loaded plugin gets it; convention-based
  filtering by `name`.

Permission required: `MessageAndLaunchOtherPlugins` (sender), `ReadCliPipes` (CLI source).

This is essentially zellij's pub/sub — and it's the obvious substrate for ark's
intent-routing layer if we want intents to flow plugin↔plugin via the same wire.

---

### 6. Versioning — `plugin_version` Export

The `register_plugin!` macro auto-exports `plugin_version()` returning the version of
`zellij-tile` the plugin was compiled against. On load, zellij compares this to its own
`zellij-tile` version. Mismatch → log a `PLUGIN_MISMATCH` warning ("rebuild self-compiled
plugins"). It's **a soft check** — zellij does not refuse to load on mismatch, but the
protobuf wire format can drift between versions and break things subtly.

Concretely: there is no semver-style negotiation, no minimum-host-version request. The
plugin says "I was built against tile X.Y.Z" and the host says "I'm Y.Z, hope it works".
Most ABI breakage shows up as protobuf decode failures at the first host call.

---

### Critical-Question Answer

> Do zellij plugins have a sidecar metadata file? Or is every `.wasm` opaque, identified
> by path + runtime capability requests?

**Pure path + runtime, no sidecar.** The default `tab-bar`/`status-bar`/`strider`
plugins are referenced in `default.kdl` as `plugin location="tab-bar"` (alias) which the
embedded `plugins {}` block resolves to `zellij:tab-bar` (compiled into the binary).
Their `Cargo.toml` files are normal Rust crate manifests that zellij never reads. The
only "metadata" zellij ever sees about a plugin is:

1. The URL string the user / layout / config used to ask for it.
2. The wasm module's exports (must include load/update/render/pipe/plugin_version).
3. The `RequestPluginPermissions` calls the running plugin makes during `load()`.
4. Optional `BTreeMap<String,String>` user configuration from the `plugins {}` alias
   block.

No header bytes, no embedded JSON, no signing, no capability whitelist read at load
time. Identity = URL. Capabilities = whatever the running code negotiates with the user.

---

### Constraints for ark (5 bullets)

1. **Wasm ABI is locked.** Every ark plugin must export exactly `load / update /
   render / pipe / plugin_version` and link `zellij-tile`. We can't replace this
   without forking zellij. Any "ark plugin manifest" we add lives *outside* the wasm —
   either in a sidecar file ark reads (zellij will ignore it) or in
   `BTreeMap<String,String>` user configuration handed to `load()`. **Wiggle room:** we
   can pile additional exports onto the wasm (e.g. an `ark_meta` custom section or an
   `ark_register` function); zellij ignores anything beyond the five it requires.

2. **No discovery — ark must do its own.** Zellij never scans a directory; it loads
   plugins by URL on request. If ark wants `~/.config/ark/plugins/*.wasm`
   auto-discovery, ark has to implement it (scan dir → register each as a `plugins {}`
   alias → optionally surface in a launcher UI). **Wiggle room:** aliases can be
   programmatically added before zellij starts, so a sidecar manifest read by ark can
   drive layout-time aliasing — and a custom-section blob (Cluster B trick) inside the
   wasm can supply alias name + display metadata without a sidecar.

3. **Permissions are negotiated at runtime, per-URL, persisted by zellij.** We cannot
   pre-declare "ark plugin foo needs X, Y, Z" in a manifest and have zellij honor it.
   The plugin code itself must call `request_permission()` from inside `load()`.
   **Wiggle room:** ark can wrap plugin loading with a UX that surfaces *expected*
   permissions (read from an ark-side custom section) before launching, but the actual
   grant flow is zellij's modal in the pane and the persistence is in
   `$ZELLIJ_CACHE_DIR/permissions.kdl`.

4. **Pipes are the ready-made intent bus.** If ark intents are "named messages with a
   payload routed to one or many plugins", that's literally `PipeMessage`. We get
   broadcast, targeted, CLI-injectable, keybind-injectable, and backpressure for free —
   all at the cost of `MessageAndLaunchOtherPlugins` permission. **Wiggle room:**
   designing ark intents on top of pipes means zero new ABI; designing them off-pipe
   means a parallel transport (e.g. a unix socket the ark host owns), which only buys
   us out-of-band reach (pi-style external processes) — not enough payoff for v1.

5. **Versioning is weak — ark must add its own gate.** `plugin_version` only asserts
   `zellij-tile` semver match, with a log warning, not a refusal. There is no
   built-in negotiation for ark-layer conventions (intent schema version, custom
   section format version). **Wiggle room (and obligation):** ark should bake an
   `ark_abi_version` into the custom section or as an additional wasm export, read at
   load, and refuse to load on mismatch — we cannot rely on zellij's check to catch
   ark-layer drift.

---

## Cluster A: VS Code, IntelliJ, Browser WebExtensions

Reference survey for the ark sidecar-manifest pivot. Each subsection covers
(1) bundle layout, (2) author CLI, (3) lifecycle hooks, (4) capability/permission model.

---

### 1. VS Code Extensions (.vsix)

#### Bundle layout

A `.vsix` is a ZIP file (Open Packaging Conventions, same family as
.docx/.nupkg). The interesting payload lives under `extension/`.

```
my-ext-0.0.1.vsix         (zip)
├── [Content_Types].xml   # OPC mime map
├── extension.vsixmanifest # OPC metadata generated by vsce
└── extension/
    ├── package.json      # THE manifest (this is what matters)
    ├── README.md
    ├── CHANGELOG.md
    ├── LICENSE
    ├── icon.png
    ├── out/
    │   └── extension.js  # compiled JS entry point
    └── node_modules/     # bundled runtime deps
```

Required: `package.json`. Everything else is optional but conventional. The
`extension/` prefix is fixed by the OPC layout; vsce adds it automatically.

#### Manifest snippet (package.json)

```json
{
  "name": "word-count",
  "displayName": "Word Count",
  "version": "0.0.1",
  "publisher": "ms-vscode",
  "engines": { "vscode": "^1.74.0" },
  "main": "./out/extension.js",
  "activationEvents": [
    "onLanguage:markdown",
    "onCommand:wordcount.show"
  ],
  "contributes": {
    "commands": [
      { "command": "wordcount.show", "title": "Show Word Count" }
    ],
    "configuration": {
      "title": "Word Count",
      "properties": {
        "wordcount.enable": { "type": "boolean", "default": true }
      }
    }
  },
  "capabilities": {
    "untrustedWorkspaces": { "supported": "limited" },
    "virtualWorkspaces": true
  }
}
```

Required: `name`, `version`, `publisher`, `engines.vscode`. Everything in
`contributes` is a static declaration the host reads without running the
extension — commands, menus, themes, languages, debuggers, views, etc.

#### Author CLI (`vsce`, npm: `@vscode/vsce`)

```bash
vsce package           # → my-ext-0.0.1.vsix
vsce publish           # uploads to Marketplace via PAT
vsce publish minor     # bumps version then publishes
vsce ls                # list files vsce would include (respects .vscodeignore)
vsce login <publisher> # store Personal Access Token
vsce unpublish
vsce show <ext-id>
```

Dev mode: no packaging required. Either symlink/copy the source dir into
`~/.vscode/extensions/<publisher>.<name>-<version>/`, or hit F5 in VS Code
which spawns an Extension Development Host process loading the workspace
as an extension. `code --install-extension my-ext-0.0.1.vsix` installs a
built bundle.

#### Lifecycle hooks

The extension's `main` module exports two functions:

```ts
export function activate(context: vscode.ExtensionContext) { ... }
export function deactivate(): Thenable<void> | undefined { ... }
```

- `activate` invoked **once** when any `activationEvents` entry fires
  (`onLanguage:foo`, `onCommand:bar`, `onStartupFinished`,
  `workspaceContains:**/*.csproj`, `onView:myView`, `onUri`, `onDebug`,
  etc. — ~25 event types).
- `deactivate` runs on shutdown / disable / uninstall; must return a
  Promise if cleanup is async.
- No explicit install/update hook — host re-reads `package.json` and re-runs
  activation events on next launch. Static `contributes` makes most "what
  does this plugin do" queries answerable without ever calling `activate`.
- Idempotency: not addressed; activate is single-shot by design.

#### Capability/permission model

VS Code has **no fine-grained permission system** for extension APIs (an
extension that can call `vscode.workspace.fs` can read any file). What
exists are *trust* declarations:

- `capabilities.untrustedWorkspaces`: `true` | `false` |
  `{ supported: "limited", restrictedConfigurations: [...] }`
- `capabilities.virtualWorkspaces`: same shape — opts into virtual FS
  workspaces.

Plus per-feature opt-ins inside `contributes` (`contributes.debuggers`,
`contributes.taskDefinitions`, etc.). The model trusts the publisher;
isolation is process-level (extension host runs in a separate Node process
per window).

---

### 2. JetBrains / IntelliJ Plugins (.jar / .zip)

#### Bundle layout

A plugin without external deps is a single `.jar`. With deps it becomes a
directory packaged as a `.zip`:

```
sample-plugin.zip
└── sample/
    ├── lib/
    │   ├── sample.jar
    │   ├── lib_foo.jar
    │   └── lib_bar.jar
    └── META-INF/
        ├── plugin.xml
        ├── pluginIcon.svg
        └── pluginIcon_dark.svg
```

Single-jar form:

```
sample.jar
├── META-INF/
│   ├── plugin.xml
│   └── pluginIcon.svg
└── com/company/Sample.class
```

Required: `META-INF/plugin.xml`. Recommended: `pluginIcon.svg` + dark
variant. Loose JARs in `lib/` go on the classpath automatically. Docs
explicitly say **do not repackage libraries** into the main jar (breaks
Plugin Verifier).

#### Manifest snippet (plugin.xml)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<idea-plugin url="https://example.com">
  <id>com.example.framework</id>
  <name>My Framework Support</name>
  <version>1.3.18</version>
  <vendor email="joe@example.com">Joe Doe</vendor>
  <description><![CDATA[Framework support with completion.]]></description>

  <idea-version since-build="231.7172" until-build="241.*"/>

  <depends>com.intellij.modules.platform</depends>
  <depends>com.intellij.modules.java</depends>
  <depends optional="true" config-file="myPlugin-withKotlin.xml">
    org.jetbrains.kotlin
  </depends>

  <extensions defaultExtensionNs="com.intellij">
    <applicationService serviceImplementation="com.example.MyService"/>
    <postStartupActivity implementation="com.example.MyStartup"/>
  </extensions>

  <actions>
    <action id="MyAction" class="com.example.MyAction" text="Do It">
      <add-to-group group-id="ToolsMenu" anchor="last"/>
      <keyboard-shortcut keymap="$default" first-keystroke="control alt G"/>
    </action>
  </actions>
</idea-plugin>
```

Required: `<id>`, `<name>`, `<version>`, `<vendor>`, `<idea-version>`.
`<depends>` resolves both other plugins and built-in modules. Optional deps
load a *secondary* config file when present — clean way to ship
plugin-aware shims without hard requirement.

#### Author CLI (intellij-platform-gradle-plugin 2.x)

```bash
./gradlew buildPlugin       # → build/distributions/sample-1.3.18.zip
./gradlew runIde            # spawns sandboxed IDE with plugin loaded (dev mode)
./gradlew runPluginVerifier # static compatibility check across IDE versions
./gradlew publishPlugin     # uploads to JetBrains Marketplace
./gradlew patchPluginXml    # injects since/until-build from gradle config
```

Dev mode = `runIde`: launches a fresh IDE instance with a sandbox config
directory and the plugin auto-installed. No need to package a `.zip`.
Dynamic-load testing requires the plugin avoid deprecated Components.

#### Lifecycle hooks

Modern model (component-free, dynamic-load-safe):

- **Services** (`<applicationService>`, `<projectService>`): lazily
  instantiated on first `getService()` call. Implement
  `Disposable.dispose()` for shutdown cleanup. Persistent state via
  `PersistentStateComponent` (`getState()` / `loadState(s)`).
- **Startup**: `com.intellij.postStartupActivity` (EDT, project open) or
  `backgroundPostStartupActivity` (5s delayed background).
- **Shutdown**: `AppLifecycleListener.appWillBeClosed`, or
  `Disposer.register` to tie object lifetime to a parent disposable
  (project, application, plugin).
- **Install/Update/Uninstall**: no explicit per-plugin self-callback. The
  platform emits `DynamicPluginListener.beforePluginLoaded` / `pluginLoaded`
  / `beforePluginUnload` / `pluginUnloaded` for *other* code observing
  plugin changes — your plugin doesn't get a callback on its own install.
- Deprecated `ApplicationComponent` / `ProjectComponent` are explicitly
  blocked from dynamic loading; new plugins must avoid them.

Idempotency: not enforced. Services are singletons per scope; load/unload
during dynamic install requires the plugin to release every classloader
reference, which is the hard part of supporting dynamic plugins.

#### Capability/permission model

IntelliJ has **no permission system**. A loaded plugin runs with full IDE
JVM privileges. The closest analogue:

- `<depends>` declares prerequisite platform modules and other plugins —
  host refuses to load if missing.
- `<extensions>` is the static surface area: every contribution is a typed
  XML element pointing at a class implementing a known EP interface. Host
  controls when those classes are instantiated.
- `<idea-version>` constrains compatibility; Plugin Verifier statically
  checks API usage against declared range.

Trust is established via Marketplace signing + JetBrains-issued certificates
for paid plugins, not via runtime enforcement.

---

### 3. Browser WebExtensions (Chrome .crx / Firefox .xpi)

#### Bundle layout

Both `.crx` (Chrome) and `.xpi` (Firefox) are renamed ZIPs. Chrome's `.crx`
adds a small binary header (magic `Cr24`, version, public key, signature)
followed by the ZIP payload. `.xpi` is a pure ZIP.

```
my-extension.crx (or .xpi)
├── manifest.json          # required
├── service-worker.js      # MV3 background
├── popup.html
├── popup.js
├── content-script.js
├── options.html
├── _locales/
│   ├── en/messages.json
│   └── de/messages.json
└── images/
    ├── icon-48.png
    └── icon-128.png
```

Required: `manifest.json` at the archive root. Everything else is referenced
by relative path from the manifest.

#### Manifest snippet (manifest.json, MV3)

```json
{
  "manifest_version": 3,
  "name": "Example Extension",
  "version": "1.0.0",
  "description": "Demonstrates key manifest features",
  "icons": { "48": "images/icon-48.png", "128": "images/icon-128.png" },

  "permissions": ["storage", "scripting", "activeTab", "notifications"],
  "optional_permissions": ["history", "bookmarks"],
  "host_permissions": ["https://*.example.com/*"],
  "optional_host_permissions": ["<all_urls>"],

  "background": { "service_worker": "service-worker.js" },
  "action": { "default_popup": "popup.html" },
  "content_scripts": [
    { "matches": ["https://example.com/*"], "js": ["content-script.js"] }
  ],
  "options_page": "options.html",
  "web_accessible_resources": [
    { "resources": ["images/*.png"], "matches": ["<all_urls>"] }
  ],
  "minimum_chrome_version": "114"
}
```

Required: `manifest_version` (must be `3`), `name`, `version`. Everything
else is opt-in capability declaration.

#### Author CLI

Chrome has no first-party CLI. Authors zip the directory themselves; the
Chrome Web Store dashboard handles signing into `.crx`. Common approach:

```bash
zip -r my-extension.zip . -x '*.git*' '*.DS_Store'
# Upload zip to chrome.google.com/webstore/devconsole
```

Dev mode (Chrome): `chrome://extensions` → enable Developer mode →
"Load unpacked" → pick directory. No zip needed; manifest changes need
explicit reload, popup HTML hot-reloads.

Firefox ships `web-ext` (Mozilla, npm):

```bash
web-ext run         # launches Firefox with extension loaded; auto-reloads on file change
web-ext build       # → web-ext-artifacts/my_ext-1.0.0.zip (rename .xpi)
web-ext lint        # validates manifest + permissions + API compat
web-ext sign        # submits to AMO, returns signed .xpi for self-distribution
web-ext docs        # opens MDN reference
web-ext dump-config # debug current resolved config
```

`web-ext run` is the canonical dev loop — point at a directory, auto-reload
on edit, no packaging step.

#### Lifecycle hooks

WebExtensions don't expose explicit install/uninstall functions. Instead the
service worker (or background page in MV2) listens to runtime events:

```js
chrome.runtime.onInstalled.addListener(({ reason, previousVersion }) => {
  // reason ∈ "install" | "update" | "chrome_update" | "shared_module_update"
  if (reason === "install") { /* first-run setup */ }
  if (reason === "update")  { /* migrate from previousVersion */ }
});
chrome.runtime.onStartup.addListener(() => { /* browser startup */ });
chrome.runtime.onSuspend.addListener(() => { /* SW about to terminate */ });
chrome.runtime.onSuspendCanceled.addListener(() => { ... });
```

The MV3 service worker is **ephemeral** — it can be killed at any time and
re-spawned by an event. This forces idempotency: handlers must reconstruct
their state from `chrome.storage` rather than in-memory globals. There is
no `deactivate` or uninstall callback (the extension is gone before any code
could run); `chrome.runtime.setUninstallURL(url)` is the only "on uninstall"
hook and it just opens a webpage.

#### Capability/permission model

This is where WebExtensions are strongest. Two axes:

1. **API permissions** (`permissions`): named strings gating browser APIs —
   `storage`, `tabs`, `scripting`, `cookies`, `history`, `bookmarks`,
   `notifications`, `contextMenus`, `downloads`, `clipboardRead`,
   `clipboardWrite`, `desktopCapture`, `geolocation`, `webNavigation`,
   `system.cpu`, etc.
2. **Host permissions** (`host_permissions`): URL match patterns gating
   which origins the extension can read/modify (`https://*.example.com/*`,
   `<all_urls>`).

Enforcement:

- Declared in manifest → host shows install-time warning derived from the
  permission set ("Read your browsing history", "Access data for all
  websites"). User accepts or refuses install.
- `optional_permissions` / `optional_host_permissions` request at runtime
  via `chrome.permissions.request({ permissions, origins })` — user gets a
  prompt, can later revoke via the extension settings.
- `activeTab` is a special low-friction permission: grants temporary access
  to the currently focused tab only on a user gesture (toolbar click,
  keyboard shortcut). No install warning.
- The browser enforces at the API call site — calling `chrome.tabs.query`
  without `tabs` returns an error or empty data.

This is the only one of the three systems that sandboxes capability at the
API level rather than trusting the publisher.

---

### Lessons for ark

- **Sidecar manifest is the norm.** All three systems keep the manifest as
  a separate file (`package.json`, `plugin.xml`, `manifest.json`) parsed by
  the host without running plugin code. Static contribution lists
  (`contributes`, `<extensions>`, `permissions`) let the host show
  capabilities, build menus, and route events before the plugin ever loads.
- **Bundle = zip with a known manifest path.** VS Code, IntelliJ-with-deps,
  and both browsers all settle on ZIP containers with one canonical entry
  file. A custom extension is just renaming (.vsix, .xpi, .crx) — pick a
  clear extension name and don't invent a new container format.
- **Dev mode = "point at a directory."** `code` symlink, `runIde` sandbox,
  `web-ext run`, "Load unpacked" — every system has a zero-package
  iteration loop. ark's CLI should ship `ark ext dev <dir>` from day one;
  authors who must `pack` to test will fight you.
- **Lifecycle is event-driven, not install-callback.** None of these systems
  give plugins an "on install" function — they give activation events
  (`onLanguage`, `postStartupActivity`, `onInstalled` with `reason`). This
  forces idempotent setup that survives crashes and updates. Avoid an
  `install()` hook; prefer `activate(reason)` where `reason` carries
  install/update/launch context.
- **Permission model is a real choice.** VS Code and IntelliJ trust the
  publisher (process isolation only); WebExtensions enforce per-API and
  per-origin with user-visible prompts. ark targets terminal multiplexer
  use — a wasm sandbox plus declared `permissions: [filesystem, network,
  spawn, ...]` in the sidecar manifest, validated at load time, gives the
  WebExtension model without runtime prompt fatigue. Make capabilities
  explicit in the manifest; let the host refuse to load mismatches.
