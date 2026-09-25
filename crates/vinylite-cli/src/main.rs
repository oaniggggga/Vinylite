use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use vinylite_core::{
    Classpath, build_class_decl_with_classpath, decompile_class_with_classpath, deobfuscate_name,
    inspect_class, parse_jar,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Platform path separator for `--classpath`, java-style.
#[cfg(windows)]
const CP_SEP: char = ';';
#[cfg(not(windows))]
const CP_SEP: char = ':';

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    classpath: Vec<PathBuf>,
}

fn print_help() {
    println!(
        "\
vinylite {VERSION}
Recovery-first JVM classfile decompiler (zero dependencies)

USAGE:
    vinylite <INPUT> [-o <PATH>] [-p <PATH>...]

ARGS:
    <INPUT>    .class file, .jar/.zip archive

OPTIONS:
    -o, --output <PATH>       Output file (.jar/.zip) or directory
    -p, --classpath <PATH>    Hierarchy lookup path (dirs/jars, java-style
                              separators, repeatable) for @Override and future
                              hierarchy-aware passes. The decompiled archive
                              itself is always indexed too
    -h, --help                Print this help
    -V, --version             Print version"
    );
}

fn parse_args() -> Option<Args> {
    let mut raw = std::env::args().skip(1).peekable();
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut classpath: Vec<PathBuf> = Vec::new();

    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("vinylite {VERSION}");
                std::process::exit(0);
            }
            "-o" | "--output" => {
                let Some(val) = raw.next() else {
                    eprintln!("fatal: --output requires a PATH");
                    std::process::exit(2);
                };
                output = Some(PathBuf::from(val));
            }
            "-p" | "--classpath" | "--class-path" => {
                let Some(val) = raw.next() else {
                    eprintln!("fatal: --classpath requires a PATH");
                    std::process::exit(2);
                };
                classpath.extend(val.split(CP_SEP).map(PathBuf::from));
            }
            s if s.starts_with('-') => {
                eprintln!("fatal: unknown flag {s} (see --help)");
                std::process::exit(2);
            }
            s => {
                if input.is_some() {
                    eprintln!("fatal: unexpected extra argument {s} (see --help)");
                    std::process::exit(2);
                }
                input = Some(PathBuf::from(s));
            }
        }
    }

    match input {
        Some(input) => Some(Args {
            input,
            output,
            classpath,
        }),
        None => {
            print_help();
            std::process::exit(2);
        }
    }
}

fn main() {
    let Some(args) = parse_args() else { return };
    let bytes = match fs::read(&args.input) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("fatal: cannot read {}: {error}", args.input.display());
            std::process::exit(2);
        }
    };

    let mut classpath = Classpath::new();
    for entry in &args.classpath {
        if entry.is_dir() {
            classpath.add_dir(entry.clone());
        } else {
            match fs::read(entry) {
                Ok(jar_bytes) => classpath.add_archive_bytes(jar_bytes),
                Err(error) => {
                    eprintln!(
                        "warning: cannot read --classpath entry {}: {error}",
                        entry.display()
                    );
                }
            }
        }
    }

    let input_str = args.input.display().to_string();
    if input_str.ends_with(".jar") || input_str.ends_with(".zip") {
        process_jar(&bytes, &args.output, &args.input, &mut classpath);
    } else {
        process_class(&bytes, &args.output, &mut classpath);
    }
}

fn print_diagnostics(diagnostics: &[vinylite_core::Diagnostic]) {
    let mut seen = std::collections::HashSet::new();
    let mut suppressed = 0u32;
    for diagnostic in diagnostics {
        if !seen.insert(diagnostic.message.clone()) {
            suppressed += 1;
            continue;
        }
        println!(
            "// {} @ {}: {}",
            diagnostic.severity, diagnostic.offset, diagnostic.message
        );
    }
    if suppressed > 0 {
        println!("// ... {suppressed} duplicate diagnostics suppressed");
    }
}

fn process_class(bytes: &[u8], output: &Option<PathBuf>, classpath: &mut Classpath) {
    let report = inspect_class(bytes);
    let source = if let Some(class) = &report.class {
        let class_decl = build_class_decl_with_classpath(class, Some(classpath));
        class_decl.render()
    } else {
        "// class: unrecoverable\n".to_string()
    };

    match output {
        Some(path) => {
            fs::write(path, &source).unwrap_or_else(|e| {
                eprintln!("fatal: cannot write {}: {e}", path.display());
                std::process::exit(2);
            });
            println!("wrote {}", path.display());
        }
        None => {
            print!("{source}");
            print_diagnostics(&report.diagnostics);
        }
    }
}

