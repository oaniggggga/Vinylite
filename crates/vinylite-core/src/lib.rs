pub mod ast;
pub mod bytecode;
pub mod cfg;
pub mod classfile;
pub mod descriptor;
pub mod diagnostic;
pub mod generics;
pub mod inference;
pub mod jar;
pub mod loop_detect;
pub mod lowering;
pub mod recovery;
pub mod zipmini;

pub use ast::{BinaryOp, ClassDecl, Expression, MethodDecl, Statement, SwitchArm};
pub use classfile::{
    BootstrapMethodInfo, ClassFile, ClassFileParser, ConstantPoolEntry, StackMapFrame,
    VerificationType,
};
pub use descriptor::parse_descriptor;
pub use diagnostic::{Diagnostic, Severity};
pub use jar::{JarEntry, deobfuscate_name, parse_jar};
pub use recovery::Recoverable;

/// First line emitted at the top of every decompiled file.
pub fn watermark() -> String {
    format!(
        "// Decompiled by Vinylite v{} (https://github.com/oaniggggga/vinylite)",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Debug, Clone)]
pub struct DecompileReport {
    pub class: Option<ClassFile>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn inspect_class(bytes: &[u8]) -> DecompileReport {
    let mut parser = ClassFileParser::new(bytes);
    let class = parser.parse().ok();

