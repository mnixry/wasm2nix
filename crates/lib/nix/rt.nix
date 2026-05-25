{ }:
let
  genPowers = n: value: if n > 62 then [ ] else [ value ] ++ genPowers (n + 1) (value * 2);

  powers = genPowers 0 1;
  pow2 = n: builtins.elemAt powers n;

  u32Mod = 4294967296;
  u32Max = 4294967295;
  i32Min = 0 - 2147483648;
  i32Sign = 2147483648;
  i64Max = 9223372036854775807;
  i64Min = 0 - 9223372036854775807 - 1;

  pageBytes = 65536;
  maxPages = 65536;

  elemAt = builtins.elemAt;
  length = builtins.length;
  toString = builtins.toString;

  mod = value: divisor: value - divisor * (value / divisor);

  floorMod =
    value: divisor:
    let
      r = mod value divisor;
    in
    if r < 0 then r + divisor else r;

  u32 = value: floorMod value u32Mod;

  toI32 =
    value:
    let
      v = u32 value;
    in
    if v >= i32Sign then v - u32Mod else v;

  checkI64 =
    value:
    if value < i64Min || value > i64Max then
      throw "wasm2nix runtime: i64 value is outside the host Nix integer range"
    else
      value;

  signExtend =
    bits: value:
    let
      modulus = pow2 bits;
      sign = pow2 (bits - 1);
      v = floorMod value modulus;
    in
    if v >= sign then v - modulus else v;

  signedDiv = lhs: rhs: if rhs == 0 then throw "wasm trap: integer divide by zero" else lhs / rhs;

  signedRem =
    lhs: rhs: if rhs == 0 then throw "wasm trap: integer divide by zero" else lhs - rhs * (lhs / rhs);

  mul32 =
    lhs: rhs:
    let
      a = u32 lhs;
      b = u32 rhs;
      aLo = floorMod a 65536;
      aHi = a / 65536;
      bLo = floorMod b 65536;
      bHi = b / 65536;
      low = aLo * bLo;
      mid = aLo * bHi + aHi * bLo;
    in
    u32 (low + floorMod mid 65536 * 65536);

  shiftCount32 = n: floorMod n 32;

  unsignedShiftRight32 = value: amount: (u32 value) / pow2 (shiftCount32 amount);

  signedShiftRight32 =
    value: amount:
    let
      signed = toI32 value;
      divisor = pow2 (shiftCount32 amount);
      quotient = if signed < 0 then 0 - (((0 - signed) + divisor - 1) / divisor) else signed / divisor;
    in
    u32 quotient;

  range = count: builtins.genList (i: i) count;

  bitSetU32 = value: bit: floorMod ((u32 value) / pow2 bit) 2 == 1;

  bitSetI64 = value: bit: i64BitSetParts (i64Parts value) bit;

  clzBy =
    width: bitSet: value:
    let
      result =
        builtins.foldl'
          (
            acc: bit:
            if acc.done then
              acc
            else if bitSet value (width - 1 - bit) then
              acc // { done = true; }
            else
              acc // { count = acc.count + 1; }
          )
          {
            done = false;
            count = 0;
          }
          (range width);
    in
    result.count;

  ctzBy =
    width: bitSet: value:
    let
      result =
        builtins.foldl'
          (
            acc: bit:
            if acc.done then
              acc
            else if bitSet value bit then
              acc // { done = true; }
            else
              acc // { count = acc.count + 1; }
          )
          {
            done = false;
            count = 0;
          }
          (range width);
    in
    result.count;

  popcntBy =
    width: bitSet: value:
    builtins.foldl' (acc: bit: if bitSet value bit then acc + 1 else acc) 0 (range width);

  i32Bin = {
    add = lhs: rhs: u32 (u32 lhs + u32 rhs);
    sub = lhs: rhs: u32 (u32 lhs - u32 rhs);
    mul = mul32;
    udiv =
      lhs: rhs:
      let
        divisor = u32 rhs;
      in
      if divisor == 0 then throw "wasm trap: integer divide by zero" else (u32 lhs) / divisor;
    sdiv =
      lhs: rhs:
      let
        a = toI32 lhs;
        b = toI32 rhs;
      in
      if a == i32Min && b == -1 then throw "wasm trap: integer overflow" else u32 (signedDiv a b);
    urem =
      lhs: rhs:
      let
        divisor = u32 rhs;
      in
      if divisor == 0 then throw "wasm trap: integer divide by zero" else floorMod (u32 lhs) divisor;
    srem =
      lhs: rhs:
      let
        a = toI32 lhs;
        b = toI32 rhs;
      in
      u32 (signedRem a b);
    band = lhs: rhs: u32 (builtins.bitAnd (u32 lhs) (u32 rhs));
    bor = lhs: rhs: u32 (builtins.bitOr (u32 lhs) (u32 rhs));
    bxor = lhs: rhs: u32 (builtins.bitXor (u32 lhs) (u32 rhs));
    bnot = value: u32Max - u32 value;
    shl = lhs: rhs: mul32 lhs (pow2 (shiftCount32 rhs));
    shr_u = unsignedShiftRight32;
    shr_s = signedShiftRight32;
    rotl =
      lhs: rhs:
      let
        n = shiftCount32 rhs;
      in
      if n == 0 then u32 lhs else i32Bin.bor (i32Bin.shl lhs n) (i32Bin.shr_u lhs (32 - n));
    rotr =
      lhs: rhs:
      let
        n = shiftCount32 rhs;
      in
      if n == 0 then u32 lhs else i32Bin.bor (i32Bin.shr_u lhs n) (i32Bin.shl lhs (32 - n));
    clz = clzBy 32 bitSetU32;
    ctz = ctzBy 32 bitSetU32;
    popcnt = popcntBy 32 bitSetU32;
  };

  isI64Parts = value: builtins.isAttrs value && value ? lo && value ? hi;

  i64Parts =
    value:
    if isI64Parts value then
      {
        lo = u32 value.lo;
        hi = u32 value.hi;
      }
    else
      let
        v = checkI64 value;
      in
      if v < 0 then
        let
          lower = v - i64Min;
        in
        {
          lo = floorMod lower u32Mod;
          hi = i32Sign + lower / u32Mod;
        }
      else
        {
          lo = floorMod v u32Mod;
          hi = v / u32Mod;
        };

  i64FromParts =
    parts:
    let
      hi = u32 parts.hi;
      lo = u32 parts.lo;
    in
    if hi >= i32Sign then
      let
        lower = (hi - i32Sign) * u32Mod + lo;
      in
      i64Min + lower
    else
      hi * u32Mod + lo;

  i64IsZeroParts = parts: parts.lo == 0 && parts.hi == 0;
  i64IsNegativeParts = parts: parts.hi >= i32Sign;
  i64PartsEq = lhs: rhs: lhs.lo == rhs.lo && lhs.hi == rhs.hi;

  i64BitSetParts =
    parts: bit: if bit < 32 then bitSetU32 parts.lo bit else bitSetU32 parts.hi (bit - 32);

  i64SetBitParts =
    parts: bit:
    if bit < 32 then
      parts // { lo = u32 (builtins.bitOr parts.lo (pow2 bit)); }
    else
      parts // { hi = u32 (builtins.bitOr parts.hi (pow2 (bit - 32))); };

  i64AddParts =
    lhs: rhs:
    let
      loSum = lhs.lo + rhs.lo;
      lo = floorMod loSum u32Mod;
      carry = loSum / u32Mod;
      hi = u32 (lhs.hi + rhs.hi + carry);
    in
    {
      inherit lo hi;
    };

  i64SubParts =
    lhs: rhs:
    let
      loRaw = lhs.lo - rhs.lo;
      borrow = if loRaw < 0 then 1 else 0;
      lo = floorMod loRaw u32Mod;
      hi = u32 (lhs.hi - rhs.hi - borrow);
    in
    {
      inherit lo hi;
    };

  i64NegParts =
    parts:
    let
      lo = u32 (0 - parts.lo);
      borrow = if parts.lo == 0 then 0 else 1;
      hi = u32 (0 - parts.hi - borrow);
    in
    {
      inherit lo hi;
    };

  i64AbsParts =
    value:
    let
      parts = i64Parts value;
    in
    if i64IsNegativeParts parts then i64NegParts parts else parts;

  i64UnsignedGteParts = lhs: rhs: lhs.hi > rhs.hi || (lhs.hi == rhs.hi && lhs.lo >= rhs.lo);
  i64UnsignedGtParts = lhs: rhs: i64UnsignedGteParts lhs rhs && !(i64PartsEq lhs rhs);

  i64ShlParts =
    parts: amount:
    let
      n = floorMod amount 64;
    in
    if n == 0 then
      parts
    else if n < 32 then
      let
        loShifted = parts.lo * pow2 n;
        lo = u32 loShifted;
        hi = u32 (parts.hi * pow2 n + parts.lo / pow2 (32 - n));
      in
      {
        inherit lo hi;
      }
    else
      {
        lo = 0;
        hi = u32 (parts.lo * pow2 (n - 32));
      };

  i64ShrUParts =
    parts: amount:
    let
      n = floorMod amount 64;
    in
    if n == 0 then
      parts
    else if n < 32 then
      let
        lo = parts.lo / pow2 n + floorMod parts.hi (pow2 n) * pow2 (32 - n);
        hi = parts.hi / pow2 n;
      in
      {
        inherit lo hi;
      }
    else
      {
        lo = parts.hi / pow2 (n - 32);
        hi = 0;
      };

  i64ShrSParts =
    parts: amount:
    builtins.foldl' (
      acc: _:
      let
        sign = i64IsNegativeParts acc;
        lo = acc.lo / 2 + (if floorMod acc.hi 2 == 1 then i32Sign else 0);
        hi = acc.hi / 2 + (if sign then i32Sign else 0);
      in
      {
        inherit lo hi;
      }
    ) parts (range (floorMod amount 64));

  i64MulParts =
    lhs: rhs:
    let
      base = 65536;
      a0 = floorMod lhs.lo base;
      a1 = lhs.lo / base;
      a2 = floorMod lhs.hi base;
      a3 = lhs.hi / base;
      b0 = floorMod rhs.lo base;
      b1 = rhs.lo / base;
      b2 = floorMod rhs.hi base;
      b3 = rhs.hi / base;

      c0 = a0 * b0;
      r0 = floorMod c0 base;
      carry0 = c0 / base;

      c1 = a0 * b1 + a1 * b0 + carry0;
      r1 = floorMod c1 base;
      carry1 = c1 / base;

      c2 = a0 * b2 + a1 * b1 + a2 * b0 + carry1;
      r2 = floorMod c2 base;
      carry2 = c2 / base;

      c3 = a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + carry2;
      r3 = floorMod c3 base;

      lo = r0 + r1 * base;
      hi = r2 + r3 * base;
    in
    {
      inherit lo hi;
    };

  i64UnsignedDivRemParts =
    lhs: rhs:
    if i64IsZeroParts rhs then
      throw "wasm trap: integer divide by zero"
    else
      builtins.foldl'
        (
          acc: i:
          let
            bit = 63 - i;
            shifted = i64ShlParts acc.rem 1;
            remWithBit = if i64BitSetParts lhs bit then i64SetBitParts shifted 0 else shifted;
          in
          if i64UnsignedGteParts remWithBit rhs then
            {
              quot = i64SetBitParts acc.quot bit;
              rem = i64SubParts remWithBit rhs;
            }
          else
            acc // { rem = remWithBit; }
        )
        {
          quot = {
            lo = 0;
            hi = 0;
          };
          rem = {
            lo = 0;
            hi = 0;
          };
        }
        (range 64);

  i64SignedDiv =
    lhs: rhs:
    let
      a = checkI64 lhs;
      b = checkI64 rhs;
      aNeg = a < 0;
      bNeg = b < 0;
    in
    if b == 0 then
      throw "wasm trap: integer divide by zero"
    else if a == i64Min && b == -1 then
      throw "wasm trap: integer overflow"
    else
      let
        quot = (i64UnsignedDivRemParts (i64AbsParts a) (i64AbsParts b)).quot;
        signed = if aNeg != bNeg then i64NegParts quot else quot;
      in
      i64FromParts signed;

  i64SignedRem =
    lhs: rhs:
    let
      a = checkI64 lhs;
      b = checkI64 rhs;
    in
    if b == 0 then
      throw "wasm trap: integer divide by zero"
    else
      let
        rem = (i64UnsignedDivRemParts (i64AbsParts a) (i64AbsParts b)).rem;
        signed = if a < 0 then i64NegParts rem else rem;
      in
      i64FromParts signed;

  i64Bin = rec {
    add = lhs: rhs: i64FromParts (i64AddParts (i64Parts lhs) (i64Parts rhs));
    sub = lhs: rhs: i64FromParts (i64SubParts (i64Parts lhs) (i64Parts rhs));
    mul = lhs: rhs: i64FromParts (i64MulParts (i64Parts lhs) (i64Parts rhs));
    udiv = lhs: rhs: i64FromParts (i64UnsignedDivRemParts (i64Parts lhs) (i64Parts rhs)).quot;
    sdiv = lhs: rhs: i64SignedDiv lhs rhs;
    urem = lhs: rhs: i64FromParts (i64UnsignedDivRemParts (i64Parts lhs) (i64Parts rhs)).rem;
    srem = i64SignedRem;
    band =
      lhs: rhs:
      let
        a = i64Parts lhs;
        b = i64Parts rhs;
      in
      i64FromParts {
        lo = builtins.bitAnd a.lo b.lo;
        hi = builtins.bitAnd a.hi b.hi;
      };
    bor =
      lhs: rhs:
      let
        a = i64Parts lhs;
        b = i64Parts rhs;
      in
      i64FromParts {
        lo = builtins.bitOr a.lo b.lo;
        hi = builtins.bitOr a.hi b.hi;
      };
    bxor =
      lhs: rhs:
      let
        a = i64Parts lhs;
        b = i64Parts rhs;
      in
      i64FromParts {
        lo = builtins.bitXor a.lo b.lo;
        hi = builtins.bitXor a.hi b.hi;
      };
    bnot =
      value:
      i64FromParts (
        i64NegParts (
          i64AddParts (i64Parts value) {
            lo = 1;
            hi = 0;
          }
        )
      );
    shl = lhs: rhs: i64FromParts (i64ShlParts (i64Parts lhs) rhs);
    shr_u = lhs: rhs: i64FromParts (i64ShrUParts (i64Parts lhs) rhs);
    shr_s = lhs: rhs: i64FromParts (i64ShrSParts (i64Parts lhs) rhs);
    rotl =
      lhs: rhs:
      let
        n = floorMod rhs 64;
      in
      if n == 0 then lhs else bor (shl lhs n) (shr_u lhs (64 - n));
    rotr =
      lhs: rhs:
      let
        n = floorMod rhs 64;
      in
      if n == 0 then lhs else bor (shr_u lhs n) (shl lhs (64 - n));
    clz = value: checkI64 (clzBy 64 bitSetI64 value);
    ctz = value: checkI64 (ctzBy 64 bitSetI64 value);
    popcnt = value: checkI64 (popcntBy 64 bitSetI64 value);
  };

  unsupportedFloat =
    ty: op: _lhs: _rhs:
    throw "wasm2nix runtime: ${ty}.${op} requires the soft IEEE-754 runtime";

  unsupportedUnaryFloat =
    ty: op: _value:
    throw "wasm2nix runtime: ${ty}.${op} requires the soft IEEE-754 runtime";

  cmp = {
    eq = lhs: rhs: lhs == rhs;
    ne = lhs: rhs: lhs != rhs;
    lt_u = lhs: rhs: u32 lhs < u32 rhs;
    le_u = lhs: rhs: u32 lhs <= u32 rhs;
    gt_u = lhs: rhs: u32 lhs > u32 rhs;
    ge_u = lhs: rhs: u32 lhs >= u32 rhs;
    lt_s = lhs: rhs: toI32 lhs < toI32 rhs;
    le_s = lhs: rhs: toI32 lhs <= toI32 rhs;
    gt_s = lhs: rhs: toI32 lhs > toI32 rhs;
    ge_s = lhs: rhs: toI32 lhs >= toI32 rhs;
  };

  cmp_i64 =
    let
      eqParts =
        lhs: rhs:
        let
          a = i64Parts lhs;
          b = i64Parts rhs;
        in
        a.lo == b.lo && a.hi == b.hi;
    in
    {
      eq = eqParts;
      ne = lhs: rhs: !(eqParts lhs rhs);
      lt_u = lhs: rhs: !(i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs));
      le_u = lhs: rhs: i64UnsignedGteParts (i64Parts rhs) (i64Parts lhs);
      gt_u = lhs: rhs: i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs) && !(eqParts lhs rhs);
      ge_u = lhs: rhs: i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs);
      lt_s = lhs: rhs: checkI64 lhs < checkI64 rhs;
      le_s = lhs: rhs: checkI64 lhs <= checkI64 rhs;
      gt_s = lhs: rhs: checkI64 lhs > checkI64 rhs;
      ge_s = lhs: rhs: checkI64 lhs >= checkI64 rhs;
    };

  floatCmpFns =
    isNan: isZero: bitsEq: lt:
    let
      eval =
        lhs: rhs:
        let
          unordered = isNan lhs || isNan rhs;
          zeros = isZero lhs && isZero rhs;
          eq = !unordered && (zeros || bitsEq lhs rhs);
          less = !unordered && !zeros && lt lhs rhs;
          greater = !unordered && !eq && !less;
        in
        {
          ord = !unordered;
          uno = unordered;
          inherit eq less greater;
          ne = unordered || less || greater;
          one = !unordered && !eq;
          ueq = unordered || eq;
          le = eq || less;
          ge = eq || greater;
          ult = unordered || less;
          ule = unordered || eq || less;
          ugt = unordered || greater;
          uge = unordered || eq || greater;
        };
    in
    {
      ord = lhs: rhs: (eval lhs rhs).ord;
      uno = lhs: rhs: (eval lhs rhs).uno;
      eq = lhs: rhs: (eval lhs rhs).eq;
      ne = lhs: rhs: (eval lhs rhs).ne;
      one = lhs: rhs: (eval lhs rhs).one;
      ueq = lhs: rhs: (eval lhs rhs).ueq;
      lt = lhs: rhs: (eval lhs rhs).less;
      le = lhs: rhs: (eval lhs rhs).le;
      gt = lhs: rhs: (eval lhs rhs).greater;
      ge = lhs: rhs: (eval lhs rhs).ge;
      ult = lhs: rhs: (eval lhs rhs).ult;
      ule = lhs: rhs: (eval lhs rhs).ule;
      ugt = lhs: rhs: (eval lhs rhs).ugt;
      uge = lhs: rhs: (eval lhs rhs).uge;
    };

  shiftRightJam =
    value: amount:
    if amount <= 0 then
      value * pow2 (0 - amount)
    else if amount >= 63 then
      if value == 0 then 0 else 1
    else
      let
        divisor = pow2 amount;
        quotient = value / divisor;
        remainder = floorMod value divisor;
      in
      if remainder == 0 then quotient else builtins.bitOr quotient 1;

  isqrt =
    value:
    let
      search =
        lo: hi:
        if lo + 1 >= hi then
          lo
        else
          let
            mid = (lo + hi) / 2;
          in
          if mid * mid <= value then search mid hi else search lo mid;
    in
    search 0 (pow2 28);

  f32Abs = value: builtins.bitAnd (u32 value) 2147483647;
  f32CanonicalNan = 2143289344;
  f32PositiveInf = 2139095040;
  f32IsNaN =
    value:
    let
      bits = u32 value;
    in
    builtins.bitAnd bits 2139095040 == 2139095040 && builtins.bitAnd bits 8388607 != 0;
  f32IsInf =
    value:
    let
      bits = u32 value;
    in
    builtins.bitAnd bits 2147483647 == f32PositiveInf;
  f32IsZero = value: f32Abs value == 0;
  f32SignBit = value: if u32 value >= i32Sign then i32Sign else 0;
  f32Key =
    value:
    let
      bits = u32 value;
    in
    if bits >= i32Sign then u32Max - bits else bits + i32Sign;

  f64IsNaN =
    value:
    let
      parts = i64Parts value;
      exponent = floorMod (parts.hi / 1048576) 2048;
      fracHi = floorMod parts.hi 1048576;
    in
    exponent == 2047 && (fracHi != 0 || parts.lo != 0);
  f64IsZero =
    value:
    let
      parts = i64Parts value;
    in
    builtins.bitAnd parts.hi 2147483647 == 0 && parts.lo == 0;
  f64Key =
    value:
    let
      parts = i64Parts value;
    in
    if i64IsNegativeParts parts then
      {
        lo = u32Max - parts.lo;
        hi = u32Max - parts.hi;
      }
    else
      {
        lo = parts.lo;
        hi = builtins.bitOr parts.hi i32Sign;
      };
  f64KeyLt =
    lhs: rhs:
    let
      a = f64Key lhs;
      b = f64Key rhs;
    in
    a.hi < b.hi || (a.hi == b.hi && a.lo < b.lo);

  fcmp = {
    f32 = floatCmpFns f32IsNaN f32IsZero (lhs: rhs: u32 lhs == u32 rhs) (
      lhs: rhs: f32Key lhs < f32Key rhs
    );
    f64 = floatCmpFns f64IsNaN f64IsZero (lhs: rhs: cmp_i64.eq lhs rhs) f64KeyLt;
  };

  f32Min =
    lhs: rhs:
    if f32IsNaN lhs || f32IsNaN rhs then
      f32BinaryNaN lhs rhs
    else if f32IsZero lhs && f32IsZero rhs then
      if u32 lhs >= i32Sign || u32 rhs >= i32Sign then i32Sign else 0
    else if fcmp.f32.le lhs rhs then
      u32 lhs
    else
      u32 rhs;

  f32Max =
    lhs: rhs:
    if f32IsNaN lhs || f32IsNaN rhs then
      f32BinaryNaN lhs rhs
    else if f32IsZero lhs && f32IsZero rhs then
      if u32 lhs < i32Sign || u32 rhs < i32Sign then 0 else i32Sign
    else if fcmp.f32.ge lhs rhs then
      u32 lhs
    else
      u32 rhs;

  f32Copysign = lhs: rhs: u32 (f32Abs lhs + (if u32 rhs >= i32Sign then i32Sign else 0));
  f32QuietNaN =
    value:
    let
      bits = u32 value;
    in
    if f32IsNaN bits then u32 (builtins.bitOr bits 4194304) else f32CanonicalNan;
  f32BinaryNaN =
    lhs: rhs:
    if f32IsNaN lhs then
      f32QuietNaN lhs
    else if f32IsNaN rhs then
      f32QuietNaN rhs
    else
      f32CanonicalNan;
  f32Inf = sign: u32 ((if sign then i32Sign else 0) + f32PositiveInf);
  f32SignedZero = sign: if sign then i32Sign else 0;
  f32Neg = value: u32 (builtins.bitXor (u32 value) i32Sign);
  f32FiniteInfo =
    value:
    let
      parts = f32Parts value;
    in
    if parts.rawExp == 0 then
      {
        inherit (parts) sign;
        exp = -126;
        sig = parts.frac;
      }
    else
      {
        inherit (parts) sign;
        exp = parts.rawExp - 127;
        sig = 8388608 + parts.frac;
      };
  f32PackExt =
    sign: exp: sigExt:
    let
      shifted =
        if exp < -126 then
          {
            exp = -126;
            sigExt = shiftRightJam sigExt (-126 - exp);
          }
        else
          { inherit exp sigExt; };
      quotient = shifted.sigExt / 8;
      remainder = floorMod shifted.sigExt 8;
      roundUp = remainder > 4 || (remainder == 4 && floorMod quotient 2 == 1);
      rounded0 = quotient + (if roundUp then 1 else 0);
      carried = rounded0 >= 16777216;
      rounded = if carried then rounded0 / 2 else rounded0;
      exp2 = shifted.exp + (if carried then 1 else 0);
      signBit = if sign then i32Sign else 0;
    in
    if rounded == 0 then
      signBit
    else if exp2 >= 128 then
      f32Inf sign
    else if exp2 == -126 && rounded < 8388608 then
      u32 (signBit + rounded)
    else
      u32 (signBit + (exp2 + 127) * 8388608 + (rounded - 8388608));
  f32RoundFromParts =
    sign: sig: scale:
    let
      bitLength = intBitLength sig;
      exp = bitLength - 1 + scale;
      shift = bitLength - 27;
      sigExt = shiftRightJam sig shift;
    in
    if sig == 0 then f32SignedZero sign else f32PackExt sign exp sigExt;
  f32NormalizeAdd =
    exp: sigExt:
    if sigExt == 0 || sigExt >= 67108864 || exp <= -126 then
      { inherit exp sigExt; }
    else
      f32NormalizeAdd (exp - 1) (sigExt * 2);
  f32AddFinite =
    lhs: rhs:
    let
      a = f32FiniteInfo lhs;
      b = f32FiniteInfo rhs;
      useAExp = a.exp >= b.exp;
      exp = if useAExp then a.exp else b.exp;
      aExt = if useAExp then a.sig * 8 else shiftRightJam (a.sig * 8) (b.exp - a.exp);
      bExt = if useAExp then shiftRightJam (b.sig * 8) (a.exp - b.exp) else b.sig * 8;
      sameSign = a.sign == b.sign;
    in
    if sameSign then
      let
        sum = aExt + bExt;
        normalized =
          if sum >= 134217728 then
            {
              exp = exp + 1;
              sigExt = shiftRightJam sum 1;
            }
          else
            {
              inherit exp;
              sigExt = sum;
            };
      in
      f32PackExt a.sign normalized.exp normalized.sigExt
    else if a.exp == b.exp && a.sig == b.sig then
      0
    else
      let
        aGreater = a.exp > b.exp || (a.exp == b.exp && a.sig > b.sig);
        diff = if aGreater then aExt - bExt else bExt - aExt;
        sign = if aGreater then a.sign else b.sign;
        normalized = f32NormalizeAdd exp diff;
      in
      f32PackExt sign normalized.exp normalized.sigExt;
  f32Add =
    lhs: rhs:
    if f32IsNaN lhs then
      f32BinaryNaN lhs rhs
    else if f32IsNaN rhs then
      f32BinaryNaN lhs rhs
    else if f32IsInf lhs || f32IsInf rhs then
      if f32IsInf lhs && f32IsInf rhs && f32SignBit lhs != f32SignBit rhs then
        f32CanonicalNan
      else if f32IsInf lhs then
        u32 lhs
      else
        u32 rhs
    else if f32IsZero lhs && f32IsZero rhs then
      if f32SignBit lhs == i32Sign && f32SignBit rhs == i32Sign then i32Sign else 0
    else if f32IsZero lhs then
      u32 rhs
    else if f32IsZero rhs then
      u32 lhs
    else
      f32AddFinite lhs rhs;
  f32Sub = lhs: rhs: f32Add lhs (f32Neg rhs);
  f32Mul =
    lhs: rhs:
    let
      sign = f32SignBit lhs != f32SignBit rhs;
    in
    if f32IsNaN lhs || f32IsNaN rhs then
      f32BinaryNaN lhs rhs
    else if (f32IsInf lhs && f32IsZero rhs) || (f32IsZero lhs && f32IsInf rhs) then
      f32CanonicalNan
    else if f32IsInf lhs || f32IsInf rhs then
      f32Inf sign
    else if f32IsZero lhs || f32IsZero rhs then
      f32SignedZero sign
    else
      let
        a = f32FiniteInfo lhs;
        b = f32FiniteInfo rhs;
      in
      f32RoundFromParts sign (a.sig * b.sig) (a.exp + b.exp - 46);
  f32Div =
    lhs: rhs:
    let
      sign = f32SignBit lhs != f32SignBit rhs;
    in
    if f32IsNaN lhs || f32IsNaN rhs then
      f32BinaryNaN lhs rhs
    else if (f32IsInf lhs && f32IsInf rhs) || (f32IsZero lhs && f32IsZero rhs) then
      f32CanonicalNan
    else if f32IsInf lhs || f32IsZero rhs then
      f32Inf sign
    else if f32IsZero lhs || f32IsInf rhs then
      f32SignedZero sign
    else
      let
        a = f32FiniteInfo lhs;
        b = f32FiniteInfo rhs;
        shift = 37;
        numerator = a.sig * pow2 shift;
        quotient = numerator / b.sig;
        remainder = floorMod numerator b.sig;
        sig = quotient + (if remainder == 0 then 0 else 1);
      in
      f32RoundFromParts sign sig (a.exp - b.exp - shift);
  f32NormalizeFinite =
    sig: exp: if sig >= 8388608 then { inherit sig exp; } else f32NormalizeFinite (sig * 2) (exp - 1);
  f32Sqrt =
    value:
    if f32IsNaN value then
      f32QuietNaN value
    else if f32IsInf value then
      if f32SignBit value == i32Sign then f32CanonicalNan else u32 value
    else if f32IsZero value then
      u32 value
    else if f32SignBit value == i32Sign then
      f32CanonicalNan
    else
      let
        info0 = f32FiniteInfo value;
        info = if info0.sig < 8388608 then f32NormalizeFinite info0.sig info0.exp else info0;
        oddExp = floorMod info.exp 2 != 0;
        resultExp = if oddExp then (info.exp - 1) / 2 else info.exp / 2;
        radicand = info.sig * pow2 (if oddExp then 30 else 29);
        root = isqrt radicand;
        sigExt = if root * root == radicand then root else builtins.bitOr root 1;
      in
      f32PackExt false resultExp sigExt;
  f32RoundIntegral =
    mode: value:
    let
      parts = f32Parts value;
      sign = parts.sign;
      signedZero = f32SignedZero sign;
    in
    if f32IsNaN value then
      f32QuietNaN value
    else if f32IsInf value || f32IsZero value then
      u32 value
    else if parts.rawExp < 127 then
      if mode == "floor" then
        if sign then f32FromExactInt "floor" (-1) else 0
      else if mode == "ceil" then
        if sign then signedZero else f32FromExactInt "ceil" 1
      else if mode == "nearest" then
        if parts.rawExp < 126 || (parts.rawExp == 126 && parts.frac == 0) then
          signedZero
        else
          f32FromExactInt "nearest" (if sign then -1 else 1)
      else
        signedZero
    else
      let
        exp = parts.rawExp - 127;
        sig = 8388608 + parts.frac;
      in
      if exp >= 23 then
        u32 value
      else
        let
          divisor = pow2 (23 - exp);
          int = sig / divisor;
          frac = floorMod sig divisor;
          half = divisor / 2;
          roundedInt =
            if mode == "floor" then
              int + (if sign && frac != 0 then 1 else 0)
            else if mode == "ceil" then
              int + (if !sign && frac != 0 then 1 else 0)
            else if mode == "nearest" then
              int + (if frac > half || (frac == half && floorMod int 2 == 1) then 1 else 0)
            else
              int;
        in
        if frac == 0 then u32 value else f32FromExactInt mode (if sign then 0 - roundedInt else roundedInt);

  f64CanonicalNan = i64FromParts {
    lo = 0;
    hi = 2146959360;
  };

  f64Min =
    lhs: rhs:
    if f64IsNaN lhs || f64IsNaN rhs then
      f64BinaryNaN lhs rhs
    else if f64IsZero lhs && f64IsZero rhs then
      if i64IsNegativeParts (i64Parts lhs) || i64IsNegativeParts (i64Parts rhs) then i64Min else 0
    else if fcmp.f64.le lhs rhs then
      lhs
    else
      rhs;

  f64Max =
    lhs: rhs:
    if f64IsNaN lhs || f64IsNaN rhs then
      f64BinaryNaN lhs rhs
    else if f64IsZero lhs && f64IsZero rhs then
      if !(i64IsNegativeParts (i64Parts lhs)) || !(i64IsNegativeParts (i64Parts rhs)) then 0 else i64Min
    else if fcmp.f64.ge lhs rhs then
      lhs
    else
      rhs;

  f64Copysign =
    lhs: rhs:
    let
      a = i64Parts lhs;
      b = i64Parts rhs;
    in
    i64FromParts {
      lo = a.lo;
      hi = builtins.bitOr (builtins.bitAnd a.hi 2147483647) (builtins.bitAnd b.hi i32Sign);
    };
  f64SignBit = value: builtins.bitAnd (i64Parts value).hi i32Sign;
  f64QuietNaN =
    value:
    let
      parts = i64Parts value;
    in
    if f64IsNaN value then
      i64FromParts {
        lo = parts.lo;
        hi = builtins.bitOr parts.hi 524288;
      }
    else
      f64CanonicalNan;
  f64Neg = f64MapParts (parts: parts // { hi = builtins.bitXor parts.hi i32Sign; });
  f64Abs = f64MapParts (parts: parts // { hi = builtins.bitAnd parts.hi 2147483647; });
  f64PositiveInfParts = {
    lo = 0;
    hi = 2146435072;
  };
  f64IsInf =
    value:
    let
      parts = i64Parts value;
    in
    builtins.bitAnd parts.hi 2147483647 == f64PositiveInfParts.hi && parts.lo == 0;
  f64BinaryNaN =
    lhs: rhs:
    if f64IsNaN lhs then
      f64QuietNaN lhs
    else if f64IsNaN rhs then
      f64QuietNaN rhs
    else
      f64CanonicalNan;
  f64Inf =
    sign:
    i64FromParts {
      lo = 0;
      hi = (if sign then i32Sign else 0) + f64PositiveInfParts.hi;
    };
  f64SignedZero = sign: if sign then i64Min else 0;
  f64FiniteInfo =
    value:
    let
      parts = f64Parts value;
      frac = parts.fracHi * u32Mod + parts.fracLo;
    in
    if parts.rawExp == 0 then
      {
        inherit (parts) sign;
        exp = -1022;
        sig = frac;
      }
    else
      {
        inherit (parts) sign;
        exp = parts.rawExp - 1023;
        sig = 4503599627370496 + frac;
      };
  big26Base = 67108864;
  big26Limb =
    limbs: idx:
    if idx == 0 then
      limbs.l0
    else if idx == 1 then
      limbs.l1
    else if idx == 2 then
      limbs.l2
    else if idx == 3 then
      limbs.l3
    else
      limbs.l4;
  big26HighToSmall =
    limbs: start:
    let
      go =
        idx: acc:
        if idx < start || idx < 0 then acc else go (idx - 1) (acc * big26Base + big26Limb limbs idx);
    in
    go 4 0;
  big26AnyBelow =
    limbs: count:
    if count <= 0 then false else builtins.any (idx: big26Limb limbs idx != 0) (range count);
  big26BitLength =
    limbs:
    if limbs.l4 != 0 then
      104 + intBitLength limbs.l4
    else if limbs.l3 != 0 then
      78 + intBitLength limbs.l3
    else if limbs.l2 != 0 then
      52 + intBitLength limbs.l2
    else if limbs.l1 != 0 then
      26 + intBitLength limbs.l1
    else
      intBitLength limbs.l0;
  big26ShiftRightJamToSmall =
    limbs: amount:
    let
      k = (amount + 25) / 26;
      excess = 26 * k - amount;
      high = big26HighToSmall limbs k;
      top = big26Limb limbs (k - 1);
      topShift = 26 - excess;
      lowContribution = if excess == 0 then 0 else top / pow2 topShift;
      topRemainder = if excess == 0 then top else floorMod top (pow2 topShift);
      sticky = topRemainder != 0 || big26AnyBelow limbs (k - 1);
      quotient = high * pow2 excess + lowContribution;
    in
    if sticky then builtins.bitOr quotient 1 else quotient;
  big26ToSmall = limbs: big26HighToSmall limbs 0;
  f64MulSigLimbs =
    lhs: rhs:
    let
      a0 = floorMod lhs big26Base;
      a1 = floorMod (lhs / big26Base) big26Base;
      a2 = lhs / (big26Base * big26Base);
      b0 = floorMod rhs big26Base;
      b1 = floorMod (rhs / big26Base) big26Base;
      b2 = rhs / (big26Base * big26Base);

      t0 = a0 * b0;
      l0 = floorMod t0 big26Base;
      carry0 = t0 / big26Base;

      t1 = a0 * b1 + a1 * b0 + carry0;
      l1 = floorMod t1 big26Base;
      carry1 = t1 / big26Base;

      t2 = a0 * b2 + a1 * b1 + a2 * b0 + carry1;
      l2 = floorMod t2 big26Base;
      carry2 = t2 / big26Base;

      t3 = a1 * b2 + a2 * b1 + carry2;
      l3 = floorMod t3 big26Base;
      carry3 = t3 / big26Base;

      t4 = a2 * b2 + carry3;
      l4 = floorMod t4 big26Base;
    in
    {
      inherit
        l0
        l1
        l2
        l3
        l4
        ;
    };
  big26Le =
    lhs: rhs:
    if lhs.l4 != rhs.l4 then
      lhs.l4 < rhs.l4
    else if lhs.l3 != rhs.l3 then
      lhs.l3 < rhs.l3
    else if lhs.l2 != rhs.l2 then
      lhs.l2 < rhs.l2
    else if lhs.l1 != rhs.l1 then
      lhs.l1 < rhs.l1
    else
      lhs.l0 <= rhs.l0;
  big26Eq =
    lhs: rhs:
    lhs.l0 == rhs.l0 && lhs.l1 == rhs.l1 && lhs.l2 == rhs.l2 && lhs.l3 == rhs.l3 && lhs.l4 == rhs.l4;
  big26ShiftSmallForSqrt =
    value: shift:
    let
      bitShift = floorMod shift 26;
      a0 = floorMod value big26Base;
      a1 = floorMod (value / big26Base) big26Base;
      a2 = value / (big26Base * big26Base);

      t2 = a0 * pow2 bitShift;
      l2 = floorMod t2 big26Base;
      carry2 = t2 / big26Base;

      t3 = a1 * pow2 bitShift + carry2;
      l3 = floorMod t3 big26Base;
      carry3 = t3 / big26Base;

      t4 = a2 * pow2 bitShift + carry3;
      l4 = floorMod t4 big26Base;
    in
    {
      l0 = 0;
      l1 = 0;
      inherit
        l2
        l3
        l4
        ;
    };
  f64SqrtRoot =
    radicand:
    let
      search =
        lo: hi:
        if lo + 1 >= hi then
          lo
        else
          let
            mid = (lo + hi) / 2;
            square = f64MulSigLimbs mid mid;
          in
          if big26Le square radicand then search mid hi else search lo mid;
    in
    search (pow2 55) (pow2 56);
  f64RoundFromBig =
    sign: limbs: scale:
    let
      bitLength = big26BitLength limbs;
      exp = bitLength - 1 + scale;
      shift = bitLength - 56;
      sigExt =
        if shift <= 0 then big26ToSmall limbs * pow2 (0 - shift) else big26ShiftRightJamToSmall limbs shift;
    in
    if bitLength == 0 then f64SignedZero sign else f64PackExt sign exp sigExt;
  f64RoundFromSmall =
    sign: sig: scale:
    let
      bitLength = intBitLength sig;
      exp = bitLength - 1 + scale;
      shift = bitLength - 56;
      sigExt = shiftRightJam sig shift;
    in
    if sig == 0 then f64SignedZero sign else f64PackExt sign exp sigExt;
  f64DivQuotientJam =
    numerator: denominator: shift:
    let
      result =
        builtins.foldl'
          (
            acc: _:
            let
              rem2 = acc.rem * 2;
            in
            if rem2 >= denominator then
              {
                rem = rem2 - denominator;
                quot = acc.quot * 2 + 1;
              }
            else
              {
                rem = rem2;
                quot = acc.quot * 2;
              }
          )
          {
            rem = floorMod numerator denominator;
            quot = numerator / denominator;
          }
          (range shift);
    in
    if result.rem == 0 then result.quot else builtins.bitOr result.quot 1;
  f64PackExt =
    sign: exp: sigExt:
    let
      shifted =
        if exp < -1022 then
          {
            exp = -1022;
            sigExt = shiftRightJam sigExt (-1022 - exp);
          }
        else
          { inherit exp sigExt; };
      quotient = shifted.sigExt / 8;
      remainder = floorMod shifted.sigExt 8;
      roundUp = remainder > 4 || (remainder == 4 && floorMod quotient 2 == 1);
      rounded0 = quotient + (if roundUp then 1 else 0);
      carried = rounded0 >= 9007199254740992;
      rounded = if carried then rounded0 / 2 else rounded0;
      exp2 = shifted.exp + (if carried then 1 else 0);
      signBit = if sign then i32Sign else 0;
      frac = rounded - 4503599627370496;
    in
    if rounded == 0 then
      f64SignedZero sign
    else if exp2 >= 1024 then
      f64Inf sign
    else if exp2 == -1022 && rounded < 4503599627370496 then
      let
        fracParts = u64PartsFromNonNegative rounded;
      in
      i64FromParts {
        lo = fracParts.lo;
        hi = signBit + fracParts.hi;
      }
    else
      let
        fracParts = u64PartsFromNonNegative frac;
      in
      i64FromParts {
        lo = fracParts.lo;
        hi = signBit + (exp2 + 1023) * 1048576 + fracParts.hi;
      };
  f64NormalizeAdd =
    exp: sigExt:
    if sigExt == 0 || sigExt >= 36028797018963968 || exp <= -1022 then
      { inherit exp sigExt; }
    else
      f64NormalizeAdd (exp - 1) (sigExt * 2);
  f64AddFinite =
    lhs: rhs:
    let
      a = f64FiniteInfo lhs;
      b = f64FiniteInfo rhs;
      useAExp = a.exp >= b.exp;
      exp = if useAExp then a.exp else b.exp;
      aExt = if useAExp then a.sig * 8 else shiftRightJam (a.sig * 8) (b.exp - a.exp);
      bExt = if useAExp then shiftRightJam (b.sig * 8) (a.exp - b.exp) else b.sig * 8;
      sameSign = a.sign == b.sign;
    in
    if sameSign then
      let
        sum = aExt + bExt;
        normalized =
          if sum >= 72057594037927936 then
            {
              exp = exp + 1;
              sigExt = shiftRightJam sum 1;
            }
          else
            {
              inherit exp;
              sigExt = sum;
            };
      in
      f64PackExt a.sign normalized.exp normalized.sigExt
    else if a.exp == b.exp && a.sig == b.sig then
      0
    else
      let
        aGreater = a.exp > b.exp || (a.exp == b.exp && a.sig > b.sig);
        diff = if aGreater then aExt - bExt else bExt - aExt;
        sign = if aGreater then a.sign else b.sign;
        normalized = f64NormalizeAdd exp diff;
      in
      f64PackExt sign normalized.exp normalized.sigExt;
  f64Add =
    lhs: rhs:
    if f64IsNaN lhs || f64IsNaN rhs then
      f64BinaryNaN lhs rhs
    else if f64IsInf lhs || f64IsInf rhs then
      if f64IsInf lhs && f64IsInf rhs && f64SignBit lhs != f64SignBit rhs then
        f64CanonicalNan
      else if f64IsInf lhs then
        lhs
      else
        rhs
    else if f64IsZero lhs && f64IsZero rhs then
      if f64SignBit lhs == i32Sign && f64SignBit rhs == i32Sign then i64Min else 0
    else if f64IsZero lhs then
      rhs
    else if f64IsZero rhs then
      lhs
    else
      f64AddFinite lhs rhs;
  f64Sub = lhs: rhs: f64Add lhs (f64Neg rhs);
  f64Mul =
    lhs: rhs:
    let
      sign = f64SignBit lhs != f64SignBit rhs;
    in
    if f64IsNaN lhs || f64IsNaN rhs then
      f64BinaryNaN lhs rhs
    else if (f64IsInf lhs && f64IsZero rhs) || (f64IsZero lhs && f64IsInf rhs) then
      f64CanonicalNan
    else if f64IsInf lhs || f64IsInf rhs then
      f64Inf sign
    else if f64IsZero lhs || f64IsZero rhs then
      f64SignedZero sign
    else
      let
        a = f64FiniteInfo lhs;
        b = f64FiniteInfo rhs;
      in
      f64RoundFromBig sign (f64MulSigLimbs a.sig b.sig) (a.exp + b.exp - 104);
  f64Div =
    lhs: rhs:
    let
      sign = f64SignBit lhs != f64SignBit rhs;
    in
    if f64IsNaN lhs || f64IsNaN rhs then
      f64BinaryNaN lhs rhs
    else if (f64IsInf lhs && f64IsInf rhs) || (f64IsZero lhs && f64IsZero rhs) then
      f64CanonicalNan
    else if f64IsInf lhs || f64IsZero rhs then
      f64Inf sign
    else if f64IsZero lhs || f64IsInf rhs then
      f64SignedZero sign
    else
      let
        a = f64FiniteInfo lhs;
        b = f64FiniteInfo rhs;
        shift = 56 + intBitLength b.sig - intBitLength a.sig;
        sig = f64DivQuotientJam a.sig b.sig shift;
      in
      f64RoundFromSmall sign sig (a.exp - b.exp - shift);
  f64NormalizeFinite =
    sig: exp:
    if sig >= 4503599627370496 then { inherit sig exp; } else f64NormalizeFinite (sig * 2) (exp - 1);
  f64Sqrt =
    value:
    if f64IsNaN value then
      f64QuietNaN value
    else if f64IsInf value then
      if f64SignBit value == i32Sign then f64CanonicalNan else value
    else if f64IsZero value then
      value
    else if f64SignBit value == i32Sign then
      f64CanonicalNan
    else
      let
        info0 = f64FiniteInfo value;
        info = if info0.sig < 4503599627370496 then f64NormalizeFinite info0.sig info0.exp else info0;
        oddExp = floorMod info.exp 2 != 0;
        resultExp = if oddExp then (info.exp - 1) / 2 else info.exp / 2;
        radicand = big26ShiftSmallForSqrt info.sig (if oddExp then 59 else 58);
        root = f64SqrtRoot radicand;
        square = f64MulSigLimbs root root;
        sigExt = if big26Eq square radicand then root else builtins.bitOr root 1;
      in
      f64PackExt false resultExp sigExt;
  f64RoundIntegral =
    mode: value:
    let
      parts = f64Parts value;
      sign = parts.sign;
      signedZero = f64SignedZero sign;
      fracFull = parts.fracHi * u32Mod + parts.fracLo;
    in
    if f64IsNaN value then
      f64QuietNaN value
    else if f64IsInf value || f64IsZero value then
      value
    else if parts.rawExp < 1023 then
      if mode == "floor" then
        if sign then f64FromExactInt "floor" (-1) else 0
      else if mode == "ceil" then
        if sign then signedZero else f64FromExactInt "ceil" 1
      else if mode == "nearest" then
        if parts.rawExp < 1022 || (parts.rawExp == 1022 && fracFull == 0) then
          signedZero
        else
          f64FromExactInt "nearest" (if sign then -1 else 1)
      else
        signedZero
    else
      let
        exp = parts.rawExp - 1023;
        sig = 4503599627370496 + fracFull;
      in
      if exp >= 52 then
        value
      else
        let
          divisor = pow2 (52 - exp);
          int = sig / divisor;
          frac = floorMod sig divisor;
          half = divisor / 2;
          roundedInt =
            if mode == "floor" then
              int + (if sign && frac != 0 then 1 else 0)
            else if mode == "ceil" then
              int + (if !sign && frac != 0 then 1 else 0)
            else if mode == "nearest" then
              int + (if frac > half || (frac == half && floorMod int 2 == 1) then 1 else 0)
            else
              int;
        in
        if frac == 0 then value else f64FromExactInt mode (if sign then 0 - roundedInt else roundedInt);

  f64MapParts = f: value: i64FromParts (f (i64Parts value));

  intBitLength =
    value:
    let
      v = if value < 0 then 0 - value else value;
    in
    if v == 0 then
      0
    else
      builtins.foldl' (acc: bit: if v >= pow2 bit then bit + 1 else acc) 0 (range 63);

  exactIntFloatError =
    ty: op: reason:
    throw "wasm2nix runtime: ${ty}.${op} only supports exact finite integer-valued soft-float operands for now (${reason})";

  f32Parts =
    value:
    let
      bits = u32 value;
      rawExp = floorMod (bits / 8388608) 256;
      frac = floorMod bits 8388608;
    in
    {
      sign = bits >= i32Sign;
      inherit rawExp frac;
    };

  f32ToExactInt =
    op: value:
    let
      parts = f32Parts value;
      exp = parts.rawExp - 127;
      sig = 8388608 + parts.frac;
      unsigned =
        if parts.rawExp == 255 then
          exactIntFloatError "f32" op "NaN or infinity"
        else if parts.rawExp == 0 then
          if parts.frac == 0 then 0 else exactIntFloatError "f32" op "subnormal"
        else if exp < 0 then
          exactIntFloatError "f32" op "fractional value"
        else if exp > 52 then
          exactIntFloatError "f32" op "outside host integer range"
        else if exp >= 23 then
          sig * pow2 (exp - 23)
        else
          let
            divisor = pow2 (23 - exp);
          in
          if floorMod sig divisor == 0 then sig / divisor else exactIntFloatError "f32" op "fractional value";
    in
    if parts.sign then 0 - unsigned else unsigned;

  f32FromExactInt =
    op: value:
    let
      negative = value < 0;
      magnitude = if negative then 0 - value else value;
      bitLength = intBitLength magnitude;
      exp = bitLength - 1;
      frac =
        if magnitude == 0 then
          0
        else if exp > 127 then
          exactIntFloatError "f32" op "overflow"
        else if exp <= 23 then
          magnitude * pow2 (23 - exp) - 8388608
        else
          let
            divisor = pow2 (exp - 23);
          in
          if floorMod magnitude divisor == 0 then
            magnitude / divisor - 8388608
          else
            exactIntFloatError "f32" op "rounded result";
      sign = if negative then i32Sign else 0;
    in
    if magnitude == 0 then sign else u32 (sign + (exp + 127) * 8388608 + frac);

  f32ExactBin =
    op: fn: lhs: rhs:
    f32FromExactInt op (fn (f32ToExactInt op lhs) (f32ToExactInt op rhs));

  f32ExactDiv =
    lhs: rhs:
    let
      a = f32ToExactInt "div" lhs;
      b = f32ToExactInt "div" rhs;
    in
    if b == 0 then
      exactIntFloatError "f32" "div" "division by zero"
    else
      let
        q = a / b;
      in
      if q * b == a then f32FromExactInt "div" q else exactIntFloatError "f32" "div" "rounded result";

  f64Parts =
    value:
    let
      parts = i64Parts value;
      rawExp = floorMod (parts.hi / 1048576) 2048;
      fracHi = floorMod parts.hi 1048576;
    in
    {
      sign = parts.hi >= i32Sign;
      inherit rawExp fracHi;
      fracLo = parts.lo;
    };

  f64ToExactInt =
    op: value:
    let
      parts = f64Parts value;
      exp = parts.rawExp - 1023;
      frac = parts.fracHi * u32Mod + parts.fracLo;
      sig = 4503599627370496 + frac;
      unsigned =
        if parts.rawExp == 2047 then
          exactIntFloatError "f64" op "NaN or infinity"
        else if parts.rawExp == 0 then
          if frac == 0 then 0 else exactIntFloatError "f64" op "subnormal"
        else if exp < 0 then
          exactIntFloatError "f64" op "fractional value"
        else if exp > 52 then
          exactIntFloatError "f64" op "outside host integer range"
        else
          let
            divisor = pow2 (52 - exp);
          in
          if floorMod sig divisor == 0 then sig / divisor else exactIntFloatError "f64" op "fractional value";
    in
    if parts.sign then 0 - unsigned else unsigned;

  f64FromExactInt =
    op: value:
    let
      negative = value < 0;
      magnitude = if negative then 0 - value else value;
      bitLength = intBitLength magnitude;
      exp = bitLength - 1;
      frac =
        if magnitude == 0 then
          0
        else if exp > 1023 then
          exactIntFloatError "f64" op "overflow"
        else if exp <= 52 then
          magnitude * pow2 (52 - exp) - 4503599627370496
        else
          exactIntFloatError "f64" op "outside host integer range";
      sign = if negative then i32Sign else 0;
      fracHi = frac / u32Mod;
      fracLo = floorMod frac u32Mod;
    in
    if magnitude == 0 then
      i64FromParts {
        lo = 0;
        hi = sign;
      }
    else
      i64FromParts {
        lo = fracLo;
        hi = u32 (sign + (exp + 1023) * 1048576 + fracHi);
      };

  f64ExactBin =
    op: fn: lhs: rhs:
    f64FromExactInt op (fn (f64ToExactInt op lhs) (f64ToExactInt op rhs));

  f64ExactDiv =
    lhs: rhs:
    let
      a = f64ToExactInt "div" lhs;
      b = f64ToExactInt "div" rhs;
    in
    if b == 0 then
      exactIntFloatError "f64" "div" "division by zero"
    else
      let
        q = a / b;
      in
      if q * b == a then f64FromExactInt "div" q else exactIntFloatError "f64" "div" "rounded result";

  u64PartsFromNonNegative = value: {
    lo = floorMod value u32Mod;
    hi = value / u32Mod;
  };

  u64ZeroParts = {
    lo = 0;
    hi = 0;
  };
  i32MaxParts = {
    lo = 2147483647;
    hi = 0;
  };
  i32MinMagnitudeParts = {
    lo = i32Sign;
    hi = 0;
  };
  i64MaxParts = {
    lo = u32Max;
    hi = 2147483647;
  };
  i64MinMagnitudeParts = {
    lo = 0;
    hi = i32Sign;
  };

  f32TruncInfo =
    value:
    let
      parts = f32Parts value;
      exp = parts.rawExp - 127;
      sig = 8388608 + parts.frac;
      magnitude =
        if parts.rawExp == 0 || exp < 0 then
          u64ZeroParts
        else if exp > 63 then
          u64ZeroParts
        else if exp >= 23 then
          i64ShlParts (u64PartsFromNonNegative sig) (exp - 23)
        else
          u64PartsFromNonNegative (sig / pow2 (23 - exp));
    in
    {
      inherit (parts) sign;
      isNan = parts.rawExp == 255 && parts.frac != 0;
      isInf = parts.rawExp == 255 && parts.frac == 0;
      overflowU64 = parts.rawExp != 0 && exp > 63;
      parts = magnitude;
    };

  f64TruncInfo =
    value:
    let
      parts = f64Parts value;
      frac = parts.fracHi * u32Mod + parts.fracLo;
      exp = parts.rawExp - 1023;
      sig = 4503599627370496 + frac;
      magnitude =
        if parts.rawExp == 0 || exp < 0 then
          u64ZeroParts
        else if exp > 63 then
          u64ZeroParts
        else if exp >= 52 then
          i64ShlParts (u64PartsFromNonNegative sig) (exp - 52)
        else
          u64PartsFromNonNegative (sig / pow2 (52 - exp));
    in
    {
      inherit (parts) sign;
      isNan = parts.rawExp == 2047 && frac != 0;
      isInf = parts.rawExp == 2047 && frac == 0;
      overflowU64 = parts.rawExp != 0 && exp > 63;
      parts = magnitude;
    };

  truncInvalid = throw "wasm trap: invalid conversion to integer";
  truncOverflow = throw "wasm trap: integer overflow";

  truncInfo = ty: if ty == "f32" then f32TruncInfo else f64TruncInfo;
  truncResultI64 = parts: i64FromParts parts;
  truncNegResultI64 = parts: i64FromParts (i64NegParts parts);

  truncToI32Signed =
    saturating: floatTy: value:
    let
      info = (truncInfo floatTy) value;
      tooPositive = info.overflowU64 || i64UnsignedGtParts info.parts i32MaxParts;
      tooNegative = info.overflowU64 || i64UnsignedGtParts info.parts i32MinMagnitudeParts;
      magnitude = if info.parts.hi == 0 then info.parts.lo else 0;
    in
    if info.isNan then
      if saturating then 0 else truncInvalid
    else if info.isInf then
      if saturating then (if info.sign then i32Sign else 2147483647) else truncOverflow
    else if info.sign then
      if tooNegative then if saturating then i32Sign else truncOverflow else u32 (0 - magnitude)
    else if tooPositive then
      if saturating then 2147483647 else truncOverflow
    else
      u32 magnitude;

  truncToI32Unsigned =
    saturating: floatTy: value:
    let
      info = (truncInfo floatTy) value;
      tooPositive = info.overflowU64 || info.parts.hi != 0;
      negativeNonZero = info.sign && !(i64IsZeroParts info.parts);
    in
    if info.isNan then
      if saturating then 0 else truncInvalid
    else if info.isInf then
      if saturating then (if info.sign then 0 else u32Max) else truncOverflow
    else if negativeNonZero then
      if saturating then 0 else truncOverflow
    else if tooPositive then
      if saturating then u32Max else truncOverflow
    else
      u32 info.parts.lo;

  truncToI64Signed =
    saturating: floatTy: value:
    let
      info = (truncInfo floatTy) value;
      tooPositive = info.overflowU64 || i64UnsignedGtParts info.parts i64MaxParts;
      tooNegative = info.overflowU64 || i64UnsignedGtParts info.parts i64MinMagnitudeParts;
    in
    if info.isNan then
      if saturating then 0 else truncInvalid
    else if info.isInf then
      if saturating then (if info.sign then i64Min else i64Max) else truncOverflow
    else if info.sign then
      if tooNegative then if saturating then i64Min else truncOverflow else truncNegResultI64 info.parts
    else if tooPositive then
      if saturating then i64Max else truncOverflow
    else
      truncResultI64 info.parts;

  truncToI64Unsigned =
    saturating: floatTy: value:
    let
      info = (truncInfo floatTy) value;
      negativeNonZero = info.sign && !(i64IsZeroParts info.parts);
    in
    if info.isNan then
      if saturating then 0 else truncInvalid
    else if info.isInf then
      if saturating then (if info.sign then 0 else -1) else truncOverflow
    else if negativeNonZero then
      if saturating then 0 else truncOverflow
    else if info.overflowU64 then
      if saturating then -1 else truncOverflow
    else
      truncResultI64 info.parts;

  u64BitLengthParts = parts: 64 - clzBy 64 i64BitSetParts parts;

  u64LowBits =
    parts: bits:
    if bits <= 0 then
      0
    else if bits < 32 then
      floorMod parts.lo (pow2 bits)
    else if bits == 32 then
      parts.lo
    else
      parts.lo + floorMod parts.hi (pow2 (bits - 32)) * u32Mod;

  u64PartsToSmall = parts: parts.hi * u32Mod + parts.lo;

  u64RoundToPrecision =
    precision: parts:
    let
      bitLength = u64BitLengthParts parts;
    in
    if bitLength == 0 then
      {
        isZero = true;
        exponent = 0;
        significand = 0;
      }
    else
      let
        exponent = bitLength - 1;
        rounded =
          if bitLength <= precision then
            u64PartsToSmall parts * pow2 (precision - bitLength)
          else
            let
              shift = bitLength - precision;
              quotient = u64PartsToSmall (i64ShrUParts parts shift);
              remainder = u64LowBits parts shift;
              half = pow2 (shift - 1);
              roundUp = remainder > half || (remainder == half && floorMod quotient 2 == 1);
            in
            quotient + (if roundUp then 1 else 0);
        carry = rounded == pow2 precision;
      in
      {
        isZero = false;
        exponent = if carry then exponent + 1 else exponent;
        significand = if carry then pow2 (precision - 1) else rounded;
      };

  f32FromUnsignedParts =
    sign: parts:
    let
      rounded = u64RoundToPrecision 24 parts;
      signBit = if sign && !rounded.isZero then i32Sign else 0;
      frac = rounded.significand - 8388608;
    in
    if rounded.isZero then signBit else u32 (signBit + (rounded.exponent + 127) * 8388608 + frac);

  f64FromUnsignedParts =
    sign: parts:
    let
      rounded = u64RoundToPrecision 53 parts;
      signBit = if sign && !rounded.isZero then i32Sign else 0;
      frac = rounded.significand - 4503599627370496;
      fracHi = frac / u32Mod;
      fracLo = floorMod frac u32Mod;
    in
    if rounded.isZero then
      i64FromParts {
        lo = 0;
        hi = signBit;
      }
    else
      i64FromParts {
        lo = fracLo;
        hi = u32 (signBit + (rounded.exponent + 1023) * 1048576 + fracHi);
      };

  f64FromRaw =
    sign: rawExp: fracHi: fracLo:
    i64FromParts {
      lo = fracLo;
      hi = u32 ((if sign then i32Sign else 0) + rawExp * 1048576 + fracHi);
    };

  roundSmallRight =
    value: shift:
    if shift <= 0 then
      value * pow2 (0 - shift)
    else
      let
        divisor = pow2 shift;
        quotient = value / divisor;
        remainder = floorMod value divisor;
        half = pow2 (shift - 1);
        roundUp = remainder > half || (remainder == half && floorMod quotient 2 == 1);
      in
      quotient + (if roundUp then 1 else 0);

  f64PromoteF32 =
    value:
    let
      parts = f32Parts value;
      sign = parts.sign;
    in
    if parts.rawExp == 255 && parts.frac != 0 then
      f64FromRaw false 2047 524288 0
    else if parts.rawExp == 255 then
      f64FromRaw sign 2047 0 0
    else if parts.rawExp == 0 then
      if parts.frac == 0 then
        f64FromRaw sign 0 0 0
      else
        let
          bitLength = intBitLength parts.frac;
          exponent = bitLength - 150;
          significand = parts.frac * pow2 (53 - bitLength);
          frac = significand - 4503599627370496;
        in
        f64FromRaw sign (exponent + 1023) (frac / u32Mod) (floorMod frac u32Mod)
    else
      let
        exponent = parts.rawExp - 127;
        frac = parts.frac * pow2 29;
      in
      f64FromRaw sign (exponent + 1023) (frac / u32Mod) (floorMod frac u32Mod);

  f32DemoteF64 =
    value:
    let
      parts = f64Parts value;
      signBit = if parts.sign then i32Sign else 0;
      frac = parts.fracHi * u32Mod + parts.fracLo;
    in
    if parts.rawExp == 2047 && frac != 0 then
      2143289344
    else if parts.rawExp == 2047 then
      u32 (signBit + 2139095040)
    else if parts.rawExp == 0 then
      signBit
    else
      let
        exponent = parts.rawExp - 1023;
        significand = 4503599627370496 + frac;
      in
      if exponent >= 128 then
        u32 (signBit + 2139095040)
      else if exponent >= -126 then
        let
          rounded = roundSmallRight significand 29;
          carry = rounded == 16777216;
          exponent2 = if carry then exponent + 1 else exponent;
          significand2 = if carry then 8388608 else rounded;
        in
        if exponent2 >= 128 then
          u32 (signBit + 2139095040)
        else
          u32 (signBit + (exponent2 + 127) * 8388608 + (significand2 - 8388608))
      else if exponent < -150 then
        signBit
      else
        let
          rounded = roundSmallRight significand (0 - exponent - 97);
        in
        if rounded == 0 then
          signBit
        else if rounded == 8388608 then
          u32 (signBit + 8388608)
        else
          u32 (signBit + rounded);

  intMagnitudeParts =
    signed: intTy: value:
    if intTy == "i32" then
      let
        signedValue = toI32 value;
        negative = signed && signedValue < 0;
      in
      {
        sign = negative;
        parts = u64PartsFromNonNegative (if negative then 0 - signedValue else u32 value);
      }
    else
      let
        parts = i64Parts value;
        negative = signed && i64IsNegativeParts parts;
      in
      {
        sign = negative;
        parts = if negative then i64NegParts parts else parts;
      };

  convertIntToFloat =
    signed: intTy: floatTy: value:
    let
      magnitude = intMagnitudeParts signed intTy value;
    in
    if floatTy == "f32" then
      f32FromUnsignedParts magnitude.sign magnitude.parts
    else
      f64FromUnsignedParts magnitude.sign magnitude.parts;

  conv = {
    uextend = value: _ty: value;
    sextend = value: value;
    ireduce = u32;
    sign_extend_8 = value: u32 (signExtend 8 value);
    sign_extend_16 = value: u32 (signExtend 16 value);
    sign_extend_8_i64 = value: checkI64 (signExtend 8 value);
    sign_extend_16_i64 = value: checkI64 (signExtend 16 value);
    sign_extend_32 = value: checkI64 (signExtend 32 value);
    trunc_s = {
      f32 = {
        i32 = truncToI32Signed false "f32";
        i64 = truncToI64Signed false "f32";
      };
      f64 = {
        i32 = truncToI32Signed false "f64";
        i64 = truncToI64Signed false "f64";
      };
    };
    trunc_u = {
      f32 = {
        i32 = truncToI32Unsigned false "f32";
        i64 = truncToI64Unsigned false "f32";
      };
      f64 = {
        i32 = truncToI32Unsigned false "f64";
        i64 = truncToI64Unsigned false "f64";
      };
    };
    trunc_sat_s = {
      f32 = {
        i32 = truncToI32Signed true "f32";
        i64 = truncToI64Signed true "f32";
      };
      f64 = {
        i32 = truncToI32Signed true "f64";
        i64 = truncToI64Signed true "f64";
      };
    };
    trunc_sat_u = {
      f32 = {
        i32 = truncToI32Unsigned true "f32";
        i64 = truncToI64Unsigned true "f32";
      };
      f64 = {
        i32 = truncToI32Unsigned true "f64";
        i64 = truncToI64Unsigned true "f64";
      };
    };
    convert_s = {
      i32 = {
        f32 = convertIntToFloat true "i32" "f32";
        f64 = convertIntToFloat true "i32" "f64";
      };
      i64 = {
        f32 = convertIntToFloat true "i64" "f32";
        f64 = convertIntToFloat true "i64" "f64";
      };
    };
    convert_u = {
      i32 = {
        f32 = convertIntToFloat false "i32" "f32";
        f64 = convertIntToFloat false "i32" "f64";
      };
      i64 = {
        f32 = convertIntToFloat false "i64" "f32";
        f64 = convertIntToFloat false "i64" "f64";
      };
    };
  };

  getMem = st: st.mem or { };
  getPages = st: (getMem st).pages or { };
  pageCount = st: (getMem st).pageCount or 1;
  setPages =
    pages: st:
    st
    // {
      mem = (getMem st) // {
        pages = pages;
      };
    };

  setPageCount =
    count: st:
    st
    // {
      mem = (getMem st) // {
        pageCount = count;
      };
    };

  dataSegments = st: st.data or { };
  getDataSegment = idx: st: (dataSegments st).${toString idx} or [ ];
  setDataSegment =
    idx: bytes: st:
    st
    // {
      data = (dataSegments st) // {
        "${toString idx}" = bytes;
      };
    };

  checkAccess =
    op: width: addr: st:
    let
      pages = pageCount st;
      limit = pages * pageBytes;
      end = addr + width;
    in
    if addr < 0 || end > limit then throw "wasm trap: ${op} out of bounds" else true;

  checkSegmentAccess =
    op: segment: offset: width:
    let
      end = offset + width;
    in
    if offset < 0 || end > length segment then throw "wasm trap: ${op} out of bounds" else true;

  wordIndex = addr: addr / 4;
  byteOffset = addr: floorMod addr 4;

  getWord = pages: idx: pages.${toString idx} or 0;

  setWord =
    pages: idx: value:
    let
      key = toString idx;
      v = u32 value;
    in
    if v == 0 then builtins.removeAttrs pages [ key ] else pages // { "${key}" = v; };

  byteAt = word: offset: floorMod (word / pow2 (offset * 8)) 256;

  setByte =
    word: offset: byte:
    let
      place = pow2 (offset * 8);
      old = byteAt word offset;
    in
    u32 (word - old * place + floorMod byte 256 * place);

  getByteUnchecked =
    pages: addr:
    let
      idx = wordIndex addr;
      offset = byteOffset addr;
    in
    byteAt (getWord pages idx) offset;

  setByteUnchecked =
    pages: addr: byte:
    let
      idx = wordIndex addr;
      offset = byteOffset addr;
      word = getWord pages idx;
    in
    setWord pages idx (setByte word offset byte);

  loadBytes =
    op: width: addr: st:
    builtins.seq (checkAccess op width addr st) (
      let
        pages = getPages st;
      in
      map (i: getByteUnchecked pages (addr + i)) (range width)
    );

  bytesToU16 = bytes: elemAt bytes 0 + elemAt bytes 1 * 256;

  bytesToU32 =
    bytes: elemAt bytes 0 + elemAt bytes 1 * 256 + elemAt bytes 2 * 65536 + elemAt bytes 3 * 16777216;

  bytesToLimitedU64 =
    bytes:
    let
      lo = bytesToU32 (builtins.genList (i: elemAt bytes i) 4);
      hi = bytesToU32 (builtins.genList (i: elemAt bytes (i + 4)) 4);
    in
    i64FromParts { inherit lo hi; };

  u32ToBytes =
    value:
    let
      v = u32 value;
    in
    [
      (floorMod v 256)
      (floorMod (v / 256) 256)
      (floorMod (v / 65536) 256)
      (floorMod (v / 16777216) 256)
    ];

  limitedU64ToBytes =
    value:
    let
      parts = i64Parts value;
    in
    u32ToBytes parts.lo ++ u32ToBytes parts.hi;

  storeBytes =
    op: bytes: addr: st:
    builtins.seq (checkAccess op (length bytes) addr st) (
      let
        pages = builtins.foldl' (acc: i: setByteUnchecked acc (addr + i) (elemAt bytes i)) (getPages st) (
          range (length bytes)
        );
      in
      setPages pages st
    );

  mem = rec {
    load_u8 = addr: st: elemAt (loadBytes "load_u8" 1 addr st) 0;
    load_s8 = addr: st: u32 (signExtend 8 (load_u8 addr st));
    load_s8_i64 = addr: st: checkI64 (signExtend 8 (load_u8 addr st));
    load_u16 = addr: st: bytesToU16 (loadBytes "load_u16" 2 addr st);
    load_s16 = addr: st: u32 (signExtend 16 (load_u16 addr st));
    load_s16_i64 = addr: st: checkI64 (signExtend 16 (load_u16 addr st));
    load_u32 = addr: st: bytesToU32 (loadBytes "load_u32" 4 addr st);
    load_s32 = addr: st: u32 (signExtend 32 (load_u32 addr st));
    load_s32_i64 = addr: st: checkI64 (signExtend 32 (load_u32 addr st));
    load_i32 = load_u32;
    load_f32 = load_u32;
    load_i64 = addr: st: bytesToLimitedU64 (loadBytes "load_i64" 8 addr st);
    load_f64 = load_i64;

    store_u8 =
      value: addr: st:
      storeBytes "store_u8" [ (floorMod value 256) ] addr st;
    store_u16 =
      value: addr: st:
      let
        v = u32 value;
      in
      storeBytes "store_u16" [
        (floorMod v 256)
        (floorMod (v / 256) 256)
      ] addr st;
    store_i32 =
      value: addr: st:
      storeBytes "store_i32" (u32ToBytes value) addr st;
    store_u32 = store_i32;
    store_f32 = store_i32;
    store_i64 =
      value: addr: st:
      storeBytes "store_i64" (limitedU64ToBytes value) addr st;
    store_f64 = store_i64;

    size = st: pageCount st;
    grow =
      delta: st:
      let
        old = pageCount st;
        new = old + delta;
        memoryMaxPages = (getMem st).maxPageCount or maxPages;
      in
      if delta < 0 || new > memoryMaxPages then
        {
          inherit st;
          results = [ u32Max ];
        }
      else
        {
          st = setPageCount new st;
          results = [ old ];
        };
    fill =
      dst: value: len: st:
      builtins.seq (checkAccess "memory.fill" len dst st) (
        let
          byte = floorMod value 256;
          pages = builtins.foldl' (acc: i: setByteUnchecked acc (dst + i) byte) (getPages st) (range len);
        in
        setPages pages st
      );
    copy =
      dst: src: len: st:
      let
        bytes = loadBytes "memory.copy" len src st;
      in
      storeBytes "memory.copy" bytes dst st;
    init =
      segIndex: dst: src: len: st:
      let
        segment = getDataSegment segIndex st;
        bytes = builtins.seq (checkSegmentAccess "memory.init segment" segment src len) (
          builtins.genList (i: elemAt segment (src + i)) len
        );
      in
      storeBytes "memory.init" bytes dst st;
    data_drop = segIndex: st: setDataSegment segIndex [ ] st;
  };

  globals = st: st.globals or { };

  global =
    let
      get = idx: st: (globals st).${toString idx} or 0;
      set =
        idx: value: st:
        st
        // {
          globals = (globals st) // {
            "${toString idx}" = value;
          };
        };
    in
    {
      get_i32 = get;
      get_i64 = get;
      get_f32 = get;
      get_f64 = get;
      set_i32 =
        idx: value: st:
        set idx (if builtins.isAttrs value then value else u32 value) st;
      set_i64 =
        idx: value: st:
        set idx (checkI64 value) st;
      set_f32 =
        idx: value: st:
        set idx (u32 value) st;
      set_f64 = set;
    };

  tables = st: st.tables or { };

  table =
    let
      getTable =
        tableIndex: st:
        (tables st).${toString tableIndex} or {
          elems = { };
          size = 0;
        };
      setTable =
        tableIndex: tableValue: st:
        st
        // {
          tables = (tables st) // {
            "${toString tableIndex}" = tableValue;
          };
        };
      getEntry = tableValue: elemIndex: tableValue.elems.${toString elemIndex} or null;
      setEntry =
        tableValue: elemIndex: entry:
        let
          key = toString elemIndex;
          elems = tableValue.elems or { };
        in
        tableValue
        // {
          elems =
            if entry == null then builtins.removeAttrs elems [ key ] else elems // { "${key}" = entry; };
        };
      checkTableAccess =
        op: tableValue: offset: width:
        let
          end = offset + width;
        in
        if offset < 0 || end > (tableValue.size or 0) then throw "wasm trap: ${op} out of bounds" else true;
      elemSegments = st: st.elemSegments or { };
      getElemSegment = segIndex: st: (elemSegments st).${toString segIndex} or [ ];
      setElemSegment =
        segIndex: values: st:
        st
        // {
          elemSegments = (elemSegments st) // {
            "${toString segIndex}" = values;
          };
        };
      applyArgs = builtins.foldl' (fn: arg: fn arg);
    in
    {
      call =
        tableIndex: typeIndex: moduleId: funcs: funcRegistry: elemIndex: args: st:
        let
          entry = getEntry (getTable tableIndex st) elemIndex;
        in
        if entry == null then
          throw "wasm trap: undefined table element"
        else if entry.type != typeIndex then
          throw "wasm trap: indirect call type mismatch"
        else
          let
            targetFuncs =
              if entry ? module && entry.module != moduleId then
                builtins.getAttr entry.module funcRegistry
              else
                funcs;
            fn = builtins.getAttr ("f" + toString entry.func) targetFuncs;
          in
          (applyArgs fn args) st;
      get =
        tableIndex: elemIndex: st:
        builtins.seq (checkTableAccess "table.get" (getTable tableIndex st) elemIndex 1) (
          let
            entry = getEntry (getTable tableIndex st) elemIndex;
          in
          if entry == null then 0 else entry
        );
      set =
        tableIndex: elemIndex: value: st:
        let
          tableValue = getTable tableIndex st;
          checked = checkTableAccess "table.set" tableValue elemIndex 1;
          newTable = builtins.seq checked (
            setEntry tableValue elemIndex (if value == 0 then null else value)
          );
        in
        builtins.deepSeq newTable (setTable tableIndex newTable st);
      size = tableIndex: st: (getTable tableIndex st).size or 0;
      grow =
        tableIndex: initValue: delta: st:
        let
          tableValue = getTable tableIndex st;
          old = tableValue.size or 0;
          new = old + delta;
          tableMaxSize = tableValue.maxSize or u32Max;
          entry = if initValue == 0 then null else initValue;
        in
        if delta < 0 || new > tableMaxSize || new > u32Max then
          {
            inherit st;
            results = [ u32Max ];
          }
        else
          let
            newTable = builtins.foldl' (acc: i: setEntry acc (old + i) entry) (tableValue // { size = new; }) (
              range delta
            );
          in
          {
            st = builtins.deepSeq newTable (setTable tableIndex newTable st);
            results = [ old ];
          };
      fill =
        tableIndex: dst: value: len: st:
        let
          tableValue = getTable tableIndex st;
          checked = checkTableAccess "table.fill" tableValue dst len;
          entry = if value == 0 then null else value;
          newTable = builtins.seq checked (
            builtins.foldl' (acc: i: setEntry acc (dst + i) entry) tableValue (range len)
          );
        in
        builtins.deepSeq newTable (setTable tableIndex newTable st);
      init =
        tableIndex: segIndex: dst: src: len: st:
        let
          tableValue = getTable tableIndex st;
          segment = getElemSegment segIndex st;
          checked = builtins.seq (checkTableAccess "table.init" tableValue dst len) (
            builtins.seq (checkSegmentAccess "table.init segment" segment src len) true
          );
          entries = builtins.seq checked (builtins.genList (i: elemAt segment (src + i)) len);
          newTable = builtins.seq checked (
            builtins.foldl' (acc: i: setEntry acc (dst + i) (elemAt entries i)) tableValue (range len)
          );
        in
        builtins.deepSeq newTable (setTable tableIndex newTable st);
      elem_drop = segIndex: st: setElemSegment segIndex [ ] st;
      copy =
        dstTableIndex: srcTableIndex: dst: src: len: st:
        let
          dstTable = getTable dstTableIndex st;
          srcTable = getTable srcTableIndex st;
          checked = builtins.seq (checkTableAccess "table.copy destination" dstTable dst len) (
            builtins.seq (checkTableAccess "table.copy source" srcTable src len) true
          );
          entries = builtins.seq checked (builtins.genList (i: getEntry srcTable (src + i)) len);
          newDstTable = builtins.seq checked (
            builtins.foldl' (acc: i: setEntry acc (dst + i) (elemAt entries i)) dstTable (range len)
          );
        in
        builtins.deepSeq newDstTable (setTable dstTableIndex newDstTable st);
    };
