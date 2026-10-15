//! `nix!` rules for literals, attrsets, lists, `let`, `if`, selection and
//! application.

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
