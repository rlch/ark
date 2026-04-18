//! Proc-macro crate for `#[derive(Extension)]` (T-089),
//! `#[derive(View)]` (T-090), `#[derive(CommandView)]` /
//! `#[derive(ZellijView)]` marker derives (T-026), and
//! `#[ark_intent]` (T-092).
//!
//! Generates `inventory::submit!` blocks that register
//! [`ark_ext_metadata_types::ExtensionMeta`] and
//! [`ark_ext_metadata_types::ViewRegistration`] values from
//! `#[extension(…)]` / `#[view(…)]` attributes on structs.
//!
//! # One crate = one extension
//!
//! The derive stamps `module_path!()` into the registration so the
//! scene compiler can group all metadata submitted from the same crate
//! into a single logical extension. Extension authors place one
//! `#[derive(Extension)]` struct per crate; the derive enforces this
//! convention at the type level (each struct gets its own registration).
//!
//! # Example
//!
//! ```ignore
//! use ark_ext_derive::Extension;
//!
//! #[derive(Extension)]
//! #[extension(name = "my-ext", version = "0.1.0")]
//! struct MyExt;
//!
//! // Optional fields:
//! #[derive(Extension)]
//! #[extension(
//!     name = "my-ext",
//!     version = "0.1.0",
//!     description = "A cool extension",
//!     ark_range = ">=0.1, <1.0",
//! )]
//! struct MyExtFull;
//! ```
//!
//! # Generated code
//!
//! The macro expands roughly to:
//!
//! ```ignore
//! inventory::submit! {
//!     ark_ext_metadata_types::ExtensionMeta {
//!         name: "my-ext",
//!         version: "0.1.0",
//!         description: "",
//!         ark_range: "",
//!         module_path: module_path!(),
//!     }
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{DeriveInput, Expr, ExprLit, Lit, Token, parse_macro_input};

