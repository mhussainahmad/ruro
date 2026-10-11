//! Derive macros for ruro. Use them through the `ruro` crate.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Attribute, Data, DeriveInput, Expr, Fields, LitStr, Type, parse_macro_input};

/// Implements `Message` for a serde type using the compact postcard encoding.
///
/// ```ignore
/// #[derive(Message, Serialize, Deserialize)]
/// #[message(name = "my_pkg/Detection")]
/// struct Detection { x: f32, y: f32, score: f32 }
/// ```
#[proc_macro_derive(Message, attributes(message))]
pub fn derive_message(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_message(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_message(input: DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "generic messages are not supported",
        ));
    }
    let mut name: Option<LitStr> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("message")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `name = \"pkg/Type\"`"))
            }
        })?;
    }
    let name = name.ok_or_else(|| {
        syn::Error::new(Span::call_site(), "missing #[message(name = \"pkg/Type\")]")
    })?;
    let ident = &input.ident;
    Ok(quote! {
        ::ruro::msg::impl_postcard_message!(#ident, #name);
    })
}

/// Generates the node descriptor, constructor and parameter setter, and
/// registers the node type with the host.
///
/// ```ignore
/// /// Blurs an image.
/// #[derive(Node)]
/// #[node(kind = "std/blur")]
/// struct Blur {
///     #[input] image: Input<Image>,
///     #[output] out: Output<Image>,
///     #[param(default = 2, min = 0, max = 20)] radius: u32,
///     scratch: Vec<u8>, // plain fields start as Default::default()
/// }
/// ```
#[proc_macro_derive(Node, attributes(node, input, output, param))]
pub fn derive_node(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_node(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

enum FieldRole {
    Input,
    Output,
    Param(Box<ParamAttr>),
    State,
}

#[derive(Default)]
struct ParamAttr {
    default: Option<Expr>,
    min: Option<Expr>,
    max: Option<Expr>,
    doc: Option<LitStr>,
}

fn doc_string(attrs: &[Attribute]) -> String {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value().trim().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    lines.join(" ").trim().to_owned()
}

fn is_string(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "String"))
}

fn field_role(field: &syn::Field) -> syn::Result<FieldRole> {
    let mut role = None;
    let mut set = |r: FieldRole, span: Span| {
        if role.is_some() {
            return Err(syn::Error::new(
                span,
                "a field can only have one of #[input], #[output], #[param]",
            ));
        }
        role = Some(r);
        Ok(())
    };
    for attr in &field.attrs {
        if attr.path().is_ident("input") {
            set(FieldRole::Input, attr.span())?;
        } else if attr.path().is_ident("output") {
            set(FieldRole::Output, attr.span())?;
        } else if attr.path().is_ident("param") {
            let mut p = ParamAttr::default();
            if matches!(attr.meta, syn::Meta::List(_)) {
                attr.parse_nested_meta(|meta| {
                    let key = meta
                        .path
                        .get_ident()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    match key.as_str() {
                        "default" => p.default = Some(meta.value()?.parse()?),
                        "min" => p.min = Some(meta.value()?.parse()?),
                        "max" => p.max = Some(meta.value()?.parse()?),
                        "doc" => p.doc = Some(meta.value()?.parse()?),
                        _ => return Err(meta.error("expected `default`, `min`, `max` or `doc`")),
                    }
                    Ok(())
                })?;
            }
            set(FieldRole::Param(Box::new(p)), attr.span())?;
        }
    }
    Ok(role.unwrap_or(FieldRole::State))
}

