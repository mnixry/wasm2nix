//! `nix!` rules for binary operators. Each precedence tier, from `||` down
//! to `*`, splits at its first operator and hands an operator-free run to the
//! next tier.

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_binop {
    (or; [$($lhs:tt)+] || $($rhs:tt)+) => {
        $crate::Expr::bin(
            $crate::BinOp::Or,
            $crate::nix!($($lhs)+),
            $crate::__nix_binop!(or; [] $($rhs)+),
        )
    };
    (or; [$($all:tt)+]) => {
        $crate::__nix_binop!(and; [] $($all)+)
    };

    (and; [$($lhs:tt)+] && $($rhs:tt)+) => {
        $crate::Expr::bin(
            $crate::BinOp::And,
            $crate::nix!($($lhs)+),
            $crate::__nix_binop!(and; [] $($rhs)+),
        )
    };
    (and; [$($all:tt)+]) => {
        $crate::__nix_binop!(cmp; [] $($all)+)
    };

    (cmp; [$($lhs:tt)+] == $($rhs:tt)+) => {
        $crate::__nix_cmp!(Eq; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($lhs:tt)+] != $($rhs:tt)+) => {
        $crate::__nix_cmp!(Neq; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($lhs:tt)+] <= $($rhs:tt)+) => {
        $crate::__nix_cmp!(Le; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($lhs:tt)+] >= $($rhs:tt)+) => {
        $crate::__nix_cmp!(Ge; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($lhs:tt)+] < $($rhs:tt)+) => {
        $crate::__nix_cmp!(Lt; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($lhs:tt)+] > $($rhs:tt)+) => {
        $crate::__nix_cmp!(Gt; [$($lhs)+] $($rhs)+)
    };
    (cmp; [$($all:tt)+]) => {
        $crate::__nix_binop!(add; [] $($all)+)
    };

    (add; [] - $($rest:tt)+) => {
        $crate::__nix_binop!(add; [-] $($rest)+)
    };
    (add; [$($lhs:tt)+] + $($rhs:tt)+) => {
        $crate::__nix_fold!(add; $crate::nix!($($lhs)+); Add [] $($rhs)+)
    };
    (add; [$($lhs:tt)+] - $($rhs:tt)+) => {
        $crate::__nix_fold!(add; $crate::nix!($($lhs)+); Sub [] $($rhs)+)
    };
    (add; [$($all:tt)+]) => {
        $crate::__nix_binop!(mul; [] $($all)+)
    };

    (mul; [$($lhs:tt)+] * $($rhs:tt)+) => {
        $crate::__nix_fold!(mul; $crate::nix!($($lhs)+); Mul [] $($rhs)+)
    };
    (mul; [$($lhs:tt)+] / $($rhs:tt)+) => {
        $crate::__nix_fold!(mul; $crate::nix!($($lhs)+); Div [] $($rhs)+)
    };
    (mul; [$($all:tt)+]) => {
        $crate::__nix_atom!($($all)+)
    };

    ($tier:ident; [$($lhs:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_binop!($tier; [$($lhs)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_cmp {
    ($op:ident; [$($lhs:tt)+] $($rhs:tt)+) => {
        $crate::Expr::bin($crate::BinOp::$op, $crate::nix!($($lhs)+), $crate::nix!($($rhs)+))
    };
}

/// Folds a `+`/`-` or `*`/`/` chain left to right, so `a - b - c` is
/// `(a - b) - c`. `$op` is the operator before the accumulated operand.
#[doc(hidden)]
#[macro_export]
macro_rules! __nix_fold {
    (add; $acc:expr; $op:ident [$($cur:tt)+] + $($rest:tt)+) => {
        $crate::__nix_fold!(add; $crate::__nix_fold!($acc; $op [$($cur)+]); Add [] $($rest)+)
    };
    (add; $acc:expr; $op:ident [$($cur:tt)+] - $($rest:tt)+) => {
        $crate::__nix_fold!(add; $crate::__nix_fold!($acc; $op [$($cur)+]); Sub [] $($rest)+)
    };
    (mul; $acc:expr; $op:ident [$($cur:tt)+] * $($rest:tt)+) => {
        $crate::__nix_fold!(mul; $crate::__nix_fold!($acc; $op [$($cur)+]); Mul [] $($rest)+)
    };
    (mul; $acc:expr; $op:ident [$($cur:tt)+] / $($rest:tt)+) => {
        $crate::__nix_fold!(mul; $crate::__nix_fold!($acc; $op [$($cur)+]); Div [] $($rest)+)
    };
    ($tier:ident; $acc:expr; $op:ident [$($cur:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_fold!($tier; $acc; $op [$($cur)* $next] $($rest)*)
    };
    ($tier:ident; $acc:expr; $op:ident [$($cur:tt)+]) => {
        $crate::__nix_fold!($acc; $op [$($cur)+])
    };
    ($acc:expr; $op:ident [$($cur:tt)+]) => {
        $crate::Expr::bin($crate::BinOp::$op, $acc, $crate::nix!($($cur)+))
    };
}
