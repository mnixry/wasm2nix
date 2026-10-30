use super::Expr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrKey {
    Ident(String),
    Str(String),
}

impl AttrKey {
    pub fn ident(name: impl Into<String>) -> Self {
        Self::Ident(name.into())
    }

    pub fn string(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
}

/// The attribute path of a select, `a.b.c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrPath(Vec<AttrKey>);

impl AttrPath {
    #[must_use]
    pub fn one(key: AttrKey) -> Self {
        Self(vec![key])
    }

    pub fn push(&mut self, key: AttrKey) {
        self.0.push(key);
    }

    pub fn iter(&self) -> impl Iterator<Item = &AttrKey> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrBind {
    pub key: AttrKey,
    pub value: Expr,
}

impl AttrBind {
    #[must_use]
    pub const fn new(key: AttrKey, value: Expr) -> Self {
        Self { key, value }
    }
}

impl From<(AttrKey, Expr)> for AttrBind {
    fn from((key, value): (AttrKey, Expr)) -> Self {
        Self::new(key, value)
    }
}
