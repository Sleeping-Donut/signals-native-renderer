#![forbid(unsafe_code)]
//! `Store` and `Patch` derives.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr};

fn named_fields(input: &DeriveInput) -> syn::Result<Vec<(String, syn::Type)>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "Store derives currently support structs only",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            input,
            "Store derives require named fields",
        ));
    };
    Ok(fields
        .named
        .iter()
        .map(|field| {
            let name = field.ident.as_ref().expect("named field").to_string();
            (name, field.ty.clone())
        })
        .collect())
}

#[proc_macro_derive(Store)]
pub fn derive_store(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let fields = match named_fields(&input) {
        Ok(fields) => fields,
        Err(error) => return error.to_compile_error().into(),
    };
    let trait_name = syn::Ident::new(&format!("{name}StoreFields"), name.span());
    let methods = fields.iter().map(|(field, ty)| {
        let method = syn::Ident::new(field, name.span());
        quote! {
            fn #method(&self) -> ::reactive_stores::Field<#ty>;
        }
    });
    let store_methods = fields.iter().map(|(field, ty)| {
        let method = syn::Ident::new(field, name.span());
        let key = LitStr::new(field, name.span());
        quote! {
            fn #method(&self) -> ::reactive_stores::Field<#ty> {
                self.project(
                    #key,
                    |value| value.#method.clone(),
                    |value, next| value.#method = next,
                )
            }
        }
    });
    let field_methods = fields.iter().map(|(field, ty)| {
        let method = syn::Ident::new(field, name.span());
        let key = LitStr::new(field, name.span());
        quote! {
            fn #method(&self) -> ::reactive_stores::Field<#ty> {
                self.project(
                    #key,
                    |value| value.#method.clone(),
                    |value, next| value.#method = next,
                )
            }
        }
    });
    let expanded = quote! {
        pub trait #trait_name {
            #(#methods)*
        }

        impl #trait_name for ::reactive_stores::Store<#name>
        where
            #name: Clone + 'static,
        {
            #(#store_methods)*
        }

        impl #trait_name for ::reactive_stores::Field<#name>
        where
            #name: Clone + 'static,
        {
            #(#field_methods)*
        }
    };
    expanded.into()
}

#[proc_macro_derive(Patch)]
pub fn derive_patch(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let fields = match named_fields(&input) {
        Ok(fields) => fields,
        Err(error) => return error.to_compile_error().into(),
    };
    let trait_name = syn::Ident::new(&format!("{name}StoreFields"), name.span());
    let patches: Vec<_> = fields
        .iter()
        .map(|(field, _)| {
            let method = syn::Ident::new(field, name.span());
            quote! {
                if old.#method != next.#method {
                    <Self as #trait_name>::#method(self).set(cx, next.#method);
                }
            }
        })
        .collect();
    let field_patches = patches.clone();
    let expanded = quote! {
        impl ::reactive_stores::StorePatch<#name> for ::reactive_stores::Store<#name>
        where
            #name: Clone + 'static,
        {
            fn patch(&self, cx: &::reactive_core::Cx<'_>, next: #name) {
                let old = self.snapshot(cx);
                #(#patches)*
            }
        }

        impl ::reactive_stores::StorePatch<#name> for ::reactive_stores::Field<#name>
        where
            #name: Clone + 'static,
        {
            fn patch(&self, cx: &::reactive_core::Cx<'_>, next: #name) {
                let old = self.get(cx);
                #(#field_patches)*
            }
        }
    };
    expanded.into()
}
