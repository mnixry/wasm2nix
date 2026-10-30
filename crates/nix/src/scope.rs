//! Nix scoping rules: which nodes bind a name, and the free occurrences of a
//! name below a node.

use crate::AttrKey;
use crate::Expr;

impl Expr {
    /// Whether this node binds `name` for its whole subtree: a lambda
    /// parameter, any `let` binding (`let` is recursive), or a `rec`
    /// attribute key.
    #[must_use]
    pub fn binds(&self, name: &str) -> bool {
        match self {
            Self::Lambda { param, .. } => param == name,
            Self::LetIn { binds, .. } => binds.iter().any(|bind| bind.name == name),
            Self::AttrSet {
                recursive: true,
                binds,
            } => binds.iter().any(
                |bind| matches!(&bind.key, AttrKey::Ident(key) | AttrKey::Str(key) if key == name),
            ),
            _ => false,
        }
    }

    /// Counts the occurrences of the variable `name` that are not shadowed by
    /// an inner binding.
    #[must_use]
    pub fn free_uses(&self, name: &str) -> usize {
        match self {
            Self::Ident(ident) if ident == name => 1,
            _ if self.binds(name) => 0,
            _ => {
                let mut count = 0;
                self.for_each_child(|child| count += child.free_uses(name));
                count
            }
        }
    }

    /// Replaces every free occurrence of the variable `name` with
    /// `replacement`.
    pub fn substitute_free(&mut self, name: &str, replacement: &Self) {
        if matches!(self, Self::Ident(ident) if ident == name) {
            *self = replacement.clone();
        } else if !self.binds(name) {
            self.for_each_child_mut(|child| child.substitute_free(name, replacement));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::AttrBind;
    use crate::AttrKey;
    use crate::Expr;
    use crate::nix;

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
        let quoted = Expr::rec_attrset([
            AttrBind::new(AttrKey::string("x"), Expr::int(1)),
            AttrBind::new(AttrKey::ident("y"), nix!(x)),
        ]);
        assert_eq!(quoted.free_uses("x"), 0);
    }

    #[test]
    fn substitutes_only_free_occurrences() {
        let mut expr = nix!(f(x, (x: x), (let y = x; in y)));
        expr.substitute_free("x", &Expr::int(7));
        assert_eq!(expr.render(), "f 7 (x: x) (let y = 7; in y)");
    }
}
