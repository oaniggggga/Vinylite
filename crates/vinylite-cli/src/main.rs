use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use vinylite_core::{
    build_class_decl, decompile_class, deobfuscate_name, inspect_class, parse_jar,
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "vinylite")]
#[command(about = "Recovery-first JVM classfile decompiler prototype")]
struct Args {
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    #[arg(
        short,
        long,
        value_name = "PATH",
        help = "Output file (.jar/.zip) or directory"
    )]
    output: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    let bytes = match fs::read(&args.input) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("fatal: cannot read {}: {error}", args.input.display());
            std::process::exit(2);
        }
    };

    let input_str = args.input.display().to_string();
    if input_str.ends_with(".jar") || input_str.ends_with(".zip") {
        process_jar(&bytes, &args.output, &args.input);
    } else {
        process_class(&bytes, &args.output);
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

fn process_class(bytes: &[u8], output: &Option<PathBuf>) {
    let report = inspect_class(bytes);
    let source = if let Some(class) = &report.class {
        let class_decl = build_class_decl(class);
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

fn process_jar(bytes: &[u8], output: &Option<PathBuf>, input: &Path) {
    let entries = parse_jar(bytes);

    let mut class_sources: HashMap<String, String> = HashMap::new();
    for entry in &entries {
        if let Some(class_bytes) = &entry.class_bytes {
            let source = decompile_class(class_bytes);
            let internal_name = entry.name.replace(".class", "");
            class_sources.insert(internal_name, source.trim_end().to_string());
        }
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
                "public class {} {{\n    // unrecoverable class bytes, fallback rendering only\n}}",
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

fn write_zip(path: &Path, files: &[(String, String)]) {
    let file = fs::File::create(path).unwrap_or_else(|e| {
        eprintln!("fatal: cannot create {}: {e}", path.display());
        std::process::exit(2);
    });
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    for (name, content) in files {
        zip.start_file(name, options).unwrap_or_else(|e| {
            eprintln!("fatal: cannot write entry {}: {e}", name);
            std::process::exit(2);
        });
        zip.write_all(content.as_bytes()).unwrap_or_else(|e| {
            eprintln!("fatal: cannot write content for {}: {e}", name);
            std::process::exit(2);
        });
    }

    zip.finish().unwrap_or_else(|e| {
        eprintln!("fatal: cannot finalize zip: {e}");
        std::process::exit(2);
    });
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

fn get_inner_class_full_name(
    class: &vinylite_core::ClassFile,
    class_index: u16,
) -> Option<String> {
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
    pool: &[vinylite_core::recovery::Recoverable<
        vinylite_core::classfile::ConstantPoolEntry,
    >],
    index: u16,
) -> Option<String> {
    match pool.get(index as usize) {
        Some(vinylite_core::recovery::Recoverable::Present(
            vinylite_core::classfile::ConstantPoolEntry::Utf8(s),
        )) => Some(s.clone()),
        _ => None,
    }
}
