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

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_merge {
    ($lhs:tt, $rhs:tt $(,)?) => {
        $crate::Expr::bin($crate::BinOp::Update, $crate::nix!($lhs), $crate::nix!($rhs))
    };
    ($($tokens:tt)+) => {{
        let mut __args = $crate::__nix_exprs_vec!($($tokens)+);
        assert!(
            __args.len() == 2,
            "nix! merge(...) expects exactly two arguments"
        );
        let __rhs = __args.pop().expect("rhs present");
        let __lhs = __args.pop().expect("lhs present");
        $crate::Expr::bin($crate::BinOp::Update, __lhs, __rhs)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_key {
    (#{ $expr:expr }) => {
        $crate::AttrKey::string($expr)
    };
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

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_select {
    ($base:expr; $($attr:tt).+) => {{
        let mut __expr = $base;
        $(
            __expr = $crate::Expr::select(__expr, $crate::__nix_attr_key!($attr));
        )+
        __expr
    }};
}

#[doc(hidden)]
#[allow(clippy::vec_init_then_push)]
#[macro_export]
macro_rules! __nix_exprs_vec {
    () => {
        Vec::new()
    };
    ($($tokens:tt)+) => {{
        let mut __items = Vec::with_capacity(0);
        $crate::__nix_exprs_vec_parse!(__items; $($tokens)+);
        __items
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_exprs_vec_parse {
    ($items:ident;) => {};
    ($items:ident; , $($rest:tt)*) => {
        $crate::__nix_exprs_vec_parse!($items; $($rest)*);
    };
    ($items:ident; $($rest:tt)+) => {
        $crate::__nix_exprs_vec_item!($items; [] $($rest)+);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_exprs_vec_item {
    ($items:ident; [$($value:tt)*] , $($rest:tt)*) => {
        $items.push($crate::nix!($($value)*));
        $crate::__nix_exprs_vec_parse!($items; $($rest)*);
    };
    ($items:ident; [$($value:tt)+]) => {
        $items.push($crate::nix!($($value)+));
    };
    ($items:ident; [$($value:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_exprs_vec_item!($items; [$($value)* $next] $($rest)*);
    };
}

#[doc(hidden)]
#[allow(clippy::vec_init_then_push)]
#[macro_export]
macro_rules! __nix_attr_binds_vec {
    () => {
        Vec::new()
    };
    ($($tokens:tt)+) => {{
        let mut __binds = Vec::with_capacity(0);
        $crate::__nix_attr_binds_parse!(__binds; $($tokens)+);
        __binds
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_binds_parse {
    ($binds:ident;) => {};
    ($binds:ident; $key:tt = $($rest:tt)+) => {
        $crate::__nix_attr_binds_value!($binds; [$key] [] $($rest)+);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_attr_binds_value {
    ($binds:ident; [$key:tt] [$($value:tt)+] ; $($rest:tt)*) => {
        $binds.push($crate::AttrBind::new(
            $crate::__nix_attr_key!($key),
            $crate::nix!($($value)+),
        ));
        $crate::__nix_attr_binds_parse!($binds; $($rest)*);
    };
    ($binds:ident; [$key:tt] [$($value:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_attr_binds_value!($binds; [$key] [$($value)* $next] $($rest)*);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_let {
    ($($tokens:tt)+) => {
        $crate::__nix_let_parse!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_let_parse {
    ([$($binds:expr,)*] in $($body:tt)+) => {
        $crate::Expr::let_binds(vec![$($binds,)*], $crate::nix!($($body)+))
    };
    ([$($binds:expr,)*] $name:ident = $($rest:tt)+) => {
        $crate::__nix_let_value!([$($binds,)*] $name [] $($rest)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_let_value {
    ([$($binds:expr,)*] $name:ident [$($value:tt)+] ; $($rest:tt)+) => {
        $crate::__nix_let_parse!(
            [$($binds,)* $crate::LetBind::new(stringify!($name), $crate::nix!($($value)+)),]
            $($rest)+
        )
    };
    ([$($binds:expr,)*] $name:ident [$($value:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_let_value!([$($binds,)*] $name [$($value)* $next] $($rest)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_if {
    ($($tokens:tt)+) => {
        $crate::__nix_if_cond!([] $($tokens)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_if_cond {
    ([$($cond:tt)+] then $($rest:tt)+) => {
        $crate::__nix_if_then!([$($cond)+] [] $($rest)+)
    };
    ([$($cond:tt)*] $next:tt $($rest:tt)*) => {
        $crate::__nix_if_cond!([$($cond)* $next] $($rest)*)
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

#[doc(hidden)]
#[macro_export]
macro_rules! __nix_atom {
    (! $($expr:tt)+) => {
        $crate::Expr::un($crate::UnOp::Not, $crate::nix!($($expr)+))
    };
    (- $($expr:tt)+) => {
        $crate::Expr::un($crate::UnOp::Neg, $crate::nix!($($expr)+))
    };
    (($($base:tt)+) $(. $attr:tt)+ ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::__nix_select!($crate::nix!($($base)+); $($attr).+),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    (($($base:tt)+) ( $($args:tt)* ) $(. $attr:tt)+) => {
        $crate::__nix_select!(
            $crate::Expr::app(
                $crate::nix!($($base)+),
                $crate::__nix_exprs_vec!($($args)*),
            );
            $($attr).+
        )
    };
    (($($base:tt)+) ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::nix!($($base)+),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    (($($base:tt)+) $(. $attr:tt)+) => {
        $crate::__nix_select!($crate::nix!($($base)+); $($attr).+)
    };
    (($($inner:tt)+)) => {
        $crate::nix!($($inner)+)
    };
    (#{ $base:expr } $(. $attr:tt)+ ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::__nix_select!($crate::IntoNixExpr::into_nix_expr($base); $($attr).+),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    (#{ $func:expr } ( $($args:tt)* ) $(. $attr:tt)+) => {
        $crate::__nix_select!(
            $crate::Expr::app(
                $crate::IntoNixExpr::into_nix_expr($func),
                $crate::__nix_exprs_vec!($($args)*),
            );
            $($attr).+
        )
    };
    (#{ $func:expr } ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::IntoNixExpr::into_nix_expr($func),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    (#{ $base:expr } $(. $attr:tt)+) => {
        $crate::__nix_select!($crate::IntoNixExpr::into_nix_expr($base); $($attr).+)
    };
    (#{ $expr:expr }) => {
        $crate::IntoNixExpr::into_nix_expr($expr)
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
        $crate::Expr::list($crate::__nix_exprs_vec!($($items)*))
    };
    (rec { $($binds:tt)* }) => {
        $crate::Expr::attrs(true, $crate::__nix_attr_binds_vec!($($binds)*))
    };
    ({ $($binds:tt)* }) => {
        $crate::Expr::attrs(false, $crate::__nix_attr_binds_vec!($($binds)*))
    };
    (merge($($args:tt)*)) => {
        $crate::__nix_merge!($($args)*)
    };
    ($head:ident $(. $attr:tt)+ ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::__nix_select!($crate::Expr::ident(stringify!($head)); $($attr).+),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    ($head:ident ( $($args:tt)* )) => {
        $crate::Expr::app(
            $crate::Expr::ident(stringify!($head)),
            $crate::__nix_exprs_vec!($($args)*),
        )
    };
    ($head:ident $(. $attr:tt)+) => {
        $crate::__nix_select!($crate::Expr::ident(stringify!($head)); $($attr).+)
    };
    ($ident:ident) => {
        $crate::Expr::ident(stringify!($ident))
    };
    ($lit:literal) => {
        $crate::IntoNixExpr::into_nix_expr($lit)
    };
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push)]
mod tests {
    use crate::{AttrKey, BinOp, Expr};

    #[test]
    fn macro_builds_basic_forms() {
        assert_eq!(nix!(foo.bar("x", 2)).render(), "foo.bar \"x\" 2");
        assert_eq!(nix!([a, "b", 3]).render(), "[ a \"b\" 3 ]");
        assert_eq!(
            nix!({
                x = 1;
                "two" = foo;
            })
            .render(),
            "{ x = 1; \"two\" = foo; }"
        );
    }

    #[test]
    fn macro_interpolates_rust_expressions() {
        let value = Expr::select(Expr::ident("st"), AttrKey::ident("mem"));
        let expr = nix!({ st = #{value}; ok = true; });

        assert_eq!(expr.render(), "{ st = st.mem; ok = true; }");
    }

    #[test]
    fn macro_builds_dynamic_selects_and_calls() {
        let attr = "field";
        let func = Expr::select(Expr::ident("rt"), AttrKey::ident("call"));

        assert_eq!(nix!(imports.(attr)).render(), "imports.\"field\"");
        assert_eq!(nix!(#{func.clone()}(x, 1)).render(), "rt.call x 1");
        assert_eq!(nix!((#{func})(x).result).render(), "(rt.call x).result");
    }

    #[test]
    fn macro_builds_let_if_lambda_and_merge() {
        let expr = nix!(arg: let x = 1; in if x == 1 then merge(arg, { y = 2; }) else arg);

        assert_eq!(
            expr.render(),
            "arg: let x = 1; in if x == 1 then arg // { y = 2; } else arg"
        );
    }

    #[test]
    fn macro_builds_binary_ops() {
        assert_eq!(nix!(a + b * c).render(), "a + b * c");
        assert_eq!(nix!(a - b - c).render(), "a - b - c");
        assert_eq!(nix!(a - b + c).render(), "a - b + c");
        assert_eq!(nix!(a / b * c).render(), "a / b * c");
        assert_eq!(
            nix!(a - b - c),
            Expr::bin(
                BinOp::Sub,
                Expr::bin(BinOp::Sub, Expr::ident("a"), Expr::ident("b")),
                Expr::ident("c"),
            )
        );
        assert_eq!(
            nix!(a - (b - c)).render(),
            Expr::bin(
                BinOp::Sub,
                Expr::ident("a"),
                Expr::bin(BinOp::Sub, Expr::ident("b"), Expr::ident("c")),
            )
            .render()
        );
    }
}
