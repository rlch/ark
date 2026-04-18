//! T-041 (cavekit-soul-phase-2-tests.md R5) — compile-time KDL-level
//! view-type validator.
//!
//! The [`validate_scene!`] procedural macro parses a small set of inline
//! extension manifests plus a scene KDL blob at macro-expansion time
//! and emits a `compile_error!` whenever the scene references:
//!
//!  1. A view-type token no manifest declares (`UnknownViewType`).
//!  2. A declared view-type under a handle expecting a different
//!     view (`KindMismatch` on `view_type` attr).
//!  3. A stack child (`spawn_into @parent`) whose parent resolves to a
//!     `Pane<V>` handle (not a `Stack<V>`).
//!  4. A manifest-declared `Pane<V>` attribute receiving a plain string
//!     literal instead of a typed handle reference.
//!  5. An intent reference (`intent "ext.verb"` node) that is neither
//!     a core op (`ark.core.*`) nor declared by any loaded manifest
//!     (T-042 R6 — manifest is sole source of intent truth per
//!     decision #2).
//!
//! Each emitted error carries a `.kdl:<line>:<col>` pointer — the
//! `.stderr` goldens under `crates/scene/tests/ui/` grep-assert these
//! to pin the diagnostic contract.
//!
//! # Input shape
//!
//! ```ignore
//! validate_scene! {
//!     manifests: [
//!         r#"extension {
//!             name "ext.a"; version "1.0.0"
//!             ark-range ">=0.1"; zellij-range ""
//!             views { view "EditorView" { component "EditorC"; kind "pane" } }
//!         }"#,
//!     ],
//!     scene_path: "tests/ui/fixtures/example.kdl",
//!     scene: r#"
//!         scene "s" {
//!             layout {
//!                 pane @h1 { view_type "ext.a.EditorView" }
//!             }
//!         }
//!     "#,
//! }
//! ```
//!
//! `scene_path` is only used for the `.kdl:line:col` prefix in the
//! emitted diagnostic; no file I/O happens.
//!
//! # Scene mini-grammar understood by the macro
//!
//! The macro does NOT attempt to parse full scene KDL — only the
//! subset needed to exercise the four R5 view-type error cases:
//!
//! | Node                                    | Meaning                                   |
//! |-----------------------------------------|-------------------------------------------|
//! | `pane @handle { view_type "tok" }`      | Pane referencing view-type `tok` (pane)   |
//! | `stack @handle { view_type "tok" }`     | Stack referencing view-type `tok` (stack) |
//! | `spawn_into @parent { … }`              | Child spawned under `@parent`             |
//! | `handle_attr @h "tok" value="lit"`      | Typed-handle attr bound to a value        |
//! | `intent "ext.verb" [args...]`           | Dispatches the named intent (T-042 R6)    |
//!
//! Handles are written as `@name` (the `@` is part of the handle name
//! in this mini-grammar, simplifying the parser). A handle used in
//! `spawn_into` MUST resolve to a `stack`-kind node; a handle used as
//! a `value=` target in `handle_attr` MUST resolve to a declared
//! `Pane<V>` — a string literal instead is rejected.
//!
//! # Cycle avoidance
//!
//! This crate deliberately does NOT depend on `ark-scene`. The runtime
//! `ViewTypeTable` + `validate_view_reference` surface in
//! `ark-scene::compile::view_types` duplicates ~30 LOC of table logic
//! here — the alternative (extracting a third crate `view-types-core`)
//! was weighed and rejected: the duplication is trivial, the
//! invariants are already pinned by `ark-scene`'s tests, and keeping
//! `scene-macros` to three deps (`ark-ext-metadata-types` + `kdl` +
//! `syn`/`quote`/`proc-macro2`) keeps the proc-macro crate fast to
//! compile.

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Ident, LitStr, Token};

use ark_ext_metadata_types::{StringNode, ViewDecl};

