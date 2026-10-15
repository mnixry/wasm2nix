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
