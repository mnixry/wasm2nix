use cranelift_codegen::ir::{self};
use snafu::OptionExt;
use snafu::ResultExt;
use wasm2nix_runtime::RuntimeFunction;
use wasm2nix_runtime::RuntimeResultKind;
use wasm2nix_runtime::intrinsics::Intrinsic;
use wasm2nix_runtime::intrinsics::UserName;
use wasm2nix_runtime::intrinsics::UserNameParts;

use crate::lower::error::InvalidUserNameSnafu;
use crate::lower::error::MissingTypeSnafu;
use crate::lower::error::Result;
use crate::lower::error::UnsupportedExternalNameSnafu;
use crate::lower::inst::InstCtx;
use crate::lower::inst::LoweredInst;
use crate::lower::inst::value_name;
use crate::lower::state::lower_table_entry;
use crate::lower::state::signature_id;
use crate::nix::Expr;
use crate::nix::nix;
use crate::translator::NixModule;

impl InstCtx<'_> {
    pub(super) fn lower_call(&self) -> Result<LoweredInst> {
        let call_info = self.data.analyze_call(&self.func.dfg.value_lists);
        let ir::instructions::CallInfo::Direct(fref, args) = call_info else {
            return Ok(self.unsupported("wasm2nix: unsupported call kind"));
        };

        let callee = &self.func.dfg.ext_funcs[fref].name;
        let (callee_expr, call_kind) = resolve_callee(self.module, self.func, callee)?;

        match call_kind {
            CallKind::Pure => Ok(self.lower_pure_call(callee_expr, args)),
            CallKind::Indirect {
                table_index,
                type_id,
            } => Ok(self.lower_indirect_call(callee_expr, table_index, type_id, args)),
            CallKind::StatefulRecord => Ok(self.lower_stateful_record_call(callee_expr, args)),
            CallKind::StatefulState => Ok(self.lower_stateful_state_call(callee_expr, args)),
        }
    }

    fn lower_pure_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        let e = Expr::app(callee_expr, self.arg_exprs(args));
        self.bind_result(e)
    }

    fn lower_indirect_call(
        &self, callee_expr: Expr, table_index: u32, type_id: String, args: &[ir::Value],
    ) -> LoweredInst {
        let callee_value = args
            .first()
            .copied()
            .map_or_else(|| Expr::int(0), |value| self.value_expr(value));
        let wasm_args = args
            .iter()
            .copied()
            .skip(1)
            .filter(|v| *v != self.vmctx)
            .map(|v| self.value_expr(v))
            .collect::<Vec<_>>();
        let indirect_call = Expr::app(
            callee_expr,
            vec![
                Expr::int(i64::from(table_index)),
                Expr::string(type_id),
                Expr::ident("moduleId"),
                Expr::ident("funcs"),
                Expr::ident("funcRegistry"),
                callee_value,
                Expr::list(wasm_args),
                Expr::ident(self.st_name),
            ],
        );
        self.record_call_step(indirect_call)
    }

    fn lower_stateful_record_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        self.record_call_step(Expr::app(callee_expr, self.arg_exprs(args)))
    }

    fn lower_stateful_state_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        self.stateful_state_step(Expr::app(callee_expr, self.arg_exprs(args)))
    }

    pub(super) fn push_record_results(&self, tmp: &str, binds: &mut Vec<(String, Expr)>) {
        let results_list = nix!(#{Expr::ident(tmp)}.results);
        for (i, &rv) in self.results.iter().enumerate() {
            let idx = Expr::int(i64::try_from(i).expect("result index fits in i64"));
            let v = nix!(builtins.elemAt(#{results_list.clone()}, #{idx}));
            binds.push((value_name(rv), v));
        }
    }
}

#[derive(Debug, Clone)]
enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
    Indirect { table_index: u32, type_id: String },
}

fn resolve_callee(
    module: &NixModule, func: &ir::Function, name: &ir::ExternalName,
) -> Result<(Expr, CallKind)> {
    let ir::ExternalName::User(user_ref) = name else {
        return UnsupportedExternalNameSnafu {
            name: format!("{name:?}"),
        }
        .fail();
    };
    let user = &func.params.user_named_funcs()[*user_ref];
    let parts = UserNameParts {
        namespace: user.namespace,
        index: user.index,
    };
    match UserName::from_parts(parts).context(InvalidUserNameSnafu)? {
        UserName::Func(func_index) => Ok((
            Expr::ident(format!("f{}", func_index.as_u32())),
            CallKind::StatefulRecord,
        )),
        UserName::Intrinsic(intrinsic) => resolve_intrinsic_callee(module, intrinsic),
        UserName::MemoryBase(_) => UnsupportedExternalNameSnafu {
            name: format!("{name:?}"),
        }
        .fail(),
    }
}

fn resolve_intrinsic_callee(module: &NixModule, intrinsic: Intrinsic) -> Result<(Expr, CallKind)> {
    match intrinsic {
        Intrinsic::Fixed(fixed) => {
            let function = fixed.runtime_function();
            Ok((
                function.expr(),
                runtime_result_to_call_kind(function.result_kind()),
            ))
        }
        Intrinsic::RefFunc { func_index } => {
            Ok((lower_table_entry(module, func_index), CallKind::Pure))
        }
        Intrinsic::CallIndirect {
            table_index,
            type_index,
        } => {
            let sig = module
                .info
                .signatures
                .get(type_index)
                .context(MissingTypeSnafu {
                    type_index: type_index.as_u32(),
                })?;
            Ok((
                RuntimeFunction::TableCall.expr(),
                CallKind::Indirect {
                    table_index: table_index.as_u32(),
                    type_id: signature_id(sig),
                },
            ))
        }
    }
}

const fn runtime_result_to_call_kind(kind: RuntimeResultKind) -> CallKind {
    match kind {
        RuntimeResultKind::Value => CallKind::Pure,
        RuntimeResultKind::State => CallKind::StatefulState,
        RuntimeResultKind::Record => CallKind::StatefulRecord,
    }
}