/// Compile-time validator for a scene KDL blob against a set of inline
/// extension manifests. See crate-level docs for the input shape and
/// scene mini-grammar.
///
/// Emits `compile_error!("<scene_path>:<line>:<col>: <msg>")` on any
/// validation failure; expands to `()` on success.
#[proc_macro]
pub fn validate_scene(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ValidateSceneArgs);
    match run(args) {
        Ok(_) => quote! { () }.into(),
        Err(msg) => {
            let lit = LitStr::new(&msg, proc_macro2::Span::call_site());
            let out: TokenStream2 = quote! { ::core::compile_error!(#lit); };
            out.into()
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Input parsing
// ───────────────────────────────────────────────────────────────────

/// Structured macro input. `manifests: [...]`, `scene_path: "..."`,
/// `scene: "..."`. Commas are optional between fields and a trailing
/// comma is accepted.
struct ValidateSceneArgs {
    manifests: Vec<String>,
    scene_path: String,
    scene: String,
}

impl Parse for ValidateSceneArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut manifests: Option<Vec<String>> = None;
        let mut scene_path: Option<String> = None;
        let mut scene: Option<String> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "manifests" => {
                    let content;
                    syn::bracketed!(content in input);
                    let mut out = Vec::new();
                    while !content.is_empty() {
                        let s: LitStr = content.parse()?;
                        out.push(s.value());
                        if content.peek(Token![,]) {
                            content.parse::<Token![,]>()?;
                        }
                    }
                    manifests = Some(out);
                }
                "scene_path" => {
                    let s: LitStr = input.parse()?;
                    scene_path = Some(s.value());
                }
                "scene" => {
                    let s: LitStr = input.parse()?;
                    scene = Some(s.value());
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown validate_scene! field `{other}` (expected manifests/scene_path/scene)"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            manifests: manifests.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "missing `manifests:` field")
            })?,
            scene_path: scene_path.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "missing `scene_path:` field")
            })?,
            scene: scene.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "missing `scene:` field")
            })?,
        })
    }
}

// ───────────────────────────────────────────────────────────────────
// Duplicated-slice view-type table (see crate docs — cycle avoidance)
// ───────────────────────────────────────────────────────────────────

/// Entry in the internal view-type table.
struct ViewEntry {
    ext_name: String,
    decl: ViewDecl,
}

fn declared_kind(v: &ViewDecl) -> &str {
    v.kind.as_ref().map(|k| k.value.as_str()).unwrap_or("pane")
}

