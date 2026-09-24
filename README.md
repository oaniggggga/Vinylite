# betterpilot

Recovery-first JVM classfile decompiler in Rust (CFR/Vineflower direction).

What it does today:

- tolerant classfile parsing (partial constant pool, diagnostics with severity + offsets);
- full bytecode decoding incl. `tableswitch`/`lookupswitch`/`wide`, malformed-input recovery;
- forward type inference seeded from descriptors, `LocalVariableTable`,
  `LocalVariableTypeTable` and `StackMapTable` frames;
- generic `Signature` attributes for methods, fields and superclasses
  (`List<String>`, `Enum<Xenon>`);
- readable output: LVT-accurate locals, boolean simplification
  (`flag != 0` → `flag`, `x = 1` → `x = true`), constant folding,
  lambda inlining, enum detection, inner-class merging;
- switch reconstruction (`tableswitch`/`lookupswitch` with multi-label
  cases, fallthrough preservation, per-arm join-value resolution);
- `StringConcatFactory` folding back to `a + b` chains;
- dead-code tolerance: unreachable traps are stack-sandboxed (never
  corrupt live values), dead noise pruned, informative dead code kept;
- CLI for single `.class` files and whole `.jar`/`.zip` archives.

Run (MSVC toolchain required on Windows, e.g. via `vcvars64.bat`):

```powershell
cargo test -p betterpilot-core
cargo run -p betterpilot-cli -- path\to\Example.class
cargo run -p betterpilot-cli -- app.jar -o out/
```