/// Derive macro that generates `inventory::submit!(ExtensionMeta { … })`
/// from `#[extension(…)]` attributes.
///
/// # Required fields
///
/// - `name` — extension identifier (must match the search-path directory
///   name per R10).
/// - `version` — semver version of the extension.
///
/// # Optional fields
///
/// - `description` — human-readable description shown in `ark ext list`.
/// - `ark_range` — semver range of supported ark protocol versions
///   (e.g. `">=0.1, <1.0"`). Empty string = "no constraint".
/// - `capabilities` — comma-separated capability-flag list (T-027).
///   Stamps a hidden `Self::ARK_CAPABILITIES: &'static [&'static str]`
///   associated constant on the annotated type, which host-dispatch
///   can surface into the ext's manifest at load time. Example:
///   `#[extension(name = "…", version = "…", capabilities =
///   "view.pane.v1,ext.lifecycle.v1")]`. The derive does NOT validate
///   the flag values — `ark-ext-derive` has no dep on `ark-ext-proto`
///   by design; the author-maintained `extension.kdl` remains the
///   authoritative surface, and the derive is a convenience per kit
///   R7 ("derive is a convenience, not a gate").
#[proc_macro_derive(Extension, attributes(extension))]
pub fn derive_extension(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_extension_inner(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Inner implementation — returns `syn::Result` so `?` works naturally.
fn derive_extension_inner(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut description: Option<String> = None;
    let mut ark_range: Option<String> = None;
    // T-027: capability-flag advertisement. Empty list => no flags.
    let mut capabilities: Vec<String> = Vec::new();

    // Walk `#[extension(…)]` attributes on the struct.
    for attr in &input.attrs {
        if !attr.path().is_ident("extension") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let ident = meta
                .path
                .get_ident()
                .ok_or_else(|| meta.error("expected identifier"))?;
            match ident.to_string().as_str() {
                "name" => name = Some(extract_str_lit(&meta)?),
                "version" => version = Some(extract_str_lit(&meta)?),
                "description" => description = Some(extract_str_lit(&meta)?),
                "ark_range" => ark_range = Some(extract_str_lit(&meta)?),
                // `capabilities = "flag1,flag2,..."` — comma-separated
                // list of capability-flag strings (T-027). The derive
                // doesn't validate the tokens against
                // `PHASE_2_CAPABILITY_FLAGS` (ark-ext-derive has no dep
                // on ark-ext-proto by design — extensions in downstream
                // crates own the taxonomy in their manifests). Empty
                // entries are filtered. Whitespace is trimmed.
                "capabilities" => {
                    let raw = extract_str_lit(&meta)?;
                    capabilities = raw
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                other => {
                    return Err(meta.error(format!(
                        "unknown extension attribute `{other}`; \
                         expected one of: name, version, description, ark_range, capabilities"
                    )));
                }
            }
            Ok(())
        })?;
    }

    let name = name.ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "missing required `name` in #[extension(…)]")
    })?;
    let version = version.ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "missing required `version` in #[extension(…)]")
    })?;
    let description = description.unwrap_or_default();
    let ark_range = ark_range.unwrap_or_default();

    let struct_ident = &input.ident;

    // T-027: stamp the capability list as a hidden inherent const on
    // the annotated type. The scene compiler / host-dispatch loader
    // (T-030 and beyond) can surface this via type-name lookup. We
    // avoid submitting into a new `inventory` collection because that
    // would require adding a record to `ark-ext-metadata-types`, which
    // this task is not allowed to touch. The const form is a minimal
    // opt-in: if the author writes
    // `#[extension(capabilities = "view.pane.v1")]`, the const is
    // populated; if they omit it, the const holds an empty slice.
    //
    // NOTE (R7 caveat): proc macros cannot see sibling `impl` blocks
    // to auto-detect overridden `ArkExtension` methods. The kit says
    // "derive is a convenience, not a gate" — the author writes the
    // flag explicitly in `extension.kdl` (or passes `capabilities =
    // "..."` here) when auto-detection is impractical. This path
    // implements the convenience surface; cross-derive inventory
    // scanning is NOT viable because `inventory::iter` is a runtime
    // surface, not a macro-expansion one.
    let cap_literals: Vec<proc_macro2::TokenStream> = capabilities
        .iter()
        .map(|c| quote! { #c })
        .collect();

    Ok(quote! {
        ::inventory::submit! {
            ::ark_ext_metadata_types::ExtensionMeta {
                name: #name,
                version: #version,
                description: #description,
                ark_range: #ark_range,
                module_path: ::core::module_path!(),
            }
        }

        #[doc(hidden)]
        #[allow(dead_code, non_upper_case_globals)]
        impl #struct_ident {
            /// Capability flags advertised by this extension (T-027).
            ///
            /// Populated from `#[extension(capabilities = "...")]`.
            /// Empty slice when the attribute is omitted. Host-dispatch
            /// reads this to build `ExtensionMetadata.capabilities` at
            /// load time without requiring the author to maintain
            /// `extension.kdl` by hand for simple cases.
            pub const ARK_CAPABILITIES: &'static [&'static str] = &[
                #( #cap_literals ),*
            ];
        }
    })
}

/// Extract a string literal from a `key = "value"` meta item.
fn extract_str_lit(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let expr: Expr = meta.value()?.parse()?;
    if let Expr::Lit(ExprLit {
        lit: Lit::Str(lit), ..
    }) = &expr
    {
        Ok(lit.value())
    } else {
        Err(meta.error("expected string literal"))
    }
}

// =========================================================================
// #[derive(View)] — T-025 (phase-2 ext-surface R7)
// =========================================================================

