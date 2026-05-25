{
  tracePrints ? true,
}:
let
  traceCall =
    name: args: st:
    let
      result = {
        results = [ ];
        inherit st;
      };
    in
    if tracePrints then
      builtins.trace {
        event = "spectest.${name}";
        inherit args;
      } result
    else
      result;

  getAttr =
    attrs: name: default:
    if builtins.hasAttr name attrs then builtins.getAttr name attrs else default;

  index = value: builtins.toString value;
in
rec {
  # Test-only imports for WebAssembly spec WAST files. This file is deliberately
  # outside rt.nix so the generated runtime stays dependency-free.
  spectest = {
    global_i32 = 666;
    global_i64 = 666;
    global_f32 = 1143383654;
    global_f64 = 4649074691427585229;

    memory = {
      pageCount = 1;
      maxPageCount = 2;
      pages = { };
    };

    table = {
      size = 10;
      maxSize = 20;
      elems = { };
    };

    print = traceCall "print" [ ];
    print_i32 = v: traceCall "print_i32" [ v ];
    print_i64 = v: traceCall "print_i64" [ v ];
    print_f32 = v: traceCall "print_f32" [ v ];
    print_f64 = v: traceCall "print_f64" [ v ];
    print_i32_f32 =
      i: f:
      traceCall "print_i32_f32" [
        i
        f
      ];
    print_f64_f64 =
      a: b:
      traceCall "print_f64_f64" [
        a
        b
      ];
  };

  dynamicMemoryLimits =
    {
      st,
      staticMin,
      staticMax ? null,
    }:
    let
      mem = st.mem or { };
    in
    {
      min = mem.pageCount or staticMin;
      max = mem.maxPageCount or staticMax;
    };

  dynamicTableLimits =
    {
      st,
      tableIndex,
      staticMin,
      staticMax ? null,
    }:
    let
      tables = st.tables or { };
      table = getAttr tables (index tableIndex) { };
    in
    {
      min = table.size or staticMin;
      max = table.maxSize or staticMax;
    };

  mergeTableEntries =
    {
      dstSt,
      targetTableIndex,
      srcSt,
      sourceTableIndex,
      offset,
      len,
    }:
    let
      dstTables = dstSt.tables or { };
      srcTables = srcSt.tables or { };
      dstKey = index targetTableIndex;
      srcKey = index sourceTableIndex;
      dstTable = getAttr dstTables dstKey {
        size = 0;
        elems = { };
      };
      srcTable = getAttr srcTables srcKey { elems = { }; };
      srcElems = srcTable.elems or { };
      entries = builtins.listToAttrs (
        builtins.genList (
          i:
          let
            slot = index (offset + i);
          in
          {
            name = slot;
            value = builtins.getAttr slot srcElems;
          }
        ) len
      );
      mergedTable = dstTable // {
        elems = (dstTable.elems or { }) // entries;
      };
    in
    dstSt
    // {
      tables = dstTables // {
        ${dstKey} = mergedTable;
      };
    };

  mergeTableState =
    {
      dstSt,
      targetTableIndex,
      srcSt,
      sourceTableIndex,
    }:
    let
      dstTables = dstSt.tables or { };
      srcTables = srcSt.tables or { };
      dstKey = index targetTableIndex;
      srcKey = index sourceTableIndex;
      dstTable = getAttr dstTables dstKey {
        size = 0;
        elems = { };
      };
      srcTable = getAttr srcTables srcKey { elems = { }; };
      mergedTable = dstTable // {
        elems = (dstTable.elems or { }) // (srcTable.elems or { });
      };
    in
    dstSt
    // {
      tables = dstTables // {
        ${dstKey} = mergedTable;
      };
    };

  mergeMemoryWords =
    {
      dstSt,
      srcSt,
      offset,
      len,
    }:
    let
      firstWord = offset / 4;
      lastWord = if len == 0 then firstWord else (offset + len - 1) / 4;
      dstMem = dstSt.mem or { };
      srcMem = srcSt.mem or { };
      srcPages = srcMem.pages or { };
      entries = builtins.listToAttrs (
        builtins.genList (
          i:
          let
            word = index (firstWord + i);
          in
          {
            name = word;
            value = builtins.getAttr word srcPages;
          }
        ) (lastWord - firstWord + 1)
      );
    in
    dstSt
    // {
      mem = dstMem // {
        pages = (dstMem.pages or { }) // entries;
      };
    };

  syncGlobalState =
    {
      dstSt,
      dstGlobalIndex,
      srcSt,
      srcGlobalIndex,
    }:
    let
      dstGlobals = dstSt.globals or { };
      srcGlobals = srcSt.globals or { };
      dstKey = index dstGlobalIndex;
      value = getAttr srcGlobals (index srcGlobalIndex) 0;
    in
    dstSt
    // {
      globals = dstGlobals // {
        ${dstKey} = value;
      };
    };

  syncMemoryState =
    {
      dstSt,
      srcSt,
      srcMemoryIndex ? 0,
    }:
    dstSt // { mem = srcSt.mem; };

  fail = builtins.abort;
  trace = builtins.trace;
  toJSON = builtins.toJSON;
  fromJSON = builtins.fromJSON;
}
