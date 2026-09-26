pub mod ast;
pub mod bytecode;
pub mod cfg;
pub mod classfile;
pub mod classpath;
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
pub use classpath::Classpath;
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

/// Like [`decompile_class`], but hierarchy queries (currently `@Override`
/// beyond `java.lang.Object`) resolve against `classpath`, which should
/// contain the archive being decompiled plus any `--classpath` entries.
pub fn decompile_class_with_classpath(bytes: &[u8], classpath: &mut Classpath) -> String {
    let report = inspect_class(bytes);
    match &report.class {
        Some(class) => {
            let class_decl = build_class_decl_with_classpath(class, Some(classpath));
            class_decl.render()
        }
        None => "// class unrecoverable\n".to_string(),
    }
}

/// Short-rendered annotation strings for a method: bytecode annotations
/// first, then `@Deprecated` (flag or attribute), then `@Override` — for the
/// five `java.lang.Object` methods (provable without a hierarchy) or any
/// method the `classpath` hierarchy confirms as an override.
fn rendered_method_annotations(
    method: &crate::classfile::MethodInfo,
    method_name: &str,
    descriptor: &str,
    class_internal_name: &str,
    classpath: Option<&mut Classpath>,
) -> Vec<String> {
    let mut out: Vec<String> = method
        .annotations
        .iter()
        .map(|a| a.render_short())
        .collect();
    if method.deprecated
        && !method
            .annotations
            .iter()
            .any(|a| a.type_name == "java.lang.Deprecated")
    {
        out.push("Deprecated".to_string());
    }
    let hierarchy_override = classpath.is_some_and(|cp| {
        cp.is_override(
            class_internal_name,
            method_name,
            descriptor,
            method.access_flags,
        )
    });
    if (hierarchy_override
        || is_object_override(
            method_name,
            descriptor,
            method.access_flags,
            class_internal_name,
        ))
        && !out.iter().any(|a| a == "Override")
    {
        out.push("Override".to_string());
    }
    out
}

/// True for instance methods that provably override `java.lang.Object`:
/// `equals/hashCode/toString/clone/finalize` with exact descriptors.
/// No hierarchy needed — any instance method with these signatures (outside
/// `java.lang.Object` itself) overrides Object.
fn is_object_override(
    method_name: &str,
    descriptor: &str,
    access_flags: u16,
    class_internal_name: &str,
) -> bool {
    if access_flags & 0x0008 != 0 || class_internal_name == "java/lang/Object" {
        return false;
    }
    matches!(
        (method_name, descriptor),
        ("equals", "(Ljava/lang/Object;)Z")
            | ("hashCode", "()I")
            | ("toString", "()Ljava/lang/String;")
            | ("clone", "()Ljava/lang/Object;")
            | ("finalize", "()V")
    )
}

/// Short-rendered annotation strings for a field or class.
fn rendered_member_annotations(
    annotations: &[crate::classfile::Annotation],
    deprecated: bool,
) -> Vec<String> {
    let mut out: Vec<String> = annotations.iter().map(|a| a.render_short()).collect();
    if deprecated
        && !annotations
            .iter()
            .any(|a| a.type_name == "java.lang.Deprecated")
    {
        out.push("Deprecated".to_string());
    }
    out
}

pub fn build_class_decl(class: &ClassFile) -> ClassDecl {
    build_class_decl_with_classpath(class, None)
}