in
rec {
  i32 = i32Bin;
  i64 = i64Bin;
  int = {
    bnot = i32Bin.bnot;
  };

  f32 = {
    add = f32Add;
    sub = f32Sub;
    mul = f32Mul;
    div = f32Div;
    min = f32Min;
    max = f32Max;
    copysign = f32Copysign;
    neg = f32Neg;
    abs = f32Abs;
    sqrt = f32Sqrt;
    ceil = f32RoundIntegral "ceil";
    floor = f32RoundIntegral "floor";
    trunc = f32RoundIntegral "trunc";
    nearest = f32RoundIntegral "nearest";
    demote_f64 = f32DemoteF64;
  };

  f64 = {
    add = f64Add;
    sub = f64Sub;
    mul = f64Mul;
    div = f64Div;
    min = f64Min;
    max = f64Max;
    copysign = f64Copysign;
    neg = f64Neg;
    abs = f64Abs;
    sqrt = f64Sqrt;
    ceil = f64RoundIntegral "ceil";
    floor = f64RoundIntegral "floor";
    trunc = f64RoundIntegral "trunc";
    nearest = f64RoundIntegral "nearest";
    promote_f32 = f64PromoteF32;
  };

  inherit
    cmp
    cmp_i64
    fcmp
    conv
    mem
    global
    table
    ;
}
