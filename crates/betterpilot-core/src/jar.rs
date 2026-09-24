use crate::classfile::ClassFile;
use crate::diagnostic::Diagnostic;
use std::io::Read;

pub struct JarEntry {
    pub name: String,
    pub class: Option<ClassFile>,
    pub diagnostics: Vec<Diagnostic>,
    pub class_bytes: Option<Vec<u8>>,
}

pub fn parse_jar(bytes: &[u8]) -> Vec<JarEntry> {
    let mut entries = Vec::new();

    let mut reader = match zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
        Ok(r) => r,
        Err(_) => return entries,
    };

    for i in 0..reader.len() {
        let mut entry = match reader.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = match entry.enclosed_name() {
            Some(n) => n,
            None => continue,
        };

        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        if !name_str.ends_with(".class") {
            continue;
        }

        let mut class_bytes = Vec::new();
        if entry.read_to_end(&mut class_bytes).is_err() {
            entries.push(JarEntry {
                name: name_str,
                class: None,
                diagnostics: vec![Diagnostic::error(0, "failed to read class bytes from JAR")],
                class_bytes: None,
            });
            continue;
        }

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
