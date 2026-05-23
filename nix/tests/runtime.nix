{
  rt,
  st0,
  run1,
  tryOk,
  u32Max,
  pageBytes,
}:
{
  test_rt_has_no_required_args = {
    expr = builtins.functionArgs (import ../rt.nix);
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
  test_i32_clz_zero = {
    expr = rt.i32.clz 0;
    expected = 32;
  };
  test_i32_clz_one = {
    expr = rt.i32.clz 1;
    expected = 31;
  };
  test_i32_clz_sign_bit = {
    expr = rt.i32.clz 2147483648;
    expected = 0;
  };
  test_i32_ctz_zero = {
    expr = rt.i32.ctz 0;
    expected = 32;
  };
  test_i32_ctz_sign_bit = {
    expr = rt.i32.ctz 2147483648;
    expected = 31;
  };
  test_i32_popcnt_all_bits = {
    expr = rt.i32.popcnt u32Max;
    expected = 32;
  };
  test_i64_clz_negative = {
    expr = rt.i64.clz (-1);
    expected = 0;
  };
  test_i64_ctz_min = {
    expr = rt.i64.ctz (0 - 9223372036854775807 - 1);
    expected = 63;
  };
  test_i64_popcnt_negative = {
    expr = rt.i64.popcnt (-1);
    expected = 64;
  };
  test_i64_add_wrap_to_min = {
    expr = rt.i64.add 9223372036854775807 1;
    expected = 0 - 9223372036854775807 - 1;
  };
  test_i64_unsigned_compare_high_bit = {
    expr = rt.cmp_i64.lt_u 1 (-1);
    expected = true;
  };
  test_i64_signed_compare_high_bit = {
    expr = rt.cmp_i64.lt_s (-1) 1;
    expected = true;
  };
  test_f32_compare_equal_finite = {
    expr = rt.fcmp.f32.eq 1077936128 1077936128;
    expected = true;
  };
  test_f32_compare_nan_unordered = {
    expr = rt.fcmp.f32.uno 2143289344 1077936128;
    expected = true;
  };
  test_f64_compare_signed_zero_equal = {
    expr = rt.fcmp.f64.eq (0 - 9223372036854775807 - 1) 0;
    expected = true;
  };
  test_f32_sqrt_zero_preserves_zero = {
    expr = rt.f32.sqrt 0;
    expected = 0;
  };
  test_f32_neg_flips_sign = {
    expr = rt.f32.neg 0;
    expected = 2147483648;
  };
  test_f64_abs_negative_zero = {
    expr = rt.f64.abs (0 - 9223372036854775807 - 1);
    expected = 0;
  };
  test_f32_exact_add = {
    expr = rt.f32.add 1065353216 1073741824;
    expected = 1077936128;
  };
  test_f32_add_negative_zeros_preserves_sign = {
    expr = rt.f32.add 2147483648 2147483648;
    expected = 2147483648;
  };
  test_f32_add_mixed_zeros_is_positive = {
    expr = rt.f32.add 2147483648 0;
    expected = 0;
  };
  test_f32_add_negative_zero_preserves_negative_subnormal = {
    expr = rt.f32.add 2147483648 2147483649;
    expected = 2147483649;
  };
  test_f32_add_negative_subnormals = {
    expr = rt.f32.add 2147483649 2147483649;
    expected = 2147483650;
  };
  test_f32_add_negative_min_subnormal_and_min_normal = {
    expr = rt.f32.add 2147483649 2155872256;
    expected = 2155872257;
  };
  test_f32_add_opposite_subnormals_cancels_positive = {
    expr = rt.f32.add 1 2147483649;
    expected = 0;
  };
  test_f32_add_quiets_nan = {
    expr = rt.f32.add 2139095041 1065353216;
    expected = 2143289345;
  };
  test_f32_exact_sub = {
    expr = rt.f32.sub 1084227584 1065353216;
    expected = 1082130432;
  };
  test_f32_sub_negative_zero_minus_positive_zero = {
    expr = rt.f32.sub 2147483648 0;
    expected = 2147483648;
  };
  test_f32_sub_negative_zero_minus_negative_zero = {
    expr = rt.f32.sub 2147483648 2147483648;
    expected = 0;
  };
  test_f32_sub_positive_zero_flips_negative_subnormal = {
    expr = rt.f32.sub 0 2147483649;
    expected = 1;
  };
  test_f32_exact_mul = {
    expr = rt.f32.mul 1073741824 1077936128;
    expected = 1086324736;
  };
  test_f32_mul_quiets_rhs_nan = {
    expr = rt.f32.mul 1065353216 2139095041;
    expected = 2143289345;
  };
  test_f32_min_signed_zero = {
    expr = rt.f32.min 0 2147483648;
    expected = 2147483648;
  };
  test_f32_max_signed_zero = {
    expr = rt.f32.max 0 2147483648;
    expected = 0;
  };
  test_f32_copysign = {
    expr = rt.f32.copysign 1065353216 2147483648;
    expected = 3212836864;
  };
  test_f64_exact_add = {
    expr = rt.f64.add 4607182418800017408 4611686018427387904;
    expected = 4613937818241073152;
  };
  test_f64_add_negative_zeros_preserves_sign = {
    expr = rt.f64.add (0 - 9223372036854775807 - 1) (0 - 9223372036854775807 - 1);
    expected = 0 - 9223372036854775807 - 1;
  };
  test_f64_add_mixed_zeros_is_positive = {
    expr = rt.f64.add (0 - 9223372036854775807 - 1) 0;
    expected = 0;
  };
  test_f64_add_negative_zero_preserves_negative_subnormal = {
    expr = rt.f64.add (0 - 9223372036854775807 - 1) (-9223372036854775807);
    expected = -9223372036854775807;
  };
  test_f64_add_negative_subnormals = {
    expr = rt.f64.add (-9223372036854775807) (-9223372036854775807);
    expected = -9223372036854775806;
  };
  test_f64_add_negative_subnormal_adjacent = {
    expr = rt.f64.add (-9223372036854775807) (-9221120237041090560);
    expected = -9221120237041090559;
  };
  test_f64_add_opposite_subnormals_cancels_positive = {
    expr = rt.f64.add 1 (-9223372036854775807);
    expected = 0;
  };
  test_f64_add_quiets_nan = {
    expr = rt.f64.add 9218868437227405313 4607182418800017408;
    expected = 9221120237041090561;
  };
  test_f64_sub_negative_zero_minus_positive_zero = {
    expr = rt.f64.sub (0 - 9223372036854775807 - 1) 0;
    expected = 0 - 9223372036854775807 - 1;
  };
  test_f64_sub_negative_zero_minus_negative_zero = {
    expr = rt.f64.sub (0 - 9223372036854775807 - 1) (0 - 9223372036854775807 - 1);
    expected = 0;
  };
  test_f64_sub_positive_zero_flips_negative_subnormal = {
    expr = rt.f64.sub 0 (-9223372036854775807);
    expected = 1;
  };
  test_f64_exact_mul = {
    expr = rt.f64.mul 4611686018427387904 4613937818241073152;
    expected = 4618441417868443648;
  };
  test_f64_mul_quiets_rhs_nan = {
    expr = rt.f64.mul 4607182418800017408 9218868437227405313;
    expected = 9221120237041090561;
  };
  test_f64_min_signed_zero = {
    expr = rt.f64.min 0 (0 - 9223372036854775807 - 1);
    expected = 0 - 9223372036854775807 - 1;
  };
  test_f64_max_signed_zero = {
    expr = rt.f64.max 0 (0 - 9223372036854775807 - 1);
    expected = 0;
  };
  test_f64_copysign = {
    expr = rt.f64.copysign 4607182418800017408 (0 - 9223372036854775807 - 1);
    expected = -4616189618054758400;
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
  test_i32_extend8_s = {
    expr = rt.conv.sign_extend_8 128;
    expected = 4294967168;
  };
  test_i32_extend16_s = {
    expr = rt.conv.sign_extend_16 32768;
    expected = 4294934528;
  };
  test_i64_extend32_s = {
    expr = rt.conv.sign_extend_32 2147483648;
    expected = -2147483648;
  };
  test_i32_trunc_f32_s = {
    expr = rt.conv.trunc_s.f32.i32 1065353216;
    expected = 1;
  };
  test_i32_trunc_f32_s_nan_traps = {
    expr = tryOk (rt.conv.trunc_s.f32.i32 2143289344);
    expected = false;
  };
  test_i32_trunc_f32_s_overflow_traps = {
    expr = tryOk (rt.conv.trunc_s.f32.i32 1325400064);
    expected = false;
  };
  test_i32_trunc_sat_f32_s_nan_is_zero = {
    expr = rt.conv.trunc_sat_s.f32.i32 2143289344;
    expected = 0;
  };
  test_i32_trunc_sat_f32_s_overflow_clamps = {
    expr = rt.conv.trunc_sat_s.f32.i32 1325400064;
    expected = 2147483647;
  };
  test_i64_trunc_f64_u_large = {
    expr = rt.conv.trunc_u.f64.i64 4846369599423283200;
    expected = 10000000000000000;
  };
  test_i64_trunc_sat_f64_u_overflow_clamps = {
    expr = rt.conv.trunc_sat_u.f64.i64 4895412794951729152;
    expected = -1;
  };
  test_f32_convert_i32_s_rounds_to_even_down = {
    expr = rt.conv.convert_s.i32.f32 16777217;
    expected = 1266679808;
  };
  test_f32_convert_i32_s_rounds_to_even_up = {
    expr = rt.conv.convert_s.i32.f32 16777219;
    expected = 1266679810;
  };
  test_f64_convert_i64_s_rounds_to_even = {
    expr = rt.conv.convert_s.i64.f64 9007199254740993;
    expected = 4845873199050653696;
  };
  test_f32_convert_i64_u_max_rounds_to_2p64 = {
    expr = rt.conv.convert_u.i64.f32 (-1);
    expected = 1602224128;
  };
  test_f64_convert_i64_u_max_rounds_to_2p64 = {
    expr = rt.conv.convert_u.i64.f64 (-1);
    expected = 4895412794951729152;
  };
  test_f64_promote_f32_one = {
    expr = rt.f64.promote_f32 1065353216;
    expected = 4607182418800017408;
  };
  test_f64_promote_f32_min_subnormal = {
    expr = rt.f64.promote_f32 1;
    expected = 3936146074321813504;
  };
  test_f32_demote_f64_one = {
    expr = rt.f32.demote_f64 4607182418800017408;
    expected = 1065353216;
  };
  test_f32_demote_f64_tiny_negative_preserves_zero_sign = {
    expr = rt.f32.demote_f64 (-9223372036854775807);
    expected = 2147483648;
  };
  test_f32_demote_f64_rounds_to_even_up = {
    expr = rt.f32.demote_f64 4607182419068452865;
    expected = 1065353217;
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
  test_mem_i64_load_s8_sign_extends_to_i64 = {
    expr =
      let
        st1 = rt.mem.store_u8 255 0 st0;
      in
      rt.mem.load_s8_i64 0 st1;
    expected = -1;
  };
  test_mem_i64_load_s16_sign_extends_to_i64 = {
    expr =
      let
        st1 = rt.mem.store_u16 65535 0 st0;
      in
      rt.mem.load_s16_i64 0 st1;
    expected = -1;
  };
  test_mem_i64_load_s32_sign_extends_to_i64 = {
    expr =
      let
        st1 = rt.mem.store_i32 4294967295 0 st0;
      in
      rt.mem.load_s32_i64 0 st1;
    expected = -1;
  };
  test_mem_store_u16_uses_low_16_bits_of_negative_i64 = {
    expr =
      let
        st1 = rt.mem.store_u16 (-1) 0 st0;
      in
      rt.mem.load_u16 0 st1;
    expected = 65535;
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
  test_mem_fill_sets_bytes = {
    expr =
      let
        st1 = rt.mem.fill 1 255 3 st0;
      in
      [
        (rt.mem.load_u8 0 st1)
        (rt.mem.load_u8 1 st1)
        (rt.mem.load_u8 2 st1)
        (rt.mem.load_u8 3 st1)
      ];
    expected = [ 0 255 255 255 ];
  };
  test_mem_copy_uses_original_source_for_overlap = {
    expr =
      let
        st1 = rt.mem.store_i32 3721182122 0 st0;
        st2 = rt.mem.copy 1 0 4 st1;
      in
      [
        (rt.mem.load_u8 1 st2)
        (rt.mem.load_u8 2 st2)
        (rt.mem.load_u8 3 st2)
        (rt.mem.load_u8 4 st2)
      ];
    expected = [ 170 187 204 221 ];
  };
  test_mem_init_reads_data_segment = {
    expr =
      let
        st1 = st0 // { data = { "0" = [ 170 187 204 ]; }; };
        st2 = rt.mem.init 0 4 1 2 st1;
      in
      [
        (rt.mem.load_u8 4 st2)
        (rt.mem.load_u8 5 st2)
      ];
    expected = [ 187 204 ];
  };
  test_data_drop_makes_segment_empty = {
    expr =
      let
        st1 = st0 // { data = { "0" = [ 1 ]; }; };
        st2 = rt.mem.data_drop 0 st1;
      in
      tryOk (rt.mem.init 0 0 0 1 st2);
    expected = false;
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
      run1 (rt.table.call 0 0 "local" funcs { } 0 [ 41 ] st1);
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
      tryOk (rt.table.call 0 0 "local" funcs { } 0 [ ] st1);
    expected = false;
  };
  test_table_get_null_ref_is_zero = {
    expr =
      let
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = { };
            };
          };
        };
      in
      rt.table.get 0 0 st1;
    expected = 0;
  };
  test_table_set_get_externref = {
    expr =
      let
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = { };
            };
          };
        };
        st2 = rt.table.set 0 0 42 st1;
      in
      rt.table.get 0 0 st2;
    expected = 42;
  };
  test_table_init_copies_element_segment = {
    expr =
      let
        funcs = {
          f0 =
            st:
            {
              inherit st;
              results = [ 42 ];
            };
        };
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = { };
            };
          };
          elemSegments = {
            "0" = [
              {
                func = 0;
                type = 0;
              }
            ];
          };
        };
        st2 = rt.table.init 0 0 0 0 1 st1;
      in
      run1 (rt.table.call 0 0 "local" funcs { } 0 [ ] st2);
    expected = 42;
  };
  test_elem_drop_makes_segment_empty = {
    expr =
      let
        st1 = st0 // {
          tables = {
            "0" = {
              size = 1;
              elems = { };
            };
          };
          elemSegments = {
            "0" = [
              {
                func = 0;
                type = 0;
              }
            ];
          };
        };
        st2 = rt.table.elem_drop 0 st1;
      in
      tryOk (rt.table.init 0 0 0 0 1 st2);
    expected = false;
  };
}
