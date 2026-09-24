//! Golden end-to-end tests: decompile a real classfile fixture and
//! verify the rendered source stays stable. These catch regressions that
//! unit tests on synthetic bytecode miss (emit ordering, statement
//! splicing, renderer drift).

use vinylite_core::decompile_class;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read fixture {name} at {path}: {e}"))
}

#[test]
fn simple_test_class_decompiles_to_expected_source() {
    let bytes = fixture("SimpleTest.class");
    let source = decompile_class(&bytes);

    // Structural anchors the whole pipeline must preserve.
    for anchor in [
        "public class SimpleTest",
        "StringBuilder var_1 = new StringBuilder()",
        "var_1.append(\"test:\")",
        "for (int var_2 = 0; var_2 < 5; var_2++)",
        "if ((var_2 % 2) != 0)",
        "var_1.append(\"odd\").append(var_2)",
        "var_1.append(\"even\").append(var_2)",
        "System.out.println(var_1.toString())",
    ] {
        assert!(
            source.contains(anchor),
            "golden anchor missing: {anchor}\n--- source ---\n{source}"
        );
    }

    // The loop body must be non-empty: if/else lives inside the for.
    let for_start = source.find("for (").expect("for present");
    let loop_body = &source[for_start..];
    assert!(
        loop_body.contains("odd") && loop_body.contains("even"),
        "loop body lost the if/else branches:\n{loop_body}"
    );
}

#[test]
fn truncated_classfile_still_renders_recovery_stub() {
    // Recovery-first contract: garbage input must not panic or hang.
    let bytes = fixture("SimpleTest.class");
    for cut in [0usize, 1, 4, 8, 10, 20] {
        let source = decompile_class(&bytes[..cut.min(bytes.len())]);
        // Either a recovery stub or a partial render, but never a panic.
        assert!(
            source.contains("//") || source.contains("class"),
            "cut {cut}: unexpected output {source}"
        );
    }
}
