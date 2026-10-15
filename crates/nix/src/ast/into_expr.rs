use super::Expr;

pub trait IntoNixExpr {
    fn into_nix_expr(self) -> Expr;
}

impl IntoNixExpr for Expr {
    fn into_nix_expr(self) -> Expr {
        self
    }
}

impl IntoNixExpr for &Expr {
    fn into_nix_expr(self) -> Expr {
        self.clone()
    }
}

impl IntoNixExpr for bool {
    fn into_nix_expr(self) -> Expr {
        Expr::bool(self)
    }
}

impl IntoNixExpr for String {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self)
    }
}

impl IntoNixExpr for &String {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self.clone())
    }
}

impl IntoNixExpr for &str {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self)
    }
}

impl<T: IntoNixExpr> IntoNixExpr for Option<T> {
    fn into_nix_expr(self) -> Expr {
        self.map_or_else(Expr::null, IntoNixExpr::into_nix_expr)
    }
}

macro_rules! impl_int_expr {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoNixExpr for $ty {
                fn into_nix_expr(self) -> Expr {
                    Expr::int(i64::from(self))
                }
            }
        )*
    };
}

impl_int_expr!(i8, i16, i32, i64, u8, u16, u32);

impl IntoNixExpr for usize {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("usize Nix literal fits in i64"))
    }
}

impl IntoNixExpr for u64 {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("u64 Nix literal fits in i64"))
    }
}

impl IntoNixExpr for isize {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("isize Nix literal fits in i64"))
    }
}