/// Derive macro that generates `inventory::submit!(ViewRegistration { … })`
/// from `#[ark_view(…)]` attributes.
///
/// # View name derivation
///
/// By default the struct name is converted from `PascalCase` to
/// `kebab-case` (e.g. `MyPanel` → `"my-panel"`, `GitStatus` →
/// `"git-status"`). An explicit name overrides this:
///
/// ```ignore
/// #[derive(View)]
/// #[ark_view(name = "custom-name")]
/// struct SomeView;
/// ```
///
/// This mirrors the name-derivation convention used by the
/// [`macro@ark_intent`] attribute so the extension surface reads the
/// same whether a developer is writing an intent handler or declaring a
/// view.
///
/// # Optional fields
///
/// - `name` — view identifier used in scene source. Defaults to the
///   struct name converted from PascalCase to kebab-case.
/// - `description` — human-readable description shown in `ark ext info`.
///
/// # Kind discriminant
///
/// The v1 derive stamps pane-kind views only. `#[derive(CommandView)]`
/// / `#[derive(ZellijView)]` marker derives (T-026, PATH A — body-less)
/// emit `impl CommandView for T {}` / `impl ZellijView for T {}`
/// respectively. They do NOT currently coordinate with
/// `#[derive(View)]` to stamp a `kind = HandleKind::Pane` discriminant
/// on the submitted record, because proc macros have no cross-derive
/// visibility at macro-expansion time. When `ViewRegistration` in
/// `ark-ext-metadata-types` grows a `kind` field (separate infra
/// work), the `#[derive(View)]` codegen above can be extended to
/// accept `#[ark_view(kind = "pane"|"stack")]` and route it through.
///
/// # Auto-`impl View`
///
/// `#[derive(View)]` also emits `impl ::ark_view::View for T {}` so
/// the marker derives above (which require the `View` supertrait) can
/// compose without hand-written impls. If the struct already has a
/// manual `impl View for T {}` the derive's emitted impl will collide
/// at compile time — pick one or the other.
///
/// # Example
///
/// ```ignore
/// use ark_ext_derive::View;
///
/// // Name auto-derived from struct: "my-panel".
/// #[derive(View)]
/// struct MyPanel;
///
/// // Explicit override.
/// #[derive(View)]
/// #[ark_view(name = "git-status", description = "Git status sidebar")]
/// struct GitStatusView;
/// ```
///
/// # Generated code
///
/// The macro expands roughly to:
///
/// ```ignore
/// inventory::submit! {
///     ark_ext_metadata_types::ViewRegistration {
///         name: "my-panel",
///         component: "MyPanel",
///         description: "",
///         module_path: module_path!(),
///     }
/// }
/// ```
#[proc_macro_derive(View, attributes(ark_view))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_view_inner(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Inner implementation for `#[derive(View)]`.
fn derive_view_inner(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    // Walk `#[ark_view(…)]` attributes on the struct.
    for attr in &input.attrs {
        if !attr.path().is_ident("ark_view") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let ident = meta
                .path
                .get_ident()
                .ok_or_else(|| meta.error("expected identifier"))?;
            let value = extract_str_lit(&meta)?;
            match ident.to_string().as_str() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                other => {
                    return Err(meta.error(format!(
                        "unknown ark_view attribute `{other}`; \
                         expected one of: name, description"
                    )));
                }
            }
            Ok(())
        })?;
    }

    let component = input.ident.to_string();
    let struct_ident = &input.ident;
    // Auto-derive name from struct when `#[ark_view(name = "…")]` omitted.
    // PascalCase → kebab-case (mirrors snake-to-kebab convention used by
    // `#[ark_intent]` for method identifiers, only adapted for struct
    // identifiers that carry uppercase word-boundaries).
    let view_name = name.unwrap_or_else(|| to_kebab_case(&component));
    let description = description.unwrap_or_default();

    // Also emit `impl ::ark_view::View for T {}` so the marker derives
    // (`#[derive(CommandView)]`, `#[derive(ZellijView)]`, and any
    // downstream refinement trait `X: View`) find their required
    // `View` supertrait without forcing the author to hand-write it.
    // This mirrors the convention of the ark-view kit R3 where `View`
    // is a pure marker — no method bodies to fill, just a type-level
    // classification.
    Ok(quote! {
        ::inventory::submit! {
            ::ark_ext_metadata_types::ViewRegistration {
                name: #view_name,
                component: #component,
                description: #description,
                module_path: ::core::module_path!(),
            }
        }

        #[automatically_derived]
        impl ::ark_view::View for #struct_ident {}
    })
}

// =========================================================================
// #[derive(CommandView)] / #[derive(ZellijView)] — T-026
// (phase-2 ext-surface R7)
// =========================================================================

