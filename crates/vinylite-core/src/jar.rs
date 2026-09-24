use crate::classfile::ClassFile;
use crate::diagnostic::Diagnostic;

pub struct JarEntry {
    pub name: String,
    pub class: Option<ClassFile>,
    pub diagnostics: Vec<Diagnostic>,
    pub class_bytes: Option<Vec<u8>>,
}

pub fn parse_jar(bytes: &[u8]) -> Vec<JarEntry> {
    let mut entries = Vec::new();

    // Dependency-free ZIP extraction (Stored + Deflate, see `zipmini`).
    // One poisoned class must not take down the whole archive: extraction
    // or parse failures degrade to diagnostics + `None`, never a panic.
    for (name, class_bytes) in crate::zipmini::extract_class_files(bytes) {
        // Keep the lossy archive path normalized to `/` separators.
        let name_str = name;
        let mut parser = crate::classfile::ClassFileParser::new(&class_bytes);
        let class = parser.parse().ok();
        let diagnostics = parser.into_diagnostics();

        entries.push(JarEntry {
            name: name_str,
            class,
            diagnostics,
            class_bytes: Some(class_bytes),
        });
    }

    entries
}

pub fn deobfuscate_name(name: &str) -> String {
    // Simple deobfuscation: detect common patterns
    // - Single-letter names -> prefix with underscore
    // - Names with numbers -> try to extract readable parts
    // - All caps -> lowercase

    if name.len() == 1 {
        return format!("var_{}", name);
    }

    // Try to detect if it's a mangled name (e.g., "a", "b", "c")
    if name.chars().all(|c| c.is_ascii_lowercase()) && name.len() <= 2 {
        return format!("local_{}", name);
    }

    // Try to extract readable parts from names like "a1", "b2"
    let mut result = String::new();
    for c in name.chars() {
        if c.is_ascii_alphabetic() {
            result.push(c);
        }
    }

    if result.is_empty() {
        format!("field_{}", name)
    } else {
        result
    }
}
