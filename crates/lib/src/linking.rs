use std::{collections::BTreeMap, convert::Infallible, fmt};

use cranelift_codegen::ir;
use snafu::Snafu;

use crate::translator::NixModule;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max {
            Some(max) => write!(f, "min {}, max {}", self.min, max),
            None => write!(f, "min {}, max none", self.min),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalType {
    pub ty: String,
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

impl fmt::Display for GlobalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mutability = if self.mutable { "mut" } else { "const" };
        write!(f, "global {} {mutability}", self.ty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableType {
    pub ty: String,
    pub limits: Limits,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleInterface {
    pub funcs: BTreeMap<String, FuncType>,
    pub globals: BTreeMap<String, GlobalType>,
    pub memories: BTreeMap<String, Limits>,
    pub tables: BTreeMap<String, TableType>,
}

impl ModuleInterface {
    #[must_use]
    pub fn from_module(module: &NixModule) -> Self {
        let mut interface = Self::default();

        for (_idx, func) in &module.info.functions {
            let ty = FuncType::from_signature(&module.info.signatures[func.entity]);
            for name in &func.export_names {
                interface.funcs.insert(name.clone(), ty.clone());
            }
        }

        for (_idx, global) in &module.info.globals {
            let ty = GlobalType::new(global.entity.wasm_ty.to_string(), global.entity.mutability);
            for name in &global.export_names {
                interface.globals.insert(name.clone(), ty.clone());
            }
        }

        for (_idx, memory) in &module.info.memories {
            let limits = Limits::new(memory.entity.minimum, memory.entity.maximum);
            for name in &memory.export_names {
                interface.memories.insert(name.clone(), limits);
            }
        }

        for (_idx, table) in &module.info.tables {
            let ty = TableType::new(
                table.entity.wasm_ty.to_string(),
                Limits::new(
                    u64::from(table.entity.minimum),
                    table.entity.maximum.map(u64::from),
                ),
            );
            for name in &table.export_names {
                interface.tables.insert(name.clone(), ty.clone());
            }
        }

        interface
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportMap {
    pub modules: BTreeMap<String, ModuleInterface>,
}

impl ImportMap {
    #[must_use]
    pub const fn new(modules: BTreeMap<String, ModuleInterface>) -> Self {
        Self { modules }
    }
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
    for (idx, (module_name, field_name)) in module.info.imported_funcs.iter().enumerate() {
        let func_index = cranelift_wasm::FuncIndex::from_u32(idx as u32);
        let sig_index = module.info.functions[func_index].entity;
        let expected =
            ExternType::Func(FuncType::from_signature(&module.info.signatures[sig_index]));
        validate_one(resolver, module_name, field_name, expected)?;
    }

    for (idx, (module_name, field_name)) in module.info.imported_globals.iter().enumerate() {
        let global_index = cranelift_wasm::GlobalIndex::from_u32(idx as u32);
        let global = &module.info.globals[global_index].entity;
        let expected = ExternType::Global(GlobalType::new(
            global.wasm_ty.to_string(),
            global.mutability,
        ));
        validate_one(resolver, module_name, field_name, expected)?;
    }

    for (idx, (module_name, field_name)) in module.info.imported_tables.iter().enumerate() {
        let table_index = cranelift_wasm::TableIndex::from_u32(idx as u32);
        let table = &module.info.tables[table_index].entity;
        let expected = ExternType::Table(TableType::new(
            table.wasm_ty.to_string(),
            Limits::new(u64::from(table.minimum), table.maximum.map(u64::from)),
        ));
        validate_one(resolver, module_name, field_name, expected)?;
    }

    for (idx, (module_name, field_name)) in module.info.imported_memories.iter().enumerate() {
        let memory_index = cranelift_wasm::MemoryIndex::from_u32(idx as u32);
        let memory = &module.info.memories[memory_index].entity;
        let expected = ExternType::Memory(Limits::new(memory.minimum, memory.maximum));
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
