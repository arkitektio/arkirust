use std::any::Any;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, token::Type, FnArg, ItemFn, Pat, ReturnType};

#[proc_macro_attribute]
pub fn json_types(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let vis = &input.vis;
    let sig = &input.sig;
    let func_name = &sig.ident;
    let block = &input.block;

    // Extract parameter info
    let mut params = Vec::new();
    let mut call_inputs = Vec::new();
    for arg in &sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name_str = pat_ident.ident.to_string();
                print!("arg_name_str: {}", pat_ident.mutability.is_some());

                let ty = &pat_type.ty;
                let type_str = quote!(#ty).to_string();

                let type_str = type_str.trim();
                let arg_type_str = match () {
                    _ if [
                        "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128",
                    ]
                    .iter()
                    .any(|&x| type_str.contains(x)) =>
                    {
                        "int"
                    }
                    _ if ["f32", "f64"].iter().any(|&x| type_str.contains(x)) => "float",
                    _ if type_str.contains("bool") => "bool",
                    _ if type_str.contains("String") || type_str.contains("str") => "string",
                    _ if type_str.contains("Vec") || type_str.contains("Array") => "list",
                    _ if type_str.contains("HashMap") || type_str.contains("Map") => "dict",
                    _ => "String",
                }
                .to_string();

                params.push((arg_name_str, arg_type_str));
                let pat = &pat_ident.ident;
                let ty = &pat_type.ty;
                call_inputs.push(quote! { #pat: #ty });
            } else {
                return syn::Error::new_spanned(
                    arg,
                    "Only simple identifier parameters are supported.",
                )
                .to_compile_error()
                .into();
            }
        } else {
            return syn::Error::new_spanned(
                arg,
                "Methods with a `self` receiver are not supported.",
            )
            .to_compile_error()
            .into();
        }
    }

    // Determine return type
    let return_type_str = match &sig.output {
        ReturnType::Default => "void".to_string(),
        ReturnType::Type(_, ty) => {
            let type_str = quote!(#ty).to_string();
            match () {
                _ if [
                    "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128",
                ]
                .iter()
                .any(|&x| type_str.contains(x)) =>
                {
                    "int"
                }
                _ if ["f32", "f64"].iter().any(|&x| type_str.contains(x)) => "float",
                _ if type_str.contains("bool") => "bool",
                _ if type_str.contains("String") || type_str.contains("str") => "string",
                _ if type_str.contains("Vec") || type_str.contains("Array") => "list",
                _ if type_str.contains("HashMap") || type_str.contains("Map") => "dict",
                _ => "String",
            }
            .to_string()
        }
    };

    // Construct JSON describing the parameters and return type
    let mut json_str = String::from(r#"{"name":""#);
    json_str.push_str(&func_name.to_string());
    json_str.push_str(r#"","args":["#);
    for (i, (n, t)) in params.iter().enumerate() {
        if i > 0 {
            json_str.push_str(",");
        }
        json_str.push_str(&format!(r#"{{"name":"{}","kind":"{}"}}"#, n, t));
    }
    json_str.push_str(r#"],"return_type":""#);
    json_str.push_str(&return_type_str);
    json_str.push_str(r#""}"#);
    let json_literal = syn::LitStr::new(&json_str, proc_macro2::Span::call_site());

    let output_type = match &sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };

    let expanded = quote! {
        #[allow(non_camel_case_types)]
        #vis struct #func_name;

        impl #func_name {
            #vis fn inspect() -> &'static str {
                #json_literal
            }

            #vis fn call(#(#call_inputs),*) -> #output_type {
                #block
            }
        }
    };

    TokenStream::from(expanded)
}
