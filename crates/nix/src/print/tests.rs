use super::*;
use crate::AttrBind;
use crate::AttrKey;
use crate::BinOp;
use crate::Expr;
use crate::LetBind;
use crate::RenderStyle;

#[test]
fn renders_attrset_and_let() {
    let e = Expr::let_in(
        [
            LetBind::new("x", Expr::int(1)),
            LetBind::new("y", Expr::bin(BinOp::Add, Expr::ident("x"), Expr::int(2))),
        ],
        Expr::attrset([
            AttrBind::new(AttrKey::ident("x"), Expr::ident("x")),
            AttrBind::new(AttrKey::ident("y"), Expr::ident("y")),
        ]),
    );

    assert_eq!(e.render(), "let x = 1; y = x + 2; in { x = x; y = y; }");
}

#[test]
fn wraps_with_render_width() {
    let e = Expr::attrset([
        AttrBind::new(AttrKey::ident("alpha"), Expr::int(1)),
        AttrBind::new(AttrKey::ident("beta"), Expr::int(2)),
    ]);

    assert_eq!(
        e.render_with(&RenderOptions {
            width: 10,
            indent: 2,
            style: RenderStyle::Pretty,
        }),
        "{\n  alpha = 1;\n  beta = 2;\n}"
    );
}

#[test]
fn compact_style_does_not_wrap_with_width() {
    let e = Expr::attrset([
        AttrBind::new(AttrKey::ident("alpha"), Expr::int(1)),
        AttrBind::new(AttrKey::ident("beta"), Expr::int(2)),
    ]);

    assert_eq!(
        e.render_with(&RenderOptions {
            width: 10,
            indent: 2,
            style: RenderStyle::Compact,
        }),
        "{ alpha = 1; beta = 2; }"
    );
}

#[test]
fn renders_assert_chain_flat() {
    let e = Expr::asserts(
        vec![
            Expr::bin(BinOp::Eq, Expr::ident("a"), Expr::ident("a")),
            Expr::bin(BinOp::Implies, Expr::ident("c"), Expr::ident("t")),
        ],
        Expr::ident("body"),
    );

    assert_eq!(e.render(), "assert a == a; assert c -> t; body");
    assert_eq!(
        e.render_with(&RenderOptions {
            width: 16,
            indent: 2,
            style: RenderStyle::Pretty,
        }),
        "assert a == a;\nassert c -> t;\nbody"
    );
}

#[test]
fn preserves_left_associative_rhs_with_parentheses() {
    let e = Expr::bin(
        BinOp::Sub,
        Expr::ident("a"),
        Expr::bin(BinOp::Sub, Expr::ident("b"), Expr::ident("c")),
    );

    assert_eq!(e.render(), "a - (b - c)");
}

#[test]
fn preserves_right_associative_lhs_with_parentheses() {
    let e = Expr::bin(
        BinOp::Update,
        Expr::bin(BinOp::Update, Expr::ident("a"), Expr::ident("b")),
        Expr::ident("c"),
    );

    assert_eq!(e.render(), "(a // b) // c");
}

#[test]
fn flattens_apply_and_select_constructors() {
    let e = Expr::app(
        Expr::select(
            Expr::select(Expr::ident("builtins"), AttrKey::ident("elemAt")),
            AttrKey::ident("nested"),
        ),
        vec![Expr::ident("xs"), Expr::int(0)],
    );

    assert_eq!(e.render(), "builtins.elemAt.nested xs 0");
}

#[test]
fn renders_large_flat_let_without_recursive_doc_chain() {
    const BIND_COUNT: usize = 50_000;

    let binds: Vec<_> = (0..BIND_COUNT)
        .map(|i| {
            LetBind::new(
                format!("v{i}"),
                Expr::int(i64::try_from(i).expect("test bind index fits in i64")),
            )
        })
        .collect();
    let last_name = format!("v{}", BIND_COUNT - 1);
    let e = Expr::let_in(binds, Expr::ident(last_name.clone()));

    let rendered = e.render();
    let rendered = rendered.trim();

    assert!(rendered.starts_with("let"));
    assert!(rendered.contains(&format!("{last_name} = {};", BIND_COUNT - 1)));
    assert!(rendered.ends_with(&last_name));
}

#[test]
fn escapes_interpolation_in_strings() {
    assert_eq!(Expr::string("${x}").render(), r#""\${x}""#);
    assert_eq!(Expr::string("$${x} $ {").render(), r#""$\${x} $ {""#);
    assert_eq!(
        Expr::attrset([AttrBind::new(AttrKey::string("${k}"), Expr::int(1))]).render(),
        r#"{ "\${k}" = 1; }"#
    );
}
