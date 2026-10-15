//! The `nix!` macro: Nix-like syntax that builds [`Expr`](crate::Expr) trees.

mod forms;
mod ops;

#[macro_export]
macro_rules! nix {
    (#{ $expr:expr }) => {
        $crate::IntoNixExpr::into_nix_expr($expr)
    };
    (let $($rest:tt)+) => {
        $crate::__nix_let!($($rest)+)
    };
    (if $($rest:tt)+) => {
        $crate::__nix_if!($($rest)+)
    };
    (rec { $($binds:tt)* }) => {
        $crate::Expr::attrs(true, $crate::__nix_attr_binds_vec!($($binds)*))
    };
    ({ $($binds:tt)* }) => {
        $crate::Expr::attrs(false, $crate::__nix_attr_binds_vec!($($binds)*))
    };
    ([ $($items:tt)* ]) => {
        $crate::Expr::list($crate::__nix_exprs_vec!($($items)*))
    };
    (merge($($args:tt)*)) => {
        $crate::__nix_merge!($($args)*)
    };
    ($param:ident : $($body:tt)+) => {
        $crate::Expr::lambda(stringify!($param), $crate::nix!($($body)+))
    };
    ($($tokens:tt)+) => {
        $crate::__nix_parse_or!($($tokens)+)
    };
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push)]
mod tests;
