use cranelift_codegen::ir::{self};
use wasm2nix_runtime::{RuntimeFunction, RuntimeResultKind, intrinsics as intr};

use crate::{
    lower::{
        error::{
            MissingTypeSnafu, Result, UnknownExternalNamespaceSnafu, UnknownIntrinsicSnafu,
            UnsupportedExternalNameSnafu,
        },
        inst::{InstCtx, LoweredInst, value_name},
        state::{lower_table_entry, signature_id},
    },
    nix::{Expr, nix},
    translator::NixModule,
};

impl InstCtx<'_> {
    pub(super) fn lower_call(&self) -> Result<LoweredInst> {
        let call_info = self.data.analyze_call(&self.func.dfg.value_lists);
        let ir::instructions::CallInfo::Direct(fref, args) = call_info else {
            return Ok(self.unsupported("wasm2nix: unsupported call kind"));
        };

        let callee = self.func.dfg.ext_funcs[fref].name.clone();
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
    module: &NixModule, func: &ir::Function, name: ir::ExternalName,
) -> Result<(Expr, CallKind)> {
    match name {
        ir::ExternalName::User(user_ref) => {
            let user = &func.params.user_named_funcs()[user_ref];
            match intr::UserNamespace::try_from(user.namespace) {
                Ok(intr::UserNamespace::Func) => {
                    // wasm function: f{index}
                    Ok((
                        Expr::ident(format!("f{}", user.index)),
                        CallKind::StatefulRecord,
                    ))
                }
                Ok(intr::UserNamespace::Intrinsic) => {
                    let intrinsic = intr::Intrinsic::try_from(user.index)
                        .map_err(|_| UnknownIntrinsicSnafu { index: user.index }.build())?;
                    resolve_intrinsic_callee(module, intrinsic, user.index)
                }
                Ok(intr::UserNamespace::Symbol) | Err(_) => UnknownExternalNamespaceSnafu {
                    namespace: user.namespace,
                }
                .fail(),
            }
        }
        other => UnsupportedExternalNameSnafu {
            name: format!("{other:?}"),
        }
        .fail(),
    }
}

fn resolve_intrinsic_callee(
    module: &NixModule, intrinsic: intr::Intrinsic, encoded_index: u32,
) -> Result<(Expr, CallKind)> {
    if let Some(function) = intrinsic.runtime_function() {
        let kind = intrinsic
            .runtime_result_kind()
            .expect("fixed runtime intrinsic has result kind");
        return Ok((function.expr(), runtime_result_to_call_kind(kind)));
    }

    match intrinsic {
        intr::Intrinsic::RefFunc { func_index } => {
            Ok((lower_table_entry(module, func_index), CallKind::Pure))
        }
        intr::Intrinsic::CallIndirect {
            table_index,
            type_index,
        } => {
            let sig_index = cranelift_wasm::TypeIndex::from_u32(type_index);
            let Some(sig) = module.info.signatures.get(sig_index) else {
                return MissingTypeSnafu { type_index }.fail();
            };
            Ok((
                RuntimeFunction::TableCall.expr(),
                CallKind::Indirect {
                    table_index,
                    type_id: signature_id(sig),
                },
            ))
        }
        _ => UnknownIntrinsicSnafu {
            index: encoded_index,
        }
        .fail(),
    }
}

const fn runtime_result_to_call_kind(kind: RuntimeResultKind) -> CallKind {
    match kind {
        RuntimeResultKind::Value => CallKind::Pure,
        RuntimeResultKind::State => CallKind::StatefulState,
        RuntimeResultKind::Record => CallKind::StatefulRecord,
    }
}
