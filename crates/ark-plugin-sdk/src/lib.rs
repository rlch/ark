//! `ark-plugin-sdk` — the single proc-macro crate plugin authors depend on.
//!
//! Implements `#[derive(Plugin)]` — parses a `#[plugin(...)]` attribute
//! block on a unit struct and emits two `#[link_section]` statics:
//!
//! * `ark-caps:v1` — postcard-encoded [`CapsManifest`] (R3).
//! * `ark-meta:v1` — postcard-encoded [`MetaManifest`] (R9, R14).
//!
//! Both payloads are computed at **macro-expansion time** by calling
//! `postcard::to_allocvec` inside the proc-macro and emitted into the
//! consumer crate as raw `&[u8; N]` byte-array literals. The generated
//! code therefore references neither `ark_plugin_protocol` nor
//! `postcard` from the guest — proc-macro deps are host-side only (R12
//! regression: `cargo tree -p echo --target wasm32-wasip2` must not
//! include `syn`/`quote`/`postcard`).
//!
//! ## Attribute surface
//!
//! ```ignore
//! use ark_plugin_sdk::Plugin;
//!
//! #[derive(Plugin)]
//! #[plugin(
//!     name = "echo",
//!     version = "0.1.0",
//!     // abi defaults to ARK_ABI_VERSION — set explicitly to pin:
//!     // abi = 1,
//!     // wit path defaults to "wit/plugin.wit" — set explicitly to override:
//!     // wit = "wit/echo.wit",
//!     capabilities(
//!         fs_read(display = "Read files", reason = "needed to load demo"),
//!         network(display = "Network",   reason = "outbound HTTP"),
//!     ),
//! )]
//! struct Echo;
//! ```
//!
//! Cap ids are bare snake_case idents converted to kebab-case for the
//! `CapDecl.id` string (`fs_read` → `"fs-read"`). Explicit `id = "..."`
//! is also accepted for caps whose ark-side name doesn't map 1:1.
//!
//! ## Compile-time checks
//!
//! * `name` matches `^[a-z][a-z0-9_]*$` (R9).
//! * `version` parses as semver 2.0.0 via the `semver` crate (R9).
//! * `abi` equals [`ark_types::ARK_ABI_VERSION`] (R14).
//! * `wit` file exists at the given path (relative to `CARGO_MANIFEST_DIR`)
//!   and declares exactly one `world <name>` matching the plugin `name` (R9).
//!
//! All failures surface as `proc_macro::Diagnostic`-equivalent
//! `compile_error!` invocations with span-precise pointers.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Error, Expr, ExprLit, Lit, Meta, Token};
use syn::punctuated::Punctuated;

use ark_plugin_protocol::{
    CapDecl, CapsManifest, MetaManifest, CAPS_SECTION_NAME, META_SECTION_NAME,
};
use ark_types::ARK_ABI_VERSION;

