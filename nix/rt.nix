{ }:
let
  genPowers =
    n: value:
    if n > 62 then
      [ ]
    else
      [ value ] ++ genPowers (n + 1) (value * 2);

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

  mod =
    value: divisor:
    value - divisor * (value / divisor);

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

  signedDiv =
    lhs: rhs:
    if rhs == 0 then
      throw "wasm trap: integer divide by zero"
    else
      lhs / rhs;

  signedRem =
    lhs: rhs:
    if rhs == 0 then
      throw "wasm trap: integer divide by zero"
    else
      lhs - rhs * (lhs / rhs);

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

  unsignedShiftRight32 =
    value: amount:
    (u32 value) / pow2 (shiftCount32 amount);

  signedShiftRight32 =
    value: amount:
    let
      signed = toI32 value;
      divisor = pow2 (shiftCount32 amount);
      quotient =
        if signed < 0 then
          0 - (((0 - signed) + divisor - 1) / divisor)
        else
          signed / divisor;
    in
    u32 quotient;

  i32Bin =
    {
      add = lhs: rhs: u32 (u32 lhs + u32 rhs);
      sub = lhs: rhs: u32 (u32 lhs - u32 rhs);
      mul = mul32;
      udiv =
        lhs: rhs:
        let
          divisor = u32 rhs;
        in
        if divisor == 0 then
          throw "wasm trap: integer divide by zero"
        else
          (u32 lhs) / divisor;
      sdiv =
        lhs: rhs:
        let
          a = toI32 lhs;
          b = toI32 rhs;
        in
        if a == i32Min && b == -1 then
          throw "wasm trap: integer overflow"
        else
          u32 (signedDiv a b);
      urem =
        lhs: rhs:
        let
          divisor = u32 rhs;
        in
        if divisor == 0 then
          throw "wasm trap: integer divide by zero"
        else
          floorMod (u32 lhs) divisor;
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
        if n == 0 then
          u32 lhs
        else
          i32Bin.bor (i32Bin.shl lhs n) (i32Bin.shr_u lhs (32 - n));
      rotr =
        lhs: rhs:
        let
          n = shiftCount32 rhs;
        in
        if n == 0 then
          u32 lhs
        else
          i32Bin.bor (i32Bin.shr_u lhs n) (i32Bin.shl lhs (32 - n));
    };

  limitedI64 =
    op: f:
    lhs: rhs:
    checkI64 (f (checkI64 lhs) (checkI64 rhs));

  i64Bin =
    {
      add = limitedI64 "add" (lhs: rhs: lhs + rhs);
      sub = limitedI64 "sub" (lhs: rhs: lhs - rhs);
      mul = limitedI64 "mul" (lhs: rhs: lhs * rhs);
      udiv =
        lhs: rhs:
        if rhs == 0 then throw "wasm trap: integer divide by zero" else checkI64 (lhs / rhs);
      sdiv =
        lhs: rhs:
        if rhs == 0 then throw "wasm trap: integer divide by zero" else checkI64 (lhs / rhs);
      urem =
        lhs: rhs:
        if rhs == 0 then throw "wasm trap: integer divide by zero" else checkI64 (mod lhs rhs);
      srem = lhs: rhs: checkI64 (signedRem lhs rhs);
      band = lhs: rhs: checkI64 (builtins.bitAnd lhs rhs);
      bor = lhs: rhs: checkI64 (builtins.bitOr lhs rhs);
      bxor = lhs: rhs: checkI64 (builtins.bitXor lhs rhs);
      bnot = value: checkI64 (0 - value - 1);
      shl = lhs: rhs: checkI64 (lhs * pow2 (floorMod rhs 63));
      shr_u = lhs: rhs: checkI64 (lhs / pow2 (floorMod rhs 63));
      shr_s = lhs: rhs: checkI64 (lhs / pow2 (floorMod rhs 63));
      rotl = lhs: _rhs: checkI64 lhs;
      rotr = lhs: _rhs: checkI64 lhs;
    };

  unsupportedFloat =
    ty: op: _lhs: _rhs:
    throw "wasm2nix runtime: ${ty}.${op} requires the soft IEEE-754 runtime";

  unsupportedUnaryFloat =
    ty: op: _value:
    throw "wasm2nix runtime: ${ty}.${op} requires the soft IEEE-754 runtime";

  cmp =
    {
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

  conv =
    {
      uextend = value: _ty: value;
      sextend = value: value;
      ireduce = u32;
      sign_extend_8 = value: u32 (signExtend 8 value);
      sign_extend_16 = value: u32 (signExtend 16 value);
      sign_extend_32 = value: checkI64 (signExtend 32 value);
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

  checkAccess =
    op: width: addr: st:
    let
      pages = pageCount st;
      limit = pages * pageBytes;
      end = addr + width;
    in
    if addr < 0 || end > limit then
      throw "wasm trap: ${op} out of bounds"
    else
      true;

  wordIndex = addr: addr / 4;
  byteOffset = addr: floorMod addr 4;

  getWord =
    pages: idx:
    pages.${toString idx} or 0;

  setWord =
    pages: idx: value:
    pages // { "${toString idx}" = u32 value; };

  byteAt =
    word: offset:
    floorMod (word / pow2 (offset * 8)) 256;

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

  range =
    count:
    builtins.genList (i: i) count;

  loadBytes =
    op: width: addr: st:
    builtins.seq (checkAccess op width addr st) (
      let
        pages = getPages st;
      in
      map (i: getByteUnchecked pages (addr + i)) (range width)
    );

  bytesToU16 =
    bytes:
    elemAt bytes 0 + elemAt bytes 1 * 256;

  bytesToU32 =
    bytes:
    elemAt bytes 0 + elemAt bytes 1 * 256 + elemAt bytes 2 * 65536 + elemAt bytes 3 * 16777216;

  bytesToLimitedU64 =
    bytes:
    let
      lo = bytesToU32 (builtins.genList (i: elemAt bytes i) 4);
      hi = bytesToU32 (builtins.genList (i: elemAt bytes (i + 4)) 4);
    in
    if hi >= i32Sign then
      throw "wasm2nix runtime: u64 load is outside the host Nix integer range"
    else
      lo + hi * u32Mod;

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
      v = checkI64 value;
      lo = floorMod v u32Mod;
      hi = v / u32Mod;
    in
    u32ToBytes lo ++ u32ToBytes hi;

  storeBytes =
    op: bytes: addr: st:
    builtins.seq (checkAccess op (length bytes) addr st) (
      let
        pages = builtins.foldl' (
          acc: i:
          setByteUnchecked acc (addr + i) (elemAt bytes i)
        ) (getPages st) (range (length bytes));
      in
      setPages pages st
    );

  mem =
    rec {
      load_u8 = addr: st: elemAt (loadBytes "load_u8" 1 addr st) 0;
      load_s8 = addr: st: u32 (signExtend 8 (load_u8 addr st));
      load_u16 = addr: st: bytesToU16 (loadBytes "load_u16" 2 addr st);
      load_s16 = addr: st: u32 (signExtend 16 (load_u16 addr st));
      load_u32 = addr: st: bytesToU32 (loadBytes "load_u32" 4 addr st);
      load_s32 = addr: st: u32 (signExtend 32 (load_u32 addr st));
      load_i32 = load_u32;
      load_f32 = load_u32;
      load_i64 = addr: st: bytesToLimitedU64 (loadBytes "load_i64" 8 addr st);
      load_f64 = load_i64;

      store_u8 = value: addr: st: storeBytes "store_u8" [ (floorMod value 256) ] addr st;
      store_u16 =
        value: addr: st:
        storeBytes "store_u16" [ (floorMod value 256) (floorMod (value / 256) 256) ] addr st;
      store_i32 = value: addr: st: storeBytes "store_i32" (u32ToBytes value) addr st;
      store_u32 = store_i32;
      store_f32 = store_i32;
      store_i64 = value: addr: st: storeBytes "store_i64" (limitedU64ToBytes value) addr st;
      store_f64 = store_i64;

      size = st: pageCount st;
      grow =
        delta: st:
        let
          old = pageCount st;
          new = old + delta;
        in
        if delta < 0 || new > maxPages then
          {
            inherit st;
            results = [ u32Max ];
          }
        else
          {
            st = setPageCount new st;
            results = [ old ];
          };
    };

  globals =
    st: st.globals or { };

  global =
    let
      get = idx: st: (globals st).${toString idx} or 0;
      set =
        idx: value: st:
        st
        // {
          globals = (globals st) // { "${toString idx}" = value; };
        };
    in
    {
      get_i32 = get;
      get_i64 = get;
      get_f32 = get;
      get_f64 = get;
      set_i32 = idx: value: st: set idx (u32 value) st;
      set_i64 = idx: value: st: set idx (checkI64 value) st;
      set_f32 = idx: value: st: set idx (u32 value) st;
      set_f64 = set;
    };

  tables =
    st: st.tables or { };

  table =
    let
      getTable = tableIndex: st: (tables st).${toString tableIndex} or { elems = { }; size = 0; };
      getEntry =
        tableIndex: elemIndex: st:
        (getTable tableIndex st).elems.${toString elemIndex} or null;
      applyArgs = builtins.foldl' (fn: arg: fn arg);
    in
    {
      call =
        tableIndex: typeIndex: funcs: elemIndex: args: st:
        let
          entry = getEntry tableIndex elemIndex st;
        in
        if entry == null then
          throw "wasm trap: undefined table element"
        else if entry.type != typeIndex then
          throw "wasm trap: indirect call type mismatch"
        else
          let
            fn = builtins.getAttr ("f" + toString entry.func) funcs;
          in
          (applyArgs fn args) st;
      size = tableIndex: st: (getTable tableIndex st).size or 0;
    };
in
rec {
  i32 = i32Bin;
  i64 = i64Bin;
  int = {
    bnot = i32Bin.bnot;
  };

  f32 = {
    add = unsupportedFloat "f32" "add";
    sub = unsupportedFloat "f32" "sub";
    mul = unsupportedFloat "f32" "mul";
    div = unsupportedFloat "f32" "div";
    neg = unsupportedUnaryFloat "f32" "neg";
    abs = unsupportedUnaryFloat "f32" "abs";
  };

  f64 = {
    add = unsupportedFloat "f64" "add";
    sub = unsupportedFloat "f64" "sub";
    mul = unsupportedFloat "f64" "mul";
    div = unsupportedFloat "f64" "div";
    neg = unsupportedUnaryFloat "f64" "neg";
    abs = unsupportedUnaryFloat "f64" "abs";
  };

  inherit cmp conv mem global table;
}
