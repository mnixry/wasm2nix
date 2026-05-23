{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };
  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in
    {
      tests = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        import ./nix/tests.nix { inherit pkgs; }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          failed = import ./nix/tests.nix { inherit pkgs; };
        in
        {
          runTests = pkgs.runCommand "wasm2nix-runTests" { FAILED = builtins.toJSON failed; } ''
            if [ "$FAILED" != "[]" ]; then
              echo "wasm2nix tests failed:" >&2
              echo "$FAILED" >&2
              exit 1
            fi
            touch "$out"
          '';
        }
      );
    };
}