/// `#[derive(Plugin)]` — emits the `ark-caps:v1` + `ark-meta:v1` custom
/// sections on a unit struct via `#[link_section]` statics.
///
/// See the crate-level doc for the full attribute surface. Any parse or
/// validation failure produces a span-precise `compile_error!` pointing
/// at the offending attribute — nothing ever reaches the runtime.
#[proc_macro_derive(Plugin, attributes(plugin))]
pub fn derive_plugin(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------
// Parsing — structured view of the `#[plugin(...)]` attribute block.
// ---------------------------------------------------------------------

/// Fully-parsed `#[plugin(...)]` attribute contents.
struct PluginArgs {
    name: (String, Span),
    version: (String, Span),
    abi: (u32, Span),
    /// Relative-to-CARGO_MANIFEST_DIR path of the crate's WIT world file.
    /// Defaults to `wit/plugin.wit`.
    wit_path: (String, Span),
    caps: Vec<ParsedCap>,
    /// Span of the whole `#[plugin(...)]` attribute for diagnostics that
    /// don't have a more specific site.
    attr_span: Span,
}

struct ParsedCap {
    id: String,
    display_name: String,
    reason: String,
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let args = parse_plugin_attr(input)?;
    validate_name(&args.name.0, args.name.1)?;
    validate_semver(&args.version.0, args.version.1)?;
    validate_abi(args.abi.0, args.abi.1)?;
    validate_wit_world(&args.wit_path.0, args.wit_path.1, &args.name.0)?;

    let caps = CapsManifest {
        plugin_name: args.name.0.clone(),
        since_version: args.version.0.clone(),
        caps: args
            .caps
            .iter()
            .map(|c| CapDecl {
                id: c.id.clone(),
                display_name: c.display_name.clone(),
                reason: c.reason.clone(),
            })
            .collect(),
    };
    let meta = MetaManifest {
        name: args.name.0.clone(),
        version: args.version.0.clone(),
        ark_abi_version: args.abi.0,
    };

    // Compute postcard bytes at macro-expansion time — the generated
    // code never calls postcard at runtime (guest has no postcard dep).
    let caps_bytes = postcard::to_allocvec(&caps).map_err(|e| {
        Error::new(
            args.attr_span,
            format!("ark-plugin-sdk: failed to postcard-encode CapsManifest: {e}"),
        )
    })?;
    let meta_bytes = postcard::to_allocvec(&meta).map_err(|e| {
        Error::new(
            args.attr_span,
            format!("ark-plugin-sdk: failed to postcard-encode MetaManifest: {e}"),
        )
    })?;

    let caps_len = caps_bytes.len();
    let meta_len = meta_bytes.len();
    let caps_byte_literals = caps_bytes.iter().map(|b| quote! { #b });
    let meta_byte_literals = meta_bytes.iter().map(|b| quote! { #b });

    // Derive per-derive-target static names so two #[derive(Plugin)]
    // invocations in the same crate don't collide on the static name.
    // The target type's identifier is a unique per-item handle inside
    // the crate; uppercased + suffixed.
    let target_ident = &input.ident;
    let caps_static = syn::Ident::new(
        &format!("__ARK_CAPS_V1_{}", target_ident.to_string().to_uppercase()),
        target_ident.span(),
    );
    let meta_static = syn::Ident::new(
        &format!("__ARK_META_V1_{}", target_ident.to_string().to_uppercase()),
        target_ident.span(),
    );

    let caps_section = CAPS_SECTION_NAME;
    let meta_section = META_SECTION_NAME;

    let expanded = quote! {
        // `ark-caps:v1` custom section (R3).
        //
        // SAFETY: bytes are postcard-encoded at macro-expansion time by
        // `ark-plugin-sdk`; the host decodes via `wasmparser` +
        // `postcard::from_bytes` without ever executing guest code.
        #[link_section = #caps_section]
        #[used]
        #[doc(hidden)]
        pub static #caps_static: [u8; #caps_len] = [ #( #caps_byte_literals ),* ];

        // `ark-meta:v1` custom section (R9).
        #[link_section = #meta_section]
        #[used]
        #[doc(hidden)]
        pub static #meta_static: [u8; #meta_len] = [ #( #meta_byte_literals ),* ];
    };
    Ok(expanded)
}

// ---------------------------------------------------------------------
// Attribute parsing.
// ---------------------------------------------------------------------

fn parse_plugin_attr(input: &DeriveInput) -> syn::Result<PluginArgs> {
    let attr = input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("plugin"))
        .ok_or_else(|| {
            Error::new_spanned(
                input,
                "#[derive(Plugin)] requires a `#[plugin(...)]` attribute — see \
                 ark_plugin_sdk crate docs for the attribute surface",
            )
        })?;
    let attr_span = attr.path().span();

    let mut name: Option<(String, Span)> = None;
    let mut version: Option<(String, Span)> = None;
    let mut abi: Option<(u32, Span)> = None;
    let mut wit_path: Option<(String, Span)> = None;
    let mut caps: Vec<ParsedCap> = Vec::new();

    let nested: Punctuated<Meta, Token![,]> =
        attr.parse_args_with(Punctuated::parse_terminated)?;

    for item in nested {
        match item {
            Meta::NameValue(nv) if nv.path.is_ident("name") => {
                let s = lit_str(&nv.value, "name")?;
                name = Some((s, nv.value.span()));
            }
            Meta::NameValue(nv) if nv.path.is_ident("version") => {
                let s = lit_str(&nv.value, "version")?;
                version = Some((s, nv.value.span()));
            }
            Meta::NameValue(nv) if nv.path.is_ident("abi") => {
                let n = lit_u32(&nv.value, "abi")?;
                abi = Some((n, nv.value.span()));
            }
            Meta::NameValue(nv) if nv.path.is_ident("wit") => {
                let s = lit_str(&nv.value, "wit")?;
                wit_path = Some((s, nv.value.span()));
            }
            Meta::List(list) if list.path.is_ident("capabilities") => {
                let entries: Punctuated<Meta, Token![,]> =
                    list.parse_args_with(Punctuated::parse_terminated)?;
                for cap_meta in entries {
                    caps.push(parse_cap(&cap_meta)?);
                }
            }
            other => {
                return Err(Error::new_spanned(
                    other,
                    "unknown `#[plugin(...)]` key — expected one of `name`, `version`, \
                     `abi`, `wit`, `capabilities(...)`",
                ));
            }
        }
    }

    let name = name.ok_or_else(|| {
        Error::new(attr_span, "`#[plugin(...)]` is missing required `name = \"...\"`")
    })?;
    let version = version.ok_or_else(|| {
        Error::new(
            attr_span,
            "`#[plugin(...)]` is missing required `version = \"...\"`",
        )
    })?;
    let abi = abi.unwrap_or((ARK_ABI_VERSION, attr_span));
    let wit_path = wit_path.unwrap_or_else(|| ("wit/plugin.wit".to_string(), attr_span));

    Ok(PluginArgs {
        name,
        version,
        abi,
        wit_path,
        caps,
        attr_span,
    })
}

fn parse_cap(meta: &Meta) -> syn::Result<ParsedCap> {
    // Two accepted shapes:
    //
    //   fs_read(display = "Read files", reason = "…")
    //   some_cap(id = "fs-read", display = "…", reason = "…")
    //
    // The bare ident is the cap id (snake_case → kebab-case); the
    // `id = "..."` override lets authors opt out of the auto-convert
    // when the cap id contains digits or other exotica.
    let list = match meta {
        Meta::List(l) => l,
        other => {
            return Err(Error::new_spanned(
                other,
                "cap entry must be a list, e.g. `fs_read(display = \"Read files\", reason = \"…\")`",
            ));
        }
    };
    let cap_ident = list
        .path
        .get_ident()
        .ok_or_else(|| Error::new_spanned(&list.path, "cap entry must be a bare identifier"))?;
    let auto_id = cap_ident.to_string().replace('_', "-");

    let mut id: Option<String> = None;
    let mut display: Option<String> = None;
    let mut reason: Option<String> = None;

    let entries: Punctuated<Meta, Token![,]> =
        list.parse_args_with(Punctuated::parse_terminated)?;
    for entry in entries {
        match entry {
            Meta::NameValue(nv) if nv.path.is_ident("id") => {
                id = Some(lit_str(&nv.value, "id")?);
            }
            Meta::NameValue(nv) if nv.path.is_ident("display") => {
                display = Some(lit_str(&nv.value, "display")?);
            }
            Meta::NameValue(nv) if nv.path.is_ident("reason") => {
                reason = Some(lit_str(&nv.value, "reason")?);
            }
            other => {
                return Err(Error::new_spanned(
                    other,
                    "unknown cap field — expected `id`, `display`, or `reason`",
                ));
            }
        }
    }

    let id = id.unwrap_or(auto_id);
    let display_name = display.ok_or_else(|| {
        Error::new_spanned(
            list,
            "cap entry is missing required `display = \"...\"`",
        )
    })?;
    let reason = reason.ok_or_else(|| {
        Error::new_spanned(
            list,
            "cap entry is missing required `reason = \"...\"`",
        )
    })?;

    Ok(ParsedCap {
        id,
        display_name,
        reason,
    })
}

fn lit_str(expr: &Expr, field: &str) -> syn::Result<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.value()),
        other => Err(Error::new_spanned(
            other,
            format!("`{field}` must be a string literal"),
        )),
    }
}

fn lit_u32(expr: &Expr, field: &str) -> syn::Result<u32> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse::<u32>(),
        other => Err(Error::new_spanned(
            other,
            format!("`{field}` must be an integer literal"),
        )),
    }
}

