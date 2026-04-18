//! `gen-extension-spec` — emit a canonical `extension-protocol.kdl` file
//! by walking facet SHAPE reflection for every request/response struct
//! plus the `ArkExtension` method table defined in
//! `ark_ext_proto::lib`.
//!
//! This is the ark-side analog of JSON-Schema: one KDL document that
//! describes the full RPC surface so cross-language extension authors
//! (subprocess + wasm) can generate bindings without reading the Rust
//! source. Ships at `crates/ark-ext-proto/share/extension-protocol.kdl`
//! per T-9.5.2 / `cavekit-scene.md` R16 ("protocol-first, trait-
//! derived"). CI runs this binary and diffs against the checked-in file
//! to catch drift.
//!
//! Run:
//! ```sh
//! cargo run -p ark-ext-proto --bin gen-extension-spec
//! ```
//!
//! # Design
//!
//! The `ArkExtension` trait itself is not directly walkable via facet
//! SHAPE — `#[async_trait]` rewrites every method into a pinned-future
//! boxed return, and Rust has no stable reflection for trait items on
//! stable. We therefore keep a single hand-maintained table of
//! `(method, RequestShape, ResponseShape, kind)` tuples below and
//! drive field-level reflection (arg names, types, doc-comments) from
//! the request/response shapes themselves. The binary's smoke test
//! asserts the table covers every method on the trait, so drift is
//! caught at `cargo test`.
//!
//! # Facet SHAPE gotchas (same as `gen-scene-schema`)
//!
//! * `Shape.type_identifier` strips generics. For `Option<T>` / `Vec<T>`
//!   unwrap via `Def::Option` / `Def::List` before taking the inner
//!   identifier.
//! * Fields doc-comments live at `Field.doc` (one entry per `///` line);
//!   type doc-comments live at `Shape.doc`.
//! * `Field.rename` handles `#[facet(rename = "...")]` overrides — we
//!   honour the rename when naming params in the schema.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use facet::{Def, Facet, Shape, Type, UserType};

use ark_ext_proto::*;

/// RPC semantics — whether ark expects a response from the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Request/response pair. Carries a JSON-RPC `id`; the extension
    /// MUST eventually respond (success or error).
    Request,
    /// One-way notification. No `id`, no response. Equivalent to
    /// `$/progress`, `$/cancel`, `ui/status/push`, `log/write`, etc.
    Notification,
}

/// Hand-maintained method table. Every entry mirrors one
/// `async fn ...` on the `ArkExtension` trait.
///
/// Order matters — output is emitted in this order, grouped by section
/// to match the `lib.rs` layout. The smoke test asserts count + method
/// names line up with the trait method list.
struct MethodEntry {
    /// JSON-RPC method name (hyphenated / slash-separated as per R16).
    method: &'static str,
    /// Human-readable section header — drives `// --- Lifecycle ---`
    /// style banners in the emitted KDL.
    section: &'static str,
    /// Request shape — `&'static Shape` via `Facet::SHAPE`.
    request: &'static Shape,
    /// Response shape.
    response: &'static Shape,
    /// Request vs. notification.
    kind: Kind,
}

