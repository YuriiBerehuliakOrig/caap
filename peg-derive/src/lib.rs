//! `#[derive(FromParseValue)]` for `caap-peg`.
//!
//! Generates a [`caap_peg::FromParseValue`] impl that maps a `ParseValue` into a
//! typed struct or enum, on top of the runtime accessors (`parse_field`,
//! `parse_as`, `node`).
//!
//! - **Struct** with named fields: each field `name: T` is read from the
//!   `name:` binding via `parse_field::<T>("name")`. `#[peg(rename = "x")]`
//!   overrides the binding name.
//! - **Enum**: each variant carries `#[peg(tag = "t")]` and is selected by the
//!   matched `Node` tag. A newtype variant `V(T)` maps the value via
//!   `parse_as::<T>()`; a unit variant `V` matches the tag alone.
//!
//! Enable with the `derive` feature of `caap-peg`:
//! `use caap_peg::FromParseValue;` then `#[derive(FromParseValue)]`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr};

/// Derive [`caap_peg::FromParseValue`] for a struct or enum. See the crate docs
/// for the supported `#[peg(...)]` field/variant attributes.
#[proc_macro_derive(FromParseValue, attributes(peg))]
pub fn derive_from_parse_value(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match build(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn build(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let (impl_g, ty_g, where_g) = input.generics.split_for_impl();
    let body = match &input.data {
        Data::Struct(data) => build_struct(data)?,
        Data::Enum(data) => build_enum(name, data)?,
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "FromParseValue cannot be derived for unions",
            ))
        }
    };
    Ok(quote! {
        impl #impl_g ::caap_peg::FromParseValue for #name #ty_g #where_g {
            fn from_parse_value(
                value: &::caap_peg::ParseValue,
            ) -> ::core::result::Result<Self, ::caap_peg::FromParseValueError> {
                #body
            }
        }
    })
}

fn build_struct(data: &syn::DataStruct) -> syn::Result<proc_macro2::TokenStream> {
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "FromParseValue derive supports only structs with named fields",
        ));
    };
    let inits = fields
        .named
        .iter()
        .map(|field| {
            let ident = field.ident.as_ref().expect("named field");
            let ty = &field.ty;
            let opts = FieldOpts::parse(&field.attrs)?;
            let binding = opts.rename.unwrap_or_else(|| ident.to_string());
            let init = if opts.text {
                // The field IS this node's own value (its matched text), not a
                // named child.
                quote! { value.parse_as::<#ty>()? }
            } else if opts.default {
                // Optional named binding: parse it when present, else `Default`.
                quote! {
                    match value.field(#binding) {
                        ::core::option::Option::Some(v) => {
                            v.parse_as::<#ty>().map_err(|e| e.at(#binding))?
                        }
                        ::core::option::Option::None => ::core::default::Default::default(),
                    }
                }
            } else {
                quote! { value.parse_field::<#ty>(#binding)? }
            };
            Ok(quote! { #ident: #init })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote! {
        ::core::result::Result::Ok(Self { #(#inits),* })
    })
}

fn build_enum(
    enum_name: &syn::Ident,
    data: &syn::DataEnum,
) -> syn::Result<proc_macro2::TokenStream> {
    let arms =
        data.variants
            .iter()
            .map(|variant| {
                let vname = &variant.ident;
                let tag = peg_tag(&variant.attrs)?.ok_or_else(|| {
                    syn::Error::new_spanned(
                        variant,
                        "each enum variant needs #[peg(tag = \"...\")] for FromParseValue",
                    )
                })?;
                let cons =
                    match &variant.fields {
                        Fields::Unit => quote! { #enum_name::#vname },
                        Fields::Unnamed(f) if f.unnamed.len() == 1 => {
                            quote! { #enum_name::#vname(value.parse_as()?) }
                        }
                        _ => return Err(syn::Error::new_spanned(
                            variant,
                            "FromParseValue enum variants must be unit or single-field newtypes",
                        )),
                    };
                Ok(quote! {
                    ::core::option::Option::Some(#tag) => ::core::result::Result::Ok(#cons),
                })
            })
            .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote! {
        match value.node().map(|(tag, _)| tag) {
            #(#arms)*
            other => ::core::result::Result::Err(
                ::caap_peg::FromParseValueError::new(::std::format!(
                    "no enum variant for node tag {:?}",
                    other
                )),
            ),
        }
    })
}

/// Per-field `#[peg(...)]` options.
#[derive(Default)]
struct FieldOpts {
    /// `#[peg(rename = "x")]` — bind to a different name than the field.
    rename: Option<String>,
    /// `#[peg(text)]` — the field is this node's own value, not a named child.
    text: bool,
    /// `#[peg(default)]` — absent binding falls back to `Default::default()`.
    default: bool,
}

impl FieldOpts {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut opts = FieldOpts::default();
        for attr in attrs {
            if !attr.path().is_ident("peg") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    opts.rename = Some(meta.value()?.parse::<LitStr>()?.value());
                    Ok(())
                } else if meta.path.is_ident("text") {
                    opts.text = true;
                    Ok(())
                } else if meta.path.is_ident("default") {
                    opts.default = true;
                    Ok(())
                } else {
                    Err(meta.error("unknown #[peg(...)] key; expected rename / text / default"))
                }
            })?;
        }
        if opts.text && (opts.default || opts.rename.is_some()) {
            return Err(syn::Error::new_spanned(
                attrs.first(),
                "#[peg(text)] cannot combine with rename/default",
            ));
        }
        Ok(opts)
    }
}

/// Read `#[peg(tag = "...")]` from a variant's attributes.
fn peg_tag(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    let mut found = None;
    for attr in attrs {
        if !attr.path().is_ident("peg") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                found = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else {
                Err(meta.error("unknown #[peg(...)] key; expected `tag`"))
            }
        })?;
    }
    Ok(found)
}
