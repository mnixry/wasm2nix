use std::{collections::BTreeMap, convert::Infallible, fmt};

use cranelift_codegen::ir;
use cranelift_entity::{EntityRef, PrimaryMap};
use cranelift_wasm::{Global, Memory, Table};
use serde::Deserialize;
use snafu::Snafu;

use crate::translator::{Exportable, NixModule};

pub type Result<T, E = Infallible> = std::result::Result<T, LinkError<E>>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum LinkError<E: snafu::Error + 'static = Infallible> {
    #[snafu(display("missing import {module}.{field}: expected {expected}"))]
    MissingImport {
        module: String,
        field: String,
        expected: Box<ExternType>,
    },
    #[snafu(display("incompatible import {module}.{field}: expected {expected}, got {actual}"))]
    IncompatibleImport {
        module: String,
        field: String,
        expected: Box<ExternType>,
        actual: Box<ExternType>,
    },
    #[snafu(display("failed to resolve import {module}.{field}"))]
    ResolveImport {
        module: String,
        field: String,
        source: E,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub min: u64,
    pub max: Option<u64>,
}

impl Limits {
    #[must_use]
    pub const fn new(min: u64, max: Option<u64>) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub fn satisfies_import(self, expected: Self) -> bool {
        if self.min < expected.min {
            return false;
        }
        expected.max.is_none_or(|expected_max| {
            self.max
                .is_some_and(|actual_max| actual_max <= expected_max)
        })
    }
}

impl From<&Memory> for Limits {
    fn from(memory: &Memory) -> Self {
        Self::new(memory.minimum, memory.maximum)
    }
}

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max {
            Some(max) => write!(f, "min {}, max {}", self.min, max),
            None => write!(f, "min {}, max none", self.min),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuncType {
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub results: Vec<String>,
}

impl FuncType {
    #[must_use]
    pub const fn new(params: Vec<String>, results: Vec<String>) -> Self {
        Self { params, results }
    }

    #[must_use]
    pub fn from_signature(sig: &ir::Signature) -> Self {
        Self {
            params: sig
                .params
                .iter()
                .map(|param| param.value_type.to_string())
                .collect(),
            results: sig
                .returns
                .iter()
                .map(|param| param.value_type.to_string())
                .collect(),
        }
    }
}

impl fmt::Display for FuncType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "func ({}) -> ({})",
            self.params.join(","),
            self.results.join(",")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalType {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub mutable: bool,
}

impl GlobalType {
    #[must_use]
    pub fn new(ty: impl Into<String>, mutable: bool) -> Self {
        Self {
            ty: ty.into(),
            mutable,
        }
    }
}

impl From<&Global> for GlobalType {
    fn from(global: &Global) -> Self {
        Self::new(global.wasm_ty.to_string(), global.mutability)
    }
}

impl fmt::Display for GlobalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mutability = if self.mutable { "mut" } else { "const" };
        write!(f, "global {} {mutability}", self.ty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "TableManifest")]
pub struct TableType {
    pub ty: String,
    pub limits: Limits,
}

/// The manifest form of a table, with its limits inline:
/// `{ "type": "funcref", "min": 1, "max": 2 }`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableManifest {
    #[serde(rename = "type")]
    ty: String,
    min: u64,
    max: Option<u64>,
}

impl From<TableManifest> for TableType {
    fn from(table: TableManifest) -> Self {
        Self::new(table.ty, Limits::new(table.min, table.max))
    }
}

impl TableType {
    #[must_use]
    pub fn new(ty: impl Into<String>, limits: Limits) -> Self {
        Self {
            ty: ty.into(),
            limits,
        }
    }

    #[must_use]
    pub fn satisfies_import(&self, expected: &Self) -> bool {
        self.ty == expected.ty && self.limits.satisfies_import(expected.limits)
    }
}

impl From<&Table> for TableType {
    fn from(table: &Table) -> Self {
        Self::new(
            table.wasm_ty.to_string(),
            Limits::new(u64::from(table.minimum), table.maximum.map(u64::from)),
        )
    }
}

impl fmt::Display for TableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "table {} ({})", self.ty, self.limits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternType {
    Func(FuncType),
    Global(GlobalType),
    Memory(Limits),
    Table(TableType),
}

impl ExternType {
    #[must_use]
    pub fn satisfies_import(&self, expected: &Self) -> bool {
        match (self, expected) {
            (Self::Func(actual), Self::Func(expected)) => actual == expected,
            (Self::Global(actual), Self::Global(expected)) => actual == expected,
            (Self::Memory(actual), Self::Memory(expected)) => actual.satisfies_import(*expected),
            (Self::Table(actual), Self::Table(expected)) => actual.satisfies_import(expected),
            _ => false,
        }
    }
}

impl fmt::Display for ExternType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Func(ty) => ty.fmt(f),
            Self::Global(ty) => ty.fmt(f),
            Self::Memory(limits) => write!(f, "memory ({limits})"),
            Self::Table(ty) => ty.fmt(f),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModuleInterface {
    pub funcs: BTreeMap<String, FuncType>,
    pub globals: BTreeMap<String, GlobalType>,
    pub memories: BTreeMap<String, Limits>,
    pub tables: BTreeMap<String, TableType>,
}

impl ModuleInterface {
    #[must_use]
    pub fn from_module(module: &NixModule) -> Self {
        let info = &module.info;
        Self {
            funcs: exports(&info.functions, |sig| {
                FuncType::from_signature(&info.signatures[*sig])
            }),
            globals: exports(&info.globals, |global| GlobalType::from(global)),
            memories: exports(&info.memories, |memory| Limits::from(memory)),
            tables: exports(&info.tables, |table| TableType::from(table)),
        }
    }