// ---------------------------------------------------------------------
// Validators (R9 name regex, R9 semver, R14 abi, R9 wit-world name).
// ---------------------------------------------------------------------

fn validate_name(name: &str, span: Span) -> syn::Result<()> {
    // ^[a-z][a-z0-9_]*$ — mirrors ark-plugin-protocol::manifest's
    // `is_valid_plugin_name`.
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if name.is_empty() || !first_ok || !rest_ok {
        return Err(Error::new(
            span,
            format!(
                "`name = \"{name}\"` must match `^[a-z][a-z0-9_]*$` — lowercase letters, \
                 digits, and underscore only, starting with a letter"
            ),
        ));
    }
    Ok(())
}

fn validate_semver(version: &str, span: Span) -> syn::Result<()> {
    semver::Version::parse(version).map_err(|e| {
        Error::new(
            span,
            format!("`version = \"{version}\"` is not valid semver 2.0.0: {e}"),
        )
    })?;
    Ok(())
}

fn validate_abi(abi: u32, span: Span) -> syn::Result<()> {
    if abi != ARK_ABI_VERSION {
        return Err(Error::new(
            span,
            format!(
                "#[plugin(abi = {abi})] mismatches ark_types::ARK_ABI_VERSION = {ARK_ABI_VERSION} \
                 — re-derive with correct abi or update ark-types"
            ),
        ));
    }
    Ok(())
}