fn process_jar(bytes: &[u8], output: &Option<PathBuf>, input: &Path, classpath: &mut Classpath) {
    let entries = parse_jar(bytes);

    // The archive being decompiled doubles as its own classpath, so
    // same-jar supertypes resolve with no extra flags.
    for entry in &entries {
        if let Some(class_bytes) = &entry.class_bytes {
            classpath.add_memory_class(entry.name.clone(), class_bytes.clone());
        }
    }

    // Recovery-first at the JAR level too: one poisoned class must not take
    // down the whole archive. A panicking decompile degrades to a stub and
    // the run continues; the panic is reported on stderr.
    let mut class_sources: HashMap<String, String> = HashMap::new();
    let mut failed_classes: usize = 0;
    for entry in &entries {
        if let Some(class_bytes) = &entry.class_bytes {
            let source = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                decompile_class_with_classpath(class_bytes, &mut *classpath)
            }))
            .unwrap_or_else(|panic| {
                failed_classes += 1;
                let detail = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                eprintln!("warning: panicked on {}: {detail}", entry.name);
                "// unrecoverable: decompiler panicked on this class\n".to_string()
            });
            let internal_name = entry.name.replace(".class", "");
            class_sources.insert(internal_name, source.trim_end().to_string());
        }
    }
    if failed_classes > 0 {
        eprintln!("warning: {failed_classes} class(es) failed and were stubbed");
    }

    let mut rendered: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let source_text = if let Some(source) = class_sources.get(&entry.name.replace(".class", ""))
        {
            if let Some(class) = &entry.class {
                let mut result = source.clone();
                let outer_internal = entry.name.replace(".class", "");
                for ic in &class.inner_classes {
                    let full_internal = get_inner_class_full_name(class, ic.inner_class_index);
                    let simple_name = get_inner_class_name(class, ic.inner_name_index);

                    if let Some(simple) = &simple_name {
                        let full_name = if let Some(ref full) = full_internal {
                            full.clone()
                        } else {
                            format!("{}${}", outer_internal, simple)
                        };

                        if let Some(inner_source) = class_sources.get(&full_name)
                            && let Some(body_start) = inner_source.find('{')
                        {
                            let body = inner_source[body_start..].trim_end_matches('}').trim();
                            for kind in &["class", "enum", "interface"] {
                                let stub = format!("{} {} {{}}", kind, simple);
                                if result.contains(&stub) {
                                    result = result
                                        .replace(&stub, &format!("{} {} {}", kind, simple, body));
                                    break;
                                }
                            }
                        }
                    }
                }
                result
            } else {
                source.clone()
            }
        } else if let Some(class) = &entry.class {
            let inferred_name = class
                .constant_pool
                .iter()
                .skip(1)
                .find_map(|cp_entry| match cp_entry {
                    vinylite_core::recovery::Recoverable::Present(
                        vinylite_core::classfile::ConstantPoolEntry::Class { name_index },
                    ) => get_utf8_from_pool(&class.constant_pool, *name_index),
                    _ => None,
                })
                .unwrap_or_else(|| entry.name.replace('/', ".").replace(".class", ""));

            let class_name = deobfuscate_name(&inferred_name);
            format!(
                "{}\npublic class {} {{\n    // unrecoverable class bytes, fallback rendering only\n}}",
                vinylite_core::watermark(),
                class_name
            )
        } else {
            "// class: unrecoverable".to_string()
        };

        let java_path = entry.name.replace(".class", ".java");
        rendered.push((java_path, source_text));
    }

    match output {
        Some(output_path) => {
            let ext = output_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            match ext {
                "jar" | "zip" => {
                    write_zip(output_path, &rendered);
                    println!(
                        "wrote {} entries to {}",
                        rendered.len(),
                        output_path.display()
                    );
                }
                _ => {
                    write_folder(output_path, &rendered);
                    println!(
                        "wrote {} entries to {}",
                        rendered.len(),
                        output_path.display()
                    );
                }
            }
        }
        None => {
            let folder_name = input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output");
            let out_dir = PathBuf::from(format!("{}_decompiled", folder_name));
            write_folder(&out_dir, &rendered);
            println!("wrote {} entries to {}", rendered.len(), out_dir.display());
        }
    }
}