    DecompileReport {
        class,
        diagnostics: parser.into_diagnostics(),
    }
}

pub fn decompile_class(bytes: &[u8]) -> String {
    let report = inspect_class(bytes);
    match &report.class {
        Some(class) => {
            let class_decl = build_class_decl(class);
            class_decl.render()
        }
        None => "// class unrecoverable\n".to_string(),
    }
}

pub fn build_class_decl(class: &ClassFile) -> ClassDecl {
    let class_name = get_class_name(class).unwrap_or_else(|| "Unknown".to_string());
    let dot_name = internal_name_to_dot(&class_name);

    // Extract package and simple name
    let (package, simple_name) = match dot_name.rfind('.') {
        Some(pos) => (
            Some(dot_name[..pos].to_string()),
            dot_name[pos + 1..].to_string(),
        ),
        None => (None, dot_name),
    };

    // Resolve super class (prefer generic Signature, e.g. `Enum<Xenon>`)
    let super_name = generics::generic_super_name(
        get_class_name_from_pool(&class.constant_pool, class.super_class)
            .map(|s| internal_name_to_dot(&s)),
        class.signature.as_deref(),
    );

    // Collect referenced classes for imports
    let imports = collect_imports(class, &class_name);

    // Collect known field names
    let known_fields: std::collections::HashSet<String> = class
        .fields
        .iter()
        .filter_map(|f| get_utf8_from_pool(&class.constant_pool, f.name_index))
        .collect();

    // Multi-pass: collect lambda bodies until stable (handles nested lambdas)
    let mut lambda_bodies: std::collections::HashMap<String, Vec<crate::ast::Statement>> =
        std::collections::HashMap::new();
    let mut all_method_decls: Vec<crate::ast::MethodDecl> = Vec::new();

    // Repeatedly lower lambda methods until bodies stabilize (handles nested lambdas)
    loop {
        let mut changed = false;
        for method in &class.methods {
            let method_name = get_method_name(class, method);
            if !method_name.starts_with("lambda$") {
                continue;
            }
            let descriptor = get_utf8_from_pool(&class.constant_pool, method.descriptor_index)
                .unwrap_or_else(|| "()V".to_string());

            let method_decl = if let Some(code) = &method.code {
                lowering::lower_method_to_ast(
                    &class.constant_pool,
                    &method_name,
                    &descriptor,
                    method.signature.as_deref(),
                    code,
                    &class_name,
                    method.access_flags,
                    &class.bootstrap_methods,
                    &lambda_bodies,
                    &known_fields,
                )
            } else {
                let (params, return_type) =
                    generics::generic_method_types(&descriptor, method.signature.as_deref());
                let param_names: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, _p)| format!("arg{i}"))
                    .collect();
                MethodDecl {
                    name: method_name.clone(),
                    statements: vec![],
                    access_flags: method.access_flags,
                    return_type,
                    param_types: params,
                    param_names,
                    class_name: simple_name.clone(),
                }
            };

            if !method_decl.statements.is_empty() {
                let new_body = method_decl.statements.clone();
                let old = lambda_bodies.insert(method_name.clone(), new_body.clone());
                if old.as_ref() != Some(&new_body) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Pass 2: lower all non-lambda methods with lambda bodies available
    for method in &class.methods {
        let method_name = get_method_name(class, method);
        if method_name.starts_with("lambda$") {
            continue;
        }
        let descriptor = get_utf8_from_pool(&class.constant_pool, method.descriptor_index)
            .unwrap_or_else(|| "()V".to_string());

        let mut method_decl = if let Some(code) = &method.code {
            lowering::lower_method_to_ast(
                &class.constant_pool,
                &method_name,
                &descriptor,
                method.signature.as_deref(),
                code,
                &class_name,
                method.access_flags,
                &class.bootstrap_methods,
                &lambda_bodies,
                &known_fields,
            )
        } else {
            let (params, return_type) =
                generics::generic_method_types(&descriptor, method.signature.as_deref());
            let param_names: Vec<String> = params
                .iter()
                .enumerate()
                .map(|(i, _p)| format!("arg{i}"))
                .collect();
            MethodDecl {
                name: method_name.clone(),
                statements: vec![],
                access_flags: method.access_flags,
                return_type,
                param_types: params,
                param_names,
                class_name: simple_name.clone(),
            }
        };

        method_decl.name = method_name;
        method_decl.access_flags = method.access_flags;
        all_method_decls.push(method_decl);
    }

    // Filter out lambda methods — they've been inlined — plus bridge and
    // synthetic methods (real source never declares them; CFR/Vineflower
    // hide them too). Filtering happens here, not earlier, because lambda
    // bodies must still be lowered above for inlining.
    const HIDDEN_METHOD_FLAGS: u16 = 0x0040 | 0x1000; // BRIDGE | SYNTHETIC
    let methods: Vec<_> = all_method_decls
        .into_iter()
        .filter(|m| !m.name.starts_with("lambda$") && m.access_flags & HIDDEN_METHOD_FLAGS == 0)
        .collect();

    // Detect enum: class extends java.lang.Enum (possibly with type args)
    let is_enum = super_name
        .as_ref()
        .map(|s| s.split('<').next().unwrap_or(s) == "java.lang.Enum")
        .unwrap_or(false);

    // Collect enum constants from static field initializers
    let enum_constants = if is_enum {
        collect_enum_constants(class)
    } else {
        vec![]
    };

    // Collect fields (prefer generic Signatures, e.g. `List<String>`)
    let fields = class
        .fields
        .iter()
        .map(|f| {
            let name = get_utf8_from_pool(&class.constant_pool, f.name_index)
                .unwrap_or_else(|| format!("field#{}", f.name_index));
            let descriptor = get_utf8_from_pool(&class.constant_pool, f.descriptor_index)
                .unwrap_or_else(|| "Ljava/lang/Object;".to_string());
            let field_type = generics::generic_field_type(&descriptor, f.signature.as_deref());
            ast::FieldDecl {
                name,
                field_type,
                access_flags: f.access_flags,
                initial_value: None,
            }
        })
        .collect();

    ClassDecl {
        name: simple_name,
        package,
        imports,
        fields,
        methods,
        access_flags: 0,
        super_name,
        is_enum,
        enum_constants,
    }
}

/// Collect enum constants by looking at static field assignments in <clinit>.
/// Enum constants are static fields whose type is the enum class itself.
fn collect_enum_constants(class: &ClassFile) -> Vec<String> {
    let class_name = get_class_name(class).unwrap_or_default();

    // Look through methods for <clinit> which assigns enum constants
    let mut constants = Vec::new();
    for method in &class.methods {
        let method_name = get_method_name(class, method);
        if method_name != "<clinit>" {
            continue;
        }
        if let Some(code) = &method.code {
            // Scan for getstatic + putstatic patterns that assign enum constants
            let mut i = 0;
            let instrs = &code.instructions;
            while i < instrs.len() {
                // Pattern: putstatic this_class.CONSTANT_NAME (opcode 0xb3)
                if let crate::bytecode::InstructionKind::Field {
                    opcode: 0xb3,
                    cp_index,
                } = instrs[i].kind
                    && let Some(Recoverable::Present(ConstantPoolEntry::FieldRef {
                        class_index,
                        name_and_type_index,
                    })) = class.constant_pool.get(cp_index as usize)
                    && let Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) =
                        class.constant_pool.get(*class_index as usize)
                    && let Some(field_class) = get_utf8_from_pool(&class.constant_pool, *name_index)
                    && field_class == class_name
                    && let Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
                        name_index: field_name_idx,
                        descriptor_index: field_desc_idx,
                    })) = class.constant_pool.get(*name_and_type_index as usize)
                {
                    let field_desc = get_utf8_from_pool(&class.constant_pool, *field_desc_idx);
                    let expected_desc = format!("L{class_name};");
                    if field_desc.as_deref() == Some(&expected_desc)
                        && let Some(field_name) =
                            get_utf8_from_pool(&class.constant_pool, *field_name_idx)
                        && !field_name.starts_with('$')
                        && field_name != "<clinit>"
                    {
                        constants.push(field_name);
                    }
                }
                i += 1;
            }
        }
    }
    constants
}

