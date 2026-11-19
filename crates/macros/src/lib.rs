#![forbid(unsafe_code)]
//! `view!` first cut.
//!
//! Supported surface:
//!
//! ```ignore
//! view! {
//!     <Column>
//!         <Text>"count"</Text>
//!         <Button on_click=move |cx| { ... }>"+"</Button>
//!     </Column>
//! }
//! ```
//!
//! Tags map to `view::column`, `view::row`, `view::text`, and `view::button`.
//! Text content is a string literal. `Button` accepts `on_click`; other
//! attributes are rejected for now. Dynamic text and styles use the builder
//! API (`text_fn`, `column`, `row`, `button`) until the macro grows.

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

struct Node {
    tag: Ident,
    on_click: Option<Expr>,
    children: Vec<Child>,
}

impl Parse for Node {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        input.parse::<Token![<]>()?;
        let tag: Ident = input.parse()?;
        let mut on_click = None;

        while !input.peek(Token![>]) {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            if key == "on_click" {
                let mut tokens = TokenStream2::new();
                while !input.peek(Token![>]) {
                    let token: TokenTree = input.parse()?;
                    tokens.extend(std::iter::once(token));
                }
                on_click = Some(syn::parse2(tokens)?);
            } else {
                let _: Expr = input.parse()?;
                return Err(syn::Error::new_spanned(
                    key,
                    "only on_click is supported by the first view! cut",
                ));
            }
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
            on_click,
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
            Ok(quote! { #constructor(vec![#(#children),*]) })
        }
        "Text" => {
            if node.children.len() != 1 {
                return Err(syn::Error::new_spanned(
                    &node.tag,
                    "Text expects one string literal",
                ));
            }
            match &node.children[0] {
                Child::Text(text) => Ok(quote! { ::view::text(#text) }),
                Child::Expr(expr) => Ok(quote! { ::view::text_fn(move |cx| (#expr).to_string()) }),
                Child::Node(_) => Err(syn::Error::new_spanned(
                    &node.tag,
                    "Text cannot contain an element",
                )),
            }
        }
        "Button" => {
            let on_click = node.on_click.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(&node.tag, "Button requires on_click=...")
            })?;
            Ok(quote! { ::view::button(vec![#(#children),*], #on_click) })
        }
        _ => Err(syn::Error::new_spanned(
            &node.tag,
            "unknown tag; expected Column, Row, Text, or Button",
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
