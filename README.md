# wasm2nix

`wasm2nix` translates a WebAssembly module into a Nix expression. You give it a `.wasm` file and get back a `.nix` file that any Nix evaluator can import and call, with no builds and no import-from-derivation. This lets you run code written in Rust, C, Zig or any other language that targets Wasm inside a Nix evaluation: parsers, encoders, validators, small interpreters.

The generated code passes the WebAssembly core spec test suite on Nix, Lix, Determinate Nix and [iets](https://github.com/numtide/iets).

## Installation

Run the CLI straight from the flake:

```sh
nix run github:mnixry/wasm2nix -- module.wasm
```

Or install it into your profile, or open a shell with it:

```sh
nix profile install github:mnixry/wasm2nix
nix shell github:mnixry/wasm2nix
```

## Quick start

Translate a module:

```sh
wasm2nix path/to/module.wasm
```

This writes two files next to the input:

- `path/to/module.nix`, the translated module.
- `path/to/module-rt.nix`, the runtime it needs.

Keep the two files together. Then call an export from Nix:

```nix
let
  wasm = import ./path/to/module.nix { };
  instance = wasm.instantiate { };
  result = wasm.add 2 3 instance.st;
in
builtins.head result.results
```

## Using a generated module

### State

Nix values are immutable, so a Wasm instance is a value too. `wasm.instantiate { }` returns `{ st; exports; }`, where `st` is the initial instance state.

Every exported function takes its Wasm arguments, then the state, and returns the results with the new state:

```nix
let
  instance = wasm.instantiate { };
  first = wasm.add 2 3 instance.st;    # { results = [ 5 ]; st = …; }
  second = wasm.add 40 2 first.st;     # continue from the state `first` left behind
in
second.results                         # [ 42 ]
```

Pass the returned `st` to the next call to keep memory and global changes.

Exports whose names are not Nix identifiers use string attributes, such as `wasm."my-func"`.

### Values

Wasm values are Nix integers:

| Wasm type | Nix value                                                                                                                  |
| --------- | -------------------------------------------------------------------------------------------------------------------------- |
| `i32`     | integer in `0 … 4294967295`. A negative number is passed as its two's-complement value; for example, `-1` is `4294967295`. |
| `i64`     | signed 64-bit integer.                                                                                                     |
| `f32`     | the IEEE 754 bit pattern, as an unsigned 32-bit integer.                                                                   |
| `f64`     | the IEEE 754 bit pattern, as a signed 64-bit integer.                                                                      |

### Memory and globals

An exported memory reads and writes bytes through the instance state:

```nix
bytes = wasm.memory.load_bytes 0 4 st;             # list of 4 integers
st' = wasm.memory.store_bytes [ 104 105 ] 1024 st; # writes "hi" at address 1024
```

An exported global has `get` and `set`:

```nix
count = wasm.count.get st;
st' = wasm.count.set 7 st;
```

### Imports

Pass the module's imports as `imports.<module>.<field>` when you import the file.

A host function receives its Wasm arguments and one extra argument it can ignore, and returns its results as a list:

```nix
wasm = import ./module.nix {
  imports.env.log = value: _: builtins.trace "log: ${toString value}" { results = [ ]; };
};
```

An imported global is its value, such as `imports.env.base = 1024;`.

### Traps

A Wasm trap, such as out-of-bounds access, division by zero or `unreachable`, becomes a Nix `throw` that names the trap, for example `wasm trap: integer divide by zero`. `builtins.tryEval` catches it.

## Command-line reference

```text
wasm2nix [OPTIONS] <INPUT>
```

| Option                    | Effect                                                                                                      |
| ------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `-o, --output <PATH>`     | Write the module here. Defaults to the input path with a `.nix` extension.                                  |
| `--no-export-rt`          | Do not write the runtime file, for example when several modules share one.                                  |
| `--export-rt-path <PATH>` | Write the runtime here and import it from here. A relative path is relative to the output file's directory. |
| `--imports <PATH>`        | Check the module's imports against a JSON manifest before writing anything.                                 |
| `--line-width <COLUMNS>`  | Wrap the generated Nix at this width. Defaults to 100.                                                      |
| `--compact`               | Emit compact Nix without line breaks or the generated header comment.                                       |

### Import manifests

`--imports` takes a JSON object keyed by import module name. Each module lists the functions, globals, memories and tables it provides:

```json
{
  "env": {
    "funcs": {
      "log": { "params": ["i32"], "results": [] }
    },
    "globals": {
      "base": { "type": "i32", "mutable": false }
    },
    "memories": {
      "memory": { "min": 1, "max": 16 }
    },
    "tables": {
      "table": { "type": "funcref", "min": 4 }
    }
  }
}
```

The CLI fails when an import is missing or its type or limits do not match, so linking mistakes show up before evaluation.

## Supported WebAssembly

wasm2nix supports [WebAssembly 1.0](https://www.w3.org/TR/wasm-core-1/) and these proposals:

- [mutable globals](https://github.com/WebAssembly/mutable-global)
- [sign-extension operators](https://github.com/WebAssembly/sign-extension-ops)
- [multi-value](https://github.com/WebAssembly/multi-value)
- [reference types](https://github.com/WebAssembly/reference-types)
- [non-trapping float-to-int conversions](https://github.com/WebAssembly/nontrapping-float-to-int-conversions)
- [bulk memory operations](https://github.com/WebAssembly/bulk-memory-operations)

Modules that use SIMD, threads, 64-bit or multiple memories, tail calls, exceptions or garbage collection fail to translate. Active data or element segments whose offset reads a global are not applied yet.

For Rust, build for `wasm32-unknown-unknown`.

Generated Nix is large and evaluates much slower than native code; the example's 270 KB Wasm module becomes about 4 MB of Nix. Use wasm2nix for work small enough to finish during evaluation, such as parsing a config format or validating input.

## Example

[`example`](./example) is a Rust crate that parses YAML and prints JSON, wrapped by `example/default.nix` as a `yamlToJson` function. From the repository root, inside `nix develop`:

```sh
./example/build.sh
```

The script builds the crate, translates it, and evaluates a sample document:

```json
{ "enabled": true, "items": [1, 2], "name": "wasm2nix" }
```

Then call the wrapper yourself:

```sh
nix eval --raw --impure --expr '
  let example = import ./example/default.nix { };
  in example.yamlToJson "name: wasm2nix\nenabled: true\n"
'
```

`example/default.nix` also shows how to pass strings in and out through linear memory.

## Development

Enter the development shell and fetch the spec tests:

```sh
nix develop
git submodule update --init
```

Run the tests, format, and lint:

```sh
cargo nextest run --workspace
nix fmt
cargo clippy --workspace --all-targets -- -D warnings
```

`nix flake check` runs the same checks as CI, including the spec tests on every supported evaluator.

## License

MIT. See [LICENSE](./LICENSE).
