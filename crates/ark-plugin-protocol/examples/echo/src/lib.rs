//! T-PP-018 partial: exports world, imports log + fs-read. Sections wired in T-PP-022.
//!
//! Reference "echo" plugin — the minimum viable implementation of the
//! `ark:plugin@0.1.0` world. Purpose: exercise the WIT contract end-
//! to-end (guest-side `wit_bindgen::generate!` through to the host's
//! coarse cap gate). No business logic; `render` returns a single
//! `text("echo")` widget.
//!
//! This crate is intentionally NOT a workspace member — `cargo check
//! --workspace` does not touch it. It builds standalone against
//! `wasm32-wasip2`. The CI gate that actually compiles this crate
//! lands in T-PP-022 once the `#[derive(Plugin)]` macro is real.
//!
//! Dep policy: no `arborium-sysroot`, no `facet-kdl`, no
//! `facet-format` (kit R12 regression check).

#![cfg(target_arch = "wasm32")]

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
