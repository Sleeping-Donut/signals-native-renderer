#![forbid(unsafe_code)]
//! `view!` authoring macro.
//!
//! Supported surface:
//!
//! ```ignore
//! view! {
//!     <Column style={stack}>
//!         <Text>{format!("count {}", count.get(cx))}</Text>
//!         <Button style={button} on_click={move |cx| { ... }}>"+"</Button>
//!         <For each={move |cx| rows.get(cx)} key={|row: &String| row.clone()}
//!              children={move |cx, row| view! { <Text>{row.clone()}</Text> }} />
//!     </Column>
//! }
//! ```
//!
//! Attributes map to builder methods (`style`, `hover_style`, `focus_style`,
//! `cursor`, `tooltip`, `disabled_when`, `tab_index`, `focusable`, `disabled`).
//! `Button` requires `on_click`. `For` expands to `view::keyed_list`.

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2, TokenTree};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Data, DeriveInput, Expr, Fields, Ident, LitStr, Result, Token};

enum Child {
    Node(Node),
    Text(LitStr),
    Expr(Expr),
}

impl Parse for Child {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        if input.peek(Token![<]) {
            return Ok(Self::Node(input.parse()?));
        }
        if input.peek(LitStr) {
            return Ok(Self::Text(input.parse()?));
        }
        if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            let expr: Expr = content.parse()?;
            return Ok(Self::Expr(expr));
        }
        Err(input.error("expected a tag, string literal, or {expression}"))
    }
}

struct Attr {
    key: Ident,
    value: Expr,
}

struct Node {
    tag: Ident,
    attrs: Vec<Attr>,
    children: Vec<Child>,
}

impl Parse for Node {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        input.parse::<Token![<]>()?;
        let tag: Ident = input.parse()?;
        let mut attrs = Vec::new();

        while !input.peek(Token![>]) {
            if input.peek(Token![/]) {
                input.parse::<Token![/]>()?;
                input.parse::<Token![>]>()?;
                return Ok(Self {
                    tag,
                    attrs,
                    children: Vec::new(),
                });
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: Expr = if input.peek(syn::token::Brace) {
                let content;
                syn::braced!(content in input);
                content.parse()?
            } else if key == "on_click" {
                let mut tokens = TokenStream2::new();
                while !input.peek(Token![>]) && !input.peek(Token![/]) {
                    let token: TokenTree = input.parse()?;
                    tokens.extend(std::iter::once(token));
                }
                syn::parse2(tokens)?
            } else {
                input.parse()?
            };
            attrs.push(Attr { key, value });
        }
        input.parse::<Token![>]>()?;

        let mut children = Vec::new();
        loop {
            if input.peek(Token![<]) && input.peek2(Token![/]) {
                break;
            }
            if input.is_empty() {
                return Err(input.error("unclosed tag"));
            }
            children.push(input.parse()?);
        }
        input.parse::<Token![<]>()?;
        input.parse::<Token![/]>()?;
        let closing: Ident = input.parse()?;
        if closing != tag {
            return Err(syn::Error::new_spanned(
                closing,
                format!("expected closing tag </{tag}>"),
            ));
        }
        input.parse::<Token![>]>()?;

        Ok(Self {
            tag,
            attrs,
            children,
        })
    }
}

