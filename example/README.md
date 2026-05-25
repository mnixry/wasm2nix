# yaml-to-json wasm2nix example

This example builds a small Rust Wasm module that parses YAML with Saphyr and
emits JSON, then translates that module to Nix with the repository's `wasm2nix`
CLI.

Run it from the repository root:

```sh
./example/build.sh
```

After the build, use the generated Nix wrapper:

```sh
nix eval --raw --impure --expr '
  let example = import ./example/default.nix { };
  in example.yamlToJson "name: wasm2nix\nenabled: true\n"
'
```

The Nix wrapper accepts ASCII input and returns ASCII JSON. The Rust module
builds a `serde_json::Value`, serializes it with `serde_json`, then escapes
non-ASCII JSON output as `\uXXXX` sequences.
