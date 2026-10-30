use crate::AttrKey;
use crate::BinOp;
use crate::Expr;

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

#[test]
fn macro_chains_postfix_selects_and_calls() {
    let f = Expr::ident("f");
    assert_eq!(nix!(#{f}(x).a(y).b).render(), "((f x).a y).b");
    assert_eq!(nix!(f(x)(y)).render(), "f x y");
    assert_eq!(nix!((a).b.c).render(), "a.b.c");
}

#[test]
fn macro_splits_merge_arguments_with_interpolations() {
    let lhs = Expr::ident("a");
    let rhs = nix!({
        b = 1;
    });
    assert_eq!(nix!(merge(#{lhs}, #{rhs})).render(), "a // { b = 1; }");
    assert_eq!(nix!(merge(f(x, y), [1, 2])).render(), "f x y // [ 1 2 ]");
}

#[test]
fn macro_builds_empty_and_nested_collections() {
    assert_eq!(nix!([]).render(), "[ ]");
    assert_eq!(nix!({}).render(), "{ }");
    assert_eq!(nix!(rec {}).render(), "rec { }");
    assert_eq!(
        nix!([[1, 2], [], f(x, y)]).render(),
        "[ [ 1 2 ] [ ] (f x y) ]"
    );
}
