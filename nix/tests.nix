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

  ctx = {
    inherit
      rt
      st0
      run1
      tryOk
      u32Max
      pageBytes
      ;
  };

  testModules = [
    ./tests/runtime.nix
  ];

  tests = builtins.foldl' (acc: module: acc // import module ctx) { } testModules;
in
lib.debug.runTests tests