/// Parse a manifest KDL blob with the `kdl` crate directly (bypassing
/// facet-kdl). facet-kdl 0.42's bare-`item` Vec rendering makes
/// cross-field disambiguation unreliable for sibling `Vec<T>`
/// collections — we only need `extension { name "…"; views { view
/// "name" { component "…"; kind "…" } } }`, so a hand-walk of the KDL
/// tree is simpler and more robust.
///
/// Expected shape:
/// ```kdl
/// extension {
///     name "ext-name"
///     views {
///         view "ViewName" {
///             component "ComponentC"
///             kind "pane"   // or "stack" — optional
///         }
///     }
/// }
/// ```
fn parse_manifest(raw: &str) -> Result<ParsedManifest, String> {
    let doc = kdl::KdlDocument::parse(raw)
        .map_err(|e| format!("invalid KDL manifest: {e}"))?;

    let ext_node = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "extension")
        .ok_or_else(|| "manifest missing top-level `extension { … }` node".to_string())?;

    let ext_children = match ext_node.children() {
        Some(c) => c,
        None => return Err("manifest `extension` node has no body".to_string()),
    };

    let ext_name = ext_children
        .nodes()
        .iter()
        .find(|n| n.name().value() == "name")
        .and_then(|n| n.entries().iter().find_map(|e| e.value().as_string()))
        .ok_or_else(|| "manifest `extension` block missing `name \"…\"` child".to_string())?
        .to_string();

    // Parse declared intents — T-042 R6 (decision #2). Each
    // `intents { intent "verb" { ... } }` child contributes to the
    // intent symbol table. Unprefixed names are qualified under the
    // extension name (matching scene `ExtensionRegistry` behaviour).
    let mut intents: Vec<String> = Vec::new();
    if let Some(intents_node) = ext_children
        .nodes()
        .iter()
        .find(|n| n.name().value() == "intents")
    {
        if let Some(intents_children) = intents_node.children() {
            for intent in intents_children.nodes() {
                if intent.name().value() != "intent" {
                    continue;
                }
                let raw_name = intent
                    .entries()
                    .iter()
                    .find_map(|e| e.value().as_string())
                    .ok_or_else(|| {
                        "`intent` node missing positional string arg (intent name)".to_string()
                    })?;
                let fq = if raw_name.contains('.') {
                    raw_name.to_string()
                } else {
                    format!("{ext_name}.{raw_name}")
                };
                intents.push(fq);
            }
        }
    }

    let mut views: Vec<ViewDecl> = Vec::new();
    if let Some(views_node) = ext_children
        .nodes()
        .iter()
        .find(|n| n.name().value() == "views")
    {
        if let Some(views_children) = views_node.children() {
            for view in views_children.nodes() {
                if view.name().value() != "view" {
                    continue;
                }
                let view_name = view
                    .entries()
                    .iter()
                    .find_map(|e| e.value().as_string())
                    .ok_or_else(|| {
                        "`view` node missing positional string arg (view name)".to_string()
                    })?
                    .to_string();
                let (mut component, mut kind) = (None::<String>, None::<String>);
                if let Some(view_children) = view.children() {
                    for child in view_children.nodes() {
                        match child.name().value() {
                            "component" => {
                                component = child
                                    .entries()
                                    .iter()
                                    .find_map(|e| e.value().as_string().map(str::to_string));
                            }
                            "kind" => {
                                kind = child
                                    .entries()
                                    .iter()
                                    .find_map(|e| e.value().as_string().map(str::to_string));
                            }
                            _ => {}
                        }
                    }
                }
                views.push(ViewDecl {
                    name: view_name,
                    component: StringNode::new(component.unwrap_or_default()),
                    kind: kind.map(StringNode::new),
                });
            }
        }
    }

    Ok(ParsedManifest {
        ext_name,
        views,
        intents,
    })
}

/// Parsed manifest snapshot produced by [`parse_manifest`].
struct ParsedManifest {
    ext_name: String,
    views: Vec<ViewDecl>,
    /// Fully-qualified intent names declared by this manifest
    /// (T-042 R6 intent symbol table).
    intents: Vec<String>,
}

/// Build the internal `<ext>.<view> -> ViewEntry` table from a list of
/// parsed manifests. Mirrors `ark-scene`'s
/// `ViewTypeTable::from_manifests` for the fields the macro exercises.
fn build_table(
    manifests: &[ParsedManifest],
) -> std::collections::BTreeMap<String, ViewEntry> {
    let mut entries = std::collections::BTreeMap::new();
    for m in manifests {
        for view in &m.views {
            let token = format!("{}.{}", m.ext_name, view.name);
            entries.insert(
                token,
                ViewEntry {
                    ext_name: m.ext_name.clone(),
                    decl: view.clone(),
                },
            );
        }
    }
    entries
}

/// Build the fully-qualified intent symbol table from the manifest set.
/// Core ops (`ark.core.*`) are always treated as declared; the table
/// tracks only extension-contributed intents (T-042 R6).
fn build_intent_table(
    manifests: &[ParsedManifest],
) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    for m in manifests {
        for intent_fqn in &m.intents {
            set.insert(intent_fqn.clone());
        }
    }
    set
}

// ───────────────────────────────────────────────────────────────────
// Validator entry point
// ───────────────────────────────────────────────────────────────────