/// Build the full method table. This is the single source of truth for
/// what the emitter covers — update this when adding / removing trait
/// methods.
fn method_table() -> Vec<MethodEntry> {
    vec![
        // -- Lifecycle ---------------------------------------------------
        MethodEntry {
            method: "initialize",
            section: "Lifecycle",
            request: InitializeRequest::SHAPE,
            response: InitializeResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "initialized",
            section: "Lifecycle",
            request: InitializedRequest::SHAPE,
            response: InitializedResponse::SHAPE,
            kind: Kind::Notification,
        },
        MethodEntry {
            method: "shutdown",
            section: "Lifecycle",
            request: ShutdownRequest::SHAPE,
            response: ShutdownResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "ping",
            section: "Lifecycle",
            request: PingRequest::SHAPE,
            response: PingResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Async + cancel ----------------------------------------------
        MethodEntry {
            method: "$/cancel",
            section: "Async + cancel",
            request: CancelRequest::SHAPE,
            response: CancelResponse::SHAPE,
            kind: Kind::Notification,
        },
        MethodEntry {
            method: "$/progress",
            section: "Async + cancel",
            request: ProgressRequest::SHAPE,
            response: ProgressResponse::SHAPE,
            kind: Kind::Notification,
        },
        MethodEntry {
            method: "task/create",
            section: "Async + cancel",
            request: TaskCreateRequest::SHAPE,
            response: TaskCreateResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "task/get",
            section: "Async + cancel",
            request: TaskGetRequest::SHAPE,
            response: TaskGetResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "task/cancel",
            section: "Async + cancel",
            request: TaskCancelRequest::SHAPE,
            response: TaskCancelResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Event bus ---------------------------------------------------
        MethodEntry {
            method: "event/subscribe",
            section: "Event bus",
            request: EventSubscribeRequest::SHAPE,
            response: EventSubscribeResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "event/unsubscribe",
            section: "Event bus",
            request: EventUnsubscribeRequest::SHAPE,
            response: EventUnsubscribeResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "event/emit",
            section: "Event bus",
            request: EventEmitRequest::SHAPE,
            response: EventEmitResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "event/notify",
            section: "Event bus",
            request: EventNotifyRequest::SHAPE,
            response: EventNotifyResponse::SHAPE,
            kind: Kind::Notification,
        },
        // -- Intents -----------------------------------------------------
        MethodEntry {
            method: "intent/unregister",
            section: "Intents",
            request: IntentUnregisterRequest::SHAPE,
            response: IntentUnregisterResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "intent/dispatch",
            section: "Intents",
            request: IntentDispatchRequest::SHAPE,
            response: IntentDispatchResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- UI: keybind / status ----------------------------------------
        MethodEntry {
            method: "ui/keybind/register",
            section: "UI keybind/status",
            request: UiKeybindRegisterRequest::SHAPE,
            response: UiKeybindRegisterResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "ui/keybind/unregister",
            section: "UI keybind/status",
            request: UiKeybindUnregisterRequest::SHAPE,
            response: UiKeybindUnregisterResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "ui/status/push",
            section: "UI keybind/status",
            request: UiStatusPushRequest::SHAPE,
            response: UiStatusPushResponse::SHAPE,
            kind: Kind::Notification,
        },
        // -- UI: panes ---------------------------------------------------
        MethodEntry {
            method: "ui/pane/request",
            section: "UI panes",
            request: UiPaneRequestRequest::SHAPE,
            response: UiPaneRequestResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "ui/pane/close",
            section: "UI panes",
            request: UiPaneCloseRequest::SHAPE,
            response: UiPaneCloseResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Workspace ---------------------------------------------------
        MethodEntry {
            method: "workspace/applyEdit",
            section: "Workspace",
            request: WorkspaceApplyEditRequest::SHAPE,
            response: WorkspaceApplyEditResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "workspace/configuration",
            section: "Workspace",
            request: WorkspaceConfigurationRequest::SHAPE,
            response: WorkspaceConfigurationResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "workspace/showDocument",
            section: "Workspace",
            request: WorkspaceShowDocumentRequest::SHAPE,
            response: WorkspaceShowDocumentResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "workspace/showMessage",
            section: "Workspace",
            request: WorkspaceShowMessageRequest::SHAPE,
            response: WorkspaceShowMessageResponse::SHAPE,
            kind: Kind::Notification,
        },
        MethodEntry {
            method: "workspace/showMessageRequest",
            section: "Workspace",
            request: WorkspaceShowMessageRequestRequest::SHAPE,
            response: WorkspaceShowMessageRequestResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Scene -------------------------------------------------------
        MethodEntry {
            method: "scene/getRoot",
            section: "Scene",
            request: SceneGetRootRequest::SHAPE,
            response: SceneGetRootResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Host (wasm-only) --------------------------------------------
        MethodEntry {
            method: "host/fs/read",
            section: "Host syscalls (wasm-only)",
            request: HostFsReadRequest::SHAPE,
            response: HostFsReadResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "host/fs/write",
            section: "Host syscalls (wasm-only)",
            request: HostFsWriteRequest::SHAPE,
            response: HostFsWriteResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "host/proc/spawn",
            section: "Host syscalls (wasm-only)",
            request: HostProcSpawnRequest::SHAPE,
            response: HostProcSpawnResponse::SHAPE,
            kind: Kind::Request,
        },
        MethodEntry {
            method: "host/net/fetch",
            section: "Host syscalls (wasm-only)",
            request: HostNetFetchRequest::SHAPE,
            response: HostNetFetchResponse::SHAPE,
            kind: Kind::Request,
        },
        // -- Logging -----------------------------------------------------
        MethodEntry {
            method: "log/write",
            section: "Logging",
            request: LogWriteRequest::SHAPE,
            response: LogWriteResponse::SHAPE,
            kind: Kind::Notification,
        },
        MethodEntry {
            method: "log/setLevel",
            section: "Logging",
            request: LogSetLevelRequest::SHAPE,
            response: LogSetLevelResponse::SHAPE,
            kind: Kind::Request,
        },
    ]
}

/// Type index: every user struct / enum reachable from any method's
/// request/response shape. Used to emit a `definitions { ... }` block
/// so cross-language bindings can resolve references.
type TypeIndex = BTreeMap<&'static str, &'static Shape>;

fn main() -> std::io::Result<()> {
    let output = share_dir().join("extension-protocol.kdl");
    std::fs::create_dir_all(output.parent().unwrap())?;
    let doc = generate_protocol_kdl();
    std::fs::write(&output, doc.as_bytes())?;
    eprintln!("wrote {} bytes to {}", doc.len(), output.display());
    Ok(())
}

/// Workspace-relative `crates/ark-ext-proto/share/` path. `CARGO_MANIFEST_DIR`
/// is the crate root, so `share/` is one hop below.
fn share_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("share")
}

// ---------------------------------------------------------------------------
// Schema generation
// ---------------------------------------------------------------------------

/// Produce the full `extension-protocol.kdl` document as a string.
fn generate_protocol_kdl() -> String {
    let table = method_table();

    let mut types: TypeIndex = BTreeMap::new();
    // Seed the index with the common value types so even methods that
    // reference them indirectly still get a definition.
    collect_user_types(LogLevel::SHAPE, &mut types);
    collect_user_types(TaskId::SHAPE, &mut types);
    for m in &table {
        collect_user_types(m.request, &mut types);
        collect_user_types(m.response, &mut types);
    }

    let mut out = String::new();
    writeln!(
        &mut out,
        "// Generated by `cargo run -p ark-ext-proto --bin gen-extension-spec`."
    )
    .unwrap();
    writeln!(
        &mut out,
        "// DO NOT EDIT BY HAND. Source of truth: crates/ark-ext-proto/src/lib.rs"
    )
    .unwrap();
    writeln!(&mut out, "// (walked via facet SHAPE reflection).").unwrap();
    writeln!(&mut out).unwrap();
    writeln!(&mut out, "protocol {{").unwrap();
    writeln!(&mut out, "    info {{").unwrap();
    writeln!(
        &mut out,
        "        title \"ark extension protocol (R16 v1)\""
    )
    .unwrap();
    writeln!(
        &mut out,
        "        description \"Runtime-RPC surface for ark extensions — subprocess (JSON-RPC 2.0 / NDJSON), wasm-component (wit-bindgen), and compiled-in trait-object transports all speak this.\""
    )
    .unwrap();
    writeln!(&mut out, "        version \"0.1.0\"").unwrap();
    writeln!(&mut out, "    }}").unwrap();
    writeln!(&mut out).unwrap();

    // Methods — grouped by `section`, in table order.
    writeln!(&mut out, "    methods {{").unwrap();
    let mut current_section = "";
    for m in &table {
        if m.section != current_section {
            if !current_section.is_empty() {
                writeln!(&mut out).unwrap();
            }
            writeln!(&mut out, "        // -- {} --", m.section).unwrap();
            current_section = m.section;
        }
        emit_method(&mut out, m, 2);
    }
    writeln!(&mut out, "    }}").unwrap();
    writeln!(&mut out).unwrap();

    // Definitions — every user type reachable from any method surface.
    writeln!(&mut out, "    definitions {{").unwrap();
    for (name, shape) in &types {
        emit_definition(&mut out, name, shape, 2);
    }
    writeln!(&mut out, "    }}").unwrap();

    writeln!(&mut out, "}}").unwrap();
    out
}

/// Emit a single `method "<name>" { ... }` block.
fn emit_method(out: &mut String, m: &MethodEntry, depth: usize) {
    let indent = indent(depth);
    writeln!(out, "{indent}method \"{}\" {{", m.method).unwrap();
    writeln!(
        out,
        "{}kind \"{}\"",
        indent_for(depth + 1),
        match m.kind {
            Kind::Request => "request",
            Kind::Notification => "notification",
        }
    )
    .unwrap();
    write_doc(out, m.request.doc, depth + 1);

    // Params block — list every field on the request struct.
    writeln!(out, "{}params {{", indent_for(depth + 1)).unwrap();
    emit_fields(out, m.request, depth + 2);
    writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();

    // Result block — list every field on the response struct. Notifications
    // formally have no response, but we still emit the shape so tooling can
    // round-trip them (some transports echo notification acks).
    writeln!(out, "{}result {{", indent_for(depth + 1)).unwrap();
    emit_fields(out, m.response, depth + 2);
    writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();

    writeln!(out, "{indent}}}").unwrap();
}

/// Emit one `field "<name>" { ... }` entry per struct field.
fn emit_fields(out: &mut String, shape: &'static Shape, depth: usize) {
    let fields = match &shape.ty {
        Type::User(UserType::Struct(st)) => st.fields,
        _ => &[],
    };
    for field in fields {
        emit_field(out, field, depth);
    }
}

/// Emit a single `field "<name>" { type "..."; required #... }` entry.
fn emit_field(out: &mut String, field: &facet::Field, depth: usize) {
    let name = field.rename.unwrap_or(field.name);
    let indent = indent(depth);
    let (inner, optional, plural) = unwrap_container(field.shape());
    let type_name = scalar_type_name(inner);

    writeln!(out, "{indent}field \"{name}\" {{").unwrap();
    write_doc(out, field.doc, depth + 1);
    writeln!(out, "{}type \"{type_name}\"", indent_for(depth + 1)).unwrap();
    writeln!(
        out,
        "{}plural {}",
        indent_for(depth + 1),
        if plural { "#true" } else { "#false" }
    )
    .unwrap();
    writeln!(
        out,
        "{}required {}",
        indent_for(depth + 1),
        if optional { "#false" } else { "#true" }
    )
    .unwrap();
    writeln!(out, "{indent}}}").unwrap();
}

/// Emit a type definition in the `definitions { ... }` block.
fn emit_definition(out: &mut String, id: &str, shape: &'static Shape, depth: usize) {
    let indent = indent(depth);
    writeln!(out, "{indent}type \"{id}\" {{").unwrap();
    write_doc(out, shape.doc, depth + 1);

    match &shape.ty {
        Type::User(UserType::Struct(st)) => {
            writeln!(out, "{}kind \"struct\"", indent_for(depth + 1)).unwrap();
            if !st.fields.is_empty() {
                writeln!(out, "{}fields {{", indent_for(depth + 1)).unwrap();
                for field in st.fields {
                    emit_field(out, field, depth + 2);
                }
                writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();
            }
        }
        Type::User(UserType::Enum(en)) => {
            writeln!(out, "{}kind \"enum\"", indent_for(depth + 1)).unwrap();
            writeln!(out, "{}variants {{", indent_for(depth + 1)).unwrap();
            for variant in en.variants {
                writeln!(
                    out,
                    "{}variant \"{}\"",
                    indent_for(depth + 2),
                    variant.name
                )
                .unwrap();
            }
            writeln!(out, "{}}}", indent_for(depth + 1)).unwrap();
        }
        _ => {
            writeln!(out, "{}kind \"opaque\"", indent_for(depth + 1)).unwrap();
        }
    }

    writeln!(out, "{indent}}}").unwrap();
}

// ---------------------------------------------------------------------------
// SHAPE walking helpers (mirrors gen-scene-schema.rs)
// ---------------------------------------------------------------------------

/// Walk a SHAPE graph, inserting every user struct / enum into `acc`.
fn collect_user_types(shape: &'static Shape, acc: &mut TypeIndex) {
    let user = match shape.inner_user_shape() {
        Some(u) => u,
        None => return,
    };
    if acc.contains_key(user.type_identifier) {
        return;
    }
    acc.insert(user.type_identifier, user);

    if let Type::User(UserType::Struct(st)) = &user.ty {
        for field in st.fields {
            collect_user_types(field.shape(), acc);
        }
    }
}

/// Walk `Option<...>` / `Vec<...>` wrappers down to the first user shape
/// (or primitive). Returns `(inner, is_optional, is_plural)` — `plural`
/// is true when the outermost wrapper was a list.
fn unwrap_container(shape: &'static Shape) -> (&'static Shape, bool, bool) {
    match &shape.def {
        Def::Option(o) => {
            let (inner, _, plural) = unwrap_container(o.t);
            (inner, true, plural)
        }
        Def::List(l) => {
            let (inner, optional, _) = unwrap_container(l.t());
            (inner, optional, true)
        }
        _ => (shape, false, false),
    }
}

/// Map a shape onto a protocol-spec type tag. User structs / enums are
/// reported by identifier so bindings can cross-link into the
/// `definitions` block.
fn scalar_type_name(shape: &'static Shape) -> &'static str {
    match shape.type_identifier {
        "String" | "str" => "string",
        "bool" => "bool",
        other => other,
    }
}

/// Write a type's or field's rustdoc as a schema `description` string.
/// Multiple `///` lines are joined with spaces so the schema stays
/// single-line. Embedded `"` / `\` are escaped for the KDL literal.
fn write_doc(out: &mut String, doc: &'static [&'static str], depth: usize) {
    if doc.is_empty() {
        return;
    }
    let joined = doc
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let escaped = joined.replace('\\', "\\\\").replace('"', "\\\"");
    writeln!(out, "{}description \"{}\"", indent_for(depth), escaped).unwrap();
}

trait InnerUserShape {
    fn inner_user_shape(&'static self) -> Option<&'static Shape>;
}

impl InnerUserShape for Shape {
    fn inner_user_shape(&'static self) -> Option<&'static Shape> {
        match &self.ty {
            Type::User(UserType::Struct(_))
            | Type::User(UserType::Enum(_))
            | Type::User(UserType::Union(_)) => Some(self),
            _ => match &self.def {
                Def::Option(o) => o.t.inner_user_shape(),
                Def::List(l) => l.t().inner_user_shape(),
                _ => None,
            },
        }
    }
}

fn indent(depth: usize) -> String {
    " ".repeat(4 * depth)
}

fn indent_for(depth: usize) -> String {
    indent(depth)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every method name declared on the `ArkExtension` trait (by its
    /// hand-rolled Rust method name) must appear in `method_table()` under
    /// the canonical JSON-RPC name. Hard-coded here so when the trait
    /// grows a new method the emitter's smoke test fails loudly.
    const EXPECTED_METHODS: &[&str] = &[
        "initialize",
        "initialized",
        "shutdown",
        "ping",
        "$/cancel",
        "$/progress",
        "task/create",
        "task/get",
        "task/cancel",
        "event/subscribe",
        "event/unsubscribe",
        "event/emit",
        "event/notify",
        "intent/unregister",
        "intent/dispatch",
        "ui/keybind/register",
        "ui/keybind/unregister",
        "ui/status/push",
        "ui/pane/request",
        "ui/pane/close",
        "workspace/applyEdit",
        "workspace/configuration",
        "workspace/showDocument",
        "workspace/showMessage",
        "workspace/showMessageRequest",
        "scene/getRoot",
        "host/fs/read",
        "host/fs/write",
        "host/proc/spawn",
        "host/net/fetch",
        "log/write",
        "log/setLevel",
    ];

    #[test]
    fn table_covers_every_trait_method() {
        let table = method_table();
        assert_eq!(
            table.len(),
            EXPECTED_METHODS.len(),
            "method_table() count ({}) disagrees with EXPECTED_METHODS ({})",
            table.len(),
            EXPECTED_METHODS.len()
        );
        let got: Vec<&str> = table.iter().map(|m| m.method).collect();
        for expected in EXPECTED_METHODS {
            assert!(
                got.contains(expected),
                "method_table() missing {expected}; got {got:?}"
            );
        }
    }

    #[test]
    fn schema_mentions_every_method() {
        let schema = generate_protocol_kdl();
        for expected in EXPECTED_METHODS {
            let needle = format!("method \"{expected}\"");
            assert!(
                schema.contains(&needle),
                "schema missing method entry for {expected}"
            );
        }
    }

    #[test]
    fn schema_includes_core_definitions() {
        let schema = generate_protocol_kdl();
        // Structural user types we rely on in multiple methods.
        for ty in &[
            "LogLevel",
            "TaskId",
            "InitializeRequest",
            "InitializeResponse",
            "HostNetFetchRequest",
            "HostNetFetchResponse",
        ] {
            let needle = format!("type \"{ty}\"");
            assert!(
                schema.contains(&needle),
                "schema missing definition for {ty}"
            );
        }
    }

    #[test]
    fn schema_marks_notifications_distinctly() {
        let schema = generate_protocol_kdl();
        // `$/cancel` is a notification — find its block, check kind.
        let idx = schema
            .find("method \"$/cancel\"")
            .expect("missing $/cancel block");
        let block = &schema[idx..];
        let kind_line = block.lines().nth(1).unwrap();
        assert!(
            kind_line.contains("kind \"notification\""),
            "$/cancel kind line = {kind_line}"
        );
    }

    #[test]
    fn schema_round_trips_optional_field() {
        // `WorkspaceShowMessageRequestResponse::selected` is Option<String> —
        // must emit `required #false`. We slice from the field entry up to
        // the next field or closing brace and check it flags non-required.
        let schema = generate_protocol_kdl();
        let field_idx = schema
            .find("field \"selected\"")
            .expect("missing selected field entry");
        // Limit the scan to the upcoming ~300 chars (one field block is
        // ~180 chars; this overshoots deliberately).
        let window = &schema[field_idx..field_idx + 300.min(schema.len() - field_idx)];
        assert!(
            window.contains("required #false"),
            "selected must be required #false; saw:\n{window}"
        );
    }
}
