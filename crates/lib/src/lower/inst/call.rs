use cranelift_codegen::ir::{self};
use snafu::OptionExt;
use snafu::ResultExt;
use wasm2nix_runtime::RuntimeFunction;
use wasm2nix_runtime::RuntimeResultKind;
use wasm2nix_runtime::intrinsics::Intrinsic;
use wasm2nix_runtime::intrinsics::UserName;
use wasm2nix_runtime::intrinsics::UserNameParts;

use super::ctx::InstCtx;
use super::step::LoweredInst;
use crate::lower::error::InvalidUserNameSnafu;
use crate::lower::error::MissingTypeSnafu;
use crate::lower::error::Result;
use crate::lower::error::UnsupportedExternalNameSnafu;
use crate::lower::names::func_name;
use crate::lower::names::value_name;
use crate::lower::table_entry::lower_table_entry;
use crate::lower::table_entry::signature_id;
use crate::nix::Expr;
use crate::nix::LetBind;
use crate::nix::nix;
use crate::translator::NixModule;

impl InstCtx<'_> {
    pub(super) fn lower_call(&self) -> Result<LoweredInst> {
        let call_info = self.data.analyze_call(&self.func.dfg.value_lists);
        let ir::instructions::CallInfo::Direct(fref, args) = call_info else {
            return Ok(self.unsupported("wasm2nix: unsupported call kind"));
        };

        let callee = resolve_callee(self.module, self.func, &self.func.dfg.ext_funcs[fref].name)?;
        Ok(match callee.kind {
            CallKind::Pure => self.bind_result(Expr::app(callee.expr, self.arg_exprs(args))),
            CallKind::StatefulRecord => {
                self.record_call_step(Expr::app(callee.expr, self.arg_exprs(args)))
            }
            CallKind::StatefulState => {
                self.stateful_state_step(Expr::app(callee.expr, self.arg_exprs(args)))
            }
            CallKind::Indirect {
                table_index,
                type_id,
            } => {
                let Some((callee_value, args)) = args.split_first() else {
                    return self.unsupported_opcode();
                };
                self.lower_indirect_call(callee.expr, table_index, type_id, *callee_value, args)
            }
        })
    }

    fn lower_indirect_call(
        &self, table_call: Expr, table_index: u32, type_id: String, callee: ir::Value,
        args: &[ir::Value],
    ) -> LoweredInst {
        let wasm_args = args
            .iter()
            .filter(|v| **v != self.vmctx)
            .map(|v| self.value_expr(*v))
            .collect();
        let indirect_call = Expr::app(
            table_call,
            [
                Expr::int(i64::from(table_index)),
                Expr::string(type_id),
                Expr::ident("moduleId"),
                Expr::ident("funcs"),
                Expr::ident("funcRegistry"),
                self.value_expr(callee),
                Expr::list(wasm_args),
                Expr::ident(self.st_name),
            ],
        );
        self.record_call_step(indirect_call)
    }

    /// Binds each result of the call record `tmp` to its value name.
    pub(super) fn push_record_results(&self, tmp: &str, binds: &mut Vec<LetBind>) {
        let results_list = nix!(#{Expr::ident(tmp)}.results);
        for (index, result) in (0_i64..).zip(self.results) {
            let value = nix!(builtins.elemAt(#{results_list.clone()}, #{index}));
            binds.push(LetBind::new(value_name(*result), value));
        }
    }
}

/// A resolved call target and how its result is used.
struct Callee {
    expr: Expr,
    kind: CallKind,
}

enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
    Indirect { table_index: u32, type_id: String },
}

fn resolve_callee(
    module: &NixModule, func: &ir::Function, name: &ir::ExternalName,
) -> Result<Callee> {
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
        UserName::Func(func_index) => Ok(Callee {
            expr: Expr::ident(func_name(func_index)),
            kind: CallKind::StatefulRecord,
        }),
        UserName::Intrinsic(intrinsic) => resolve_intrinsic_callee(module, intrinsic),
        UserName::MemoryBase(_) => UnsupportedExternalNameSnafu {
            name: format!("{name:?}"),
        }
        .fail(),
    }
}

fn resolve_intrinsic_callee(module: &NixModule, intrinsic: Intrinsic) -> Result<Callee> {
    Ok(match intrinsic {
        Intrinsic::Fixed(fixed) => {
            let function = fixed.runtime_function();
            Callee {
                expr: function.expr(),
                kind: runtime_result_to_call_kind(function.result_kind()),
            }
        }
        Intrinsic::RefFunc { func_index } => Callee {
            expr: lower_table_entry(module, func_index),
            kind: CallKind::Pure,
        },
        Intrinsic::CallIndirect {
            table_index,
            type_index,
        } => {
            let sig = module
                .info
                .signatures
                .get(type_index)
                .context(MissingTypeSnafu { type_index })?;
            Callee {
                expr: RuntimeFunction::TableCall.expr(),
                kind: CallKind::Indirect {
                    table_index: table_index.as_u32(),
                    type_id: signature_id(sig),
                },
            }
        }
    })
}

const fn runtime_result_to_call_kind(kind: RuntimeResultKind) -> CallKind {
    match kind {
        RuntimeResultKind::Value => CallKind::Pure,
        RuntimeResultKind::State => CallKind::StatefulState,
        RuntimeResultKind::Record => CallKind::StatefulRecord,
    }
}