fn run(args: ValidateSceneArgs) -> Result<(), String> {
    // Parse manifests. Each inline string is expected to be a KDL blob
    // of the shape `extension { name "…"; intents { intent "…" { … } } views { view "…" { … } } }`.
    let mut parsed_manifests: Vec<ParsedManifest> = Vec::new();
    for (i, raw) in args.manifests.iter().enumerate() {
        let parsed = parse_manifest(raw).map_err(|e| {
            format!(
                "{}:0:0: manifest #{} failed to parse: {}",
                args.scene_path, i, e
            )
        })?;
        parsed_manifests.push(parsed);
    }
    let table = build_table(&parsed_manifests);
    let intent_table = build_intent_table(&parsed_manifests);

    // Parse scene KDL via the `kdl` crate so we keep span info.
    let doc = kdl::KdlDocument::parse(&args.scene).map_err(|e| {
        format!(
            "{}:0:0: scene failed to parse as KDL: {}",
            args.scene_path, e
        )
    })?;

    // Collect handles declared by pane/stack nodes (maps handle-name →
    // declared kind "pane"|"stack") before walking validation nodes.
    // Handles in this mini-grammar are the `@name` entry that appears
    // as an entry on `pane`/`stack` nodes.
    let mut handle_kinds: std::collections::BTreeMap<String, (&'static str, (usize, usize))> =
        std::collections::BTreeMap::new();
    for node in walk_nodes(&doc) {
        let kind = match node.name().value() {
            "pane" => "pane",
            "stack" => "stack",
            _ => continue,
        };
        if let Some(handle_entry) = node.entries().iter().find(|e| {
            e.value()
                .as_string()
                .map(|s| s.starts_with('@'))
                .unwrap_or(false)
        }) {
            let handle = handle_entry.value().as_string().unwrap().to_string();
            let loc = offset_to_line_col(&args.scene, node.name().span().offset());
            handle_kinds.insert(handle, (kind, loc));
        }
    }

    // Walk every node, dispatch per-kind validation.
    for node in walk_nodes(&doc) {
        match node.name().value() {
            "pane" | "stack" => check_pane_or_stack(node, &args, &table)?,
            "spawn_into" => check_spawn_into(node, &args, &handle_kinds)?,
            "handle_attr" => check_handle_attr(node, &args, &table)?,
            "intent" => check_intent(node, &args, &intent_table)?,
            _ => {}
        }
    }

    Ok(())
}

/// Iterator over every node in a KDL document (depth-first).
fn walk_nodes(doc: &kdl::KdlDocument) -> Vec<&kdl::KdlNode> {
    let mut out = Vec::new();
    fn recur<'a>(doc: &'a kdl::KdlDocument, out: &mut Vec<&'a kdl::KdlNode>) {
        for node in doc.nodes() {
            out.push(node);
            if let Some(children) = node.children() {
                recur(children, out);
            }
        }
    }
    recur(doc, &mut out);
    out
}

/// Case (1) + (2): pane/stack referencing a view-type token.
///
/// Expects either a positional first string arg OR a `view_type "tok"`
/// child — whichever is present identifies the referenced view-type.
fn check_pane_or_stack(
    node: &kdl::KdlNode,
    args: &ValidateSceneArgs,
    table: &std::collections::BTreeMap<String, ViewEntry>,
) -> Result<(), String> {
    let ctx_kind = node.name().value(); // "pane" or "stack"

    // Look for explicit `view_type "tok"` child first; fall back to the
    // first positional string entry that isn't a `@handle`.
    let (token, span_offset): (String, usize) = if let Some(child) = node
        .children()
        .and_then(|c| c.nodes().iter().find(|n| n.name().value() == "view_type"))
    {
        match child
            .entries()
            .iter()
            .find_map(|e| e.value().as_string().map(|s| (s.to_string(), child.name().span().offset())))
        {
            Some(v) => v,
            None => return Ok(()),
        }
    } else {
        match node.entries().iter().find_map(|e| {
            let s = e.value().as_string()?;
            if s.starts_with('@') {
                None
            } else {
                Some((s.to_string(), node.name().span().offset()))
            }
        }) {
            Some(v) => v,
            None => return Ok(()),
        }
    };

    let (line, col) = offset_to_line_col(&args.scene, span_offset);

    let Some(entry) = table.get(&token) else {
        return Err(format!(
            "{}:{}:{}: unknown view type `{}` — no installed extension declares this view",
            args.scene_path, line, col, token
        ));
    };
    let declared = declared_kind(&entry.decl);
    if declared != ctx_kind {
        return Err(format!(
            "{}:{}:{}: view type `{}` declared by extension `{}` with kind=`{}` but used in a `{}` context (kind=`{}`)",
            args.scene_path,
            line,
            col,
            token,
            entry.ext_name,
            declared,
            ctx_kind,
            ctx_kind
        ));
    }
    Ok(())
}

