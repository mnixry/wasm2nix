use crate::nix::BinOp;
use crate::nix::Expr;
use crate::nix::LetBind;
use crate::nix::nix;

/// The bindings one instruction adds to its block, and what the block must
/// force for it.
pub(in crate::lower) struct Step {
    pub(in crate::lower) binds: Vec<LetBind>,
    pub(in crate::lower) force: Option<Force>,
}

/// Evaluation a block performs, in program order, before its terminator. Each
/// force lowers to an `assert` condition, which evaluates in place without a
/// call or a thunk for the rest of the block.
pub(in crate::lower) enum Force {
    /// A bound SSA value. Values are numbers, booleans or small ref attrsets,
    /// so `v == v` is a cheap way to force one.
    Value(Expr),
    /// A bound state, which is too large to compare.
    State(Expr),
    /// Throws `message` when `cond` is true (`trapnz`).
    TrapIf { cond: Expr, message: String },
    /// Throws `message` when `cond` is false (`trapz`).
    TrapUnless { cond: Expr, message: String },
}

impl Force {
    pub(in crate::lower) fn condition(self) -> Expr {
        match self {
            Self::Value(value) => nix!(#{value.clone()} == #{value}),
            Self::State(state) => nix!(builtins.seq(#{state}, true)),
            Self::TrapIf { cond, message } => {
                Expr::bin(BinOp::Implies, cond, nix!(throw(#{message})))
            }
            Self::TrapUnless { cond, message } => nix!(#{cond} || throw(#{message})),
        }
    }

    pub(in crate::lower) const fn expr(&self) -> &Expr {
        match self {
            Self::Value(expr)
            | Self::State(expr)
            | Self::TrapIf { cond: expr, .. }
            | Self::TrapUnless { cond: expr, .. } => expr,
        }
    }

    pub(in crate::lower) const fn expr_mut(&mut self) -> &mut Expr {
        match self {
            Self::Value(expr)
            | Self::State(expr)
            | Self::TrapIf { cond: expr, .. }
            | Self::TrapUnless { cond: expr, .. } => expr,
        }
    }
}

pub(in crate::lower) struct LoweredInst {
    pub(in crate::lower) step: Option<Step>,
    /// The state variable later instructions read, when this one replaced it.
    pub(in crate::lower) next_state: Option<String>,
}

impl LoweredInst {
    pub(super) const fn step(step: Step) -> Self {
        Self {
            step: Some(step),
            next_state: None,
        }
    }

    pub(super) const fn step_and_state(step: Step, next_state: String) -> Self {
        Self {
            step: Some(step),
            next_state: Some(next_state),
        }
    }
}