/// Marker-only derive that emits `impl ark_view::CommandView for T {}`.
///
/// # Composition with `#[derive(View)]`
///
/// `CommandView: View` — the supertrait bound must be satisfied. The
/// simplest path is to co-derive `View` and `CommandView` on the same
/// struct:
///
/// ```ignore
/// use ark_ext_derive::{CommandView, View};
///
/// #[derive(View, CommandView)]
/// struct MyCommand;
/// ```
///
/// `#[derive(View)]` emits `impl ::ark_view::View for T {}`;
/// `#[derive(CommandView)]` emits `impl ::ark_view::CommandView for T
/// {}`. Hand-rolled `impl View for T {}` works equally well — the
/// marker derive only cares that the supertrait is in scope.
///
/// # Scope (v1 = PATH A)
///
/// Per kit R7 + build-site T-026: this derive is body-less — it does
/// NOT coordinate with a sibling `#[derive(View)]` to stamp a `kind =
/// HandleKind::Pane` discriminant, because proc macros run in
/// isolation per-attribute (no cross-derive visibility at macro time).
/// When `ViewRegistration` grows a `kind` field (separate infra work)
/// this derive can be extended — the author can explicitly specify
/// kind via the existing `#[ark_view(...)]` attribute family.
#[proc_macro_derive(CommandView)]
pub fn derive_command_view(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    quote! {
        #[automatically_derived]
        impl ::ark_view::CommandView for #name {}
    }
    .into()
}

/// Marker-only derive that emits `impl ark_view::ZellijView for T {}`.
///
/// # Composition with `#[derive(View)]`
///
/// `ZellijView: View` — supertrait bound is satisfied by co-deriving
/// `View` or writing `impl View for T {}` by hand:
///
/// ```ignore
/// use ark_ext_derive::{View, ZellijView};
///
/// #[derive(View, ZellijView)]
/// struct MyPlugin;
/// ```
///
/// See [`macro@CommandView`] for the rationale behind the PATH A
/// body-less shape.
#[proc_macro_derive(ZellijView)]
pub fn derive_zellij_view(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    quote! {
        #[automatically_derived]
        impl ::ark_view::ZellijView for #name {}
    }
    .into()
}

/// Convert a PascalCase identifier to kebab-case.
///
/// Shares the word-boundary logic with [`to_snake_case`] and simply
/// joins with `-` instead of `_`. Examples:
/// - `MyPanel` → `my-panel`
/// - `HTTPRequest` → `http-request`
/// - `GitStatus` → `git-status`
/// - `A` → `a`
fn to_kebab_case(s: &str) -> String {
    to_snake_case(s).replace('_', "-")
}

// =========================================================================
// #[ark_intent] — T-092
// =========================================================================

/// Attribute macro that registers an intent handler method via
/// `inventory::submit!(IntentMeta { … })`.
///
/// Place on a method inside an `impl` block. The method body is
/// preserved unchanged; the macro only appends a registration block.
///
/// # Intent name derivation
///
/// By default the method name is converted from `snake_case` to
/// `kebab-case` (e.g. `fn open_file(…)` becomes intent `"open-file"`).
/// An explicit name overrides this:
///
/// ```ignore
/// #[ark_intent(name = "custom-name")]
/// fn my_handler(&self) { … }
/// ```
///
/// # Scope
///
/// v1 always registers [`IntentScope::Global`]. Location-based scope
/// detection (`impl ExtStruct` = global, `impl ViewStruct` = targeted)
/// is deferred to a follow-up task.
///
/// # Generated code
///
/// ```ignore
/// fn open_file(&self) { /* original body */ }
///
/// inventory::submit! {
///     ark_ext_metadata_types::IntentMeta {
///         name: "open-file",
///         module_path: module_path!(),
///         scope: ark_ext_metadata_types::IntentScope::Global,
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn ark_intent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as IntentArgs);
    let item_fn = parse_macro_input!(item as syn::ItemFn);
    match ark_intent_inner(&args, &item_fn) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Parsed arguments from `#[ark_intent(…)]`.
struct IntentArgs {
    /// Explicit name override, if provided.
    name: Option<String>,
}

impl Parse for IntentArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(IntentArgs { name: None });
        }

        let mut name = None;
        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            match ident.to_string().as_str() {
                "name" => {
                    let _eq: Token![=] = input.parse()?;
                    let lit: syn::LitStr = input.parse()?;
                    name = Some(lit.value());
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown ark_intent attribute `{other}`; expected: name"
                        ),
                    ));
                }
            }
            // Consume optional trailing comma.
            if input.peek(Token![,]) {
                let _comma: Token![,] = input.parse()?;
            }
        }

        Ok(IntentArgs { name })
    }
}