/// Case (3): `spawn_into @parent { … }` — the parent handle MUST
/// resolve to a stack node.
fn check_spawn_into(
    node: &kdl::KdlNode,
    args: &ValidateSceneArgs,
    handle_kinds: &std::collections::BTreeMap<String, (&'static str, (usize, usize))>,
) -> Result<(), String> {
    let parent_handle = node
        .entries()
        .iter()
        .find_map(|e| e.value().as_string().map(|s| s.to_string()));
    let Some(parent) = parent_handle else {
        return Ok(());
    };
    let offset = node.name().span().offset();
    let (line, col) = offset_to_line_col(&args.scene, offset);

    let Some((kind, _parent_loc)) = handle_kinds.get(&parent) else {
        return Err(format!(
            "{}:{}:{}: spawn_into references undeclared handle `{}` — no pane/stack node declares it",
            args.scene_path, line, col, parent
        ));
    };
    if *kind != "stack" {
        return Err(format!(
            "{}:{}:{}: spawn_into target `{}` resolves to a `Pane<V>` handle but a `Stack<V>` parent is required — stack children must be nested under a stack",
            args.scene_path, line, col, parent
        ));
    }
    Ok(())
}

/// Case (4): `handle_attr @h "<view-token>" value="<lit>"` — the value
/// property MUST itself be a handle reference (starting with `@`). A
/// plain string literal is rejected because the manifest-declared attr
/// expects `Pane<V>`.
fn check_handle_attr(
    node: &kdl::KdlNode,
    args: &ValidateSceneArgs,
    _table: &std::collections::BTreeMap<String, ViewEntry>,
) -> Result<(), String> {
    // Find the `value="..."` property.
    let value_prop = node
        .entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some("value"));
    let Some(value) = value_prop else {
        return Ok(());
    };
    let Some(v) = value.value().as_string() else {
        return Ok(());
    };
    if !v.starts_with('@') {
        let offset = value.span().offset();
        let (line, col) = offset_to_line_col(&args.scene, offset);
        // Name the offending handle-kind (Pane<V>) in plain English.
        return Err(format!(
            "{}:{}:{}: handle_attr `value` expects a `Pane<V>` handle reference (written as `@handle`) but got the string literal `{}` — typed-handle attributes cannot be bound to non-handle values",
            args.scene_path, line, col, v
        ));
    }
    Ok(())
}

/// Case (5) T-042 R6: `intent "ext.verb" [args...]` — the first
/// positional string arg is the fully-qualified intent name and MUST
/// be either a core op (`ark.core.*`) OR declared by one of the
/// loaded manifests. Per decision #2, the manifest is the SOLE source
/// of intent registration in v0.1.
fn check_intent(
    node: &kdl::KdlNode,
    args: &ValidateSceneArgs,
    intent_table: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let Some(name_entry) = node.entries().iter().find(|e| e.name().is_none()) else {
        // No positional arg -> nothing to validate.
        return Ok(());
    };
    let Some(name) = name_entry.value().as_string() else {
        return Ok(());
    };
    // `ark.core.*` are always in scope.
    if name.starts_with("ark.core.") || intent_table.contains(name) {
        return Ok(());
    }
    let offset = name_entry.span().offset();
    let (line, col) = offset_to_line_col(&args.scene, offset);
    Err(format!(
        "{}:{}:{}: unknown intent `{}` — no loaded extension manifest declares it (per decision #2 the manifest is the sole source of intent registration)",
        args.scene_path, line, col, name
    ))
}

/// Convert a byte offset into 1-indexed (line, column).
fn offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
