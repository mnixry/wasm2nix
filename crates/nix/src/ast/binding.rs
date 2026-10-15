use super::Expr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetBind {
    pub name: String,
    pub value: Expr,
}

impl LetBind {
    pub fn new(name: impl Into<String>, value: Expr) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

impl From<(String, Expr)> for LetBind {
    fn from((name, value): (String, Expr)) -> Self {
        Self::new(name, value)
    }
}
