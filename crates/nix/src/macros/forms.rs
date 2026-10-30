//! `nix!` rules for literals, attrsets, lists, `let`, `if`, selection and
//! application.
//!
//! The rules are token munchers: each step moves one token into a bracketed
//! accumulator until a separator ends the current item.

/// `merge(lhs, rhs)`: splits at the first top-level comma.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_merge {
    ([$($lhs:tt)+] , $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Update, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_merge!([$($lhs)* $next] $($rest)*)
    };
    ([$($lhs:tt)*]) => {
        compile_error!("nix! merge(...) expects exactly two arguments")
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_key {
    (($expr:expr)) => {
        $crate::AttrKey::string($expr)
    };
    ($key:literal) => {
        $crate::AttrKey::string($key)
    };
    ($key:ident) => {
        $crate::AttrKey::ident(stringify!($key))
    };
}

/// Comma-separated expressions, as a `Vec`. Finished items are kept as
/// parenthesized groups in the first accumulator.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_exprs_vec {
    ([$($done:tt)*] []) => {
        vec![$($crate::nix! $done),*]
    };
    ([$($done:tt)*] [$($cur:tt)+]) => {
        vec![$($crate::nix! $done,)* $crate::nix!($($cur)+)]
    };
    ([$($done:tt)*] [] , $($rest:tt)*) => {
        $crate::__nix_exprs_vec!([$($done)*] [] $($rest)*)
    };
    ([$($done:tt)*] [$($cur:tt)+] , $($rest:tt)*) => {
        $crate::__nix_exprs_vec!([$($done)* ($($cur)+)] [] $($rest)*)
    };
    ([$($done:tt)*] [$($cur:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_exprs_vec!([$($done)*] [$($cur)* $next] $($rest)*)
    };
}

/// `key = value;` bindings, as a `Vec<AttrBind>`.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_binds {
    ([$(($key:tt $($value:tt)+))*]) => {
        vec![$($crate::AttrBind::new($crate::__nix_attr_key!($key), $crate::nix!($($value)+))),*]
    };
    ([$($done:tt)*] $key:tt = $($rest:tt)+) => {
        $crate::__nix_attr_bind_value!([$($done)*] $key [] $($rest)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_bind_value {
    ([$($done:tt)*] $key:tt [$($value:tt)+] ; $($rest:tt)*) => {
        $crate::__nix_attr_binds!([$($done)* ($key $($value)+)] $($rest)*)
    };
    ([$($done:tt)*] $key:tt [$($value:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_attr_bind_value!([$($done)*] $key [$($value)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_let {
    ([$(($name:ident $($value:tt)+))*] in $($body:tt)+) => {
        $crate::Expr::let_in(
            [$($crate::LetBind::new(stringify!($name), $crate::nix!($($value)+))),*],
            $crate::nix!($($body)+),
        )
    };
    ([$($done:tt)*] $name:ident = $($rest:tt)+) => {
        $crate::__nix_let_value!([$($done)*] $name [] $($rest)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_let_value {
    ([$($done:tt)*] $name:ident [$($value:tt)+] ; $($rest:tt)+) => {
        $crate::__nix_let!([$($done)* ($name $($value)+)] $($rest)+)
    };
    ([$($done:tt)*] $name:ident [$($value:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_let_value!([$($done)*] $name [$($value)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_if {
    ([$($cond:tt)+] then $($rest:tt)+) => {
        $crate::__nix_if_then!([$($cond)+] [] $($rest)+)
    };
    ([$($cond:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_if!([$($cond)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_if_then {
    ([$($cond:tt)+] [$($then:tt)+] else $($else:tt)+) => {
        $crate::Expr::if_then_else(
            $crate::nix!($($cond)+),
            $crate::nix!($($then)+),
            $crate::nix!($($else)+),
        )
    };
    ([$($cond:tt)+] [$($then:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_if_then!([$($cond)+] [$($then)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_atom {
    (! $($expr:tt)+) => {
        $crate::Expr::un($crate::UnOp::Not, $crate::nix!($($expr)+))
    };
    (- $($expr:tt)+) => {
        $crate::Expr::un($crate::UnOp::Neg, $crate::nix!($($expr)+))
    };
    (null) => {
        $crate::Expr::null()
    };
    (true) => {
        $crate::Expr::bool(true)
    };
    (false) => {
        $crate::Expr::bool(false)
    };
    ([ $($items:tt)* ]) => {
        $crate::Expr::list($crate::__nix_exprs_vec!([] [] $($items)*))
    };
    (rec { $($binds:tt)* }) => {
        $crate::Expr::rec_attrset($crate::__nix_attr_binds!([] $($binds)*))
    };
    ({ $($binds:tt)* }) => {
        $crate::Expr::attrset($crate::__nix_attr_binds!([] $($binds)*))
    };
    (merge($($args:tt)*)) => {
        $crate::__nix_merge!([] $($args)*)
    };
    (( $($inner:tt)+ ) $($postfix:tt)*) => {
        $crate::__nix_postfix!($crate::nix!($($inner)+); $($postfix)*)
    };
    (#{ $expr:expr } $($postfix:tt)*) => {
        $crate::__nix_postfix!($crate::Expr::from($expr); $($postfix)*)
    };
    ($head:ident $($postfix:tt)*) => {
        $crate::__nix_postfix!($crate::Expr::ident(stringify!($head)); $($postfix)*)
    };
    ($lit:literal) => {
        $crate::Expr::from($lit)
    };
}

/// Applies `.attr` selections and `(args)` calls to `$base`, left to right.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_postfix {
    ($base:expr;) => {
        $base
    };
    ($base:expr; . $attr:tt $($rest:tt)*) => {
        $crate::__nix_postfix!($crate::Expr::select($base, $crate::__nix_attr_key!($attr)); $($rest)*)
    };
    ($base:expr; ( $($args:tt)* ) $($rest:tt)*) => {
        $crate::__nix_postfix!(
            $crate::Expr::app($base, $crate::__nix_exprs_vec!([] [] $($args)*));
            $($rest)*
        )
    };
}
