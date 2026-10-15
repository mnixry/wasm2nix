//! `nix!` rules for binary operators, by precedence from `||` down to `*`.

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_or {
    ($($tokens:tt)+) => {
        $crate::__nix_parse_or_acc!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_or_acc {
    ([$($lhs:tt)+] || $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Or, $crate::nix!($($lhs)+), $crate::__nix_parse_or!($($rhs)+))
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_parse_or_acc!([$($lhs)* $next] $($rest)*)
    };
    ([$($all:tt)+]) => {
        $crate::__nix_parse_and!($($all)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_and {
    ($($tokens:tt)+) => {
        $crate::__nix_parse_and_acc!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_and_acc {
    ([$($lhs:tt)+] && $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::And, $crate::nix!($($lhs)+), $crate::__nix_parse_and!($($rhs)+))
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_parse_and_acc!([$($lhs)* $next] $($rest)*)
    };
    ([$($all:tt)+]) => {
        $crate::__nix_parse_cmp!($($all)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_cmp {
    ($($tokens:tt)+) => {
        $crate::__nix_parse_cmp_acc!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_cmp_acc {
    ([$($lhs:tt)+] == $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Eq, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)+] != $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Neq, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)+] <= $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Le, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)+] >= $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Ge, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)+] < $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Lt, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)+] > $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::Gt, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_parse_cmp_acc!([$($lhs)* $next] $($rest)*)
    };
    ([$($all:tt)+]) => {
        $crate::__nix_parse_add!($($all)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_add {
    ($($tokens:tt)+) => {
        $crate::__nix_parse_add_acc!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_add_acc {
    ([] - $($rest:tt)+) => {
        $crate::__nix_parse_add_acc!([-] $($rest)+)
    };
    ([$($lhs:tt)+] + $($rhs:tt)+) => {
        $crate::__nix_fold_add!($crate::nix!($($lhs)+); Add [] $($rhs)+)
    };
    ([$($lhs:tt)+] - $($rhs:tt)+) => {
        $crate::__nix_fold_add!($crate::nix!($($lhs)+); Sub [] $($rhs)+)
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_parse_add_acc!([$($lhs)* $next] $($rest)*)
    };
    ([$($all:tt)+]) => {
        $crate::__nix_parse_mul!($($all)+)
    };
}

/// Folds a `+`/`-` chain left to right, so `a - b - c` is `(a - b) - c`.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_fold_add {
    ($acc:expr; $op:ident [$($cur:tt)+] + $($rest:tt)+) => {
        $crate::__nix_fold_add!(
            $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+)); Add [] $($rest)+
        )
    };
    ($acc:expr; $op:ident [$($cur:tt)+] - $($rest:tt)+) => {
        $crate::__nix_fold_add!(
            $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+)); Sub [] $($rest)+
        )
    };
    ($acc:expr; $op:ident [$($cur:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_fold_add!($acc; $op [$($cur)* $next] $($rest)*)
    };
    ($acc:expr; $op:ident [$($cur:tt)+]) => {
        $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+))
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_mul {
    ($($tokens:tt)+) => {
        $crate::__nix_parse_mul_acc!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_parse_mul_acc {
    ([$($lhs:tt)+] * $($rhs:tt)+) => {
        $crate::__nix_fold_mul!($crate::nix!($($lhs)+); Mul [] $($rhs)+)
    };
    ([$($lhs:tt)+] / $($rhs:tt)+) => {
        $crate::__nix_fold_mul!($crate::nix!($($lhs)+); Div [] $($rhs)+)
    };
    ([$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_parse_mul_acc!([$($lhs)* $next] $($rest)*)
    };
    ([$($all:tt)+]) => {
        $crate::__nix_atom!($($all)+)
    };
}

/// Folds a `*`/`/` chain left to right, so `a / b / c` is `(a / b) / c`.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_fold_mul {
    ($acc:expr; $op:ident [$($cur:tt)+] * $($rest:tt)+) => {
        $crate::__nix_fold_mul!(
            $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+)); Mul [] $($rest)+
        )
    };
    ($acc:expr; $op:ident [$($cur:tt)+] / $($rest:tt)+) => {
        $crate::__nix_fold_mul!(
            $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+)); Div [] $($rest)+
        )
    };
    ($acc:expr; $op:ident [$($cur:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_fold_mul!($acc; $op [$($cur)* $next] $($rest)*)
    };
    ($acc:expr; $op:ident [$($cur:tt)+]) => {
        $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+))
    };
}