pub fn build_class_decl_with_classpath(
    class: &ClassFile,
    mut classpath: Option<&mut Classpath>,
) -> ClassDecl {
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

    // Resolve super class and interfaces using generic Signature when available
    let (type_params, super_name, interfaces) = if let Some(sig) = class.signature.as_deref()
        && let Some(parsed) = generics::parse_class_signature(sig)
    {
        let (tp, sup, ifaces) = generics::format_class_sig(&parsed);
        let ifaces_dotted: Vec<String> = ifaces.iter().map(|i| internal_name_to_dot(i)).collect();
        (tp, Some(sup), ifaces_dotted)
    } else {
        let sup = generics::generic_super_name(
            get_class_name_from_pool(&class.constant_pool, class.super_class)
                .map(|s| internal_name_to_dot(&s)),
            class.signature.as_deref(),
        );
        let ifaces_dotted: Vec<String> = class
            .interfaces
            .iter()
            .map(|i| internal_name_to_dot(i))
            .collect();
        (String::new(), sup, ifaces_dotted)
    };

    let is_interface = class.access_flags & 0x0200 != 0;
    let is_annotation = is_interface && class.access_flags & 0x2000 != 0;

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
                    annotations: Vec::new(),
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

    // Pass 2a: lower <clinit> first to extract static field initializers
    let mut static_inits = std::collections::HashMap::new();
    for method in &class.methods {
        let method_name = get_method_name(class, method);
        if method_name != "<clinit>" {
            continue;
        }
        let descriptor = get_utf8_from_pool(&class.constant_pool, method.descriptor_index)
            .unwrap_or_else(|| "()V".to_string());

        let mut clinit_decl = if let Some(code) = &method.code {
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
            MethodDecl {
                name: method_name.clone(),
                statements: vec![],
                access_flags: method.access_flags,
                return_type: "void".to_string(),
                param_types: vec![],
                param_names: vec![],
                class_name: simple_name.clone(),
                annotations: Vec::new(),
            }
        };

        // Extract static field initializers from <clinit> statements
        for stmt in &clinit_decl.statements {
            if let Statement::Assign { target, value } = stmt {
                static_inits.insert(target.clone(), value.clone());
            }
        }
        // Clear the <clinit> body since initializers are now in field declarations
        clinit_decl.statements.clear();
    }

    // Pass 2b: lower all other non-lambda methods with lambda bodies available
    for method in &class.methods {
        let method_name = get_method_name(class, method);
        if method_name.starts_with("lambda$") || method_name == "<clinit>" {
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
                annotations: rendered_method_annotations(
                    method,
                    &method_name,
                    &descriptor,
                    &class_name,
                    classpath.as_deref_mut(),
                ),
            }
        };

        method_decl.name = method_name;
        method_decl.access_flags = method.access_flags;
        // Annotations render from pool-free short strings computed here,
        // where the descriptor and flags are still at hand.
        if method_decl.annotations.is_empty() {
            method_decl.annotations = rendered_method_annotations(
                method,
                &get_method_name(class, method),
                &descriptor,
                &class_name,
                classpath.as_deref_mut(),
            );
        }
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
            let initial_value = static_inits.get(&name).cloned();
            ast::FieldDecl {
                name,
                field_type,
                access_flags: f.access_flags,
                initial_value,
                annotations: rendered_member_annotations(&f.annotations, f.deprecated),
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
        interfaces,
        type_params,
        is_enum,
        is_interface,
        is_annotation,
        enum_constants,
        annotations: rendered_member_annotations(&class.annotations, class.deprecated),
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

/// Collect static field initializers from <clinit> method.
/// Returns a map of field_name -> initial_value Expression.
/// Used for interfaces and classes to render `static final Type NAME = VALUE;`
#[allow(dead_code)]
fn collect_static_field_initializers(
    class: &ClassFile,
    pool: &[Recoverable<ConstantPoolEntry>],
) -> std::collections::HashMap<String, Expression> {
    let inits = std::collections::HashMap::new();

    // Find <clinit> method
    let clinit_method = class
        .methods
        .iter()
        .find(|m| get_method_name(class, m) == "<clinit>");

    if let Some(method) = clinit_method
        && let Some(code) = &method.code
    {
        let instrs = &code.instructions;
        let mut i = 0;
        while i < instrs.len() {
            // Pattern: putstatic Class.field = value
            if let crate::bytecode::InstructionKind::Field {
                opcode: 0xb3, // putstatic
                cp_index,
            } = instrs[i].kind
            {
                // Get field info
                if let Some(Recoverable::Present(ConstantPoolEntry::FieldRef {
                    class_index,
                    name_and_type_index,
                })) = pool.get(cp_index as usize)
                    && let Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) =
                        pool.get(*class_index as usize)
                    && let Some(_field_class) = get_utf8_from_pool(pool, *name_index)
                {
                    // Only process fields of this class (or interfaces)
                    if let Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
                        name_index: field_name_idx,
                        descriptor_index: field_desc_idx,
                    })) = pool.get(*name_and_type_index as usize)
                        && let Some(_field_name) = get_utf8_from_pool(pool, *field_name_idx)
                        && let Some(_field_desc) = get_utf8_from_pool(pool, *field_desc_idx)
                    {
                        // Look back to find the value being stored
                        // Pattern: ... value putstatic
                        if i > 0 {
                            // The value should be on the stack before putstatic
                            // We can't easily reconstruct it from instructions alone,
                            // but for simple cases (new, ldc, getstatic), we can try
                            // For now, skip - we'd need full stack simulation
                        }
                    }
                }
            }
            i += 1;
        }
    }

    inits
}
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

    // Annotation types (plus enum/class value types inside them) also need
    // imports; the render filter keeps only ones actually used in the body.
    // Nested types (`com.foo.Outer$Inner`) import via their outer class.
    let mut annotation_types = Vec::new();
    for annotation in class
        .annotations
        .iter()
        .chain(class.methods.iter().flat_map(|m| m.annotations.iter()))
        .chain(class.fields.iter().flat_map(|f| f.annotations.iter()))
    {
        annotation_types.extend(annotation.referenced_types());
    }
    for dotted in annotation_types {
        let outer = dotted.split('$').next().unwrap_or(&dotted).to_string();
        if outer != this_dot
            && !outer.starts_with("java.lang.")
            && Some(outer.as_str()) != this_pkg
            && outer.contains('.')
        {
            imports.push(outer);
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
