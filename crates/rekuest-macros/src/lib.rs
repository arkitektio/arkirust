//! Proc-macros for rekuest / arkitekt.
//!
//! `#[action]` turns a plain function into an Arkitekt action:
//!
//! ```ignore
//! /// Greet someone
//! ///
//! /// Says hello, possibly several times.
//! ///
//! /// # Arguments
//! /// * `name` - Who to greet
//! /// * `times` - How often
//! ///
//! /// # Returns
//! /// The greeting
//! #[arkitekt::action]
//! async fn greet(name: String, #[port(default = 1)] times: i64) -> String {
//!     name.repeat(times as usize)
//! }
//! ```
//!
//! The function is replaced by a unit struct of the same name that
//! implements `Action` (register it with `App::action(greet)`); the original
//! body stays callable as `greet::call(..)`.
//!
//! * Every parameter is a port, typed through `PortType`, except
//!   `#[inject]` parameters (service clients, looked up by type) and a
//!   parameter of type `Task`.
//! * `#[port(description = "…", label = "…", default = <expr>)]` refines a port.
//! * The return value becomes `return0` (a tuple fans out into
//!   `return0..n`, `()` has no returns). `Result<T, E>` is unwrapped; an
//!   `Err` reports the task as CRITICAL.
//! * Returning `impl Stream<Item = T>` (or `BoxStream<T>`) makes a GENERATOR
//!   that yields once per item.
//! * Doc comments give the description (first section), port descriptions
//!   (`# Arguments` bullets) and the return description (`# Returns`). An
//!   undocumented action is marked as in development.

use heck::ToTitleCase;
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Expr, ExprArray, FnArg, GenericArgument, Ident, ItemFn, LitStr, Pat, Path,
    PathArguments, ReturnType, Type, TypeParamBound,
};

mod service;

/// Turn a builder function into an arkitekt `Service`:
///
/// ```ignore
/// /// Mikro: the user's images, files and metadata.
/// #[arkitekt::service(name = "mikro")]
/// pub async fn service(
///     #[require("live.arkitekt.mikro", "Where the user's images live")] mikro: Alias,
///     #[require("live.arkitekt.s3")] s3: Option<Alias>,
///     fakts: Fakts,
/// ) -> anyhow::Result<Mikro> {
///     ..
/// }
/// ```
///
/// The function is replaced by a unit struct of the same name that
/// implements `Service` (use it with `App::service(service)`); the body stays
/// callable as `service::call(..)`.
///
/// * Each `#[require(service, description?)]` parameter is a requirement
///   keyed by the parameter name, resolved to an `Alias` before the body
///   runs. `Option<Alias>` makes it optional (`None` when not granted).
/// * A `Fakts` parameter receives the app's fakts (e.g. as token loader).
/// * The return value (`T` or `Result<T, E>`) is inserted into the context,
///   so actions can `#[inject]` it.
/// * `name = "…"` names the service (default: the function name).
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut options = service::ServiceOptions::default();
    let parser = syn::meta::parser(|meta| options.parse(meta));
    parse_macro_input!(attr with parser);
    let function = parse_macro_input!(item as ItemFn);
    match service::expand_service(options, function) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut options = ActionOptions::default();
    let parser = syn::meta::parser(|meta| options.parse(meta));
    parse_macro_input!(attr with parser);
    let function = parse_macro_input!(item as ItemFn);
    match expand_action(options, function) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct ActionOptions {
    name: Option<LitStr>,
    description: Option<LitStr>,
    interface: Option<LitStr>,
    collections: Vec<LitStr>,
    locks: Vec<LitStr>,
    parallel: bool,
    auto_locks: bool,
    krate: Option<Path>,
}

fn string_list(array: ExprArray, what: &str) -> syn::Result<Vec<LitStr>> {
    array
        .elems
        .into_iter()
        .map(|elem| match elem {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Ok(s),
            other => Err(syn::Error::new(
                other.span(),
                format!("{what} must be string literals"),
            )),
        })
        .collect()
}

impl Default for ActionOptions {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            interface: None,
            collections: vec![],
            locks: vec![],
            parallel: false,
            auto_locks: true,
            krate: None,
        }
    }
}

