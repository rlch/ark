//! Proc-macro crate for `#[derive(Extension)]` (T-089),
//! `#[derive(View)]` (T-090), and `#[ark_intent]` (T-092).
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
            let value = extract_str_lit(&meta)?;
            match ident.to_string().as_str() {
                "name" => name = Some(value),
                "version" => version = Some(value),
                "description" => description = Some(value),
                "ark_range" => ark_range = Some(value),
                other => {
                    return Err(meta.error(format!(
                        "unknown extension attribute `{other}`; \
                         expected one of: name, version, description, ark_range"
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
// #[derive(View)] — T-090
// =========================================================================

/// Derive macro that generates `inventory::submit!(ViewRegistration { … })`
/// from `#[view(…)]` attributes.
///
/// # Required fields
///
/// - `name` — view identifier used in scene source (e.g. `"edit"`,
///   `"git-status"`).
///
/// # Optional fields
///
/// - `description` — human-readable description shown in `ark ext info`.
///
/// # Example
///
/// ```ignore
/// use ark_ext_derive::View;
///
/// #[derive(View)]
/// #[view(name = "edit")]
/// struct EditView;
///
/// #[derive(View)]
/// #[view(name = "git-status", description = "Git status sidebar")]
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
///         name: "edit",
///         component: "EditView",
///         description: "",
///         module_path: module_path!(),
///     }
/// }
/// ```
#[proc_macro_derive(View, attributes(view))]
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

    // Walk `#[view(…)]` attributes on the struct.
    for attr in &input.attrs {
        if !attr.path().is_ident("view") {
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
                        "unknown view attribute `{other}`; \
                         expected one of: name, description"
                    )));
                }
            }
            Ok(())
        })?;
    }

    let name = name.ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "missing required `name` in #[view(…)]")
    })?;
    let description = description.unwrap_or_default();
    let component = input.ident.to_string();

    Ok(quote! {
        ::inventory::submit! {
            ::ark_ext_metadata_types::ViewRegistration {
                name: #name,
                component: #component,
                description: #description,
                module_path: ::core::module_path!(),
            }
        }
    })
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
