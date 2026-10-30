//! Generates `RuntimeFunction` from the `# wasm2nix:runtime-fn` annotations
//! in `nix/rt.nix`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use snafu::OptionExt;
use snafu::ResultExt;
use snafu::Snafu;
use snafu::ensure;

const ANNOTATION: &str = "# wasm2nix:runtime-fn ";

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("failed to read {}", path.display()))]
    Read { path: PathBuf, source: io::Error },

    #[snafu(display("failed to write {}", path.display()))]
    Write { path: PathBuf, source: io::Error },

    #[snafu(display("OUT_DIR must be set"))]
    MissingOutDir,

    #[snafu(display("rt.nix contains no runtime function annotations"))]
    NoAnnotations,

    #[snafu(display("invalid runtime function annotation at rt.nix:{line}: {reason}"))]
    Annotation { line: usize, reason: &'static str },

    #[snafu(display("unknown runtime result kind `{kind}` at rt.nix:{line}"))]
    UnknownResultKind { line: usize, kind: String },

    #[snafu(display("duplicate runtime path `{path}`"))]
    DuplicatePath { path: String },

    #[snafu(display("runtime path `{path}` is not a valid Rust variant `{variant}`"))]
    InvalidVariant {
        path: String,
        variant: String,
        source: syn::Error,
    },

    #[snafu(display("duplicate runtime enum variant `{variant}`"))]
    DuplicateVariant { variant: String },
}

struct RuntimeFn {
    path: Vec<String>,
    variant: Ident,
    arity: usize,
    /// The `RuntimeResultKind` variant.
    result: Ident,
}

fn main() -> Result<(), Error> {
    let runtime_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("nix/rt.nix");
    println!("cargo:rerun-if-changed={}", runtime_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let runtime = fs::read_to_string(&runtime_path).context(ReadSnafu {
        path: &runtime_path,
    })?;
    let functions = parse_runtime_functions(&runtime)?;

    let out_path =
        PathBuf::from(env::var_os("OUT_DIR").context(MissingOutDirSnafu)?).join("runtime_calls.rs");
    let generated = runtime_calls_tokens(&functions).to_string();
    fs::write(&out_path, generated).context(WriteSnafu { path: out_path })
}

/// Parses every annotation, sorted by path.
fn parse_runtime_functions(source: &str) -> Result<Vec<RuntimeFn>, Error> {
    let mut functions = BTreeMap::new();
    let mut variants = BTreeSet::new();
    for (line, annotation) in (1..).zip(source.lines()) {
        let Some(annotation) = annotation.trim().strip_prefix(ANNOTATION) else {
            continue;
        };
        let function = parse_runtime_function(line, annotation)?;
        let path = function.path.join(".");
        ensure!(!functions.contains_key(&path), DuplicatePathSnafu { path });
        let variant = function.variant.to_string();
        ensure!(
            !variants.contains(&variant),
            DuplicateVariantSnafu { variant }
        );
        variants.insert(variant);
        functions.insert(path, function);
    }
    ensure!(!functions.is_empty(), NoAnnotationsSnafu);
    Ok(functions.into_values().collect())
}

/// Parses `a.b.c(arg, …) -> kind`.
fn parse_runtime_function(line: usize, annotation: &str) -> Result<RuntimeFn, Error> {
    let malformed = |reason| AnnotationSnafu { line, reason };
    let (head, kind) = annotation
        .split_once(" -> ")
        .context(malformed("missing ` -> result-kind`"))?;
    let result = match kind.trim() {
        "value" => format_ident!("Value"),
        "state" => format_ident!("State"),
        "record" => format_ident!("Record"),
        kind => return UnknownResultKindSnafu { line, kind }.fail(),
    };
    let (path, args) = head
        .strip_suffix(')')
        .and_then(|head| head.split_once('('))
        .context(malformed("expected `path(args)`"))?;

    let path = path.split('.').map(str::trim).collect::<Vec<_>>();
    ensure!(
        !path.iter().any(|segment| segment.is_empty()),
        malformed("runtime path must contain non-empty segments")
    );
    let args = args.trim();
    let arity = if args.is_empty() {
        0
    } else {
        args.split(',').count()
    };

    let variant = variant_name(&path);
    let variant = syn::parse_str::<Ident>(&variant).context(InvalidVariantSnafu {
        path: path.join("."),
        variant,
    })?;
    Ok(RuntimeFn {
        path: path.into_iter().map(str::to_string).collect(),
        variant,
        arity,
        result,
    })
}

/// `i32.trunc_sat_f32_s` becomes `I32TruncSatF32S`.
fn variant_name(path: &[&str]) -> String {
    path.iter()
        .flat_map(|segment| segment.split('_'))
        .flat_map(|word| {
            // Leading digits stay; the first letter after them is uppercased.
            let letter = word
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(word.len());
            let (digits, rest) = word.split_at(letter);
            let mut rest = rest.chars();
            let upper = rest.next().map(|c| c.to_ascii_uppercase());
            digits.chars().chain(upper).chain(rest)
        })
        .collect()
}

fn runtime_calls_tokens(functions: &[RuntimeFn]) -> TokenStream {
    let variants = functions.iter().map(|function| &function.variant);
    let path_arms = functions.iter().map(|function| {
        let variant = &function.variant;
        let segments = &function.path;
        quote!(Self::#variant => &[#(#segments),*])
    });
    let arity_arms = grouped_arms(functions, |function| {
        let arity = function.arity;
        quote!(#arity)
    });
    let result_arms = grouped_arms(functions, |function| {
        let kind = &function.result;
        quote!(crate::RuntimeResultKind::#kind)
    });
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

            /// `rt.<path>`.
            #[must_use]
            pub fn expr(self) -> wasm2nix_ast::Expr {
                crate::rt_path(self.path())
            }

            /// Calls this runtime function with exactly [`Self::arity`] arguments.
            #[must_use]
            pub fn call<const N: usize>(self, args: [wasm2nix_ast::Expr; N]) -> wasm2nix_ast::Expr {
                debug_assert_eq!(N, self.arity(), "runtime call arity mismatch for {self:?}");
                wasm2nix_ast::Expr::app(self.expr(), args)
            }
        }
    }
}

/// Match arms grouping variants by the tokens `value` renders for them.
fn grouped_arms(
    functions: &[RuntimeFn], value: impl Fn(&RuntimeFn) -> TokenStream,
) -> Vec<TokenStream> {
    let mut groups = BTreeMap::<String, (TokenStream, Vec<&Ident>)>::new();
    for function in functions {
        let tokens = value(function);
        groups
            .entry(tokens.to_string())
            .or_insert_with(|| (tokens, Vec::new()))
            .1
            .push(&function.variant);
    }
    groups
        .into_values()
        .map(|(value, variants)| quote!(#(Self::#variants)|* => #value))
        .collect()
}
