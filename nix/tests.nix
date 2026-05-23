{
  pkgs ? import <nixpkgs> { },
}:
let
  lib = pkgs.lib;
  rt = import ./rt.nix { };

  st0 = {
    mem = {
      pages = { };
    };
    globals = { };
  };

  # Helpers inspired by wasm spec-style assertions:
  # - `run1` extracts the first result from `{ st, results = [ ... ] }`.
  run1 = r: builtins.elemAt r.results 0;

  tryOk = e: (builtins.tryEval e).success;

  # Decimal-only constants (Nix literals): avoid 0x... forms.
  u32Max = 4294967295;
  pageBytes = 65536;
in
lib.debug.runTests {
  test_rt_has_no_required_args = {
    expr = builtins.functionArgs (import ./rt.nix);
    expected = { };
  };

  # i32.add (wasm spec: i32.add)
  test_i32_add_0_0 = {
    expr = rt.i32.add 0 0;
    expected = 0;
  };
  test_i32_add_1_2 = {
    expr = rt.i32.add 1 2;
    expected = 3;
  };
  test_i32_add_wrap = {
    expr = rt.i32.add u32Max 1;
    expected = 0;
  };

  # i32.sub wrap-around (wasm spec: i32.sub)
  test_i32_sub_wrap = {
    expr = rt.i32.sub 0 1;
    expected = u32Max;
  };

  test_i32_mul_wrap = {
    expr = rt.i32.mul 65536 65536;
    expected = 0;
  };
  test_i32_shl_sign_bit = {
    expr = rt.i32.shl 1 31;
    expected = 2147483648;
  };
  test_i32_shr_u_sign_bit = {
    expr = rt.i32.shr_u 2147483648 31;
    expected = 1;
  };
  test_i32_shr_s_sign_bit = {
    expr = rt.i32.shr_s 2147483648 31;
    expected = u32Max;
  };
  test_int_bnot_compat = {
    expr = rt.int.bnot 0;
    expected = u32Max;
  };
  test_i32_div_zero_traps = {
    expr = tryOk (rt.i32.udiv 1 0);
    expected = false;
  };
  test_i32_signed_overflow_traps = {
    expr = tryOk (rt.i32.sdiv 2147483648 u32Max);
    expected = false;
  };

  # Little-endian memory behavior via i32 store/load
  # 16909060 == 0x01020304, bytes in memory are [4,3,2,1]
  test_mem_store_load_u32 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u32 0 st1;
    expected = 16909060;
  };
  test_mem_le_u8_0 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u8 0 st1;
    expected = 4;
  };
  test_mem_le_u8_1 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u8 1 st1;
    expected = 3;
  };
  test_mem_le_u8_2 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u8 2 st1;
    expected = 2;
  };
  test_mem_le_u8_3 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u8 3 st1;
    expected = 1;
  };

  # Unaligned i32 load within a page should work (wasm spec allows unaligned accesses).
  # From bytes [4,3,2,1] at addr 0, reading u32 at addr 1 yields bytes [3,2,1,0] => 66051.
  test_mem_unaligned_load_u32 = {
    expr =
      let
        st1 = rt.mem.store_i32 16909060 0 st0;
      in
      rt.mem.load_u32 1 st1;
    expected = 66051;
  };

  # Unaligned i32 store within a page should work (and may touch two words).
  # Store 1 at addr 1: first word becomes 256 (byte1=1).
  test_mem_unaligned_store_u32_updates_word0 = {
    expr =
      let
        st1 = rt.mem.store_i32 1 1 st0;
      in
      rt.mem.load_u32 0 st1;
    expected = 256;
  };

  # With the default one-page state, accesses that cross the page end trap as out-of-bounds.
  test_mem_cross_page_load_u16_traps = {
    expr = tryOk (rt.mem.load_u16 (pageBytes - 1) st0);
    expected = false;
  };
  test_mem_cross_page_store_u16_traps = {
    expr = tryOk (rt.mem.store_u16 1 (pageBytes - 1) st0);
    expected = false;
  };
  test_mem_cross_page_unaligned_load_u32_traps = {
    expr = tryOk (rt.mem.load_u32 (pageBytes - 1) st0);
    expected = false;
  };

  test_mem_grow_allows_cross_page_load = {
    expr =
      let
        grown = (rt.mem.grow 1 st0).st;
        st1 = rt.mem.store_u16 513 (pageBytes - 1) grown;
      in
      rt.mem.load_u16 (pageBytes - 1) st1;
    expected = 513;
  };
  test_mem_grow_returns_old_size = {
    expr = run1 (rt.mem.grow 1 st0);
    expected = 1;
  };
  test_mem_size_after_grow = {
    expr = rt.mem.size (rt.mem.grow 2 st0).st;
    expected = 3;
  };

  test_global_set_get_i32 = {
    expr =
      let
        st1 = rt.global.set_i32 7 42 st0;
      in
      rt.global.get_i32 7 st1;
    expected = 42;
  };

  test_table_call_dispatches_function = {
    expr =
      let
        funcs = {
          f0 =
            x: st:
            {
              inherit st;
              results = [ (rt.i32.add x 1) ];
            };
        };
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = {
                "0" = {
                  func = 0;
                  type = 0;
                };
              };
            };
          };
        };
      in
      run1 (rt.table.call 0 0 funcs 0 [ 41 ] st1);
    expected = 42;
  };
  test_table_call_type_mismatch_traps = {
    expr =
      let
        funcs = {
          f0 =
            st:
            {
              inherit st;
              results = [ ];
            };
        };
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = {
                "0" = {
                  func = 0;
                  type = 1;
                };
              };
            };
          };
        };
      in
      tryOk (rt.table.call 0 0 funcs 0 [ ] st1);
    expected = false;
  };
}