/// Collect all classes referenced in the constant pool for import generation.
fn collect_imports(class: &ClassFile, this_class: &str) -> Vec<String> {
    let this_dot = this_class.replace('/', ".");
    let this_pkg = this_dot.rfind('.').map(|i| &this_dot[..i]);

    let mut imports = Vec::new();

    for entry in &class.constant_pool {
        match entry {
            Recoverable::Present(ConstantPoolEntry::Class { name_index }) => {
                if let Some(name) = get_utf8_from_pool(&class.constant_pool, *name_index) {
                    // Skip array types (e.g., [Lfoo/Bar;)
                    if name.starts_with('[') {
                        continue;
                    }
                    let dot = name.replace('/', ".");
                    if dot != this_dot
                        && !dot.starts_with("java.lang.")
                        && Some(dot.as_str()) != this_pkg
                        && dot.contains('.')
                    {
                        imports.push(dot);
                    }
                }
            }
            Recoverable::Present(ConstantPoolEntry::MethodRef { class_index, .. })
            | Recoverable::Present(ConstantPoolEntry::FieldRef { class_index, .. })
            | Recoverable::Present(ConstantPoolEntry::InterfaceMethodRef { class_index, .. }) => {
                if let Some(name) = get_class_name_from_pool(&class.constant_pool, *class_index) {
                    if name.starts_with('[') {
                        continue;
                    }
                    let dot = name.replace('/', ".");
                    if dot != this_dot
                        && !dot.starts_with("java.lang.")
                        && Some(dot.as_str()) != this_pkg
                        && dot.contains('.')
                    {
                        imports.push(dot);
                    }
                }
            }
            _ => {}
        }
    }

    imports.sort();
    imports.dedup();
    imports
}

pub fn get_class_name(class: &ClassFile) -> Option<String> {
    get_class_name_from_pool(&class.constant_pool, class.this_class)
}

pub fn get_method_name(class: &ClassFile, method: &crate::classfile::MethodInfo) -> String {
    get_utf8_from_pool(&class.constant_pool, method.name_index)
        .unwrap_or_else(|| "unknown".to_string())
}

fn get_class_name_from_pool(
    pool: &[Recoverable<ConstantPoolEntry>],
    class_index: u16,
) -> Option<String> {
    match pool.get(class_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            get_utf8_from_pool(pool, *name_index)
        }
        _ => None,
    }
}

fn internal_name_to_dot(name: &str) -> String {
    name.replace('/', ".")
}

fn get_utf8_from_pool(pool: &[Recoverable<ConstantPoolEntry>], index: u16) -> Option<String> {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(s))) => Some(s.clone()),
        _ => None,
    }
}
