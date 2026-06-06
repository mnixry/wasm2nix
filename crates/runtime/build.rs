use std::{env, error::Error, fs, path::PathBuf};

use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::{format_ident, quote};

#[derive(Debug, Clone)]
struct RuntimeFn {
    path: Vec<String>,
    args: Vec<String>,
    result: RuntimeResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeResult {
    Value,
    State,
    Record,
}

impl RuntimeResult {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "value" => Ok(Self::Value),
            "state" => Ok(Self::State),
            "record" => Ok(Self::Record),
            other => Err(format!("unknown runtime result kind `{other}`")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::State => "state",
            Self::Record => "record",
        }
    }

    fn tokens(self) -> TokenStream {
        match self {
            Self::Value => quote!(crate::RuntimeResultKind::Value),
            Self::State => quote!(crate::RuntimeResultKind::State),
            Self::Record => quote!(crate::RuntimeResultKind::Record),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let runtime_path = manifest_dir.join("nix/rt.nix");
    println!("cargo:rerun-if-changed={}", runtime_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let runtime = fs::read_to_string(&runtime_path)?;
    let mut functions = parse_runtime_functions(&runtime)?;
    functions.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
    validate(&functions)?;

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR must be set")?);
    fs::write(
        out_dir.join("runtime-functions.json"),
        runtime_functions_json(&functions)?,
    )?;

    let generated = runtime_calls_tokens(&functions);
    syn::parse2::<syn::File>(generated.clone())?;
    fs::write(out_dir.join("runtime_calls.rs"), generated.to_string())?;

    Ok(())
}

fn parse_runtime_functions(source: &str) -> Result<Vec<RuntimeFn>, Box<dyn Error>> {
    let mut functions = Vec::new();
    for (line_index, line) in source.lines().enumerate() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("# wasm2nix:runtime-fn ") else {
            continue;
        };
        functions.push(parse_runtime_function(rest).map_err(|err| {
            format!(
                "invalid runtime function annotation at rt.nix:{}: {err}",
                line_index + 1
            )
        })?);
    }
    if functions.is_empty() {
        return Err("rt.nix contains no runtime function annotations".into());
    }
    Ok(functions)
}

fn parse_runtime_function(annotation: &str) -> Result<RuntimeFn, String> {
    let (head, result) = annotation
        .split_once(" -> ")
        .ok_or("missing ` -> result-kind`")?;
    let result = RuntimeResult::parse(result.trim())?;
    let open = head.find('(').ok_or("missing argument list")?;
    let close = head.rfind(')').ok_or("missing closing `)`")?;
    if close != head.len() - 1 {
        return Err("unexpected trailing content after argument list".into());
    }

    let path = head[..open]
        .split('.')
        .map(str::trim)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if path.is_empty() || path.iter().any(String::is_empty) {
        return Err("runtime path must contain non-empty segments".into());
    }

    let args = head[open + 1..close].trim();
    let args = if args.is_empty() {
        Vec::new()
    } else {
        args.split(',')
            .map(str::trim)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    for arg in &args {
        validate_rust_ident(arg).map_err(|err| format!("invalid arg `{arg}`: {err}"))?;
    }

    Ok(RuntimeFn { path, args, result })
}

fn validate(functions: &[RuntimeFn]) -> Result<(), Box<dyn Error>> {
    let mut paths = std::collections::BTreeSet::new();
    let mut variants = std::collections::BTreeSet::new();
    let mut helpers = std::collections::BTreeSet::new();

    for function in functions {
        let path = function.path.join(".");
        if !paths.insert(path.clone()) {
            return Err(format!("duplicate runtime path `{path}`").into());
        }

        let variant = variant_name(&function.path);
        validate_rust_ident(&variant)?;
        if !variants.insert(variant.clone()) {
            return Err(format!("duplicate runtime enum variant `{variant}`").into());
        }

        let helper = helper_name(&function.path);
        validate_rust_ident(&helper)?;
        if !helpers.insert(helper.clone()) {
            return Err(format!("duplicate runtime helper `{helper}`").into());
        }
    }

    Ok(())
}

fn validate_rust_ident(s: &str) -> Result<(), String> {
    syn::parse_str::<syn::Ident>(s)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn runtime_functions_json(functions: &[RuntimeFn]) -> Result<String, serde_json::Error> {
    let functions = functions
        .iter()
        .map(|function| {
            serde_json::json!({
                "path": function.path,
                "args": function.args,
                "result": function.result.as_str(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({ "functions": functions }))
}

fn runtime_calls_tokens(functions: &[RuntimeFn]) -> TokenStream {
    let variants = functions
        .iter()
        .map(|function| format_ident!("{}", variant_name(&function.path)))
        .collect::<Vec<_>>();
    let helper_idents = functions
        .iter()
        .map(|function| format_ident!("{}", helper_name(&function.path)))
        .collect::<Vec<_>>();
    let path_arms = functions
        .iter()
        .zip(&variants)
        .map(|(function, variant)| {
            let segments = string_literals(&function.path);
            quote!(Self::#variant => &[#(#segments),*])
        })
        .collect::<Vec<_>>();
    let arity_arms = grouped_arity_arms(functions, &variants);
    let result_arms = grouped_result_arms(functions, &variants);
    let helper_fns = functions
        .iter()
        .zip(&variants)
        .zip(&helper_idents)
        .map(|((function, variant), helper)| {
            let args = function
                .args
                .iter()
                .map(|arg| Ident::new(arg, Span::call_site()))
                .collect::<Vec<_>>();
            quote! {
                #[must_use]
                pub fn #helper(#(#args: wasm2nix_ast::Expr),*) -> wasm2nix_ast::Expr {
                    crate::RuntimeFunction::#variant.call(vec![#(#args),*])
                }
            }
        })
        .collect::<Vec<_>>();

    quote! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum RuntimeFunction {
            #(#variants),*
        }

        impl RuntimeFunction {
            #[must_use]
            pub const fn path(self) -> &'static [&'static str] {
                match self {
                    #(#path_arms),*
                }
            }

            #[must_use]
            pub const fn arity(self) -> usize {
                match self {
                    #(#arity_arms),*
                }
            }

            #[must_use]
            pub const fn result_kind(self) -> crate::RuntimeResultKind {
                match self {
                    #(#result_arms),*
                }
            }

            #[must_use]
            pub fn expr(self) -> wasm2nix_ast::Expr {
                let mut expr = wasm2nix_ast::Expr::ident("rt");
                for segment in self.path() {
                    expr = wasm2nix_ast::Expr::select(expr, wasm2nix_ast::AttrKey::ident(*segment));
                }
                expr
            }

            #[must_use]
            #[doc = "Builds a Nix expression that calls this runtime function."]
            #[doc = ""]
            #[doc = "# Panics"]
            #[doc = ""]
            #[doc = "Panics when `args.len()` does not match this runtime function's arity."]
            pub fn call(self, args: Vec<wasm2nix_ast::Expr>) -> wasm2nix_ast::Expr {
                assert_eq!(
                    args.len(),
                    self.arity(),
                    "runtime call argument count mismatch for {self:?}"
                );
                wasm2nix_ast::Expr::app(self.expr(), args)
            }
        }

        pub mod calls {
            #![allow(clippy::too_many_arguments)]

            #(#helper_fns)*
        }
    }
}

fn grouped_arity_arms(functions: &[RuntimeFn], variants: &[Ident]) -> Vec<TokenStream> {
    let mut variants_by_arity = std::collections::BTreeMap::<usize, Vec<&Ident>>::new();
    for (function, variant) in functions.iter().zip(variants) {
        variants_by_arity
            .entry(function.args.len())
            .or_default()
            .push(variant);
    }

    variants_by_arity
        .into_iter()
        .map(|(arity, variants)| quote!(#(Self::#variants)|* => #arity))
        .collect()
}

fn grouped_result_arms(functions: &[RuntimeFn], variants: &[Ident]) -> Vec<TokenStream> {
    [
        RuntimeResult::Value,
        RuntimeResult::State,
        RuntimeResult::Record,
    ]
    .into_iter()
    .filter_map(|result| {
        let variants = functions
            .iter()
            .zip(variants)
            .filter_map(|(function, variant)| (function.result == result).then_some(variant))
            .collect::<Vec<_>>();
        if variants.is_empty() {
            return None;
        }

        let result = result.tokens();
        Some(quote!(#(Self::#variants)|* => #result))
    })
    .collect()
}

fn string_literals(strings: &[String]) -> Vec<Literal> {
    strings.iter().map(|s| Literal::string(s)).collect()
}

fn variant_name(path: &[String]) -> String {
    path.iter()
        .flat_map(|segment| segment.split('_'))
        .map(to_pascal_segment)
        .collect::<String>()
}

fn helper_name(path: &[String]) -> String {
    path.iter()
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>()
        .join("_")
}

fn to_pascal_segment(segment: &str) -> String {
    let mut out = String::new();
    let mut chars = segment.chars();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            out.push(ch);
            continue;
        }
        out.push(ch.to_ascii_uppercase());
        out.extend(chars);
        break;
    }
    out
}
