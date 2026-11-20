#![forbid(unsafe_code)]
//! `Store` and `Patch` derives.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
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
    if let Data::Enum(data) = &input.data {
        return derive_store_enum(&input, data).into();
    }
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

fn derive_store_enum(input: &DeriveInput, data: &syn::DataEnum) -> TokenStream2 {
    use quote::quote;
    let name = &input.ident;
    let trait_name = syn::Ident::new(&format!("{name}StoreFields"), name.span());
    let mut methods = Vec::new();
    let mut store_methods = Vec::new();
    let mut field_methods = Vec::new();

    for variant in &data.variants {
        let variant_name = &variant.ident;
        let method = match &variant.fields {
            Fields::Unit => syn::Ident::new(
                &format!("is_{}", variant_name.to_string().to_lowercase()),
                name.span(),
            ),
            Fields::Unnamed(_) => syn::Ident::new(
                &format!("as_{}", variant_name.to_string().to_lowercase()),
                name.span(),
            ),
            Fields::Named(_) => continue,
        };
        let key = LitStr::new(&format!("#{variant_name}"), name.span());
        match &variant.fields {
            Fields::Unit => {
                methods.push(quote! {
                    fn #method(&self) -> ::reactive_stores::Field<bool>;
                });
                let method_s = method.clone();
                let key_s = key.clone();
                let v = variant_name.clone();
                store_methods.push(quote! {
                    fn #method_s(&self) -> ::reactive_stores::Field<bool> {
                        self.project(
                            #key_s,
                            |value| matches!(value, #name::#v),
                            |value, next| { if next { *value = #name::#v; } },
                        )
                    }
                });
                let method_f = method.clone();
                let key_f = key.clone();
                let vf = variant_name.clone();
                field_methods.push(quote! {
                    fn #method_f(&self) -> ::reactive_stores::Field<bool> {
                        self.project(
                            #key_f,
                            |value| matches!(value, #name::#vf),
                            |value, next| { if next { *value = #name::#vf; } },
                        )
                    }
                });
            }
            Fields::Unnamed(fields) => {
                let types: Vec<_> = fields.unnamed.iter().map(|f| f.ty.clone()).collect();
                let type_tuple = if types.len() == 1 {
                    let ty = &types[0];
                    quote! { #ty }
                } else {
                    quote! { ( #(#types),* ) }
                };
                methods.push(quote! {
                    fn #method(&self) -> ::reactive_stores::Field<Option<#type_tuple>>;
                });
                let method_s = method.clone();
                let key_s = key.clone();
                let v = variant_name.clone();
                let bindings: Vec<_> = (0..types.len())
                    .map(|i| syn::Ident::new(&format!("v{i}"), name.span()))
                    .collect();
                let value_expr = if bindings.len() == 1 {
                    let b = &bindings[0];
                    quote! { #b.clone() }
                } else {
                    quote! { ( #( #bindings.clone() ),* ) }
                };
                let set_bindings = bindings.clone();
                let set_values: Vec<_> = bindings.iter().map(|b| quote! { #b }).collect();
                store_methods.push(quote! {
                    fn #method_s(&self) -> ::reactive_stores::Field<Option<#type_tuple>> {
                        self.project(
                            #key_s,
                            |value| match value {
                                #name::#v( #(#bindings),* ) => Some(#value_expr),
                                _ => None,
                            },
                            |value, next| {
                                if let Some( ( #(#set_bindings),* ) ) = next {
                                    *value = #name::#v( #(#set_values),* );
                                }
                            },
                        )
                    }
                });
                let method_f = method.clone();
                let key_f = key.clone();
                let vf = variant_name.clone();
                let bindings: Vec<_> = (0..types.len())
                    .map(|i| syn::Ident::new(&format!("f{i}"), name.span()))
                    .collect();
                let value_expr = if bindings.len() == 1 {
                    let b = &bindings[0];
                    quote! { #b.clone() }
                } else {
                    quote! { ( #( #bindings.clone() ),* ) }
                };
                let set_bindings = bindings.clone();
                let set_values: Vec<_> = bindings.iter().map(|b| quote! { #b }).collect();
                field_methods.push(quote! {
                    fn #method_f(&self) -> ::reactive_stores::Field<Option<#type_tuple>> {
                        self.project(
                            #key_f,
                            |value| match value {
                                #name::#vf( #(#bindings),* ) => Some(#value_expr),
                                _ => None,
                            },
                            |value, next| {
                                if let Some( ( #(#set_bindings),* ) ) = next {
                                    *value = #name::#vf( #(#set_values),* );
                                }
                            },
                        )
                    }
                });
            }
            Fields::Named(_) => {}
        }
    }

    quote! {
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
    }
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
