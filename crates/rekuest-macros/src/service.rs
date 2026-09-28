//! `#[service]`: a builder function whose parameters are the aliases it needs.

use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{FnArg, Ident, ItemFn, LitStr, Pat, Path, ReturnType, Type};

use crate::{generic_types, last_segment, result_inner};

#[derive(Default)]
pub(crate) struct ServiceOptions {
    name: Option<LitStr>,
    krate: Option<Path>,
}

impl ServiceOptions {
    pub(crate) fn parse(&mut self, meta: syn::meta::ParseNestedMeta) -> syn::Result<()> {
        if meta.path.is_ident("name") {
            self.name = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("crate") {
            self.krate = Some(meta.value()?.parse()?);
        } else {
            return Err(meta.error("unknown service option; expected name or crate"));
        }
        Ok(())
    }
}

/// Where the arkitekt crate lives for the crate being compiled.
fn arkitekt_path() -> TokenStream2 {
    match crate_name("arkitekt") {
        Ok(FoundCrate::Itself) => quote!(::arkitekt),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident)
        }
        Err(_) => quote!(::arkitekt),
    }
}

enum ServiceParam {
    /// `#[require(service, description?)]` on `Alias` or `Option<Alias>`.
    Alias {
        key: String,
        service: LitStr,
        description: Option<LitStr>,
        optional: bool,
    },
    /// Any other parameter must be `Fakts`.
    Fakts,
}

/// `Option<T>` → `Some(T)`.
fn option_inner(ty: &Type) -> Option<Type> {
    let segment = last_segment(ty)?;
    if segment.ident != "Option" {
        return None;
    }
    generic_types(segment).first().map(|t| (*t).clone())
}

fn is_named(ty: &Type, name: &str) -> bool {
    last_segment(ty).is_some_and(|s| s.ident == name)
}

fn parse_require(attr: &syn::Attribute) -> syn::Result<(LitStr, Option<LitStr>)> {
    let args = attr
        .parse_args_with(syn::punctuated::Punctuated::<LitStr, syn::Token![,]>::parse_terminated)?;
    let mut args = args.into_iter();
    let service = args.next().ok_or_else(|| {
        syn::Error::new(
            attr.span(),
            "#[require] needs the service identifier, e.g. \"live.arkitekt.mikro\"",
        )
    })?;
    let description = args.next();
    if let Some(extra) = args.next() {
        return Err(syn::Error::new(
            extra.span(),
            "#[require] takes a service and an optional description",
        ));
    }
    Ok((service, description))
}

fn parse_params(function: &mut ItemFn) -> syn::Result<Vec<ServiceParam>> {
    let mut params = vec![];
    let fn_ident = function.sig.ident.clone();
    for input in function.sig.inputs.iter_mut() {
        let FnArg::Typed(typed) = input else {
            return Err(syn::Error::new(input.span(), "services cannot take self"));
        };
        let Pat::Ident(pat) = &*typed.pat else {
            return Err(syn::Error::new(
                typed.pat.span(),
                "service parameters must be plain identifiers",
            ));
        };
        let key = pat.ident.to_string().trim_start_matches("r#").to_owned();
        if pat.ident == fn_ident {
            return Err(syn::Error::new(
                pat.ident.span(),
                "a parameter cannot share the service's name (the function becomes a unit struct of that name)",
            ));
        }

        let mut require = None;
        let mut error = None;
        typed.attrs.retain(|attr| {
            if !attr.path().is_ident("require") {
                return true;
            }
            match parse_require(attr) {
                Ok(parsed) => require = Some(parsed),
                Err(e) => error = Some(e),
            }
            false
        });
        if let Some(e) = error {
            return Err(e);
        }

        let param = match require {
            Some((service, description)) => {
                let (optional, inner) = match option_inner(&typed.ty) {
                    Some(inner) => (true, inner),
                    None => (false, (*typed.ty).clone()),
                };
                if !is_named(&inner, "Alias") {
                    return Err(syn::Error::new(typed.ty.span(), "#[require] parameters must be `Alias` or `Option<Alias>`"));
                }
                ServiceParam::Alias {
                    key,
                    service,
                    description,
                    optional,
                }
            }
            None if is_named(&typed.ty, "Fakts") => ServiceParam::Fakts,
            None => {
                return Err(syn::Error::new(
                    typed.ty.span(),
                    "service parameters are `#[require(..)] Alias`, `#[require(..)] Option<Alias>` or `Fakts`",
                ))
            }
        };
        params.push(param);
    }
    Ok(params)
}

