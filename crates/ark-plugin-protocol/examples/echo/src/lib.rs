//! T-PP-022: full `#[derive(Plugin)]` wire-up.
//!
//! Reference "echo" plugin — the minimum viable implementation of the
//! `ark:plugin@0.1.0` world. Purpose: exercise the WIT contract end-
//! to-end (guest-side `wit_bindgen::generate!` through to the host's
//! coarse cap gate). No business logic; `render` returns a single
//! `text("echo")` widget.
//!
//! The `#[derive(Plugin)]` invocation below emits the `ark-caps:v1` +
//! `ark-meta:v1` custom sections into the compiled `.wasm` via
//! `#[link_section]`. See `crates/ark-plugin-sdk` for the attribute
//! surface.
//!
//! NOTE on the `name = "plugin"` attribute: the shared WIT world in
//! `crates/ark-plugin-protocol/wit/plugin.wit` is itself named `plugin`.
//! Per R9 "the WIT world name must equal the plugin name", the echo
//! example adopts `"plugin"` as its plugin name. If/when echo needs its
//! own identity, a dedicated `wit/echo.wit` world named `echo` can be
//! introduced and `name = "echo"` used in its place.
//!
//! This crate is intentionally NOT a workspace member — `cargo check
//! --workspace` does not touch it. It builds standalone against
//! `wasm32-wasip2`. The CI gate that actually compiles this crate
//! lives in `crates/ark-plugin-protocol/tests/echo_sections.rs`
//! (ignored by default — run with `cargo test -p ark-plugin-protocol --
//! --ignored`).
//!
//! Dep policy: no `arborium-sysroot`, no `facet-kdl`, no
//! `facet-format` (kit R12 regression check). `ark-plugin-sdk` is a
//! proc-macro-only dep and does NOT appear in the wasm guest's runtime
//! dep graph.

#![cfg(target_arch = "wasm32")]

use ark_plugin_sdk::Plugin;

// Generate the guest bindings against the repo's canonical WIT.
//
// The generated code lives in this module; it drives:
//   - the `Guest` trait that `EchoPlugin` implements below;
//   - the `export!` macro wiring `EchoPlugin` to the five lifecycle
//     exports declared in `wit/plugin.wit`;
//   - the host-import shims we reach below (`ark:host/log`,
//     `ark:cap/fs-read`).
wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
    // `generate_all` would pull *every* cap interface — but this
    // plugin intentionally only imports `ark:host/log` + `ark:cap/fs-read`.
    // `wit-bindgen` still generates trait stubs for the full world
    // since the world declares every interface; the linker-side
    // cap gate ensures uncalled cap interfaces don't actually wire
    // at instantiation time.
});

/// Identity + cap-declaration target for the `#[derive(Plugin)]` macro.
///
/// The macro sees this unit struct as the hook for its
/// `#[link_section]` emission — `Echo` itself doesn't carry any state;
/// the guest trait is implemented on `EchoPlugin` below.
#[derive(Plugin)]
#[plugin(
    // See the crate-level note on why `name = "plugin"` — tracks the
    // shared WIT world's name. The `wit = "..."` path is relative to
    // this Cargo.toml (`examples/echo/Cargo.toml`).
    name = "plugin",
    version = "0.1.0",
    wit = "../../wit/plugin.wit",
    capabilities(
        fs_read(display = "Read files", reason = "echo example reads demo file"),
    ),
)]
struct Echo;

struct EchoPlugin;

impl Guest for EchoPlugin {
    fn on_install(event: InstallEvent) {
        ark::plugin::log::log(&format!("echo: on-install {:?}", install_event_tag(&event)));
    }

    fn load() {
        ark::plugin::log::log("echo: load");
    }

    fn update(_event: HostEvent) -> bool {
        // Echo has nothing to update on — return `false` so the host
        // takes the no-op fast path (no re-render triggered).
        false
    }

    fn render(_view_id: String, _width: u32, _height: u32) -> WidgetTree {
        // Single `text("echo")` node wrapped in the terminal arm of
        // `widget-tree` per R10.
        WidgetTree::Terminal(TerminalWidgetTree::Text(TextNode {
            content: "echo".to_string(),
            style: None,
        }))
    }

    fn pipe(_message: PipeMessage) -> bool {
        false
    }
}

/// Collapse an [`InstallEvent`] to a static string tag so the `log`
/// message doesn't require `{:?}` on the generated variant type (whose
/// shape is fixed by `wit-bindgen` and may not implement `Debug` in
/// every version).
fn install_event_tag(e: &InstallEvent) -> &'static str {
    match e {
        InstallEvent::Install => "install",
        InstallEvent::Update(_) => "update",
        InstallEvent::HostUpdate(_) => "host-update",
        InstallEvent::Reload => "reload",
        InstallEvent::ReservedFuture(_) => "reserved-future",
    }
}

// Register `EchoPlugin` as the guest for the world's five exports.
export!(EchoPlugin);
