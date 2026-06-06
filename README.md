# wasm2nix

`wasm2nix` is a small experiment that translates WebAssembly modules into Nix.

The goal is simple: give it a `.wasm` file, get back a `.nix` file that can be imported and evaluated by Nix. The generated file uses a small runtime file, so the CLI normally writes both:

- `module.nix`
- `module-rt.nix`

> [!CAUTION]
> This is not a polished compiler, package manager, or production runtime. Most of the codebase is vibecoded, so anything can happen. It is full of land mines, dragons, and dinosaurs with laser guns. If it works for your case, great. If it does something strange, that is also part of the current contract.

## What is in here

- `crates/cli`: the `wasm2nix` command line tool
- `crates/lib`: the Wasm-to-Nix lowering code
- `crates/runtime`: the Nix runtime used by generated modules
- `crates/nix`: a small Nix AST and formatter
- `crates/wast`: spec-test harness support
- `example`: a tiny Rust-to-Wasm example that converts YAML to JSON from Nix without IFD (import from derivation)

## Requirements

You probably want Nix and Rust installed. The flake provides a development shell with the Rust toolchain and the `wasm32-unknown-unknown` target.

```sh
nix develop
```

If you are not using the flake, make sure your Rust toolchain can build this workspace and, for the example, can compile to `wasm32-unknown-unknown`.

## Usage

Build the CLI:

```sh
cargo build -p wasm2nix-cli
# or use nix shell
nix shell "github:mnixry/wasm2nix"
```

Translate a Wasm module:

```sh
wasm2nix-cli -- path/to/module.wasm -o path/to/module.nix
```

By default this also writes a runtime file next to the generated Nix file. For the command above, that would be:

```text
path/to/module-rt.nix
```

Then import the generated module from Nix:

```nix
let
  wasm = import ./path/to/module.nix { };
  instance = wasm.instantiate { };
in
instance
```

The exact functions you call after that depend on the exports in your Wasm module.

Useful CLI flags:

```sh
wasm2nix input.wasm
wasm2nix input.wasm -o output.nix
wasm2nix input.wasm --no-export-rt
wasm2nix input.wasm --export-rt-path ./shared-rt.nix
wasm2nix input.wasm --imports imports.json
wasm2nix input.wasm --line-width 100
wasm2nix input.wasm --compact
```

`--imports` validates the Wasm imports before writing the output. The manifest is a JSON object grouped by import module:

```json
{
  "env": {
    "funcs": {
      "log": {
        "params": ["i32"],
        "results": []
      }
    },
    "memories": {
      "memory": {
        "min": 1
      }
    }
  }
}
```

## Example

The repository includes an example that builds a Rust Wasm module, translates it with `wasm2nix`, and evaluates it from Nix.

From the repository root:

```sh
./example/build.sh
```

If everything works, it prints:

```json
{"enabled":true,"items":[1,2],"name":"wasm2nix"}
```

You can also call the generated wrapper yourself:

```sh
nix eval --raw --impure --expr '
  let example = import ./example/default.nix { };
  in example.yamlToJson "name: wasm2nix\nenabled: true\n"
'
```

Expected output:

```json
{"enabled":true,"name":"wasm2nix"}
```

## Development

Run the Rust tests:

```sh
cargo test
```

Run the flake checks if you want the Nix-backed test matrix too:

```sh
nix flake check
```

Expect rough edges. Generated Nix can be large, slow, and surprising. Some Wasm features may be incomplete, broken, and harmful to your computer RAM ;).

## License

MIT. See [LICENSE](./LICENSE).
