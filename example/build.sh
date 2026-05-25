#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
generated_dir="$script_dir/generated"
wasm_path="$repo_root/target/wasm32-unknown-unknown/release/yaml_to_json.wasm"

cargo build --manifest-path "$repo_root/Cargo.toml" -p wasm2nix-cli
cargo build --manifest-path "$script_dir/Cargo.toml" --release --target wasm32-unknown-unknown

if [[ -d "$generated_dir" ]]; then
  rm -rf "$generated_dir"
fi
mkdir -p "$generated_dir"
"$repo_root/target/debug/wasm2nix" "$wasm_path" -o "$generated_dir/yaml_to_json.nix"

cd "$repo_root"
result="$(
  nix eval --raw --impure --expr '
    let
      example = import ./example/default.nix { };
    in
    example.yamlToJson "name: wasm2nix\nenabled: true\nitems:\n  - 1\n  - 2\n"
  '
)"
expected='{"enabled":true,"items":[1,2],"name":"wasm2nix"}'

if [[ "$result" != "$expected" ]]; then
  printf 'unexpected yamlToJson result\nexpected: %s\nactual:   %s\n' "$expected" "$result" >&2
  exit 1
fi

printf '%s\n' "$result"
