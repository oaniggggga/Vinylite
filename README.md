# Vinylite

Recovery-first JVM classfile decompiler in Rust — in the spirit of CFR and Vineflower.

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
- ternary reconstruction (`cond ? a : b` from if/else assignments, paired
  returns, and early-return-with-fallthrough `if (c) { return a; } return b;`).
  Deliberately flat-only: folds that would nest (`a ? b : (c ? d : e)`)
  stay as explicit if/else for readability;
- try/catch/finally reconstruction incl. multi-catch and
  try-with-resources (suppression-chain folding);
- switch reconstruction (`tableswitch`/`lookupswitch` with multi-label
  cases, fallthrough preservation, per-arm join-value resolution);
- `StringConcatFactory` folding back to `a + b` chains;
- counted-loop canonicalization: `init; while (i < n) { ...; i++ }` ->
  `for (int i = 0; i < n; i++)` with compound assignments (`i++`, `x += n`);
- CFR-style else rendering: `} else {` on one line, `else if` chains;
- discarded method calls kept as expression statements (`sb.append("x");`);
- non-boolean conditions restored to valid Java (`if (i % 2 != 0)`),
  incl. reference comparisons (`in.peek() == JsonToken.NULL`);
- `synchronized` blocks reconstructed from monitorenter/monitorexit
  pairing, with correct `final`/`synchronized`/`native`/`abstract`
  modifiers; bridge/synthetic methods hidden like CFR/Vineflower do;
- annotations: `RuntimeVisible/InvisibleAnnotations` rendered
  (`@Deprecated`, `@FunctionalInterface`, `@SerializedName`-style custom
  ones with imports), `@Override` via hierarchy: the decompiled archive
  is indexed automatically, plus java-style `-p/--classpath` for dirs
  and jars (commons-lang3: 373 `@Override` vs CFR's 505; the rest needs
  JDK supertypes, which the runtime-module format doesn't expose);
- dead-code tolerance: unreachable traps are stack-sandboxed (never
  corrupt live values), dead noise pruned, informative dead code kept;
- `LineNumberTable` parsing (statement ordering / diagnostics groundwork);
- CLI for single `.class` files and whole `.jar`/`.zip` archives.

## Zero dependencies

Vinylite is fully self-contained: no third-party crates, no C compiler,
no JVM at runtime. Argument parsing, error types, ZIP reading (Stored +
Deflate via a built-in RFC1951 inflate) and ZIP writing are all
hand-rolled in pure Rust — `cargo build` works offline from a clean
registry cache.

## Install

Grab a prebuilt binary from [Releases](https://github.com/oaniggggga/vinylite/releases)
(Linux / macOS x64 + ARM64 / Windows), or build from source:

```powershell
cargo install --git https://github.com/oaniggggga/vinylite
```

## Example

Input bytecode of a trivial loop with an if/else inside (from `javac`):

```java
// Decompiled by Vinylite v0.3.0 (https://github.com/oaniggggga/vinylite)
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

A stable Rust toolchain is enough (no C compiler, no external crates).

```powershell
cargo test --workspace
cargo run -p vinylite-cli -- path\to\Example.class
cargo run -p vinylite-cli -- app.jar -o out/
cargo run -p vinylite-cli -- app.jar -o out/ -p "lib\dep1.jar;lib\dep2.jar"
cargo run -p vinylite-cli -- --help
```

Single class prints to stdout; `-o` writes a file, directory, or archive
(extension decides). Jars are decompiled class-by-class with inner-class
re-inlining; unrecoverable entries get a fallback stub.

## Benchmarks

Wall-clock time to decompile complete jars (Windows x64, release build,
single run; CFR 0.152 with a warmed JVM — its cost includes JVM startup
and JIT that Vinylite does not pay):

| Jar | Classes | Vinylite | CFR 0.152 | Speedup |
|---|---|---|---|---|
| gson 2.11.0 | 224 | **0.20 s** | 1.59 s | 7.8x |
| commons-lang3 3.14.0 | 404 | **0.53 s** | 3.75 s | 7.1x |
| commons-io 2.15.1 | 339 | **0.32 s** | 2.39 s | 7.1x |

Vinylite emits one `.java` per classfile including every nested/anonymous
class; CFR inlines most anonymous classes into their parent and skips
`module-info`, so file counts differ by design.

Known output gaps vs CFR (measured on the same jars): guarded resource
close with conditional `addSuppressed` outside the classic javac shapes
stays expanded instead of folding into `try (...)` — 9 leftover
suppression calls on commons-io vs CFR's 6 (3 vs 2 on
commons-lang3); enum constant bodies with method overrides also render
better in CFR. Speed and robustness (zero panics across
the corpora, per-class panic isolation) are Vinylite's current strengths.
Try/catch coverage on commons-lang3: 77 try + 7 try-with-resources
blocks vs CFR's 89 + 10 (on commons-io: 33 TWR vs 41); multi-catch,
finally and single-resource TWR shapes are reconstructed canonically.
boolean-as-int returns are re-typed from the declared return type
(`return 0;` in `()Z` renders `return false;`, incl. ternary arms);
boolean locals/args rely on usage heuristics.

## Robustness

Measured where recovery-first matters, not just on clean libraries:

- **Big jars** (time / peak RSS, Windows x64 release): Guava (2018
  classes) 1.95 s / 77 MB vs CFR 9.00 s / 806 MB; Spring-core (1183
  classes) 1.16 s / 55 MB vs 7.35 s / 650 MB; a 102 MB protected client
  jar (1220 classes) 2.14 s / 236 MB vs 16.48 s / 926 MB. Zero panics,
  zero stubs on all three.
- **Corrupted input** (60 commons-io classes × 10 damage operators:
  truncation, byte flips in pool/code zones, zeroed headers): Vinylite
  always emits output — full render or named fallback stub with
  diagnostics; CFR goes silent on 50–100% of damaged classes.
  Full-render rate: flip1 82% vs 50%, flip5 42% vs 15%, flip20 7% vs 2%.
  A try/catch cycle that overflowed the stack on adversarial tables is
  guarded by a visited-set on ranges.
- **Obfuscated input** (gson via ProGuard 7.10 renaming, `A`/`a` twins):
  223/223 files via case-collision disambiguation with type renames
  (no silent overwrites), 0 panics, `synchronized` 7 vs 10,
  `@Override` 205 vs 284 (the rest needs JDK supertypes on the
  classpath).

## Development

```powershell
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
```

CI runs fmt check, clippy `-D warnings`, tests and a release build on every push.

## License

MIT