impl ActionOptions {
    fn parse(&mut self, meta: syn::meta::ParseNestedMeta) -> syn::Result<()> {
        if meta.path.is_ident("name") {
            self.name = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("description") {
            self.description = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("interface") {
            self.interface = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("collections") {
            self.collections = string_list(meta.value()?.parse()?, "collections")?;
        } else if meta.path.is_ident("locks") {
            self.locks = string_list(meta.value()?.parse()?, "locks")?;
        } else if meta.path.is_ident("concurrency") {
            let value: LitStr = meta.value()?.parse()?;
            self.parallel = match value.value().as_str() {
                "serial" => false,
                "parallel" => true,
                _ => {
                    return Err(syn::Error::new(
                        value.span(),
                        "concurrency is \"serial\" or \"parallel\"",
                    ))
                }
            };
        } else if meta.path.is_ident("auto_locks") {
            let value: syn::LitBool = meta.value()?.parse()?;
            self.auto_locks = value.value;
        } else if meta.path.is_ident("crate") {
            self.krate = Some(meta.value()?.parse()?);
        } else {
            return Err(meta.error("unknown action option; expected name, description, interface, collections, locks, concurrency, auto_locks or crate"));
        }
        Ok(())
    }
}

/// Where the rekuest runtime lives for the crate being compiled.
fn rekuest_path() -> TokenStream2 {
    match crate_name("rekuest") {
        Ok(FoundCrate::Itself) => return quote!(::rekuest),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            return quote!(::#ident);
        }
        Err(_) => {}
    }
    match crate_name("arkitekt") {
        Ok(FoundCrate::Itself) => quote!(::arkitekt::__private::rekuest),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident::__private::rekuest)
        }
        Err(_) => quote!(::rekuest),
    }
}

/// The parsed doc comment.
#[derive(Default)]
struct Docs {
    description: Option<String>,
    args: Vec<(String, String)>,
    returns: Option<String>,
}

fn parse_docs(attrs: &[syn::Attribute]) -> Docs {
    let mut lines = vec![];
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                let value = s.value();
                if value.trim().is_empty() {
                    // A bare `///` is a paragraph break.
                    lines.push(String::new());
                }
                for line in value.lines() {
                    lines.push(line.strip_prefix(' ').unwrap_or(line).trim_end().to_owned());
                }
            }
        }
    }

    enum Section {
        Description,
        Args,
        Returns,
        Other,
    }
    let mut docs = Docs::default();
    let mut section = Section::Description;
    let mut description = vec![];
    let mut returns = vec![];
    for line in lines {
        if let Some(heading) = line.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim().to_lowercase();
            section = match heading.as_str() {
                "arguments" | "args" | "parameters" | "params" => Section::Args,
                "returns" | "return" => Section::Returns,
                _ => Section::Other,
            };
            continue;
        }
        match section {
            Section::Description => description.push(line),
            Section::Returns => returns.push(line),
            Section::Args => {
                let bullet = line.trim_start();
                let Some(rest) = bullet
                    .strip_prefix('*')
                    .or_else(|| bullet.strip_prefix('-'))
                else {
                    // A continuation line of the previous argument.
                    if let (Some(last), false) = (docs.args.last_mut(), bullet.is_empty()) {
                        last.1.push(' ');
                        last.1.push_str(bullet);
                    }
                    continue;
                };
                let rest = rest.trim();
                let split = rest
                    .find(" - ")
                    .map(|i| (i, 3))
                    .or_else(|| rest.find(':').map(|i| (i, 1)));
                if let Some((i, len)) = split {
                    let name = rest[..i].trim().trim_matches('`').to_owned();
                    let text = rest[i + len..].trim().to_owned();
                    docs.args.push((name, text));
                }
            }
            Section::Other => {}
        }
    }
    let join = |lines: Vec<String>| {
        let text = lines.join("\n").trim().to_owned();
        (!text.is_empty()).then_some(text)
    };
    docs.description = join(description);
    docs.returns = join(returns);
    docs
}

enum ParamKind {
    Port {
        description: Option<LitStr>,
        label: Option<LitStr>,
        default: Option<Expr>,
    },
    Inject,
    Task,
    /// `StateMut<T>` (write) or `StateRef<T>` (read-only).
    State {
        state: Type,
        write: bool,
    },
}