/// Dependency-free ZIP writer (Stored only — source text is already compact;
/// jars of `.java` output stay small without a deflate encoder).
fn write_zip(path: &Path, files: &[(String, String)]) {
    let mut out: Vec<u8> = Vec::new();
    // (name, crc, size, local_header_offset)
    let mut central: Vec<(String, u32, u32, u32)> = Vec::new();

    for (name, content) in files {
        let bytes = content.as_bytes();
        let crc = vinylite_core::zipmini::crc32(bytes);
        let size = bytes.len() as u32;
        let local_off = out.len() as u32;
        write_local_header(&mut out, name, crc, size);
        out.extend_from_slice(bytes);
        central.push((name.clone(), crc, size, local_off));
    }

    let cd_start = out.len() as u32;
    for (name, crc, size, local_off) in &central {
        write_central_entry(&mut out, name, *crc, *size, *local_off);
    }
    let cd_size = out.len() as u32 - cd_start;
    write_eocd(&mut out, central.len() as u16, cd_size, cd_start);

    fs::write(path, &out).unwrap_or_else(|e| {
        eprintln!("fatal: cannot write {}: {e}", path.display());
        std::process::exit(2);
    });
}

fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_local_header(out: &mut Vec<u8>, name: &str, crc: u32, size: u32) {
    write_u32(out, 0x0403_4b50);
    write_u16(out, 20); // version needed
    write_u16(out, 0x0800); // UTF-8 flag
    write_u16(out, 0); // stored
    write_u16(out, 0); // time
    write_u16(out, 0); // date
    write_u32(out, crc);
    write_u32(out, size);
    write_u32(out, size);
    write_u16(out, name.len() as u16);
    write_u16(out, 0); // extra
    out.extend_from_slice(name.as_bytes());
}

fn write_central_entry(out: &mut Vec<u8>, name: &str, crc: u32, size: u32, local_off: u32) {
    write_u32(out, 0x0201_4b50);
    write_u16(out, 20); // version made by
    write_u16(out, 20); // version needed
    write_u16(out, 0x0800); // UTF-8
    write_u16(out, 0); // stored
    write_u16(out, 0);
    write_u16(out, 0);
    write_u32(out, crc);
    write_u32(out, size);
    write_u32(out, size);
    write_u16(out, name.len() as u16);
    write_u16(out, 0);
    write_u16(out, 0);
    write_u16(out, 0);
    write_u16(out, 0);
    write_u32(out, 0);
    write_u32(out, local_off);
    out.extend_from_slice(name.as_bytes());
}

fn write_eocd(out: &mut Vec<u8>, count: u16, cd_size: u32, cd_start: u32) {
    write_u32(out, 0x0605_4b50);
    write_u16(out, 0);
    write_u16(out, 0);
    write_u16(out, count);
    write_u16(out, count);
    write_u32(out, cd_size);
    write_u32(out, cd_start);
    write_u16(out, 0);
}

fn write_folder(dir: &Path, files: &[(String, String)]) {
    fs::create_dir_all(dir).unwrap_or_else(|e| {
        eprintln!("fatal: cannot create directory {}: {e}", dir.display());
        std::process::exit(2);
    });

    for (name, content) in files {
        let file_path = dir.join(name);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("fatal: cannot create directory {}: {e}", parent.display());
                std::process::exit(2);
            });
        }
        fs::write(&file_path, content).unwrap_or_else(|e| {
            eprintln!("fatal: cannot write {}: {e}", file_path.display());
            std::process::exit(2);
        });
    }
}

fn get_inner_class_full_name(class: &vinylite_core::ClassFile, class_index: u16) -> Option<String> {
    use vinylite_core::classfile::ConstantPoolEntry;
    use vinylite_core::recovery::Recoverable;
    match class.constant_pool.get(class_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            match class.constant_pool.get(*name_index as usize) {
                Some(Recoverable::Present(ConstantPoolEntry::Utf8(name))) => Some(name.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn get_inner_class_name(class: &vinylite_core::ClassFile, name_index: u16) -> Option<String> {
    use vinylite_core::classfile::ConstantPoolEntry;
    use vinylite_core::recovery::Recoverable;
    match class.constant_pool.get(name_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(name))) => Some(name.clone()),
        _ => None,
    }
}

fn get_utf8_from_pool(
    pool: &[vinylite_core::recovery::Recoverable<vinylite_core::classfile::ConstantPoolEntry>],
    index: u16,
) -> Option<String> {
    match pool.get(index as usize) {
        Some(vinylite_core::recovery::Recoverable::Present(
            vinylite_core::classfile::ConstantPoolEntry::Utf8(s),
        )) => Some(s.clone()),
        _ => None,
    }
}
