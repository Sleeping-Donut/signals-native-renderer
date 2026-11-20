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
use syn::{parse_macro_input, Expr, Ident, LitStr, Result, Token};

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
            let mut fields: Vec<TokenStream2> = node
                .attrs
                .iter()
                .map(|attr| {
                    let key = &attr.key;
                    let value = &attr.value;
                    quote! { #key: #value }
                })
                .collect();
            if !node.children.is_empty() {
                if node.attrs.iter().any(|attr| attr.key == "children") {
                    return Err(syn::Error::new_spanned(
                        &node.tag,
                        "component children cannot be both an attribute and content",
                    ));
                }
                fields.push(quote! {
                    children: ::view::fragment(vec![#(#children),*])
                });
            }
            Ok(quote! { #tag_ident(&cx, #props { #(#fields),* }) })
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
