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

impl From<&str> for AttrKey {
    fn from(value: &str) -> Self {
        Self::ident(value)
    }
}

impl From<String> for AttrKey {
    fn from(value: String) -> Self {
        Self::ident(value)
    }
}

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

impl From<AttrKey> for AttrPath {
    fn from(value: AttrKey) -> Self {
        Self::one(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrBind {
    pub path: AttrPath,
    pub value: Expr,
}

impl AttrBind {
    #[must_use]
    pub fn new(path: impl Into<AttrPath>, value: Expr) -> Self {
        Self {
            path: path.into(),
            value,
        }
    }
}

impl From<(AttrKey, Expr)> for AttrBind {
    fn from((key, value): (AttrKey, Expr)) -> Self {
        Self::new(key, value)
    }
}
