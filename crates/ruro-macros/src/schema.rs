//! `Describe` implementations derived from a type definition.
//!
//! The generated code mirrors how serde and postcard lay the type out. Field
//! types that do not implement `Describe` become opaque instead of failing to
//! compile, so adding a schema never breaks an existing message.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Fields, Type};

use crate::doc_string;

/// How serde treats a field, as far as the wire layout is concerned.
enum FieldSerde {
    Normal,
    Skipped,
    /// Custom (de)serialization or flattening: layout unknown.
    Custom,
}

fn serde_words(attrs: &[Attribute]) -> Vec<String> {
    let mut words = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(ident) = meta.path.get_ident() {
                words.push(ident.to_string());
            }
            // Consume `= value` if present so parsing can continue.
            if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                let _: TokenStream2 = content.parse()?;
            }
            Ok(())
        });
    }
    words
}

fn field_serde(attrs: &[Attribute]) -> FieldSerde {
    let words = serde_words(attrs);
    let has = |w: &str| words.iter().any(|x| x == w);
    if has("skip") || has("skip_serializing") {
        FieldSerde::Skipped
    } else if has("with")
        || has("serialize_with")
        || has("deserialize_with")
        || has("flatten")
        || has("skip_serializing_if")
    {
        FieldSerde::Custom
    } else {
        FieldSerde::Normal
    }
}

fn field_schema(ty: &Type, attrs: &[Attribute]) -> TokenStream2 {
    match field_serde(attrs) {
        FieldSerde::Custom => quote! {
            ::ruro::msg::Schema::opaque(::core::stringify!(#ty))
        },
        _ => quote! {{
            #[allow(unused_imports)]
            use ::ruro::msg::schema::__probe::{Probe, ViaDescribe as _, ViaFallback as _};
            (&&Probe::<#ty>::new()).schema_of()
        }},
    }
}

fn named_fields(fields: &syn::FieldsNamed) -> Vec<TokenStream2> {
    fields
        .named
        .iter()
        .filter(|f| !matches!(field_serde(&f.attrs), FieldSerde::Skipped))
        .map(|f| {
            let name = f.ident.as_ref().unwrap().to_string();
            let schema = field_schema(&f.ty, &f.attrs);
            let doc = doc_string(&f.attrs);
            quote! {
                ::ruro::msg::schema::Field::new(#name, #schema).doc(#doc)
            }
        })
        .collect()
}

fn unnamed_fields(fields: &syn::FieldsUnnamed) -> Vec<TokenStream2> {
    fields
        .unnamed
        .iter()
        .filter(|f| !matches!(field_serde(&f.attrs), FieldSerde::Skipped))
        .map(|f| field_schema(&f.ty, &f.attrs))
        .collect()
}

/// The body of `Describe::describe` for `input`.
fn schema_expr(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = input.ident.to_string();
    let s = quote!(::ruro::msg::schema);
    let container = serde_words(&input.attrs);
    if container.iter().any(|w| w == "transparent")
        && let Data::Struct(data) = &input.data
        && let Some(field) = data.fields.iter().next()
    {
        return Ok(field_schema(&field.ty, &field.attrs));
    }
    if container
        .iter()
        .any(|w| w == "tag" || w == "untagged" || w == "into" || w == "from")
    {
        return Ok(quote!(#s::Schema::opaque(#name)));
    }
    Ok(match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(f) => {
                let fields = named_fields(f);
                quote!(#s::Schema::structure(#name, ::std::vec![#(#fields),*]))
            }
            Fields::Unnamed(f) => {
                let items = unnamed_fields(f);
                quote!(#s::Schema::tuple_struct(#name, ::std::vec![#(#items),*]))
            }
            Fields::Unit => quote!(#s::Schema::Unit),
        },
        Data::Enum(data) => {
            let variants = data.variants.iter().map(|v| {
                let vname = v.ident.to_string();
                let payload = match &v.fields {
                    Fields::Unit => quote!(#s::VariantPayload::Unit),
                    Fields::Unnamed(f) => {
                        let items = unnamed_fields(f);
                        quote!(#s::VariantPayload::Tuple { items: ::std::vec![#(#items),*] })
                    }
                    Fields::Named(f) => {
                        let fields = named_fields(f);
                        quote!(#s::VariantPayload::Struct { fields: ::std::vec![#(#fields),*] })
                    }
                };
                quote!(#s::Variant { name: #vname.into(), payload: #payload })
            });
            quote!(#s::Schema::Enum {
                name: #name.into(),
                variants: ::std::vec![#(#variants),*],
            })
        }
        Data::Union(_) => {
            return Err(syn::Error::new(
                input.ident.span(),
                "unions cannot describe their layout",
            ));
        }
    })
}

/// `impl Describe for T`.
pub fn expand_describe(input: &DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "generic types cannot derive Describe",
        ));
    }
    let ident = &input.ident;
    let body = schema_expr(input)?;
    Ok(quote! {
        impl ::ruro::msg::Describe for #ident {
            fn describe() -> ::ruro::msg::Schema {
                #body
            }
        }
    })
}