/// Convert a `snake_case` identifier to `kebab-case`.
fn snake_to_kebab(s: &str) -> String {
    s.replace('_', "-")
}

/// Inner implementation for `#[ark_intent]`.
fn ark_intent_inner(
    args: &IntentArgs,
    item_fn: &syn::ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let intent_name = match &args.name {
        Some(n) => n.clone(),
        None => snake_to_kebab(&item_fn.sig.ident.to_string()),
    };

    Ok(quote! {
        #item_fn

        ::inventory::submit! {
            ::ark_ext_metadata_types::IntentMeta {
                name: #intent_name,
                module_path: ::core::module_path!(),
                scope: ::ark_ext_metadata_types::IntentScope::Global,
            }
        }
    })
}

// =========================================================================
// #[derive(Event)] — T-091
// =========================================================================

/// Derive macro that generates `inventory::submit!(EventMeta { … })`
/// from a struct definition.
///
/// # Event name
///
/// By default the event name is derived from the struct name via
/// snake_case conversion (e.g. `FileEdited` → `file_edited`). Override
/// with `#[event(name = "custom-name")]`.
///
/// # Example
///
/// ```ignore
/// use ark_ext_derive::Event;
///
/// #[derive(Event)]
/// struct FileEdited {
///     path: String,
/// }
///
/// // With custom name:
/// #[derive(Event)]
/// #[event(name = "buffer-saved")]
/// struct SaveEvent;
/// ```
///
/// # Generated code
///
/// The macro expands roughly to:
///
/// ```ignore
/// inventory::submit! {
///     ark_ext_metadata_types::EventMeta {
///         name: "file_edited",
///         payload_type: core::any::type_name::<FileEdited>(),
///         module_path: module_path!(),
///     }
/// }
/// ```
#[proc_macro_derive(Event, attributes(event))]
pub fn derive_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_event_inner(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Inner implementation for `#[derive(Event)]`.
fn derive_event_inner(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_ident = &input.ident;
    let mut custom_name: Option<String> = None;

    // Walk `#[event(…)]` attributes on the struct.
    for attr in &input.attrs {
        if !attr.path().is_ident("event") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let ident = meta
                .path
                .get_ident()
                .ok_or_else(|| meta.error("expected identifier"))?;
            let value = extract_str_lit(&meta)?;
            match ident.to_string().as_str() {
                "name" => custom_name = Some(value),
                other => {
                    return Err(meta.error(format!(
                        "unknown event attribute `{other}`; expected: name"
                    )));
                }
            }
            Ok(())
        })?;
    }

    let event_name = match custom_name {
        Some(n) => n,
        None => to_snake_case(&struct_ident.to_string()),
    };

    Ok(quote! {
        ::inventory::submit! {
            ::ark_ext_metadata_types::EventMeta {
                name: #event_name,
                payload_type: ::core::any::type_name::<#struct_ident>(),
                module_path: ::core::module_path!(),
            }
        }
    })
}

/// Convert a PascalCase identifier to snake_case.
///
/// Splits on uppercase boundaries: each uppercase letter that follows a
/// lowercase letter (or precedes a lowercase letter in an acronym run)
/// starts a new segment. All segments are lowercased and joined with `_`.
///
/// Examples:
/// - `FileEdited` → `file_edited`
/// - `HTTPRequest` → `http_request`
/// - `MyXMLParser` → `my_xml_parser`
/// - `A` → `a`
fn to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                let prev = chars[i - 1];
                // Insert underscore before an uppercase letter if:
                // 1. Previous char is lowercase (e.g. "fileE" → "file_e")
                // 2. Previous char is uppercase AND next char is lowercase
                //    (e.g. "HTTPReq" → "HTTP_Req" → "http_req")
                if prev.is_lowercase()
                    || (prev.is_uppercase()
                        && i + 1 < chars.len()
                        && chars[i + 1].is_lowercase())
                {
                    result.push('_');
                }
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}