    #[must_use]
    pub fn get_export(&self, field: &str) -> Option<ExternType> {
        self.funcs
            .get(field)
            .cloned()
            .map(ExternType::Func)
            .or_else(|| self.globals.get(field).cloned().map(ExternType::Global))
            .or_else(|| self.memories.get(field).copied().map(ExternType::Memory))
            .or_else(|| self.tables.get(field).cloned().map(ExternType::Table))
    }
}

/// Maps every export name of `entities` to the type `ty` derives for it.
fn exports<K: EntityRef, E, T: Clone>(
    entities: &PrimaryMap<K, Exportable<E>>, ty: impl Fn(&E) -> T,
) -> BTreeMap<String, T> {
    entities
        .values()
        .flat_map(|entity| {
            let entity_ty = ty(&entity.entity);
            entity
                .export_names
                .iter()
                .map(move |name| (name.clone(), entity_ty.clone()))
        })
        .collect()
}

/// Import manifest: the interface each import module provides, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ImportMap {
    pub modules: BTreeMap<String, ModuleInterface>,
}

pub trait ImportResolver {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Resolves an import by module and field name.
    ///
    /// # Errors
    ///
    /// Returns an error when the resolver cannot inspect the requested import.
    fn resolve_import(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<Option<ExternType>, Self::Error>;
}

impl ImportResolver for ImportMap {
    type Error = Infallible;

    fn resolve_import(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<Option<ExternType>, Self::Error> {
        Ok(self
            .modules
            .get(module_name)
            .and_then(|module| module.get_export(field_name)))
    }
}

/// Validates that every import in `module` can be resolved to a compatible
/// export.
///
/// # Errors
///
/// Returns an error when an import is missing, incompatible, or cannot be
/// resolved by `resolver`.
pub fn validate_imports<R: ImportResolver>(
    module: &NixModule, resolver: &mut R,
) -> Result<(), R::Error> {
    // Imported entities occupy the first indices of each index space.
    let info = &module.info;
    let funcs = info
        .functions
        .values()
        .zip(&info.imported_funcs)
        .map(|(func, name)| {
            let ty = FuncType::from_signature(&info.signatures[func.entity]);
            (name, ExternType::Func(ty))
        });
    let globals = info
        .globals
        .values()
        .zip(&info.imported_globals)
        .map(|(global, name)| (name, ExternType::Global(GlobalType::from(&global.entity))));
    let tables = info
        .tables
        .values()
        .zip(&info.imported_tables)
        .map(|(table, name)| (name, ExternType::Table(TableType::from(&table.entity))));
    let memories = info
        .memories
        .values()
        .zip(&info.imported_memories)
        .map(|(memory, name)| (name, ExternType::Memory(Limits::from(&memory.entity))));

    for ((module_name, field_name), expected) in funcs.chain(globals).chain(tables).chain(memories)
    {
        validate_one(resolver, module_name, field_name, expected)?;
    }

    Ok(())
}

fn validate_one<R: ImportResolver>(
    resolver: &mut R, module: &str, field: &str, expected: ExternType,
) -> Result<(), R::Error> {
    let actual = resolver
        .resolve_import(module, field)
        .map_err(|source| LinkError::ResolveImport {
            module: module.to_string(),
            field: field.to_string(),
            source,
        })?
        .ok_or_else(|| LinkError::MissingImport {
            module: module.to_string(),
            field: field.to_string(),
            expected: Box::new(expected.clone()),
        })?;

    if actual.satisfies_import(&expected) {
        Ok(())
    } else {
        Err(LinkError::IncompatibleImport {
            module: module.to_string(),
            field: field.to_string(),
            expected: Box::new(expected),
            actual: Box::new(actual),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm_with_spectest_memory_import() -> Vec<u8> {
        let wat = r#"
            (module
              (memory (import "spectest" "memory") 1 2)
              (func (export "size") (result i32) memory.size))
        "#;
        let buf = wast::parser::ParseBuffer::new(wat).expect("wat should parse");
        let mut wat = wast::parser::parse::<wast::Wat<'_>>(&buf).expect("wat should parse");
        wat.encode().expect("wat should encode")
    }

    #[test]
    fn validates_compatible_imports() {
        let module = NixModule::translate(&wasm_with_spectest_memory_import())
            .expect("wasm should translate");
        let mut imports = ImportMap::default();
        imports.modules.insert(
            "spectest".to_string(),
            ModuleInterface {
                memories: BTreeMap::from([("memory".to_string(), Limits::new(1, Some(2)))]),
                ..ModuleInterface::default()
            },
        );

        validate_imports(&module, &mut imports).expect("imports should validate");
    }

    #[test]
    fn rejects_missing_imports() {
        let module = NixModule::translate(&wasm_with_spectest_memory_import())
            .expect("wasm should translate");
        let mut imports = ImportMap::default();

        let err = validate_imports(&module, &mut imports).expect_err("import should be missing");
        assert!(matches!(err, LinkError::MissingImport { .. }));
    }

    #[test]
    fn rejects_incompatible_limits() {
        let module = NixModule::translate(&wasm_with_spectest_memory_import())
            .expect("wasm should translate");
        let mut imports = ImportMap::default();
        imports.modules.insert(
            "spectest".to_string(),
            ModuleInterface {
                memories: BTreeMap::from([("memory".to_string(), Limits::new(0, Some(3)))]),
                ..ModuleInterface::default()
            },
        );

        let err = validate_imports(&module, &mut imports).expect_err("limits should fail");
        assert!(matches!(err, LinkError::IncompatibleImport { .. }));
    }
}
