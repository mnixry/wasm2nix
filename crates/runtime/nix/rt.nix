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

  mod = value: divisor: value - divisor * (value / divisor);

  floorMod =
    value: divisor:
    let
      r = mod value divisor;
    in
    if r < 0 then r + divisor else r;

  u32 = value: builtins.bitAnd value u32Max;

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
      v = builtins.bitAnd value (modulus - 1);
    in
    if v >= sign then v - modulus else v;

  signedDiv = lhs: rhs: if rhs == 0 then throw "wasm trap: integer divide by zero" else lhs / rhs;

  signedRem =
    lhs: rhs: if rhs == 0 then throw "wasm trap: integer divide by zero" else lhs - rhs * (lhs / rhs);

  # a * b mod 2^32, split so that no intermediate product exceeds 2^48.
  mul32 =
    lhs: rhs:
    let
      a = u32 lhs;
      b = u32 rhs;
    in
    u32 (a * builtins.bitAnd b 65535 + builtins.bitAnd (a * (b / 65536)) 65535 * 65536);

  shiftCount32 = n: builtins.bitAnd n 31;

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

  bitSetU32 = value: bit: builtins.bitAnd ((u32 value) / pow2 bit) 1 == 1;

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
    # wasm2nix:runtime-fn i32.add(lhs, rhs) -> value
    add = lhs: rhs: u32 (u32 lhs + u32 rhs);
    # wasm2nix:runtime-fn i32.sub(lhs, rhs) -> value
    sub = lhs: rhs: u32 (u32 lhs - u32 rhs);
    # wasm2nix:runtime-fn i32.mul(lhs, rhs) -> value
    mul = mul32;
    # wasm2nix:runtime-fn i32.udiv(lhs, rhs) -> value
    udiv =
      lhs: rhs:
      let
        divisor = u32 rhs;
      in
      if divisor == 0 then throw "wasm trap: integer divide by zero" else (u32 lhs) / divisor;
    # wasm2nix:runtime-fn i32.sdiv(lhs, rhs) -> value
    sdiv =
      lhs: rhs:
      let
        a = toI32 lhs;
        b = toI32 rhs;
      in
      if a == i32Min && b == -1 then throw "wasm trap: integer overflow" else u32 (signedDiv a b);
    # wasm2nix:runtime-fn i32.urem(lhs, rhs) -> value
    urem =
      lhs: rhs:
      let
        divisor = u32 rhs;
      in
      if divisor == 0 then throw "wasm trap: integer divide by zero" else floorMod (u32 lhs) divisor;
    # wasm2nix:runtime-fn i32.srem(lhs, rhs) -> value
    srem =
      lhs: rhs:
      let
        a = toI32 lhs;
        b = toI32 rhs;
      in
      u32 (signedRem a b);
    # wasm2nix:runtime-fn i32.band(lhs, rhs) -> value
    band = lhs: rhs: u32 (builtins.bitAnd (u32 lhs) (u32 rhs));
    # wasm2nix:runtime-fn i32.bor(lhs, rhs) -> value
    bor = lhs: rhs: u32 (builtins.bitOr (u32 lhs) (u32 rhs));
    # wasm2nix:runtime-fn i32.bxor(lhs, rhs) -> value
    bxor = lhs: rhs: u32 (builtins.bitXor (u32 lhs) (u32 rhs));
    # wasm2nix:runtime-fn i32.bnot(value) -> value
    bnot = value: u32Max - u32 value;
    # wasm2nix:runtime-fn i32.shl(lhs, rhs) -> value
    shl = lhs: rhs: u32 (u32 lhs * pow2 (shiftCount32 rhs));
    # wasm2nix:runtime-fn i32.shr_u(lhs, rhs) -> value
    shr_u = unsignedShiftRight32;
    # wasm2nix:runtime-fn i32.shr_s(lhs, rhs) -> value
    shr_s = signedShiftRight32;
    # wasm2nix:runtime-fn i32.rotl(lhs, rhs) -> value
    rotl =
      lhs: rhs:
      let
        n = shiftCount32 rhs;
      in
      if n == 0 then u32 lhs else i32Bin.bor (i32Bin.shl lhs n) (i32Bin.shr_u lhs (32 - n));
    # wasm2nix:runtime-fn i32.rotr(lhs, rhs) -> value
    rotr =
      lhs: rhs:
      let
        n = shiftCount32 rhs;
      in
      if n == 0 then u32 lhs else i32Bin.bor (i32Bin.shr_u lhs n) (i32Bin.shl lhs (32 - n));
    # wasm2nix:runtime-fn i32.clz(value) -> value
    clz = clzBy 32 bitSetU32;
    # wasm2nix:runtime-fn i32.ctz(value) -> value
    ctz = ctzBy 32 bitSetU32;
    # wasm2nix:runtime-fn i32.popcnt(value) -> value
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
          lo = builtins.bitAnd lower u32Max;
          hi = i32Sign + lower / u32Mod;
        }
      else
        {
          lo = builtins.bitAnd v u32Max;
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
      lo = builtins.bitAnd loSum u32Max;
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
      lo = builtins.bitAnd loRaw u32Max;
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
      n = builtins.bitAnd amount 63;
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
      n = builtins.bitAnd amount 63;
    in
    if n == 0 then
      parts
    else if n < 32 then
      let
        lo = parts.lo / pow2 n + builtins.bitAnd parts.hi (pow2 n - 1) * pow2 (32 - n);
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
        lo = acc.lo / 2 + (if builtins.bitAnd acc.hi 1 == 1 then i32Sign else 0);
        hi = acc.hi / 2 + (if sign then i32Sign else 0);
      in
      {
        inherit lo hi;
      }
    ) parts (range (builtins.bitAnd amount 63));

  i64MulParts =
    lhs: rhs:
    let
      base = 65536;
      a0 = builtins.bitAnd lhs.lo (base - 1);
      a1 = lhs.lo / base;
      a2 = builtins.bitAnd lhs.hi (base - 1);
      a3 = lhs.hi / base;
      b0 = builtins.bitAnd rhs.lo (base - 1);
      b1 = rhs.lo / base;
      b2 = builtins.bitAnd rhs.hi (base - 1);
      b3 = rhs.hi / base;

      c0 = a0 * b0;
      r0 = builtins.bitAnd c0 (base - 1);
      carry0 = c0 / base;

      c1 = a0 * b1 + a1 * b0 + carry0;
      r1 = builtins.bitAnd c1 (base - 1);
      carry1 = c1 / base;

      c2 = a0 * b2 + a1 * b1 + a2 * b0 + carry1;
      r2 = builtins.bitAnd c2 (base - 1);
      carry2 = c2 / base;

      c3 = a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + carry2;
      r3 = builtins.bitAnd c3 (base - 1);

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
    # wasm2nix:runtime-fn i64.add(lhs, rhs) -> value
    add = lhs: rhs: i64FromParts (i64AddParts (i64Parts lhs) (i64Parts rhs));
    # wasm2nix:runtime-fn i64.sub(lhs, rhs) -> value
    sub = lhs: rhs: i64FromParts (i64SubParts (i64Parts lhs) (i64Parts rhs));
    # wasm2nix:runtime-fn i64.mul(lhs, rhs) -> value
    mul = lhs: rhs: i64FromParts (i64MulParts (i64Parts lhs) (i64Parts rhs));
    # wasm2nix:runtime-fn i64.udiv(lhs, rhs) -> value
    udiv = lhs: rhs: i64FromParts (i64UnsignedDivRemParts (i64Parts lhs) (i64Parts rhs)).quot;
    # wasm2nix:runtime-fn i64.sdiv(lhs, rhs) -> value
    sdiv = lhs: rhs: i64SignedDiv lhs rhs;
    # wasm2nix:runtime-fn i64.urem(lhs, rhs) -> value
    urem = lhs: rhs: i64FromParts (i64UnsignedDivRemParts (i64Parts lhs) (i64Parts rhs)).rem;
    # wasm2nix:runtime-fn i64.srem(lhs, rhs) -> value
    srem = i64SignedRem;
    # wasm2nix:runtime-fn i64.band(lhs, rhs) -> value
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
    # wasm2nix:runtime-fn i64.bor(lhs, rhs) -> value
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
    # wasm2nix:runtime-fn i64.bxor(lhs, rhs) -> value
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
    # wasm2nix:runtime-fn i64.bnot(value) -> value
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
    # wasm2nix:runtime-fn i64.shl(lhs, rhs) -> value
    shl = lhs: rhs: i64FromParts (i64ShlParts (i64Parts lhs) rhs);
    # wasm2nix:runtime-fn i64.shr_u(lhs, rhs) -> value
    shr_u = lhs: rhs: i64FromParts (i64ShrUParts (i64Parts lhs) rhs);
    # wasm2nix:runtime-fn i64.shr_s(lhs, rhs) -> value
    shr_s = lhs: rhs: i64FromParts (i64ShrSParts (i64Parts lhs) rhs);
    # wasm2nix:runtime-fn i64.rotl(lhs, rhs) -> value
    rotl =
      lhs: rhs:
      let
        n = builtins.bitAnd rhs 63;
      in
      if n == 0 then lhs else bor (shl lhs n) (shr_u lhs (64 - n));
    # wasm2nix:runtime-fn i64.rotr(lhs, rhs) -> value
    rotr =
      lhs: rhs:
      let
        n = builtins.bitAnd rhs 63;
      in
      if n == 0 then lhs else bor (shr_u lhs n) (shl lhs (64 - n));
    # wasm2nix:runtime-fn i64.clz(value) -> value
    clz = value: checkI64 (clzBy 64 bitSetI64 value);
    # wasm2nix:runtime-fn i64.ctz(value) -> value
    ctz = value: checkI64 (ctzBy 64 bitSetI64 value);
    # wasm2nix:runtime-fn i64.popcnt(value) -> value
    popcnt = value: checkI64 (popcntBy 64 bitSetI64 value);
  };

  cmp = {
    # wasm2nix:runtime-fn cmp.eq(lhs, rhs) -> value
    eq = lhs: rhs: lhs == rhs;
    # wasm2nix:runtime-fn cmp.ne(lhs, rhs) -> value
    ne = lhs: rhs: lhs != rhs;
    # wasm2nix:runtime-fn cmp.lt_u(lhs, rhs) -> value
    lt_u = lhs: rhs: u32 lhs < u32 rhs;
    # wasm2nix:runtime-fn cmp.le_u(lhs, rhs) -> value
    le_u = lhs: rhs: u32 lhs <= u32 rhs;
    # wasm2nix:runtime-fn cmp.gt_u(lhs, rhs) -> value
    gt_u = lhs: rhs: u32 lhs > u32 rhs;
    # wasm2nix:runtime-fn cmp.ge_u(lhs, rhs) -> value
    ge_u = lhs: rhs: u32 lhs >= u32 rhs;
    # wasm2nix:runtime-fn cmp.lt_s(lhs, rhs) -> value
    lt_s = lhs: rhs: toI32 lhs < toI32 rhs;
    # wasm2nix:runtime-fn cmp.le_s(lhs, rhs) -> value
    le_s = lhs: rhs: toI32 lhs <= toI32 rhs;
    # wasm2nix:runtime-fn cmp.gt_s(lhs, rhs) -> value
    gt_s = lhs: rhs: toI32 lhs > toI32 rhs;
    # wasm2nix:runtime-fn cmp.ge_s(lhs, rhs) -> value
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
      # wasm2nix:runtime-fn cmp_i64.eq(lhs, rhs) -> value
      eq = eqParts;
      # wasm2nix:runtime-fn cmp_i64.ne(lhs, rhs) -> value
      ne = lhs: rhs: !(eqParts lhs rhs);
      # wasm2nix:runtime-fn cmp_i64.lt_u(lhs, rhs) -> value
      lt_u = lhs: rhs: !(i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs));
      # wasm2nix:runtime-fn cmp_i64.le_u(lhs, rhs) -> value
      le_u = lhs: rhs: i64UnsignedGteParts (i64Parts rhs) (i64Parts lhs);
      # wasm2nix:runtime-fn cmp_i64.gt_u(lhs, rhs) -> value
      gt_u = lhs: rhs: i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs) && !(eqParts lhs rhs);
      # wasm2nix:runtime-fn cmp_i64.ge_u(lhs, rhs) -> value
      ge_u = lhs: rhs: i64UnsignedGteParts (i64Parts lhs) (i64Parts rhs);
      # wasm2nix:runtime-fn cmp_i64.lt_s(lhs, rhs) -> value
      lt_s = lhs: rhs: checkI64 lhs < checkI64 rhs;
      # wasm2nix:runtime-fn cmp_i64.le_s(lhs, rhs) -> value
      le_s = lhs: rhs: checkI64 lhs <= checkI64 rhs;
      # wasm2nix:runtime-fn cmp_i64.gt_s(lhs, rhs) -> value
      gt_s = lhs: rhs: checkI64 lhs > checkI64 rhs;
      # wasm2nix:runtime-fn cmp_i64.ge_s(lhs, rhs) -> value
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
        remainder = builtins.bitAnd value (divisor - 1);
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
      exponent = builtins.bitAnd (parts.hi / 1048576) 2047;
      fracHi = builtins.bitAnd parts.hi 1048575;
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
    # wasm2nix:runtime-fn fcmp.f32.ord(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.uno(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.eq(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ne(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.one(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ueq(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.lt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.le(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.gt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ge(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ult(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ule(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.ugt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f32.uge(lhs, rhs) -> value
    f32 = floatCmpFns f32IsNaN f32IsZero (lhs: rhs: u32 lhs == u32 rhs) (
      lhs: rhs: f32Key lhs < f32Key rhs
    );
    # wasm2nix:runtime-fn fcmp.f64.ord(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.uno(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.eq(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ne(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.one(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ueq(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.lt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.le(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.gt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ge(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ult(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ule(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.ugt(lhs, rhs) -> value
    # wasm2nix:runtime-fn fcmp.f64.uge(lhs, rhs) -> value
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
      remainder = builtins.bitAnd shifted.sigExt 7;
      roundUp = remainder > 4 || (remainder == 4 && builtins.bitAnd quotient 1 == 1);
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
        oddExp = builtins.bitAnd info.exp 1 != 0;
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
          frac = builtins.bitAnd sig (divisor - 1);
          half = divisor / 2;
          roundedInt =
            if mode == "floor" then
              int + (if sign && frac != 0 then 1 else 0)
            else if mode == "ceil" then
              int + (if !sign && frac != 0 then 1 else 0)
            else if mode == "nearest" then
              int + (if frac > half || (frac == half && builtins.bitAnd int 1 == 1) then 1 else 0)
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
  big26Mask = 67108863;
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
      topRemainder = if excess == 0 then top else builtins.bitAnd top (pow2 topShift - 1);
      sticky = topRemainder != 0 || big26AnyBelow limbs (k - 1);
      quotient = high * pow2 excess + lowContribution;
    in
    if sticky then builtins.bitOr quotient 1 else quotient;
  big26ToSmall = limbs: big26HighToSmall limbs 0;
  f64MulSigLimbs =
    lhs: rhs:
    let
      a0 = builtins.bitAnd lhs big26Mask;
      a1 = builtins.bitAnd (lhs / big26Base) big26Mask;
      a2 = lhs / (big26Base * big26Base);
      b0 = builtins.bitAnd rhs big26Mask;
      b1 = builtins.bitAnd (rhs / big26Base) big26Mask;
      b2 = rhs / (big26Base * big26Base);

      t0 = a0 * b0;
      l0 = builtins.bitAnd t0 big26Mask;
      carry0 = t0 / big26Base;

      t1 = a0 * b1 + a1 * b0 + carry0;
      l1 = builtins.bitAnd t1 big26Mask;
      carry1 = t1 / big26Base;

      t2 = a0 * b2 + a1 * b1 + a2 * b0 + carry1;
      l2 = builtins.bitAnd t2 big26Mask;
      carry2 = t2 / big26Base;

      t3 = a1 * b2 + a2 * b1 + carry2;
      l3 = builtins.bitAnd t3 big26Mask;
      carry3 = t3 / big26Base;

      t4 = a2 * b2 + carry3;
      l4 = builtins.bitAnd t4 big26Mask;
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
      a0 = builtins.bitAnd value big26Mask;
      a1 = builtins.bitAnd (value / big26Base) big26Mask;
      a2 = value / (big26Base * big26Base);

      t2 = a0 * pow2 bitShift;
      l2 = builtins.bitAnd t2 big26Mask;
      carry2 = t2 / big26Base;

      t3 = a1 * pow2 bitShift + carry2;
      l3 = builtins.bitAnd t3 big26Mask;
      carry3 = t3 / big26Base;

      t4 = a2 * pow2 bitShift + carry3;
      l4 = builtins.bitAnd t4 big26Mask;
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
      remainder = builtins.bitAnd shifted.sigExt 7;
      roundUp = remainder > 4 || (remainder == 4 && builtins.bitAnd quotient 1 == 1);
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
        oddExp = builtins.bitAnd info.exp 1 != 0;
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
          frac = builtins.bitAnd sig (divisor - 1);
          half = divisor / 2;
          roundedInt =
            if mode == "floor" then
              int + (if sign && frac != 0 then 1 else 0)
            else if mode == "ceil" then
              int + (if !sign && frac != 0 then 1 else 0)
            else if mode == "nearest" then
              int + (if frac > half || (frac == half && builtins.bitAnd int 1 == 1) then 1 else 0)
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
      rawExp = builtins.bitAnd (bits / 8388608) 255;
      frac = builtins.bitAnd bits 8388607;
    in
    {
      sign = bits >= i32Sign;
      inherit rawExp frac;
    };

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
          if builtins.bitAnd magnitude (divisor - 1) == 0 then
            magnitude / divisor - 8388608
          else
            exactIntFloatError "f32" op "rounded result";
      sign = if negative then i32Sign else 0;
    in
    if magnitude == 0 then sign else u32 (sign + (exp + 127) * 8388608 + frac);

  f64Parts =
    value:
    let
      parts = i64Parts value;
      rawExp = builtins.bitAnd (parts.hi / 1048576) 2047;
      fracHi = builtins.bitAnd parts.hi 1048575;
    in
    {
      sign = parts.hi >= i32Sign;
      inherit rawExp fracHi;
      fracLo = parts.lo;
    };

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
      fracLo = builtins.bitAnd frac u32Max;
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

  u64PartsFromNonNegative = value: {
    lo = builtins.bitAnd value u32Max;
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
      builtins.bitAnd parts.lo (pow2 bits - 1)
    else if bits == 32 then
      parts.lo
    else
      parts.lo + builtins.bitAnd parts.hi (pow2 (bits - 32) - 1) * u32Mod;

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
              roundUp = remainder > half || (remainder == half && builtins.bitAnd quotient 1 == 1);
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
      fracLo = builtins.bitAnd frac u32Max;
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
        remainder = builtins.bitAnd value (divisor - 1);
        half = pow2 (shift - 1);
        roundUp = remainder > half || (remainder == half && builtins.bitAnd quotient 1 == 1);
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
        f64FromRaw sign (exponent + 1023) (frac / u32Mod) (builtins.bitAnd frac u32Max)
    else
      let
        exponent = parts.rawExp - 127;
        frac = parts.frac * pow2 29;
      in
      f64FromRaw sign (exponent + 1023) (frac / u32Mod) (builtins.bitAnd frac u32Max);

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
    # wasm2nix:runtime-fn conv.uextend(value, ty) -> value
    uextend = value: _ty: value;
    # wasm2nix:runtime-fn conv.sextend(value) -> value
    sextend = value: value;
    # wasm2nix:runtime-fn conv.ireduce(value) -> value
    ireduce = u32;
    # wasm2nix:runtime-fn conv.sign_extend_8(value) -> value
    sign_extend_8 = value: u32 (signExtend 8 value);
    # wasm2nix:runtime-fn conv.sign_extend_16(value) -> value
    sign_extend_16 = value: u32 (signExtend 16 value);
    # wasm2nix:runtime-fn conv.sign_extend_8_i64(value) -> value
    sign_extend_8_i64 = value: checkI64 (signExtend 8 value);
    # wasm2nix:runtime-fn conv.sign_extend_16_i64(value) -> value
    sign_extend_16_i64 = value: checkI64 (signExtend 16 value);
    # wasm2nix:runtime-fn conv.sign_extend_32(value) -> value
    sign_extend_32 = value: checkI64 (signExtend 32 value);
    trunc_s = {
      f32 = {
        # wasm2nix:runtime-fn conv.trunc_s.f32.i32(value) -> value
        i32 = truncToI32Signed false "f32";
        # wasm2nix:runtime-fn conv.trunc_s.f32.i64(value) -> value
        i64 = truncToI64Signed false "f32";
      };
      f64 = {
        # wasm2nix:runtime-fn conv.trunc_s.f64.i32(value) -> value
        i32 = truncToI32Signed false "f64";
        # wasm2nix:runtime-fn conv.trunc_s.f64.i64(value) -> value
        i64 = truncToI64Signed false "f64";
      };
    };
    trunc_u = {
      f32 = {
        # wasm2nix:runtime-fn conv.trunc_u.f32.i32(value) -> value
        i32 = truncToI32Unsigned false "f32";
        # wasm2nix:runtime-fn conv.trunc_u.f32.i64(value) -> value
        i64 = truncToI64Unsigned false "f32";
      };
      f64 = {
        # wasm2nix:runtime-fn conv.trunc_u.f64.i32(value) -> value
        i32 = truncToI32Unsigned false "f64";
        # wasm2nix:runtime-fn conv.trunc_u.f64.i64(value) -> value
        i64 = truncToI64Unsigned false "f64";
      };
    };
    trunc_sat_s = {
      f32 = {
        # wasm2nix:runtime-fn conv.trunc_sat_s.f32.i32(value) -> value
        i32 = truncToI32Signed true "f32";
        # wasm2nix:runtime-fn conv.trunc_sat_s.f32.i64(value) -> value
        i64 = truncToI64Signed true "f32";
      };
      f64 = {
        # wasm2nix:runtime-fn conv.trunc_sat_s.f64.i32(value) -> value
        i32 = truncToI32Signed true "f64";
        # wasm2nix:runtime-fn conv.trunc_sat_s.f64.i64(value) -> value
        i64 = truncToI64Signed true "f64";
      };
    };
    trunc_sat_u = {
      f32 = {
        # wasm2nix:runtime-fn conv.trunc_sat_u.f32.i32(value) -> value
        i32 = truncToI32Unsigned true "f32";
        # wasm2nix:runtime-fn conv.trunc_sat_u.f32.i64(value) -> value
        i64 = truncToI64Unsigned true "f32";
      };
      f64 = {
        # wasm2nix:runtime-fn conv.trunc_sat_u.f64.i32(value) -> value
        i32 = truncToI32Unsigned true "f64";
        # wasm2nix:runtime-fn conv.trunc_sat_u.f64.i64(value) -> value
        i64 = truncToI64Unsigned true "f64";
      };
    };
    convert_s = {
      i32 = {
        # wasm2nix:runtime-fn conv.convert_s.i32.f32(value) -> value
        f32 = convertIntToFloat true "i32" "f32";
        # wasm2nix:runtime-fn conv.convert_s.i32.f64(value) -> value
        f64 = convertIntToFloat true "i32" "f64";
      };
      i64 = {
        # wasm2nix:runtime-fn conv.convert_s.i64.f32(value) -> value
        f32 = convertIntToFloat true "i64" "f32";
        # wasm2nix:runtime-fn conv.convert_s.i64.f64(value) -> value
        f64 = convertIntToFloat true "i64" "f64";
      };
    };
    convert_u = {
      i32 = {
        # wasm2nix:runtime-fn conv.convert_u.i32.f32(value) -> value
        f32 = convertIntToFloat false "i32" "f32";
        # wasm2nix:runtime-fn conv.convert_u.i32.f64(value) -> value
        f64 = convertIntToFloat false "i32" "f64";
      };
      i64 = {
        # wasm2nix:runtime-fn conv.convert_u.i64.f32(value) -> value
        f32 = convertIntToFloat false "i64" "f32";
        # wasm2nix:runtime-fn conv.convert_u.i64.f64(value) -> value
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

  # Memory holds little-endian u32 words in a two-level attrset: chunks of
  # `chunkWords` words keyed by chunk index, each keyed by the word's slot
  # within its chunk. Accesses work on whole words, and a store rewrites one
  # chunk plus the chunk index instead of an attrset spanning all of memory.
  # Nix layers small right operands of `//` and flattens them once the layer
  # chain fills, so the flatten cost is what the chunking bounds.
  chunkWords = 128;
  slotMask = chunkWords - 1;
  wordMask = u32Max;
  byteShift = offset: pow2 (offset * 8);

  getWord =
    pages: idx:
    (pages.${toString (idx / chunkWords)} or { }).${toString (builtins.bitAnd idx slotMask)} or 0;

  # Words are forced before they are stored so that pages never hold thunks
  # that retain older pages.
  setWord =
    pages: idx: value:
    let
      key = toString (idx / chunkWords);
      chunk = (pages.${key} or { }) // {
        ${toString (builtins.bitAnd idx slotMask)} = value;
      };
    in
    builtins.seq value (builtins.seq chunk (pages // { ${key} = chunk; }));

  # Merges a two-level overlay into `base` chunk by chunk.
  overlayPages =
    base: overlay: base // builtins.mapAttrs (key: chunk: (base.${key} or { }) // chunk) overlay;

  # Reads `width` (1..4) bytes starting at `offset` within word `idx`, spilling
  # into the next word when the access crosses a word boundary.
  loadSpan =
    pages: idx: offset: width:
    let
      lo = getWord pages idx;
      shift = byteShift offset;
      mask = pow2 (width * 8) - 1;
    in
    if offset + width <= 4 then
      builtins.bitAnd (lo / shift) mask
    else
      builtins.bitAnd (lo / shift + getWord pages (idx + 1) * byteShift (4 - offset)) mask;

  # Writes the low `width` (1..4) bytes of `value` at `offset` within word `idx`.
  storeSpan =
    pages: idx: offset: width: value:
    let
      lo = getWord pages idx;
      shift = byteShift offset;
      mask = pow2 (width * 8) - 1;
      v = builtins.bitAnd value mask;
      loBits = builtins.bitAnd (mask * shift) wordMask;
      newLo = lo - builtins.bitAnd lo loBits + builtins.bitAnd (v * shift) wordMask;
      withLo = setWord pages idx newLo;
    in
    if offset + width <= 4 then
      withLo
    else
      let
        hi = getWord pages (idx + 1);
        hiShift = byteShift (4 - offset);
      in
      setWord withLo (idx + 1) (hi - builtins.bitAnd hi (mask / hiShift) + v / hiShift);

  # The load and store paths below run for every memory access, so they read
  # the state and check bounds inline instead of calling getMem, getPages and
  # checkAccess, and handle aligned whole words without loadSpan/storeSpan.
  loadWidth =
    op: width: addr: st:
    let
      mem = st.mem or { };
      pages = mem.pages or { };
      offset = builtins.bitAnd addr 3;
    in
    if addr < 0 || addr + width > (mem.pageCount or 1) * pageBytes then
      throw "wasm trap: ${op} out of bounds"
    else if offset == 0 && width == 4 then
      getWord pages (addr / 4)
    else
      loadSpan pages (addr / 4) offset width;

  storeWidth =
    op: width: value: addr: st:
    let
      mem = st.mem or { };
      pages = mem.pages or { };
      offset = builtins.bitAnd addr 3;
      idx = addr / 4;
    in
    if addr < 0 || addr + width > (mem.pageCount or 1) * pageBytes then
      throw "wasm trap: ${op} out of bounds"
    else
      st
      // {
        mem = mem // {
          pages =
            if offset == 0 && width == 4 then
              setWord pages idx (builtins.bitAnd value wordMask)
            else
              storeSpan pages idx offset width value;
        };
      };

  getByteUnchecked = pages: addr: loadSpan pages (addr / 4) (builtins.bitAnd addr 3) 1;

  loadBytes =
    op: width: addr: st:
    builtins.seq (checkAccess op width addr st) (
      let
        pages = getPages st;
      in
      builtins.genList (i: getByteUnchecked pages (addr + i)) width
    );

  # Writes a byte list with one chunk-wise overlay covering every touched word.
  storeBytes =
    op: bytes: addr: st:
    let
      len = length bytes;
      end = addr + len;
      pages = getPages st;
      firstWord = addr / 4;
      lastWord = (end - 1) / 4;
      byteAtAddr = a: if a >= addr && a < end then elemAt bytes (a - addr) else getByteUnchecked pages a;
      wordAt =
        idx:
        let
          base = idx * 4;
        in
        byteAtAddr base
        + byteAtAddr (base + 1) * 256
        + byteAtAddr (base + 2) * 65536
        + byteAtAddr (base + 3) * 16777216;
      firstChunk = firstWord / chunkWords;
      chunkOverlay =
        chunk:
        let
          from = if chunk == firstChunk then firstWord else chunk * chunkWords;
          to = if chunk == lastWord / chunkWords then lastWord else chunk * chunkWords + slotMask;
        in
        builtins.listToAttrs (
          builtins.genList (i: {
            name = toString (builtins.bitAnd (from + i) slotMask);
            value = wordAt (from + i);
          }) (to - from + 1)
        );
      overlay = builtins.listToAttrs (
        builtins.genList (
          i:
          let
            chunk = firstChunk + i;
          in
          {
            name = toString chunk;
            value = chunkOverlay chunk;
          }
        ) (lastWord / chunkWords - firstChunk + 1)
      );
      newPages = overlayPages pages overlay;
    in
    builtins.seq (checkAccess op len addr st) (
      if len == 0 then st else builtins.deepSeq overlay (builtins.deepSeq newPages (setPages newPages st))
    );

  mem = rec {
    # wasm2nix:runtime-fn mem.load_u8(addr, st) -> value
    load_u8 = loadWidth "load_u8" 1;
    # wasm2nix:runtime-fn mem.load_s8(addr, st) -> value
    load_s8 = addr: st: u32 (signExtend 8 (load_u8 addr st));
    # wasm2nix:runtime-fn mem.load_s8_i64(addr, st) -> value
    load_s8_i64 = addr: st: checkI64 (signExtend 8 (load_u8 addr st));
    # wasm2nix:runtime-fn mem.load_u16(addr, st) -> value
    load_u16 = loadWidth "load_u16" 2;
    # wasm2nix:runtime-fn mem.load_s16(addr, st) -> value
    load_s16 = addr: st: u32 (signExtend 16 (load_u16 addr st));
    # wasm2nix:runtime-fn mem.load_s16_i64(addr, st) -> value
    load_s16_i64 = addr: st: checkI64 (signExtend 16 (load_u16 addr st));
    # wasm2nix:runtime-fn mem.load_u32(addr, st) -> value
    load_u32 = loadWidth "load_u32" 4;
    # wasm2nix:runtime-fn mem.load_s32(addr, st) -> value
    load_s32 = load_u32;
    # wasm2nix:runtime-fn mem.load_s32_i64(addr, st) -> value
    load_s32_i64 = addr: st: checkI64 (signExtend 32 (load_u32 addr st));
    # wasm2nix:runtime-fn mem.load_i32(addr, st) -> value
    load_i32 = load_u32;
    # wasm2nix:runtime-fn mem.load_f32(addr, st) -> value
    load_f32 = load_u32;
    # wasm2nix:runtime-fn mem.load_i64(addr, st) -> value
    load_i64 =
      addr: st:
      let
        mem = st.mem or { };
        pages = mem.pages or { };
        idx = addr / 4;
        offset = builtins.bitAnd addr 3;
        lo = if offset == 0 then getWord pages idx else loadSpan pages idx offset 4;
        hi = if offset == 0 then getWord pages (idx + 1) else loadSpan pages (idx + 1) offset 4;
      in
      if addr < 0 || addr + 8 > (mem.pageCount or 1) * pageBytes then
        throw "wasm trap: load_i64 out of bounds"
      else if hi >= i32Sign then
        (hi - u32Mod) * u32Mod + lo
      else
        hi * u32Mod + lo;
    # wasm2nix:runtime-fn mem.load_f64(addr, st) -> value
    load_f64 = load_i64;

    # wasm2nix:runtime-fn mem.store_u8(value, addr, st) -> state
    store_u8 = storeWidth "store_u8" 1;
    # wasm2nix:runtime-fn mem.store_u16(value, addr, st) -> state
    store_u16 = storeWidth "store_u16" 2;
    # wasm2nix:runtime-fn mem.store_i32(value, addr, st) -> state
    store_i32 = storeWidth "store_i32" 4;
    # wasm2nix:runtime-fn mem.store_u32(value, addr, st) -> state
    store_u32 = store_i32;
    # wasm2nix:runtime-fn mem.store_f32(value, addr, st) -> state
    store_f32 = store_i32;
    # wasm2nix:runtime-fn mem.store_i64(value, addr, st) -> state
    store_i64 =
      value: addr: st:
      let
        mem = st.mem or { };
        pages = mem.pages or { };
        idx = addr / 4;
        offset = builtins.bitAnd addr 3;
        lo = builtins.bitAnd value wordMask;
        # value - lo is a multiple of 2^32, so the division is exact.
        hi = builtins.bitAnd ((value - lo) / u32Mod) wordMask;
      in
      if addr < 0 || addr + 8 > (mem.pageCount or 1) * pageBytes then
        throw "wasm trap: store_i64 out of bounds"
      else
        st
        // {
          mem = mem // {
            pages =
              if offset == 0 then
                setWord (setWord pages idx lo) (idx + 1) hi
              else
                storeSpan (storeSpan pages idx offset 4 lo) (idx + 1) offset 4 hi;
          };
        };
    # wasm2nix:runtime-fn mem.store_f64(value, addr, st) -> state
    store_f64 = store_i64;

    # wasm2nix:runtime-fn mem.size(st) -> value
    size = st: pageCount st;
    # wasm2nix:runtime-fn mem.grow(delta, st) -> record
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
    # wasm2nix:runtime-fn mem.fill(dst, value, len, st) -> state
    fill =
      dst: value: len: st:
      storeBytes "memory.fill" (builtins.genList (_: builtins.bitAnd value 255) len) dst st;
    # wasm2nix:runtime-fn mem.copy(dst, src, len, st) -> state
    copy =
      dst: src: len: st:
      let
        bytes = loadBytes "memory.copy" len src st;
      in
      storeBytes "memory.copy" bytes dst st;
    # wasm2nix:runtime-fn mem.init(seg_index, dst, src, len, st) -> state
    init =
      segIndex: dst: src: len: st:
      let
        segment = getDataSegment segIndex st;
        bytes = builtins.seq (checkSegmentAccess "memory.init segment" segment src len) (
          builtins.genList (i: elemAt segment (src + i)) len
        );
      in
      storeBytes "memory.init" bytes dst st;
    # wasm2nix:runtime-fn mem.data_drop(seg_index, st) -> state
    data_drop = segIndex: st: setDataSegment segIndex [ ] st;
    # wasm2nix:runtime-fn mem.load_bytes(addr, len, st) -> value
    load_bytes =
      addr: len: st:
      loadBytes "load_bytes" len addr st;
    # wasm2nix:runtime-fn mem.store_bytes(bytes, addr, st) -> state
    store_bytes = storeBytes "store_bytes";
    # wasm2nix:runtime-fn mem.overlay_pages(base, overlay) -> value
    overlay_pages = overlayPages;
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
      # wasm2nix:runtime-fn global.get_i32(index, st) -> value
      get_i32 = get;
      # wasm2nix:runtime-fn global.get_i64(index, st) -> value
      get_i64 = get;
      # wasm2nix:runtime-fn global.get_f32(index, st) -> value
      get_f32 = get;
      # wasm2nix:runtime-fn global.get_f64(index, st) -> value
      get_f64 = get;
      # wasm2nix:runtime-fn global.set_i32(index, value, st) -> state
      set_i32 =
        idx: value: st:
        set idx (if builtins.isAttrs value then value else u32 value) st;
      # wasm2nix:runtime-fn global.set_i64(index, value, st) -> state
      set_i64 =
        idx: value: st:
        set idx (checkI64 value) st;
      # wasm2nix:runtime-fn global.set_f32(index, value, st) -> state
      set_f32 =
        idx: value: st:
        set idx (u32 value) st;
      # wasm2nix:runtime-fn global.set_f64(index, value, st) -> state
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
      # wasm2nix:runtime-fn table.call(table_index, type_id, module_id, funcs, func_registry, elem_index, args, st) -> record
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
      # wasm2nix:runtime-fn table.get(table_index, elem_index, st) -> value
      get =
        tableIndex: elemIndex: st:
        builtins.seq (checkTableAccess "table.get" (getTable tableIndex st) elemIndex 1) (
          let
            entry = getEntry (getTable tableIndex st) elemIndex;
          in
          if entry == null then 0 else entry
        );
      # wasm2nix:runtime-fn table.set(table_index, elem_index, value, st) -> state
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
      # wasm2nix:runtime-fn table.size(table_index, st) -> value
      size = tableIndex: st: (getTable tableIndex st).size or 0;
      # wasm2nix:runtime-fn table.grow(table_index, init_value, delta, st) -> record
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
      # wasm2nix:runtime-fn table.fill(table_index, dst, value, len, st) -> state
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
      # wasm2nix:runtime-fn table.init(table_index, seg_index, dst, src, len, st) -> state
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
      # wasm2nix:runtime-fn table.elem_drop(seg_index, st) -> state
      elem_drop = segIndex: st: setElemSegment segIndex [ ] st;
      # wasm2nix:runtime-fn table.copy(dst_table_index, src_table_index, dst, src, len, st) -> state
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
{
  i32 = i32Bin;
  i64 = i64Bin;
  int = {
    # wasm2nix:runtime-fn int.bnot(value) -> value
    bnot = i32Bin.bnot;
  };
  f32 = {
    # wasm2nix:runtime-fn f32.add(lhs, rhs) -> value
    add = f32Add;
    # wasm2nix:runtime-fn f32.sub(lhs, rhs) -> value
    sub = f32Sub;
    # wasm2nix:runtime-fn f32.mul(lhs, rhs) -> value
    mul = f32Mul;
    # wasm2nix:runtime-fn f32.div(lhs, rhs) -> value
    div = f32Div;
    # wasm2nix:runtime-fn f32.min(lhs, rhs) -> value
    min = f32Min;
    # wasm2nix:runtime-fn f32.max(lhs, rhs) -> value
    max = f32Max;
    # wasm2nix:runtime-fn f32.copysign(lhs, rhs) -> value
    copysign = f32Copysign;
    # wasm2nix:runtime-fn f32.neg(value) -> value
    neg = f32Neg;
    # wasm2nix:runtime-fn f32.abs(value) -> value
    abs = f32Abs;
    # wasm2nix:runtime-fn f32.sqrt(value) -> value
    sqrt = f32Sqrt;
    # wasm2nix:runtime-fn f32.ceil(value) -> value
    ceil = f32RoundIntegral "ceil";
    # wasm2nix:runtime-fn f32.floor(value) -> value
    floor = f32RoundIntegral "floor";
    # wasm2nix:runtime-fn f32.trunc(value) -> value
    trunc = f32RoundIntegral "trunc";
    # wasm2nix:runtime-fn f32.nearest(value) -> value
    nearest = f32RoundIntegral "nearest";
    # wasm2nix:runtime-fn f32.demote_f64(value) -> value
    demote_f64 = f32DemoteF64;
  };
  f64 = {
    # wasm2nix:runtime-fn f64.add(lhs, rhs) -> value
    add = f64Add;
    # wasm2nix:runtime-fn f64.sub(lhs, rhs) -> value
    sub = f64Sub;
    # wasm2nix:runtime-fn f64.mul(lhs, rhs) -> value
    mul = f64Mul;
    # wasm2nix:runtime-fn f64.div(lhs, rhs) -> value
    div = f64Div;
    # wasm2nix:runtime-fn f64.min(lhs, rhs) -> value
    min = f64Min;
    # wasm2nix:runtime-fn f64.max(lhs, rhs) -> value
    max = f64Max;
    # wasm2nix:runtime-fn f64.copysign(lhs, rhs) -> value
    copysign = f64Copysign;
    # wasm2nix:runtime-fn f64.neg(value) -> value
    neg = f64Neg;
    # wasm2nix:runtime-fn f64.abs(value) -> value
    abs = f64Abs;
    # wasm2nix:runtime-fn f64.sqrt(value) -> value
    sqrt = f64Sqrt;
    # wasm2nix:runtime-fn f64.ceil(value) -> value
    ceil = f64RoundIntegral "ceil";
    # wasm2nix:runtime-fn f64.floor(value) -> value
    floor = f64RoundIntegral "floor";
    # wasm2nix:runtime-fn f64.trunc(value) -> value
    trunc = f64RoundIntegral "trunc";
    # wasm2nix:runtime-fn f64.nearest(value) -> value
    nearest = f64RoundIntegral "nearest";
    # wasm2nix:runtime-fn f64.promote_f32(value) -> value
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
