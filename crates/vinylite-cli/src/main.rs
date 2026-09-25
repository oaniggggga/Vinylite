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

    // ProGuard-style obfuscation routinely emits `A`/`a` twins that are
    // distinct classes but the same file on case-insensitive filesystems.
    // Without disambiguation the second silently overwrites the first.
    let files = disambiguate_case_collisions(files);
    let mut collisions = 0usize;
    for (name, content) in &files {
        if content.contains("filesystem case collision") {
            collisions += 1;
        }
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
    if collisions > 0 {
        eprintln!("warning: {collisions} file(s) renamed (filesystem case collision)");
    }
}

/// Resolve case-insensitive path collisions between output files.
///
/// Returns `(path, content)` pairs where every path is unique even under
/// case-insensitive comparison. The first member of a collision group keeps
/// its path; the rest get a `_<n>` stem suffix, a header comment with the
/// original class, and a matching rename of the top-level type declaration
/// plus its constructors/`new`/`.class` references, so each file stays
/// self-consistent. Cross-file references to renamed classes are NOT
/// rewritten (documented limitation, shared with the general inner-class
/// merging approach).
fn disambiguate_case_collisions(files: &[(String, String)]) -> Vec<(String, String)> {
    use std::collections::{HashMap, HashSet};

    // Group indices by lowercase path, preserving input order.
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (idx, (path, _)) in files.iter().enumerate() {
        let key = path.to_lowercase();
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(idx);
    }
    let needs_work = groups.values().any(|g| g.len() > 1);
    if !needs_work {
        return files.to_vec();
    }

    let mut used: HashSet<String> = files.iter().map(|(path, _)| path.to_lowercase()).collect();
    let mut renamed: HashMap<usize, (String, String, String)> = HashMap::new();
    // (idx) -> (new_path, old_simple, new_simple)
    for key in &order {
        let members = &groups[key];
        if members.len() < 2 {
            continue;
        }
        for (n, &idx) in members.iter().enumerate().skip(1) {
            let (path, _) = &files[idx];
            let (dir, stem, ext) = split_file_path(path);
            let mut candidate_n = n - 1;
            let new_stem = loop {
                let candidate = format!("{stem}_{candidate_n}");
                let candidate_path = join_file_path(&dir, &candidate, &ext);
                if used.insert(candidate_path.to_lowercase()) {
                    break candidate;
                }
                candidate_n += 1;
            };
            let new_path = join_file_path(&dir, &new_stem, &ext);
            renamed.insert(idx, (new_path, stem.clone(), new_stem));
        }
    }

    files
        .iter()
        .enumerate()
        .map(|(idx, (path, content))| match renamed.get(&idx) {
            Some((new_path, old_simple, new_simple)) => (
                new_path.clone(),
                rename_top_level_type(content, path, old_simple, new_simple),
            ),
            None => (path.clone(), content.clone()),
        })
        .collect()
}

fn split_file_path(path: &str) -> (String, String, String) {
    let slash = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let dir = path[..slash].to_string();
    let file = &path[slash..];
    match file.rfind('.') {
        Some(dot) => (dir, file[..dot].to_string(), file[dot..].to_string()),
        None => (dir, file.to_string(), String::new()),
    }
}

fn join_file_path(dir: &str, stem: &str, ext: &str) -> String {
    format!("{dir}{stem}{ext}")
}