struct Param {
    ident: Ident,
    key: String,
    ty: Type,
    kind: ParamKind,
}

fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(p) => p.path.segments.last(),
        Type::Group(g) => last_segment(&g.elem),
        Type::Paren(p) => last_segment(&p.elem),
        _ => None,
    }
}

fn generic_types(segment: &syn::PathSegment) -> Vec<&Type> {
    match &segment.arguments {
        PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|a| match a {
                GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// `StateMut<T>` → `(T, true)`, `StateRef<T>` → `(T, false)`.
fn state_param(ty: &Type) -> Option<(Type, bool)> {
    let segment = last_segment(ty)?;
    let write = match segment.ident.to_string().as_str() {
        "StateMut" => true,
        "StateRef" => false,
        _ => return None,
    };
    generic_types(segment)
        .first()
        .map(|t| ((*t).clone(), write))
}

/// `Result<T, ..>` → `Some(T)`.
fn result_inner(ty: &Type) -> Option<Type> {
    let segment = last_segment(ty)?;
    if segment.ident != "Result" {
        return None;
    }
    generic_types(segment).first().map(|t| (*t).clone())
}

/// `impl Stream<Item = T>` or `BoxStream<'_, T>` → `Some(T)`.
fn stream_item(ty: &Type) -> Option<Type> {
    match ty {
        Type::ImplTrait(imp) => imp.bounds.iter().find_map(|b| match b {
            TypeParamBound::Trait(t) => {
                let segment = t.path.segments.last()?;
                if segment.ident != "Stream" {
                    return None;
                }
                match &segment.arguments {
                    PathArguments::AngleBracketed(args) => args.args.iter().find_map(|a| match a {
                        GenericArgument::AssocType(assoc) if assoc.ident == "Item" => {
                            Some(assoc.ty.clone())
                        }
                        _ => None,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }),
        _ => {
            let segment = last_segment(ty)?;
            if segment.ident == "BoxStream" || segment.ident == "LocalBoxStream" {
                generic_types(segment).last().map(|t| (*t).clone())
            } else {
                None
            }
        }
    }
}

/// The value types a return (or yielded item) fans out into.
fn fan_out(ty: &Type) -> Vec<Type> {
    match ty {
        Type::Tuple(t) => t.elems.iter().cloned().collect(),
        Type::Paren(p) => fan_out(&p.elem),
        other => vec![other.clone()],
    }
}

fn parse_params(function: &mut ItemFn) -> syn::Result<Vec<Param>> {
    let mut params = vec![];
    for input in function.sig.inputs.iter_mut() {
        let FnArg::Typed(typed) = input else {
            return Err(syn::Error::new(input.span(), "actions cannot take self"));
        };
        let Pat::Ident(pat) = &*typed.pat else {
            return Err(syn::Error::new(
                typed.pat.span(),
                "action parameters must be plain identifiers",
            ));
        };
        let ident = pat.ident.clone();
        let key = ident.to_string().trim_start_matches("r#").to_owned();

        let mut inject = false;
        let mut description = None;
        let mut label = None;
        let mut default = None;
        let mut error = None;
        typed.attrs.retain(|attr| {
            if attr.path().is_ident("inject") {
                inject = true;
                false
            } else if attr.path().is_ident("port") {
                let result = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("description") {
                        description = Some(meta.value()?.parse()?);
                    } else if meta.path.is_ident("label") {
                        label = Some(meta.value()?.parse()?);
                    } else if meta.path.is_ident("default") {
                        default = Some(meta.value()?.parse()?);
                    } else {
                        return Err(meta
                            .error("unknown port option; expected description, label or default"));
                    }
                    Ok(())
                });
                if let Err(e) = result {
                    error = Some(e);
                }
                false
            } else {
                true
            }
        });
        if let Some(e) = error {
            return Err(e);
        }

        let ty = (*typed.ty).clone();
        let kind = if inject {
            ParamKind::Inject
        } else if last_segment(&ty).is_some_and(|s| s.ident == "Task") {
            ParamKind::Task
        } else if let Some((state, write)) = state_param(&ty) {
            ParamKind::State { state, write }
        } else {
            if key == "value" {
                return Err(syn::Error::new(
                    pat.ident.span(),
                    "'value' is a reserved port key (it names a port's own value in validators and effects); rename the parameter",
                ));
            }
            ParamKind::Port {
                description,
                label,
                default,
            }
        };
        params.push(Param {
            ident,
            key,
            ty,
            kind,
        });
    }
    Ok(params)
}

fn expand_action(options: ActionOptions, mut function: ItemFn) -> syn::Result<TokenStream2> {
    let rk = options
        .krate
        .as_ref()
        .map(|p| quote!(#p))
        .unwrap_or_else(rekuest_path);
    let p = quote!(#rk::__private);

    if !function.sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            function.sig.generics.span(),
            "actions cannot be generic",
        ));
    }

    let docs = parse_docs(&function.attrs);
    let params = parse_params(&mut function)?;

    let fn_ident = function.sig.ident.clone();
    let fn_name = fn_ident.to_string().trim_start_matches("r#").to_owned();
    let vis = function.vis.clone();
    let is_async = function.sig.asyncness.is_some();

    let key = fn_name.clone();
    let interface = options
        .interface
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| fn_name.clone());
    let name = options
        .name
        .as_ref()
        .map(LitStr::value)
        .unwrap_or_else(|| fn_name.to_title_case());
    let description = options
        .description
        .as_ref()
        .map(LitStr::value)
        .or(docs.description.clone());
    let is_dev = description.is_none();
    let description = description.unwrap_or_else(|| "No Description".into());
    let collections = &options.collections;

    // ---- return analysis -------------------------------------------------
    let ret_ty: Option<Type> = match &function.sig.output {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some((**ty).clone()),
    };
    let (outer_is_result, value_ty) = match &ret_ty {
        None => (false, None),
        Some(ty) => match result_inner(ty) {
            Some(inner) => (true, Some(inner)),
            None => (false, Some(ty.clone())),
        },
    };
    let stream = value_ty.as_ref().and_then(stream_item);
    let is_generator = stream.is_some();
    let (item_is_result, item_ty) = match &stream {
        Some(item) => match result_inner(item) {
            Some(inner) => (true, Some(inner)),
            None => (false, Some(item.clone())),
        },
        None => (false, value_ty.clone()),
    };
    let returns: Vec<Type> = item_ty.as_ref().map(fan_out).unwrap_or_default();

    // ---- definition ------------------------------------------------------
    let doc_for = |key: &str| {
        docs.args
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, d)| d.clone())
    };
    let arg_ports = params.iter().filter_map(|param| {
        let ParamKind::Port {
            description,
            label,
            default,
        } = &param.kind
        else {
            return None;
        };
        let key = &param.key;
        let ty = &param.ty;
        let description = description
            .as_ref()
            .map(LitStr::value)
            .or_else(|| doc_for(key));
        let describe = description.map(|d| quote!(let port = port.describe(#d);));
        let label = label.as_ref().map(|l| quote!(let port = port.label(#l);));
        let default = default
            .as_ref()
            .map(|d| quote!(let port = port.default_value(#p::serde_json::json!(#d));));
        Some(quote_spanned! {ty.span()=> {
            let port: #p::Port = <#ty as #p::PortType>::port(#key);
            #describe
            #label
            #default
            port
        }})
    });
    let return_ports = returns.iter().enumerate().map(|(i, ty)| {
        let key = format!("return{i}");
        let describe = (i == 0)
            .then(|| docs.returns.clone())
            .flatten()
            .map(|d| quote!(let port = port.describe(#d);));
        quote_spanned! {ty.span()=> {
            let port: #p::Port = <#ty as #p::PortType>::port(#key).into_return();
            #describe
            port
        }}
    });
    let kind = if is_generator {
        quote!(#p::ActionKind::Generator)
    } else {
        quote!(#p::ActionKind::Function)
    };

    // ---- run -------------------------------------------------------------
    let bindings = params.iter().map(|param| {
        let ident = &param.ident;
        let ty = &param.ty;
        let key = &param.key;
        match &param.kind {
            ParamKind::Port { default, .. } => {
                let fallback = match default {
                    Some(d) => quote!(#p::serde_json::json!(#d)),
                    None => quote!(#p::serde_json::Value::Null),
                };
                quote_spanned! {ty.span()=>
                    let #ident: #ty = <#ty as #p::PortType>::expand(
                        __rk_args.remove(#key).filter(|v| !v.is_null()).unwrap_or_else(|| #fallback),
                        &__rk_ctx,
                    )
                    .await
                    .map_err(|e| #p::ActionError::Failed(e.at(#key).to_string()))?;
                }
            }
            ParamKind::Inject => quote_spanned! {ty.span()=>
                let #ident: #ty = __rk_ctx
                    .require::<#ty>()
                    .map_err(|e| #p::ActionError::Failed(e.to_string()))?;
            },
            ParamKind::Task => quote!(let #ident: #ty = __rk_task.clone();),
            ParamKind::State { state, write } => {
                let getter = if *write { quote!(state_mut) } else { quote!(state_ref) };
                quote_spanned! {ty.span()=>
                    let #ident: #ty = __rk_task
                        .#getter::<#state>()
                        .map_err(|e| #p::ActionError::Failed(e.to_string()))?;
                }
            }
        }
    });
    let call_args: Vec<&Ident> = params.iter().map(|param| &param.ident).collect();

    let invoke = if is_async {
        quote!(#fn_ident::call(#(#call_args),*).await)
    } else {
        quote! {
            #p::tokio::task::spawn_blocking(move || #fn_ident::call(#(#call_args),*))
                .await
                .map_err(|e| #p::ActionError::Critical(format!("the action panicked: {e}")))?
        }
    };
    let unwrap_outer =
        outer_is_result.then(|| quote!(let value = value.map_err(#p::ActionError::from_body)?;));
    let unwrap_item =
        item_is_result.then(|| quote!(let value = value.map_err(#p::ActionError::from_body)?;));

    let shrink_value = {
        let names: Vec<Ident> = (0..returns.len())
            .map(|i| format_ident!("__return{}", i))
            .collect();
        let destructure = match returns.len() {
            0 => quote!(let () = value;),
            1 if !matches!(item_ty, Some(Type::Tuple(_))) => {
                let n = &names[0];
                quote!(let #n = value;)
            }
            _ => quote!(let (#(#names,)*) = value;),
        };
        let inserts = names.iter().enumerate().map(|(i, n)| {
            let key = format!("return{i}");
            quote! {
                returns.insert(
                    #key.to_owned(),
                    #p::PortType::shrink(#n, &__rk_ctx)
                        .await
                        .map_err(|e| #p::ActionError::Failed(e.at(#key).to_string()))?,
                );
            }
        });
        quote! {
            #unwrap_item
            #destructure
            let mut returns = #p::serde_json::Map::new();
            #(#inserts)*
            __rk_task.yield_returns(returns);
        }
    };

    let body = if is_generator {
        quote! {
            let value = #invoke;
            #unwrap_outer
            let mut stream = ::std::pin::pin!(value);
            while let Some(value) = #p::futures::StreamExt::next(&mut stream).await {
                #shrink_value
            }
        }
    } else {
        quote! {
            let value = #invoke;
            #unwrap_outer
            #shrink_value
        }
    };

    // ---- states, locks, concurrency ----------------------------------------
    let state_params: Vec<(&Type, bool)> = params
        .iter()
        .filter_map(|param| match &param.kind {
            ParamKind::State { state, write } => Some((state, *write)),
            _ => None,
        })
        .collect();
    let stateful = !state_params.is_empty();
    let written_states: Vec<&Type> = state_params
        .iter()
        .filter(|(_, w)| *w)
        .map(|(t, _)| *t)
        .collect();
    // As in Python, locks are inferred from every state an action takes, read-only or not.
    let lock_states: Vec<&Type> = if options.auto_locks {
        state_params.iter().map(|(t, _)| *t).collect()
    } else {
        vec![]
    };
    let declared_locks = &options.locks;
    let concurrency = if options.parallel {
        quote!(#p::Concurrency::Parallel)
    } else {
        quote!(#p::Concurrency::Serial)
    };

    // ---- output ----------------------------------------------------------
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
            /// Call the action's function directly.
            #(#other_attrs)*
            pub #sig #block
        }

        impl #p::Action for #fn_ident {
            fn interface(&self) -> ::std::string::String {
                #interface.to_owned()
            }

            fn locks(&self) -> ::std::vec::Vec<::std::string::String> {
                let mut locks: ::std::vec::Vec<::std::string::String> =
                    ::std::vec![#(#declared_locks.to_owned()),*];
                #(
                    locks.extend(
                        <#lock_states as #p::StateType>::REQUIRED_LOCKS.iter().map(|l| l.to_string()),
                    );
                )*
                locks.sort();
                locks.dedup();
                locks
            }

            fn manipulates(&self) -> ::std::vec::Vec<::std::string::String> {
                let mut states: ::std::vec::Vec<::std::string::String> =
                    ::std::vec![#(<#written_states as #p::StateType>::NAME.to_owned()),*];
                states.sort();
                states.dedup();
                states
            }

            fn concurrency(&self) -> #p::Concurrency {
                #concurrency
            }

            fn definition(&self) -> #p::Definition {
                let mut definition = #p::Definition::new(#key, #name, #kind);
                definition.description = ::std::option::Option::Some(#description.to_owned());
                definition.is_dev = #is_dev;
                definition.stateful = #stateful;
                definition.collections = ::std::vec![#(#collections.to_owned()),*];
                definition.args = ::std::vec![#(#arg_ports),*];
                definition.returns = ::std::vec![#(#return_ports),*];
                definition
            }

            #[allow(unused_mut, unused_variables)]
            fn run(
                &self,
                mut __rk_args: #p::serde_json::Map<::std::string::String, #p::serde_json::Value>,
                __rk_ctx: #p::Context,
                __rk_task: #p::Task,
            ) -> #p::futures::future::BoxFuture<'static, ::std::result::Result<(), #p::ActionError>> {
                ::std::boxed::Box::pin(async move {
                    #(#bindings)*
                    #body
                    ::std::result::Result::Ok(())
                })
            }
        }
    })
}

/// Make a struct an agent state.
///
/// ```ignore
/// #[derive(Clone, Serialize, Deserialize, State)]
/// #[state(name = "CameraState", locks = ["camera"])]
/// struct CameraState {
///     /// Whether the camera is connected
///     connected: bool,
///     exposure_ms: f64,
/// }
/// ```
///
/// * `name` is the state's interface (default: the struct name).
/// * `locks` are the locks an action must hold to change the state; actions
///   taking the state hold them automatically.
/// * Each field becomes a port (typed through `PortType`); a field's doc
///   comment is its description. Values travel as their serde JSON, so fields
///   should not be structures (e.g. an `ArrayDataset`) and should not be
///   renamed with serde.
#[proc_macro_derive(State, attributes(state))]
pub fn derive_state(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    match expand_state(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_state(input: syn::DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let mut name = ident.to_string();
    let mut locks: Vec<LitStr> = vec![];
    let mut krate: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("state")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: LitStr = meta.value()?.parse()?;
                name = value.value();
            } else if meta.path.is_ident("locks") {
                locks = string_list(meta.value()?.parse()?, "locks")?;
            } else if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("unknown state option; expected name, locks or crate"));
            }
            Ok(())
        })?;
    }
    let rk = krate
        .as_ref()
        .map(|p| quote!(#p))
        .unwrap_or_else(rekuest_path);
    let p = quote!(#rk::__private);

    let syn::Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            ident.span(),
            "a state must be a struct with named fields",
        ));
    };
    let syn::Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            ident.span(),
            "a state must be a struct with named fields",
        ));
    };

    let ports = fields.named.iter().map(|field| {
        let key = field
            .ident
            .as_ref()
            .expect("named field")
            .to_string()
            .trim_start_matches("r#")
            .to_owned();
        let ty = &field.ty;
        let describe = parse_docs(&field.attrs)
            .description
            .map(|d| quote!(let port = port.describe(#d);));
        quote_spanned! {ty.span()=> {
            let port: #p::Port = <#ty as #p::PortType>::port(#key).into_return();
            #describe
            port
        }}
    });

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #p::StateType for #ident #ty_generics #where_clause {
            const NAME: &'static str = #name;
            const REQUIRED_LOCKS: &'static [&'static str] = &[#(#locks),*];

            fn ports() -> ::std::vec::Vec<#p::Port> {
                ::std::vec![#(#ports),*]
            }
        }
    })
}
