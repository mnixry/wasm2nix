//! Nix scoping rules: which nodes bind a name, and the free occurrences of a
//! name below a node.

use crate::{
    AttrKey, Expr,
    visit::{Visit, VisitMut, walk_expr, walk_expr_mut},
};

impl Expr {
    /// Whether this node binds `name` for its whole subtree: a lambda
    /// parameter, any `let` binding (`let` is recursive), or the first key of
    /// a `rec` attribute binding.
    #[must_use]
    pub fn binds(&self, name: &str) -> bool {
        match self {
            Self::Lambda { param, .. } => param == name,
            Self::LetIn { binds, .. } => binds.iter().any(|bind| bind.name == name),
            Self::AttrSet {
                recursive: true,
                binds,
            } => binds.iter().any(|bind| {
                bind.path.iter().next().is_some_and(
                    |key| matches!(key, AttrKey::Ident(key) | AttrKey::Str(key) if key == name),
                )
            }),
            _ => false,
        }
    }

    /// Counts the occurrences of the variable `name` that are not shadowed by
    /// an inner binding.
    #[must_use]
    pub fn free_uses(&self, name: &str) -> usize {
        let mut uses = FreeUses { name, count: 0 };
        uses.visit_expr(self);
        uses.count
    }

    /// Replaces every free occurrence of the variable `name` with
    /// `replacement`.
    pub fn substitute_free(&mut self, name: &str, replacement: &Self) {
        Substitute { name, replacement }.visit_expr_mut(self);
    }
}

struct FreeUses<'a> {
    name: &'a str,
    count: usize,
}

impl<'ast> Visit<'ast> for FreeUses<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Ident(ident) if ident == self.name => self.count += 1,
            _ if expr.binds(self.name) => {}
            _ => walk_expr(self, expr),
        }
    }
}

struct Substitute<'a> {
    name: &'a str,
    replacement: &'a Expr,
}

impl VisitMut for Substitute<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if matches!(expr, Expr::Ident(ident) if ident == self.name) {
            *expr = self.replacement.clone();
        } else if !expr.binds(self.name) {
            walk_expr_mut(self, expr);
        }
    }
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push)] // `nix!` expansions build lists with `push`.
mod tests {
    use crate::{AttrKey, Expr, nix};

    #[test]
    fn counts_free_uses_through_nested_expressions() {
        let expr = nix!(if x == 1 then [x, { a = x; }] else f(x, y));
        assert_eq!(expr.free_uses("x"), 4);
        assert_eq!(expr.free_uses("y"), 1);
        assert_eq!(expr.free_uses("z"), 0);
    }

    #[test]
    fn binders_shadow_their_whole_subtree() {
        assert_eq!(nix!(x: x).free_uses("x"), 0);
        assert_eq!(nix!(let x = 1; y = x; in x).free_uses("x"), 0);
        assert_eq!(nix!(rec { x = 1; y = x; }).free_uses("x"), 0);
        assert_eq!(
            nix!({
                x = 1;
                y = x;
            })
            .free_uses("x"),
            1
        );
        let quoted = Expr::attrset(
            true,
            vec![
                (AttrKey::string("x"), Expr::int(1)),
                (AttrKey::ident("y"), nix!(x)),
            ],
        );
        assert_eq!(quoted.free_uses("x"), 0);
    }

    #[test]
    fn substitutes_only_free_occurrences() {
        let mut expr = nix!(f(x, (x: x), (let y = x; in y)));
        expr.substitute_free("x", &Expr::int(7));
        assert_eq!(expr.render(), "f 7 (x: x) (let y = 7; in y)");
    }
}
