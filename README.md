# Vinylite

Recovery-first JVM classfile decompiler in Rust ? in the spirit of CFR and Vineflower.

It turns JVM bytecode back into readable Java and is built to survive malformed,
obfuscated, and partially corrupted classfiles instead of giving up on them.

## What it does today

- tolerant classfile parsing (partial constant pool, diagnostics with severity + offsets);
- full bytecode decoding incl. `tableswitch`/`lookupswitch`/`wide`, malformed-input recovery;
- forward type inference seeded from descriptors, `LocalVariableTable`,
  `LocalVariableTypeTable` and `StackMapTable` frames;
- generic `Signature` attributes for methods, fields and superclasses
  (`List<String>`, `Enum<Xenon>`);
- readable output: LVT-accurate locals, boolean simplification
  (`flag != 0` -> `flag`, `x = 1` -> `x = true`), constant folding,
  lambda inlining, enum detection, inner-class merging;
- switch reconstruction (`tableswitch`/`lookupswitch` with multi-label
  cases, fallthrough preservation, per-arm join-value resolution);
- `StringConcatFactory` folding back to `a + b` chains;
- counted-loop canonicalization: `init; while (i < n) { ...; i++ }` ->
  `for (int i = 0; i < n; i++)` with compound assignments (`i++`, `x += n`);
- CFR-style else rendering: `} else {` on one line, `else if` chains;
- discarded method calls kept as expression statements (`sb.append("x");`);
- non-boolean conditions restored to valid Java (`if (i % 2 != 0)`);
- dead-code tolerance: unreachable traps are stack-sandboxed (never
  corrupt live values), dead noise pruned, informative dead code kept;
- CLI for single `.class` files and whole `.jar`/`.zip` archives.

## Install

Grab a prebuilt binary from [Releases](https://github.com/oaniggggga/vinylite/releases)
(Linux / macOS x64 + ARM64 / Windows), or build from source:

```powershell
cargo install --git https://github.com/oaniggggga/vinylite
```

## Example

Input bytecode of a trivial loop with an if/else inside (from `javac`):

```java
// vinylite output
public class SimpleTest {
    public static void main(String[] arg0) {
        StringBuilder var_1 = new StringBuilder();
        var_1.append("test:");
        for (int var_2 = 0; var_2 < 5; var_2++) {
            if ((var_2 % 2) != 0) {
                var_1.append("odd").append(var_2);
            } else {
                var_1.append("even").append(var_2);
            }
        }
        System.out.println(var_1.toString());
    }
}
```

Counted loops render canonically, discarded calls are kept, and non-boolean
conditions are restored to valid Java.

## Build & run

A stable Rust toolchain is enough (no C compiler needed).

```powershell
cargo test --workspace
cargo run -p vinylite-cli -- path\to\Example.class
cargo run -p vinylite-cli -- app.jar -o out/
```

Single class prints to stdout; `-o` writes a file, directory, or archive
(extension decides). Jars are decompiled class-by-class with inner-class
re-inlining; unrecoverable entries get a fallback stub.

## Known limitations

- ternary reconstruction and try-with-resources are not implemented yet;
- some complex try/catch shapes are not fully reconstructed;
- javac line-number tables are not used yet;
- `BufferedImage`-style StackMapTable types occasionally fall back to `var`.

## Development

```powershell
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
```

CI runs fmt check, clippy `-D warnings`, tests and a release build on every push.

## License

MIT
