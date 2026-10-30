//! The `nix!` macro: Nix-like syntax that builds [`Expr`](crate::Expr) trees.

mod forms;
mod ops;

#[macro_export]
macro_rules! nix {
    (let $($rest:tt)+) => {
        $crate::__nix_let!([] $($rest)+)
    };
    (if $($rest:tt)+) => {
        $crate::__nix_if!([] $($rest)+)
    };
    ($param:ident : $($body:tt)+) => {
        $crate::Expr::lambda(stringify!($param), $crate::nix!($($body)+))
    };
    ($($tokens:tt)+) => {
        $crate::__nix_binop!(or; [] $($tokens)+)
    };
}

#[cfg(test)]
mod tests;