fn child_tokens(child: &Child) -> Result<TokenStream2> {
    match child {
        Child::Node(node) => node_tokens(node),
        Child::Text(text) => Ok(quote! { ::view::text(#text) }),
        Child::Expr(expr) => Ok(quote! { ::view::text_fn(move |cx| (#expr).to_string()) }),
    }
}

fn attr<'a>(node: &'a Node, name: &str) -> Option<&'a Expr> {
    node.attrs
        .iter()
        .find(|attr| attr.key == name)
        .map(|attr| &attr.value)
}

fn apply_attrs(
    mut tokens: TokenStream2,
    node: &Node,
    skip: &[&str],
    allowed: &[&str],
) -> Result<TokenStream2> {
    for attr in &node.attrs {
        let name = attr.key.to_string();
        if skip.contains(&name.as_str()) {
            continue;
        }
        if !allowed.contains(&name.as_str()) {
            return Err(syn::Error::new_spanned(
                &attr.key,
                format!("unsupported attribute `{name}`"),
            ));
        }
        let value = &attr.value;
        tokens = match name.as_str() {
            "style" => quote! { (#tokens).style(#value) },
            "hover_style" => quote! { (#tokens).hover_style(#value) },
            "focus_style" => quote! { (#tokens).focus_style(#value) },
            "cursor" => quote! { (#tokens).cursor(#value) },
            "tooltip" => quote! { (#tokens).tooltip(#value) },
            "disabled_when" => quote! { (#tokens).disabled_when(#value) },
            "tab_index" => quote! { (#tokens).tab_index(#value) },
            "focusable" => quote! { (#tokens).focusable(#value) },
            "disabled" => quote! { (#tokens).disabled(#value) },
            _ => unreachable!(),
        };
    }
    Ok(tokens)
}

const NODE_ATTRS: &[&str] = &[
    "style",
    "hover_style",
    "focus_style",
    "cursor",
    "tooltip",
    "disabled_when",
    "tab_index",
    "focusable",
    "disabled",
];

fn node_tokens(node: &Node) -> Result<TokenStream2> {
    let tag = node.tag.to_string();
    let children: Vec<TokenStream2> = node
        .children
        .iter()
        .map(child_tokens)
        .collect::<Result<_>>()?;

    match tag.as_str() {
        "Column" | "Row" => {
            let constructor = if tag == "Column" {
                quote! { ::view::column }
            } else {
                quote! { ::view::row }
            };
            let base = quote! { #constructor(vec![#(#children),*]) };
            apply_attrs(base, node, &[], NODE_ATTRS)
        }
        "Text" => {
            if node.children.len() != 1 {
                return Err(syn::Error::new_spanned(
                    &node.tag,
                    "Text expects one string literal or {expression}",
                ));
            }
            let base = match &node.children[0] {
                Child::Text(text) => quote! { ::view::text(#text) },
                Child::Expr(expr) => quote! { ::view::text_fn(move |cx| (#expr).to_string()) },
                Child::Node(_) => {
                    return Err(syn::Error::new_spanned(
                        &node.tag,
                        "Text cannot contain an element",
                    ))
                }
            };
            apply_attrs(base, node, &[], NODE_ATTRS)
        }
        "Button" => {
            let on_click = attr(node, "on_click").ok_or_else(|| {
                syn::Error::new_spanned(&node.tag, "Button requires on_click=...")
            })?;
            let base = quote! { ::view::button(vec![#(#children),*], #on_click) };
            let mut allowed = NODE_ATTRS.to_vec();
            allowed.push("on_click");
            apply_attrs(base, node, &["on_click"], &allowed)
        }
        "Show" => {
            let when = attr(node, "when")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "Show requires when=..."))?;
            let fallback = attr(node, "fallback")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "Show requires fallback=..."))?;
            if node.children.is_empty() {
                return Err(syn::Error::new_spanned(&node.tag, "Show requires children"));
            }
            let body = if node.children.len() == 1 {
                child_tokens(&node.children[0])?
            } else {
                quote! { ::view::fragment(vec![#(#children),*]) }
            };
            Ok(quote! {
                ::view::show(#when, move |cx| #body, #fallback)
            })
        }
        "Loading" => {
            let fallback = attr(node, "fallback").ok_or_else(|| {
                syn::Error::new_spanned(&node.tag, "Loading requires fallback=...")
            })?;
            if node.children.is_empty() {
                return Err(syn::Error::new_spanned(
                    &node.tag,
                    "Loading requires children",
                ));
            }
            let body = if node.children.len() == 1 {
                child_tokens(&node.children[0])?
            } else {
                quote! { ::view::fragment(vec![#(#children),*]) }
            };
            if let Some(ready) = attr(node, "ready") {
                Ok(quote! {
                    ::view::loading_when(#ready, #fallback, move |cx| #body)
                })
            } else {
                Ok(quote! {
                    ::view::loading_auto(#fallback, move |cx| #body)
                })
            }
        }
        "Embed" => {
            let expr = attr(node, "expr")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "Embed requires expr=..."))?;
            if !node.children.is_empty() {
                return Err(syn::Error::new_spanned(
                    &node.tag,
                    "Embed is self-closing; put expr=... on the tag",
                ));
            }
            let mut allowed = NODE_ATTRS.to_vec();
            allowed.push("expr");
            apply_attrs(quote! { #expr }, node, &["expr"], &allowed)
        }
        "For" => {
            let each = attr(node, "each")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "For requires each=..."))?;
            let key = attr(node, "key")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "For requires key=..."))?;
            let children = attr(node, "children")
                .ok_or_else(|| syn::Error::new_spanned(&node.tag, "For requires children=..."))?;
            if !node.children.is_empty() {
                return Err(syn::Error::new_spanned(
                    &node.tag,
                    "For is self-closing; put children=... on the tag",
                ));
            }
            Ok(quote! { ::view::keyed_list(#each, #key, #children) })
        }
        tag if tag.chars().next().is_some_and(char::is_uppercase) => {
            let props = syn::Ident::new(&format!("{tag}Props"), node.tag.span());
            let tag_ident = &node.tag;
            let mut calls: Vec<TokenStream2> = node
                .attrs
                .iter()
                .map(|attr| {
                    let key = &attr.key;
                    let value = &attr.value;
                    quote! { .#key(#value) }
                })
                .collect();
            if !node.children.is_empty() {
                if node.attrs.iter().any(|attr| attr.key == "children") {
                    return Err(syn::Error::new_spanned(
                        &node.tag,
                        "component children cannot be both an attribute and content",
                    ));
                }
                calls.push(quote! {
                    .children(::view::fragment(vec![#(#children),*]))
                });
            }
            Ok(quote! {
                #tag_ident(&cx, #props::builder()#(#calls)*.build())
            })
        }
        _ => Err(syn::Error::new_spanned(
            &node.tag,
            "unknown tag; expected a component or Column, Row, Text, Button, Show, Loading, Embed, or For",
        )),
    }
}

#[proc_macro]
pub fn view(input: TokenStream) -> TokenStream {
    let node = parse_macro_input!(input as Node);
    match node_tokens(&node) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Mark a function as a component.
///
/// Components take `cx` first and optionally a props struct. Component tags in
/// `view!` expand to `Name(cx, NameProps { ... })`.
#[proc_macro_attribute]
pub fn component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as syn::ItemFn);
    if function.sig.inputs.is_empty() {
        return syn::Error::new_spanned(&function.sig.ident, "a component requires a Cx parameter")
            .to_compile_error()
            .into();
    }
    quote! { #[allow(non_snake_case)] #function }.into()
}

enum BuilderArg {
    Builder(Builder),
    Expr(Expr),
}

impl Parse for BuilderArg {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        if input.peek(Ident) {
            let fork = input.fork();
            let ident: Ident = fork.parse()?;
            if ident
                .to_string()
                .chars()
                .next()
                .is_some_and(char::is_uppercase)
            {
                return Ok(Self::Builder(input.parse()?));
            }
        }
        Ok(Self::Expr(input.parse()?))
    }
}

struct BuilderMethod {
    name: Ident,
    arg: Option<BuilderArg>,
}

struct Builder {
    tag: Ident,
    methods: Vec<BuilderMethod>,
}

impl Parse for Builder {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let tag: Ident = input.parse()?;
        if !tag
            .to_string()
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
        {
            return Err(syn::Error::new_spanned(
                tag,
                "builder tags start with an uppercase letter",
            ));
        }
        let mut methods = Vec::new();
        while input.peek(Token![.]) {
            input.parse::<Token![.]>()?;
            let name: Ident = input.parse()?;
            let arg = if input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in input);
                Some(content.parse()?)
            } else {
                None
            };
            methods.push(BuilderMethod { name, arg });
        }
        Ok(Self { tag, methods })
    }
}

fn builder_tokens(builder: &Builder) -> Result<TokenStream2> {
    let tag = builder.tag.to_string();
    let mut children = Vec::new();
    let mut value = None;
    let mut on_click = None;
    let mut attrs: Vec<(&'static str, Expr)> = Vec::new();

    for method in &builder.methods {
        let name = method.name.to_string();
        match name.as_str() {
            "child" => match method.arg.as_ref() {
                Some(BuilderArg::Builder(child)) => children.push(builder_tokens(child)?),
                Some(BuilderArg::Expr(expr)) => children.push(quote! { #expr }),
                None => {
                    return Err(syn::Error::new_spanned(
                        &method.name,
                        "child requires an argument",
                    ))
                }
            },
            "value" => {
                let Some(BuilderArg::Expr(expr)) = method.arg.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        &method.name,
                        "value requires an expression",
                    ));
                };
                value = Some(expr.clone());
            }
            "on_click" => {
                let Some(BuilderArg::Expr(expr)) = method.arg.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        &method.name,
                        "on_click requires a closure",
                    ));
                };
                on_click = Some(expr.clone());
            }
            "style" | "hover_style" | "focus_style" | "cursor" | "tooltip" | "disabled_when"
            | "tab_index" | "focusable" | "disabled" => {
                let Some(BuilderArg::Expr(expr)) = method.arg.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        &method.name,
                        "attribute requires an expression",
                    ));
                };
                let key = match name.as_str() {
                    "style" => "style",
                    "hover_style" => "hover_style",
                    "focus_style" => "focus_style",
                    "cursor" => "cursor",
                    "tooltip" => "tooltip",
                    "disabled_when" => "disabled_when",
                    "tab_index" => "tab_index",
                    "focusable" => "focusable",
                    "disabled" => "disabled",
                    _ => unreachable!(),
                };
                attrs.push((key, expr.clone()));
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    &method.name,
                    format!("unsupported builder method `{name}`"),
                ))
            }
        }
    }

    let base = match tag.as_str() {
        "Column" => quote! { ::view::column(vec![#(#children),*]) },
        "Row" => quote! { ::view::row(vec![#(#children),*]) },
        "Text" => {
            let value = value.ok_or_else(|| {
                syn::Error::new_spanned(&builder.tag, "Text requires .value(...)")
            })?;
            quote! { ::view::text_fn(move |cx| (#value).to_string()) }
        }
        "Button" => {
            let on_click = on_click.ok_or_else(|| {
                syn::Error::new_spanned(&builder.tag, "Button requires .on_click(...)")
            })?;
            quote! { ::view::button(vec![#(#children),*], #on_click) }
        }
        _ => {
            return Err(syn::Error::new_spanned(
                &builder.tag,
                "builder supports Column, Row, Text, and Button",
            ))
        }
    };

    let mut tokens = base;
    for (key, value) in attrs {
        let key = syn::Ident::new(key, builder.tag.span());
        tokens = quote! { (#tokens).#key(#value) };
    }
    Ok(tokens)
}

/// Fluent builder-chain authoring.
///
/// ```ignore
/// view_builder! {
///     Column
///         .style(styles::root())
///         .child(Text.value(format!("count {}", count.get(cx))))
///         .child(Button.on_click(move |cx| bump(cx)).child(Text.value("+")))
/// }
/// ```
#[proc_macro]
pub fn view_builder(input: TokenStream) -> TokenStream {
    let builder = parse_macro_input!(input as Builder);
    match builder_tokens(&builder) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

struct PropField {
    ident: syn::Ident,
    ty: syn::Type,
    into: bool,
    optional: bool,
    default: Option<Expr>,
}

/// Derive a component props builder.
///
/// Field attributes:
/// - `#[prop(into)]` — the builder accepts `impl Into<FieldType>`
/// - `#[prop(optional)]` — missing value uses `Default`
/// - `#[prop(default = expr)]` — missing value uses `expr`
#[proc_macro_derive(Props, attributes(prop))]
pub fn derive_props(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(&input, "Props supports structs only")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(&input, "Props requires named fields")
            .to_compile_error()
            .into();
    };

    let mut prop_fields = Vec::new();
    for field in &fields.named {
        let ident = field.ident.clone().expect("named field");
        let mut into = false;
        let mut optional = false;
        let mut default = None;
        for attr in &field.attrs {
            if !attr.path().is_ident("prop") {
                continue;
            }
            if let Err(error) = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("into") {
                    into = true;
                    Ok(())
                } else if meta.path.is_ident("optional") {
                    optional = true;
                    Ok(())
                } else if meta.path.is_ident("default") {
                    let expr: Expr = meta.value()?.parse()?;
                    default = Some(expr);
                    Ok(())
                } else {
                    Err(meta.error("unsupported prop attribute"))
                }
            }) {
                return error.to_compile_error().into();
            }
        }
        prop_fields.push(PropField {
            ident,
            ty: field.ty.clone(),
            into,
            optional,
            default,
        });
    }

    let builder = syn::Ident::new(&format!("{name}Builder"), name.span());
    let builder_fields = prop_fields.iter().map(|field| {
        let ident = &field.ident;
        let ty = &field.ty;
        quote! { #ident: Option<#ty> }
    });
    let setters = prop_fields.iter().map(|field| {
        let ident = &field.ident;
        let ty = &field.ty;
        if field.into {
            quote! {
                pub fn #ident<T: Into<#ty>>(mut self, value: T) -> Self {
                    self.#ident = Some(value.into());
                    self
                }
            }
        } else {
            quote! {
                pub fn #ident(mut self, value: #ty) -> Self {
                    self.#ident = Some(value);
                    self
                }
            }
        }
    });
    let builds = prop_fields.iter().map(|field| {
        let ident = &field.ident;
        let value = if let Some(default) = &field.default {
            quote! { self.#ident.unwrap_or_else(|| #default) }
        } else if field.optional {
            quote! { self.#ident.unwrap_or_default() }
        } else {
            quote! { self.#ident.expect(concat!("missing prop `", stringify!(#ident), "`")) }
        };
        quote! { #ident: #value }
    });

    quote! {
        #[derive(Default)]
        pub struct #builder {
            #(#builder_fields,)*
        }

        impl #builder {
            #(#setters)*
            #[must_use]
            pub fn build(self) -> #name {
                #name {
                    #(#builds,)*
                }
            }
        }

        impl #name {
            #[must_use]
            pub fn builder() -> #builder {
                #builder::default()
            }
        }
    }
    .into()
}