pub(crate) fn expand_service(
    options: ServiceOptions,
    mut function: ItemFn,
) -> syn::Result<TokenStream2> {
    let ark = options
        .krate
        .as_ref()
        .map(|p| quote!(#p))
        .unwrap_or_else(arkitekt_path);
    let p = quote!(#ark::__private);

    if !function.sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            function.sig.generics.span(),
            "services cannot be generic",
        ));
    }
    let params = parse_params(&mut function)?;

    let fn_ident = function.sig.ident.clone();
    let fn_name = fn_ident.to_string().trim_start_matches("r#").to_owned();
    let name = options.name.as_ref().map(LitStr::value).unwrap_or(fn_name);
    let vis = function.vis.clone();

    let returns_result = match &function.sig.output {
        ReturnType::Default => {
            return Err(syn::Error::new(
                function.sig.span(),
                "a service returns the client it builds",
            ))
        }
        ReturnType::Type(_, ty) => result_inner(ty).is_some(),
    };

    let requirements = params.iter().filter_map(|param| {
        let ServiceParam::Alias {
            key,
            service,
            description,
            optional,
            ..
        } = param
        else {
            return None;
        };
        let describe = description.as_ref().map(|d| quote!(.description(#d)));
        Some(quote!(#p::fakts::Requirement::new(#key, #service).optional(#optional) #describe))
    });

    // Bind to generated names so they cannot collide with anything in scope.
    let locals: Vec<Ident> = (0..params.len())
        .map(|i| format_ident!("__ark_arg{i}"))
        .collect();
    let bindings = params.iter().zip(&locals).map(|(param, local)| match param {
        ServiceParam::Alias { key, optional: false, .. } => quote! {
            let #local = __ark_fakts.get_alias(#key).await?;
        },
        ServiceParam::Alias { key, optional: true, .. } => quote! {
            let #local = match __ark_fakts.get_alias(#key).await {
                ::std::result::Result::Ok(alias) => ::std::option::Option::Some(alias),
                ::std::result::Result::Err(#p::fakts::FaktsError::MissingInstance { .. }) => ::std::option::Option::None,
                ::std::result::Result::Err(e) => return ::std::result::Result::Err(e.into()),
            };
        },
        ServiceParam::Fakts => quote! {
            let #local = __ark_fakts.clone();
        },
    });
    let args = locals.iter();

    let call = if function.sig.asyncness.is_some() {
        quote!(#fn_ident::call(#(#args),*).await)
    } else {
        quote!(#fn_ident::call(#(#args),*))
    };
    let call = if returns_result { quote!(#call?) } else { call };

    let doc_attrs: Vec<_> = function
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .collect();
    let other_attrs: Vec<_> = function
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("doc"))
        .collect();
    let sig = {
        let mut sig = function.sig.clone();
        sig.ident = format_ident!("call");
        sig
    };
    let block = &function.block;

    Ok(quote! {
        #(#doc_attrs)*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, Default)]
        #vis struct #fn_ident;

        impl #fn_ident {
            /// Call the service's builder directly.
            #(#other_attrs)*
            pub #sig #block
        }

        #[#p::async_trait]
        impl #p::Service for #fn_ident {
            fn name(&self) -> &'static str {
                #name
            }

            fn requirements(&self) -> ::std::vec::Vec<#p::fakts::Requirement> {
                ::std::vec![#(#requirements),*]
            }

            async fn build(
                &self,
                __ark_fakts: &#p::fakts::Fakts,
                __ark_clients: &mut #p::rekuest::ContextBuilder,
            ) -> #p::anyhow::Result<()> {
                #(#bindings)*
                let client = #call;
                __ark_clients.insert(client);
                ::std::result::Result::Ok(())
            }
        }
    })
}