fn expand_node(input: DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "generic nodes are not supported",
        ));
    }
    let ident = &input.ident;

    let mut kind: Option<LitStr> = None;
    let mut doc: Option<LitStr> = None;
    let mut rate: Option<Expr> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("node")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("kind") {
                kind = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("doc") {
                doc = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("rate") {
                rate = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected `kind`, `doc` or `rate`"));
            }
            Ok(())
        })?;
    }
    let kind = kind.ok_or_else(|| {
        syn::Error::new(Span::call_site(), "missing #[node(kind = \"pkg/name\")]")
    })?;
    let doc = doc
        .map(|d| d.value())
        .unwrap_or_else(|| doc_string(&input.attrs));
    let rate = match rate {
        Some(r) => quote!(::core::option::Option::Some((#r) as f64)),
        None => quote!(::core::option::Option::None),
    };

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[derive(Node)] only supports structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "#[derive(Node)] needs named fields",
        ));
    };

    let p = quote!(::ruro::node);
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut params = Vec::new();
    let mut inits = Vec::new();
    let mut setters = Vec::new();

    for field in &fields.named {
        let name = field.ident.as_ref().unwrap();
        let name_str = name.to_string();
        let ty = &field.ty;
        match field_role(field)? {
            FieldRole::Input => {
                inputs.push(quote! {
                    ::ruro::graph::PortDesc {
                        name: #name_str.into(),
                        ty: <#ty as #p::PortType>::TY.into(),
                    }
                });
                inits.push(quote!(#name: b.input(#name_str)?));
            }
            FieldRole::Output => {
                outputs.push(quote! {
                    ::ruro::graph::PortDesc {
                        name: #name_str.into(),
                        ty: <#ty as #p::PortType>::TY.into(),
                    }
                });
                inits.push(quote!(#name: b.output(#name_str)?));
            }
            FieldRole::Param(attr) => {
                let default = match &attr.default {
                    Some(e) if is_string(ty) => {
                        quote_spanned!(e.span()=> ::std::string::String::from(#e))
                    }
                    Some(e) => quote_spanned!(e.span()=> #e),
                    None => quote!(::core::default::Default::default()),
                };
                let opt = |e: &Option<Expr>| match e {
                    Some(e) => quote!(::core::option::Option::Some((#e) as f64)),
                    None => quote!(::core::option::Option::None),
                };
                let (min, max) = (opt(&attr.min), opt(&attr.max));
                let pdoc = attr
                    .doc
                    .map(|d| d.value())
                    .unwrap_or_else(|| doc_string(&field.attrs));
                params.push(quote! {
                    ::ruro::graph::ParamDesc {
                        name: #name_str.into(),
                        kind: <#ty as #p::ParamValue>::KIND,
                        default: {
                            let v: #ty = #default;
                            #p::ParamValue::to_json(&v)
                        },
                        min: #min,
                        max: #max,
                        doc: #pdoc.into(),
                    }
                });
                inits.push(quote! {
                    #name: {
                        let v: #ty = #default;
                        b.param(#name_str, v)?
                    }
                });
                setters.push(quote! {
                    #name_str => {
                        self.#name = <#ty as #p::ParamValue>::from_json(value)?;
                        ::core::result::Result::Ok(())
                    }
                });
            }
            FieldRole::State => inits.push(quote!(#name: ::core::default::Default::default())),
        }
    }

    Ok(quote! {
        impl #p::NodeSpec for #ident {
            fn descriptor() -> #p::NodeDescriptor {
                #p::NodeDescriptor {
                    kind: #kind.into(),
                    doc: #doc.into(),
                    default_rate: #rate,
                    inputs: ::std::vec![#(#inputs),*],
                    outputs: ::std::vec![#(#outputs),*],
                    params: ::std::vec![#(#params),*],
                }
            }

            fn build(b: &mut #p::Builder) -> #p::Result<Self> {
                ::core::result::Result::Ok(Self { #(#inits),* })
            }

            fn set_param(
                &mut self,
                name: &str,
                value: &#p::__private::serde_json::Value,
            ) -> #p::Result {
                #[allow(unused_variables)]
                let value = value;
                match name {
                    #(#setters)*
                    other => ::core::result::Result::Err(::ruro::anyhow::anyhow!(
                        "{} has no parameter `{}`", #kind, other
                    )),
                }
            }
        }

        #p::__private::inventory::submit! {
            #p::Registration {
                descriptor: <#ident as #p::NodeSpec>::descriptor,
                factory: #p::__private::factory::<#ident>,
            }
        }
    })
}
