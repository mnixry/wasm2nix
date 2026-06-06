use cranelift_codegen::ir::Opcode;

pub(super) const fn float_to_int_conv_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::FcvtToSint => Some("trunc_s"),
        Opcode::FcvtToUint => Some("trunc_u"),
        Opcode::FcvtToSintSat => Some("trunc_sat_s"),
        Opcode::FcvtToUintSat => Some("trunc_sat_u"),
        _ => None,
    }
}

pub(super) const fn int_to_float_conv_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::FcvtFromSint => Some("convert_s"),
        Opcode::FcvtFromUint => Some("convert_u"),
        _ => None,
    }
}

pub(super) const fn int_unary_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::Clz => Some("clz"),
        Opcode::Ctz => Some("ctz"),
        Opcode::Popcnt => Some("popcnt"),
        _ => None,
    }
}

pub(super) const fn float_unary_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::Fneg => Some("neg"),
        Opcode::Fabs => Some("abs"),
        Opcode::Sqrt => Some("sqrt"),
        Opcode::Ceil => Some("ceil"),
        Opcode::Floor => Some("floor"),
        Opcode::Trunc => Some("trunc"),
        Opcode::Nearest => Some("nearest"),
        _ => None,
    }
}

pub(super) const fn binary_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::Iadd | Opcode::Fadd => Some("add"),
        Opcode::Isub | Opcode::Fsub => Some("sub"),
        Opcode::Imul | Opcode::Fmul => Some("mul"),
        Opcode::Udiv => Some("udiv"),
        Opcode::Sdiv => Some("sdiv"),
        Opcode::Urem => Some("urem"),
        Opcode::Srem => Some("srem"),
        Opcode::Band => Some("band"),
        Opcode::Bor => Some("bor"),
        Opcode::Bxor => Some("bxor"),
        Opcode::Ishl => Some("shl"),
        Opcode::Ushr => Some("shr_u"),
        Opcode::Sshr => Some("shr_s"),
        Opcode::Rotl => Some("rotl"),
        Opcode::Rotr => Some("rotr"),
        Opcode::Fdiv => Some("div"),
        Opcode::Fmin => Some("min"),
        Opcode::Fmax => Some("max"),
        Opcode::Fcopysign => Some("copysign"),
        _ => None,
    }
}

pub(super) const fn binary_imm_name(op: Opcode) -> Option<&'static str> {
    match op {
        Opcode::IaddImm => Some("add"),
        Opcode::ImulImm => Some("mul"),
        Opcode::BandImm => Some("band"),
        Opcode::BorImm => Some("bor"),
        Opcode::BxorImm => Some("bxor"),
        Opcode::IshlImm => Some("shl"),
        Opcode::UshrImm => Some("shr_u"),
        Opcode::SshrImm => Some("shr_s"),
        _ => None,
    }
}