fn validate_wit_world(wit_path: &str, span: Span, plugin_name: &str) -> syn::Result<()> {
    // CARGO_MANIFEST_DIR inside a proc-macro refers to the CONSUMER
    // crate's Cargo.toml dir — this is the expected anchor per
    // R9 acceptance.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        Error::new(
            span,
            "CARGO_MANIFEST_DIR is not set — cannot resolve `wit = \"...\"` path",
        )
    })?;
    let abs = std::path::Path::new(&manifest_dir).join(wit_path);
    let text = std::fs::read_to_string(&abs).map_err(|_| {
        Error::new(
            span,
            format!(
                "could not read WIT file `{}` — pass `wit = \"path/relative/to/Cargo.toml\"` \
                 (absolute path: `{}`)",
                wit_path,
                abs.display()
            ),
        )
    })?;

    // Scan for `world <name>` declarations. The WIT grammar permits
    // whitespace between `world` and the name and between the name and
    // `{`; we accept any ASCII whitespace run.
    let mut worlds: Vec<String> = Vec::new();
    for line in text.lines() {
        // Strip leading whitespace; skip `//` comments.
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        // Very small scanner — not a full WIT parser, but the grammar
        // is simple enough that `world <ident>` at the start of a
        // logical line is unambiguous for a name-check gate.
        if let Some(rest) = trimmed.strip_prefix("world") {
            let rest = rest.trim_start();
            // `rest` must start with an ident char.
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !name.is_empty()
                && rest[name.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_whitespace() || c == '{')
            {
                worlds.push(name);
            }
        }
    }

    if worlds.is_empty() {
        return Err(Error::new(
            span,
            format!(
                "WIT file `{}` contains no `world <name>` declaration",
                abs.display()
            ),
        ));
    }
    if worlds.len() > 1 {
        return Err(Error::new(
            span,
            format!(
                "WIT file `{}` contains multiple `world` declarations ({}) — \
                 `#[derive(Plugin)]` requires exactly one",
                abs.display(),
                worlds.join(", ")
            ),
        ));
    }
    let found = &worlds[0];
    if found != plugin_name {
        return Err(Error::new(
            span,
            format!(
                "WIT world \"{found}\" does not match #[plugin(name = \"{plugin_name}\")] \
                 — either rename the world or update the `name` attribute"
            ),
        ));
    }
    Ok(())
}

// syn::spanned::Spanned is needed to call `.span()` on `Path`.
use syn::spanned::Spanned as _;