/// Rename the top-level type in one rendered file after a case-collision
/// disambiguation: header comment, the `public class Old` declaration,
/// same-indented constructors, method return types, field declarations,
/// extends/implements/throws/instanceof clauses, `new Old(`
/// instantiations and `Old.class` literals. Word-boundary aware so `Old`
/// never matches `Older` or locals like `old`. Cross-file references,
/// casts, annotations and generic arguments are NOT rewritten
/// (documented limitation).
fn rename_top_level_type(content: &str, original_path: &str, old: &str, new: &str) -> String {
    let original_class = original_path
        .replace('/', ".")
        .strip_suffix(".java")
        .unwrap_or(original_path)
        .to_string();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    // Header comment goes right after the watermark line.
    let note = format!("// Original class: {original_class} (renamed: filesystem case collision)");
    if lines
        .first()
        .is_some_and(|l| l.starts_with("// Decompiled by"))
    {
        lines.insert(1, note);
    } else {
        lines.insert(0, note);
    }
    let mut renamed_decl = false;
    for line in lines.iter_mut() {
        // Top-level type declaration: `public ... class|interface|enum Old`.
        // Only the first (column-0) match is the file's own type; indented
        // inner types are left alone.
        if !renamed_decl && let Some(rest) = line.strip_prefix("public ") {
            let mut best: Option<(usize, usize)> = None;
            for keyword in ["class ", "interface ", "enum ", "@interface "] {
                if let Some(pos) = rest.find(keyword)
                    && best.is_none_or(|(best_pos, _)| pos < best_pos)
                {
                    best = Some((pos, keyword.len()));
                }
            }
            if let Some((pos, len)) = best {
                let name_pos = pos + len;
                if rest[name_pos..].starts_with(old)
                    && rest[name_pos + old.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !is_ident_char(c))
                {
                    *line = format!(
                        "public {}{}{}",
                        &rest[..name_pos],
                        new,
                        &rest[name_pos + old.len()..]
                    );
                    renamed_decl = true;
                    continue;
                }
            }
        }
        // Same-indented constructor declarations `    Old(`.
        if line.starts_with("    ")
            && let Some(after) = line[4..].strip_prefix(old)
            && after.starts_with('(')
        {
            *line = format!("    {new}{after}");
            continue;
        }
        // Method return types and field declarations `    [modifiers] Old name[(=;]`.
        if let Some(rewritten) = rename_leading_type_use(line, old, new) {
            *line = replace_type_token(&rewritten, old, new);
            continue;
        }
        // extends/implements/throws/instanceof clauses plus `new Old(`,
        // `Old.class` and `Old::x` references.
        *line = rename_clause_types(line, old, new);
        *line = replace_type_token(line, old, new);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Rename a leading type use on a class-level member line:
/// `    [modifiers...] Old name(` (method return type) or
/// `    [modifiers...] Old name [=;]` (field declaration).
/// Returns `None` when the line has any other shape.
fn rename_leading_type_use(line: &str, old: &str, new: &str) -> Option<String> {
    const MODIFIERS: &[&str] = &[
        "public",
        "private",
        "protected",
        "static",
        "final",
        "abstract",
        "synchronized",
        "native",
        "strictfp",
        "transient",
        "volatile",
        "default",
    ];
    let mut rest = line.strip_prefix("    ")?;
    let mut prefix_len = 4;
    let (type_word, after_type) = loop {
        let word_end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
        if word_end == 0 {
            return None;
        }
        let word = &rest[..word_end];
        if MODIFIERS.contains(&word) {
            let spaces = rest[word_end..].len() - rest[word_end..].trim_start_matches(' ').len();
            if spaces == 0 {
                return None;
            }
            prefix_len += word_end + spaces;
            rest = &rest[word_end + spaces..];
            continue;
        }
        break (word, &rest[word_end..]);
    };
    if type_word != old {
        return None;
    }
    // `Old` must be followed by a member name and `(`, `=` or `;`.
    let tail = after_type.trim_start_matches(' ');
    if tail.is_empty() || tail.starts_with('(') || tail.starts_with('.') {
        return None;
    }
    let name_end = tail.find(|c: char| !is_ident_char(c)).unwrap_or(tail.len());
    if name_end == 0 {
        return None;
    }
    let after_name = tail[name_end..].trim_start_matches(' ');
    if !(after_name.starts_with('(') || after_name.starts_with('=') || after_name.starts_with(';'))
    {
        return None;
    }
    Some(format!(
        "{}{new}{}",
        &line[..prefix_len],
        &line[prefix_len + old.len()..]
    ))
}

/// Rename whole-word `Old` inside `extends`/`implements`/`throws` clauses
/// and `instanceof` checks on one line. Casts, annotations and generic
/// arguments are deliberately left alone (documented limitation).
fn rename_clause_types(line: &str, old: &str, new: &str) -> String {
    let mut out = line.to_string();
    for keyword in ["extends ", "implements ", "throws ", "instanceof "] {
        if let Some(pos) = out.find(keyword) {
            let head_end = pos + keyword.len();
            let (head, tail) = out.split_at(head_end);
            out = format!("{head}{}", replace_whole_word(tail, old, new));
        }
    }
    out
}

/// Replace whole-word occurrences of `old` with `new`.
fn replace_whole_word(text: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(old) {
        let before_ok = pos == 0
            || rest[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident_char(c));
        let after_ok = rest[pos + old.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            out.push_str(&rest[..pos]);
            out.push_str(new);
            rest = &rest[pos + old.len()..];
        } else {
            out.push_str(&rest[..pos + old.len()]);
            rest = &rest[pos + old.len()..];
        }
    }
    out.push_str(rest);
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Replace `new Old(`, `Old.class` and `Old::x` where `Old` is a whole
/// identifier (never a prefix of a longer name, never a qualified tail
/// like `pkg.Old` — our renderer always uses short names with imports).
fn replace_type_token(line: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find(old) {
        let before_ok = pos == 0
            || rest[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident_char(c) && c != '.');
        let after = &rest[pos + old.len()..];
        let after_ok =
            after.starts_with('(') || after.starts_with(".class") || after.starts_with("::");
        if before_ok && after_ok {
            out.push_str(&rest[..pos]);
            out.push_str(new);
            rest = after;
        } else {
            out.push_str(&rest[..pos + old.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn twin_files() -> Vec<(String, String)> {
        vec![
            (
                "com/example/F.java".to_string(),
                "// Decompiled by Vinylite v0\npublic final class F {\n    F() {\n    }\n}\n"
                    .to_string(),
            ),
            (
                "com/example/f.java".to_string(),
                "// Decompiled by Vinylite v0\npublic class f {\n    public static f make() {\n        return new f();\n    }\n}\n"
                    .to_string(),
            ),
            (
                "com/example/Plain.java".to_string(),
                "// Decompiled by Vinylite v0\npublic class Plain {\n}\n".to_string(),
            ),
        ]
    }

    #[test]
    fn case_twins_are_disambiguated_without_loss() {
        let out = disambiguate_case_collisions(&twin_files());
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths.len(), 3);
        // First twin keeps its path.
        assert!(paths.contains(&"com/example/F.java"));
        // Second twin is renamed deterministically; no silent overwrite.
        assert!(paths.contains(&"com/example/f_0.java"));
        assert!(paths.contains(&"com/example/Plain.java"));
        let renamed = out
            .iter()
            .find(|(p, _)| p == "com/example/f_0.java")
            .map(|(_, c)| c.clone())
            .expect("renamed twin present");
        assert!(
            renamed.contains("Original class: com.example.f"),
            "header comment missing:\n{renamed}"
        );
        assert!(
            renamed.contains("public class f_0 {"),
            "declaration not renamed:\n{renamed}"
        );
        assert!(
            renamed.contains("return new f_0();"),
            "instantiation not renamed:\n{renamed}"
        );
        assert!(
            renamed.contains("public static f_0 make()"),
            "factory return type not renamed:\n{renamed}"
        );
        assert!(
            !renamed.contains("class f {") && !renamed.contains("new f("),
            "stale references remain:\n{renamed}"
        );
        // Untouched files pass through byte-identical.
        let plain = out
            .iter()
            .find(|(p, _)| p == "com/example/Plain.java")
            .unwrap();
        assert_eq!(plain.1, twin_files()[2].1);
    }

    #[test]
    fn replace_type_token_respects_word_boundaries() {
        // `F` must not match inside `Foo`, `AF`, or locals like `of`.
        let line = "Foo x = new Foo(); AF y = of;";
        assert_eq!(replace_type_token(line, "F", "F_0"), line);
        assert_eq!(
            replace_type_token("return new F();", "F", "F_0"),
            "return new F_0();"
        );
        assert_eq!(
            replace_type_token("Class<?> c = F.class;", "F", "F_0"),
            "Class<?> c = F_0.class;"
        );
    }
}
