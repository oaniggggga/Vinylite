use std::collections::HashMap;

use crate::bytecode::{Instruction, InstructionKind};
use crate::classfile::{CodeAttribute, ConstantPoolEntry, VerificationType};
use crate::descriptor::parse_descriptor;
use crate::recovery::Recoverable;

// ──── Type representation ────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaType {
    Void,
    Int,
    Long,
    Float,
    Double,
    Boolean,
    Byte,
    Char,
    Short,
    Object(String),
    Array(Box<JavaType>),
    Null,
    Unknown,
}

impl std::fmt::Display for JavaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JavaType::Void => write!(f, "void"),
            JavaType::Int => write!(f, "int"),
            JavaType::Long => write!(f, "long"),
            JavaType::Float => write!(f, "float"),
            JavaType::Double => write!(f, "double"),
            JavaType::Boolean => write!(f, "boolean"),
            JavaType::Byte => write!(f, "byte"),
            JavaType::Char => write!(f, "char"),
            JavaType::Short => write!(f, "short"),
            JavaType::Object(name) => write!(f, "{name}"),
            JavaType::Array(inner) => write!(f, "{inner}[]"),
            JavaType::Null => write!(f, "null"),
            JavaType::Unknown => write!(f, "Object"),
        }
    }
}

impl JavaType {
    pub fn category(&self) -> u8 {
        match self {
            JavaType::Long | JavaType::Double => 2,
            _ => 1,
        }
    }

    pub fn is_wide(&self) -> bool {
        self.category() == 2
    }

    /// JVM internal type descriptor (Lpackage/Class;)
    pub fn to_descriptor(&self) -> String {
        match self {
            JavaType::Void => "V".into(),
            JavaType::Int => "I".into(),
            JavaType::Long => "J".into(),
            JavaType::Float => "F".into(),
            JavaType::Double => "D".into(),
            JavaType::Boolean => "Z".into(),
            JavaType::Byte => "B".into(),
            JavaType::Char => "C".into(),
            JavaType::Short => "S".into(),
            JavaType::Object(name) => format!("L{};", name.replace('.', "/")),
            JavaType::Array(inner) => format!("[{}", inner.to_descriptor()),
            JavaType::Null => "Ljava/lang/Object;".into(),
            JavaType::Unknown => "Ljava/lang/Object;".into(),
        }
    }
}

// ──── Verification type → JavaType ────

fn verification_to_java(
    vt: &VerificationType,
    pool: &[Recoverable<ConstantPoolEntry>],
) -> JavaType {
    match vt {
        VerificationType::Top => JavaType::Unknown,
        VerificationType::Integer => JavaType::Int,
        VerificationType::Float => JavaType::Float,
        VerificationType::Long => JavaType::Long,
        VerificationType::Double => JavaType::Double,
        VerificationType::Null => JavaType::Null,
        VerificationType::UninitializedThis | VerificationType::Uninitialized(_) => {
            JavaType::Unknown
        }
        VerificationType::Object(idx_str) => {
            // Parse "#N" format we stored during parsing
            if let Some(idx_str) = idx_str.strip_prefix('#')
                && let Ok(idx) = idx_str.parse::<u16>()
            {
                return resolve_class_type(pool, idx);
            }
            JavaType::Object(idx_str.clone())
        }
    }
}

fn resolve_class_type(pool: &[Recoverable<ConstantPoolEntry>], class_index: u16) -> JavaType {
    match pool.get(class_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            match pool.get(*name_index as usize) {
                Some(Recoverable::Present(ConstantPoolEntry::Utf8(name))) => {
                    JavaType::Object(name.replace('/', "."))
                }
                _ => JavaType::Unknown,
            }
        }
        _ => JavaType::Unknown,
    }
}

// ──── Local variable metadata ────

#[derive(Debug, Clone, Default)]
pub struct LocalVarMeta {
    pub name: Option<String>,
    pub ty: Option<JavaType>,
    pub generic_signature: Option<String>,
    pub start_pc: u16,
    pub length: u16,
}

pub fn collect_local_metadata(
    code_attr: &CodeAttribute,
    pool: &[Recoverable<ConstantPoolEntry>],
) -> HashMap<u16, Vec<LocalVarMeta>> {
    let mut meta: HashMap<u16, Vec<LocalVarMeta>> = HashMap::new();

    if let Some(ref table) = code_attr.local_variable_table {
        for entry in table {
            let m_vec = meta.entry(entry.index).or_default();
            m_vec.push(LocalVarMeta {
                name: cp_utf8(pool, entry.name_index),
                ty: descriptor_to_type(cp_utf8(pool, entry.descriptor_index).as_deref()),
                generic_signature: None,
                start_pc: entry.start_pc,
                length: entry.length,
            });
        }
    }

    if let Some(ref table) = code_attr.local_variable_type_table {
        for entry in table {
            let m_vec = meta.entry(entry.index).or_default();
            if let Some(m) = m_vec
                .iter_mut()
                .find(|m| m.start_pc == entry.start_pc && m.length == entry.length)
            {
                m.generic_signature = cp_utf8(pool, entry.signature_index);
            }
        }
    }

    meta
}

// ──── Type environment ────

pub struct TypeEnvironment {
    /// Types at each bytecode offset: locals[idx] and stack[slot]
    pub locals_at: HashMap<usize, Vec<JavaType>>,
    pub stack_at: HashMap<usize, Vec<JavaType>>,
    /// Best-known types for each local variable index (final snapshot)
    pub locals: Vec<JavaType>,
    /// Local variable metadata (names, types from debug info)
    pub local_meta: HashMap<u16, Vec<LocalVarMeta>>,
}

/// Build a TypeEnvironment by forward-propagating types through the bytecode.
pub fn infer_types(
    instructions: &[Instruction],
    code_attr: &CodeAttribute,
    method_descriptor: &str,
    is_static: bool,
    pool: &[Recoverable<ConstantPoolEntry>],
) -> TypeEnvironment {
    let local_meta = collect_local_metadata(code_attr, pool);
    let max_locals = code_attr.max_locals as usize;

    // Initialize local types from method descriptor.
    // NOTE: `parse_descriptor` returns *display* names ("boolean",
    // "java.lang.String", "int[]"), not raw descriptors ("Z", ...), so they
    // need a display-aware mapping (previously this silently produced
    // `Unknown` for every parameter — booleans rendered as `int`).
    let (param_types, _) = parse_descriptor(method_descriptor);
    let mut initial_locals = vec![JavaType::Unknown; max_locals];
    let mut slot = if is_static { 0 } else { 1 };
    for p in &param_types {
        let jt = erased_display_to_type(p).unwrap_or(JavaType::Unknown);
        if slot < max_locals {
            initial_locals[slot] = jt.clone();
        }
        slot += jt.category() as usize;
    }

    // If not static, local 0 is `this`
    if !is_static && max_locals > 0 {
        initial_locals[0] = JavaType::Unknown; // will be refined by stack map
    }

    // Seed from local variable metadata (name/debug info takes precedence for names)
    for (&idx, m_vec) in &local_meta {
        if let Some(m) = m_vec.first()
            && let Some(ref ty) = m.ty
        {
            initial_locals[idx as usize] = ty.clone();
        }
    }

    // Build offset → instruction map
    let offset_map: HashMap<usize, usize> = instructions
        .iter()
        .enumerate()
        .map(|(i, ins)| (ins.offset, i))
        .collect();

    // Seed from StackMapTable frames
    let mut seed_locals: HashMap<usize, Vec<JavaType>> = HashMap::new();
    if let Some(ref frames) = code_attr.stack_map_table {
        let mut current_offset = 0u16;
        for frame in frames {
            current_offset = current_offset
                .saturating_add(frame.offset_delta)
                .saturating_add(1);
            let frame_offset = current_offset as usize;

            let mut locals = initial_locals.clone();
            // If the frame is a full_frame or append_frame, it defines some locals
            if !frame.locals.is_empty() {
                let mut slot = 0usize;
                for vt in &frame.locals {
                    let jt = verification_to_java(vt, pool);
                    if slot < max_locals {
                        locals[slot] = jt.clone();
                    }
                    slot += jt.category() as usize;
                }
            }
            seed_locals.insert(frame_offset, locals);
        }
    }

    // Forward propagation
    let mut locals_at: HashMap<usize, Vec<JavaType>> = HashMap::new();
    let mut stack_at: HashMap<usize, Vec<JavaType>> = HashMap::new();

    // Merge StackMapTable seeds: at branch targets the frame types are
    // authoritative (CFR/Vineflower seed their type flow the same way).
    for (offset, seeded) in &seed_locals {
        merge_locals_into(&mut locals_at, *offset, seeded, max_locals);
    }

    // Start with initial locals at offset 0
    let mut worklist: Vec<usize> = Vec::new();
    if let Some(first) = instructions.first() {
        merge_locals_into(&mut locals_at, first.offset, &initial_locals, max_locals);
        worklist.push(0);
    } else if locals_at.is_empty() {
        // No instructions and no frames: fall back to entry types.
        return TypeEnvironment {
            locals_at,
            stack_at,
            locals: initial_locals,
            local_meta,
        };
    }

    let mut visited = vec![false; instructions.len()];

    while let Some(ins_idx) = worklist.pop() {
        if visited[ins_idx] {
            continue;
        }
        visited[ins_idx] = true;

        let ins = &instructions[ins_idx];
        let mut locals = locals_at
            .get(&ins.offset)
            .cloned()
            .unwrap_or_else(|| vec![JavaType::Unknown; max_locals]);

        let mut stack: Vec<JavaType> = stack_at.get(&ins.offset).cloned().unwrap_or_default();

        // Execute instruction effect on types
        execute_type_effect(&ins.kind, &mut locals, &mut stack, pool, max_locals);

        // Snapshot locals and stack after this instruction
        let next_offset = ins.offset + ins.length;
        locals_at.insert(next_offset, locals.clone());
        stack_at.insert(next_offset, stack.clone());

        // Propagate to successors
        // 1. Fallthrough
        if ins.kind.has_fallthrough()
            && let Some(&next_idx) = offset_map.get(&next_offset)
            && !visited[next_idx]
        {
            // Merge locals
            merge_locals_into(&mut locals_at, next_offset, &locals, max_locals);
            worklist.push(next_idx);
        }

        // 2. Branch targets
        for target in ins.kind.branch_targets() {
            if target >= 0
                && let Some(&target_idx) = offset_map.get(&(target as usize))
                && !visited[target_idx]
            {
                merge_locals_into(&mut locals_at, target as usize, &locals, max_locals);
                worklist.push(target_idx);
            }
        }
    }

    // Final locals = best snapshot from the method body
    let mut final_locals = initial_locals.clone();
    for (i, slot) in final_locals.iter_mut().enumerate().take(max_locals) {
        for locals in locals_at.values() {
            if let Some(ty) = locals.get(i)
                && *ty != JavaType::Unknown
                && *ty != JavaType::Null
            {
                *slot = ty.clone();
            }
        }
    }

    TypeEnvironment {
        locals_at,
        stack_at,
        locals: final_locals,
        local_meta,
    }
}

fn merge_locals_into(
    locals_at: &mut HashMap<usize, Vec<JavaType>>,
    offset: usize,
    incoming: &[JavaType],
    max_locals: usize,
) {
    let existing = locals_at.get(&offset);
    let merged: Vec<JavaType> = (0..max_locals)
        .map(|i| {
            let a = existing.map(|v| &v[i]).unwrap_or(&JavaType::Unknown);
            let b = incoming.get(i).unwrap_or(&JavaType::Unknown);
            merge_type(a, b)
        })
        .collect();
    locals_at.insert(offset, merged);
}

fn merge_type(a: &JavaType, b: &JavaType) -> JavaType {
    if a == b {
        return a.clone();
    }
    if matches!(a, JavaType::Unknown) {
        return b.clone();
    }
    if matches!(b, JavaType::Unknown) {
        return a.clone();
    }
    // Both known but different → find common supertype (simplified: Object)
    JavaType::Unknown
}

/// Execute an instruction's effect on local types and stack types (pure type tracking).
fn execute_type_effect(
    kind: &InstructionKind,
    locals: &mut [JavaType],
    stack: &mut Vec<JavaType>,
    pool: &[Recoverable<ConstantPoolEntry>],
    max_locals: usize,
) {
    match kind {
        InstructionKind::Nop => {}
        InstructionKind::AconstNull => stack.push(JavaType::Null),
        InstructionKind::Iconst(_) | InstructionKind::Bipush(_) | InstructionKind::Sipush(_) => {
            stack.push(JavaType::Int)
        }
        InstructionKind::Lconst(_) => stack.push(JavaType::Long),
        InstructionKind::Fconst(_) => stack.push(JavaType::Float),
        InstructionKind::Dconst(_) => stack.push(JavaType::Double),
        InstructionKind::Ldc(index) => {
            let ty = match pool.get(*index as usize) {
                Some(Recoverable::Present(ConstantPoolEntry::Integer(_))) => JavaType::Int,
                Some(Recoverable::Present(ConstantPoolEntry::Float(_))) => JavaType::Float,
                Some(Recoverable::Present(ConstantPoolEntry::Long(_))) => JavaType::Long,
                Some(Recoverable::Present(ConstantPoolEntry::Double(_))) => JavaType::Double,
                Some(Recoverable::Present(ConstantPoolEntry::String { .. })) => {
                    JavaType::Object("java.lang.String".into())
                }
                Some(Recoverable::Present(ConstantPoolEntry::Class { .. })) => {
                    JavaType::Object("java.lang.Class".into())
                }
                _ => JavaType::Unknown,
            };
            stack.push(ty);
        }
        InstructionKind::Load { index, .. } => {
            let ty = locals
                .get(*index as usize)
                .cloned()
                .unwrap_or(JavaType::Unknown);
            stack.push(ty);
        }
        InstructionKind::Store { index, .. } => {
            let ty = stack.pop().unwrap_or(JavaType::Unknown);
            if (*index as usize) < max_locals {
                locals[*index as usize] = ty;
            }
        }
        InstructionKind::Iinc { index, .. } => {
            if (*index as usize) < max_locals {
                locals[*index as usize] = JavaType::Int;
            }
        }
        InstructionKind::Stack(op) => {
            use crate::bytecode::StackOp;
            match op {
                StackOp::Dup => {
                    if let Some(top) = stack.last().cloned() {
                        stack.push(top);
                    }
                }
                StackOp::DupX1 if stack.len() >= 2 => {
                    let top = stack.pop().unwrap();
                    let below = stack.pop().unwrap();
                    stack.push(top.clone());
                    stack.push(below);
                    stack.push(top);
                }
                StackOp::DupX2 if stack.len() >= 3 => {
                    let v1 = stack.pop().unwrap();
                    let v2 = stack.pop().unwrap();
                    let v3 = stack.pop().unwrap();
                    stack.push(v1.clone());
                    stack.push(v3);
                    stack.push(v2);
                    stack.push(v1);
                }
                StackOp::Dup2 if stack.len() >= 2 => {
                    let v1 = stack.pop().unwrap();
                    let v2 = stack.pop().unwrap();
                    stack.push(v2.clone());
                    stack.push(v1.clone());
                    stack.push(v2);
                    stack.push(v1);
                }
                StackOp::Pop | StackOp::Pop2 => {
                    stack.pop();
                }
                StackOp::Swap if stack.len() >= 2 => {
                    let a = stack.pop().unwrap();
                    let b = stack.pop().unwrap();
                    stack.push(a);
                    stack.push(b);
                }
                _ => {}
            }
        }
        InstructionKind::ArrayLoad(_) => {
            stack.pop(); // index
            stack.pop(); // array
            stack.push(JavaType::Unknown); // element type unknown without array type
        }
        InstructionKind::ArrayStore(_) => {
            stack.pop(); // value
            stack.pop(); // index
            stack.pop(); // array
        }
        InstructionKind::Arithmetic { opcode } => {
            if *opcode >= 0x74 && *opcode <= 0x77 {
                // unary negate
                stack.pop();
                stack.push(JavaType::Int); // simplified
            } else if *opcode <= 0x63
                || (*opcode >= 0x6b && *opcode <= 0x6f)
                || (*opcode >= 0x7a && *opcode <= 0x83)
            {
                // int/long arithmetic
                stack.pop();
                stack.pop();
                stack.push(if *opcode <= 0x63 {
                    JavaType::Int
                } else {
                    JavaType::Long
                });
            } else {
                stack.pop();
                stack.pop();
                stack.push(JavaType::Int);
            }
        }
        InstructionKind::Convert { opcode } => {
            stack.pop();
            let ty = match opcode {
                0x85 => JavaType::Long,
                0x86 => JavaType::Float,
                0x87 => JavaType::Double,
                0x88 => JavaType::Int,
                0x89 => JavaType::Float,
                0x8a => JavaType::Double,
                0x8b => JavaType::Int,
                0x8c => JavaType::Long,
                0x8d => JavaType::Double,
                0x8e => JavaType::Int,
                0x8f => JavaType::Long,
                0x90 => JavaType::Float,
                0x91 => JavaType::Int,
                0x92 => JavaType::Int,
                0x93 => JavaType::Int,
                _ => JavaType::Unknown,
            };
            stack.push(ty);
        }
        InstructionKind::Compare { .. } => {
            stack.pop();
            stack.pop();
            stack.push(JavaType::Int);
        }
        InstructionKind::If { .. } => {
            // if_icmp* pops 2, ifeq/ifne/etc pops 1
            stack.pop();
        }
        InstructionKind::Goto(_) | InstructionKind::Jsr(_) => {}
        InstructionKind::Ret(_) => {}
        InstructionKind::TableSwitch { .. } | InstructionKind::LookupSwitch { .. } => {
            stack.pop(); // discriminant
        }
        InstructionKind::Return(_) => {}
        InstructionKind::Field { opcode, cp_index } => {
            match opcode {
                0xb2 => {
                    // getstatic
                    let ty = resolve_field_type(pool, *cp_index);
                    stack.push(ty);
                }
                0xb3 => {
                    // putstatic
                    stack.pop();
                }
                0xb4 => {
                    // getfield
                    stack.pop(); // object
                    let ty = resolve_field_type(pool, *cp_index);
                    stack.push(ty);
                }
                0xb5 => {
                    // putfield
                    stack.pop(); // value
                    stack.pop(); // object
                }
                _ => {}
            }
        }
        InstructionKind::Invoke { opcode, cp_index } => {
            let ret_ty = resolve_method_return_type(pool, *cp_index);
            let param_count = resolve_method_param_count(pool, *cp_index);
            // Pop args + receiver (for non-static)
            for _ in 0..param_count {
                stack.pop();
            }
            if *opcode != 0xb8 {
                stack.pop(); // receiver for invokevirtual/invokeinterface/invokespecial
            }
            if ret_ty != JavaType::Void {
                stack.push(ret_ty);
            }
        }
        InstructionKind::Type { opcode, cp_index } => {
            match opcode {
                0xbb => {
                    // new — push uninitialized
                    stack.push(JavaType::Unknown);
                }
                0xbd => {
                    // anewarray (element may itself be an array: `[[F`)
                    stack.pop(); // size
                    stack.push(resolve_array_type(pool, *cp_index));
                }
                0xc0 => {
                    // checkcast
                    stack.pop();
                    let class = resolve_object_type(pool, *cp_index);
                    stack.push(JavaType::Object(class.to_string()));
                }
                0xc1 => {
                    // instanceof
                    stack.pop();
                    stack.push(JavaType::Int);
                }
                _ => {}
            }
        }
        InstructionKind::NewArray(kind) => {
            stack.pop(); // size
            let ty = match kind {
                4 => JavaType::Array(Box::new(JavaType::Boolean)),
                5 => JavaType::Array(Box::new(JavaType::Char)),
                6 => JavaType::Array(Box::new(JavaType::Float)),
                7 => JavaType::Array(Box::new(JavaType::Double)),
                8 => JavaType::Array(Box::new(JavaType::Byte)),
                9 => JavaType::Array(Box::new(JavaType::Short)),
                10 => JavaType::Array(Box::new(JavaType::Int)),
                11 => JavaType::Array(Box::new(JavaType::Long)),
                _ => JavaType::Array(Box::new(JavaType::Unknown)),
            };
            stack.push(ty);
        }
        InstructionKind::MultiANewArray {
            dimensions,
            cp_index,
        } => {
            for _ in 0..*dimensions {
                stack.pop();
            }
            stack.push(resolve_object_type(pool, *cp_index));
        }
        InstructionKind::ArrayLength => {
            stack.pop();
            stack.push(JavaType::Int);
        }
        InstructionKind::Throw => {
            stack.pop();
        }
        InstructionKind::MonitorEnter | InstructionKind::MonitorExit => {
            stack.pop();
        }
        InstructionKind::Wide => {}
        InstructionKind::Unknown(_) => {}
        InstructionKind::Malformed { .. } => {}
    }
}

fn resolve_field_type(pool: &[Recoverable<ConstantPoolEntry>], cp_index: u16) -> JavaType {
    match pool.get(cp_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::FieldRef {
            name_and_type_index,
            ..
        })) => match pool.get(*name_and_type_index as usize) {
            Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
                descriptor_index, ..
            })) => {
                let desc = cp_utf8(pool, *descriptor_index);
                descriptor_to_type(desc.as_deref()).unwrap_or(JavaType::Unknown)
            }
            _ => JavaType::Unknown,
        },
        _ => JavaType::Unknown,
    }
}

fn resolve_method_return_type(pool: &[Recoverable<ConstantPoolEntry>], cp_index: u16) -> JavaType {
    match pool.get(cp_index as usize) {
        Some(Recoverable::Present(
            ConstantPoolEntry::MethodRef {
                name_and_type_index,
                ..
            }
            | ConstantPoolEntry::InterfaceMethodRef {
                name_and_type_index,
                ..
            },
        )) => match pool.get(*name_and_type_index as usize) {
            Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
                descriptor_index, ..
            })) => {
                let desc = cp_utf8(pool, *descriptor_index).unwrap_or_default();
                let ret_desc = desc.split(')').nth(1).unwrap_or("");
                descriptor_to_type(Some(ret_desc)).unwrap_or(JavaType::Unknown)
            }
            _ => JavaType::Unknown,
        },
        _ => JavaType::Unknown,
    }
}

fn resolve_method_param_count(pool: &[Recoverable<ConstantPoolEntry>], cp_index: u16) -> usize {
    match pool.get(cp_index as usize) {
        Some(Recoverable::Present(
            ConstantPoolEntry::MethodRef {
                name_and_type_index,
                ..
            }
            | ConstantPoolEntry::InterfaceMethodRef {
                name_and_type_index,
                ..
            },
        )) => match pool.get(*name_and_type_index as usize) {
            Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
                descriptor_index, ..
            })) => {
                let desc = cp_utf8(pool, *descriptor_index).unwrap_or_default();
                let (params, _) = parse_descriptor(&desc);
                params.len()
            }
            _ => 0,
        },
        _ => 0,
    }
}

/// Array-aware class resolution: `[F` → `float[]`, `[[F` → `float[][]`,
/// `[Ljava/lang/String;` → `java.lang.String[]`, else dotted object name.
fn resolve_object_type(pool: &[Recoverable<ConstantPoolEntry>], cp_index: u16) -> JavaType {
    let raw = match pool.get(cp_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            cp_utf8(pool, *name_index).unwrap_or_default()
        }
        _ => return JavaType::Unknown,
    };
    if raw.trim_start().starts_with('[') {
        descriptor_to_type(Some(raw.trim())).unwrap_or(JavaType::Unknown)
    } else {
        JavaType::Object(raw.replace('/', "."))
    }
}

/// Full array type for `anewarray`: `[F` → `float[]`, `[[F` → `float[][]`
/// (descriptor nesting handles multi-dim directly).
fn resolve_array_type(pool: &[Recoverable<ConstantPoolEntry>], cp_index: u16) -> JavaType {
    let raw = match pool.get(cp_index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            cp_utf8(pool, *name_index).unwrap_or_default()
        }
        _ => return JavaType::Array(Box::new(JavaType::Unknown)),
    };
    descriptor_to_type(Some(raw.trim())).unwrap_or(JavaType::Unknown)
}

/// Map an *erased display* type (as produced by `parse_descriptor`:
/// "boolean", "int", "java.lang.String", "byte[]") to a `JavaType`.
fn erased_display_to_type(display: &str) -> Option<JavaType> {
    match display {
        "void" => Some(JavaType::Void),
        "int" => Some(JavaType::Int),
        "long" => Some(JavaType::Long),
        "float" => Some(JavaType::Float),
        "double" => Some(JavaType::Double),
        "boolean" => Some(JavaType::Boolean),
        "byte" => Some(JavaType::Byte),
        "char" => Some(JavaType::Char),
        "short" => Some(JavaType::Short),
        _ => {
            if let Some(inner) = display.strip_suffix("[]") {
                return erased_display_to_type(inner).map(|t| JavaType::Array(Box::new(t)));
            }
            if display.is_empty() || display.starts_with("unknown@") {
                return None;
            }
            Some(JavaType::Object(display.to_string()))
        }
    }
}

fn descriptor_to_type(desc: Option<&str>) -> Option<JavaType> {
    let desc = desc?;
    if desc.is_empty() {
        return None;
    }
    let bytes = desc.as_bytes();
    match bytes[0] {
        b'V' => Some(JavaType::Void),
        b'I' => Some(JavaType::Int),
        b'J' => Some(JavaType::Long),
        b'F' => Some(JavaType::Float),
        b'D' => Some(JavaType::Double),
        b'B' => Some(JavaType::Byte),
        b'C' => Some(JavaType::Char),
        b'S' => Some(JavaType::Short),
        b'Z' => Some(JavaType::Boolean),
        b'L' => {
            let end = desc.find(';').unwrap_or(desc.len());
            let name = &desc[1..end];
            Some(JavaType::Object(name.replace('/', ".")))
        }
        b'[' => {
            let inner = descriptor_to_type(Some(&desc[1..]))?;
            Some(JavaType::Array(Box::new(inner)))
        }
        _ => None,
    }
}

fn cp_utf8(pool: &[Recoverable<ConstantPoolEntry>], index: u16) -> Option<String> {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(s))) => Some(s.clone()),
        _ => None,
    }
}

/// Get the type of a local variable at a specific bytecode offset.
pub fn get_local_type(env: &TypeEnvironment, offset: usize, index: u16) -> JavaType {
    if let Some(meta_vec) = env.local_meta.get(&index)
        && let Some(meta) = meta_vec.iter().find(|m| {
            offset >= m.start_pc as usize && offset < (m.start_pc as usize + m.length as usize)
        })
        && let Some(ref ty) = meta.ty
    {
        return ty.clone();
    }
    // Try locals_at snapshot
    if let Some(locals) = env.locals_at.get(&offset)
        && let Some(ty) = locals.get(index as usize)
        && *ty != JavaType::Unknown
    {
        return ty.clone();
    }
    // Fall back to final snapshot
    if let Some(ty) = env.locals.get(index as usize)
        && *ty != JavaType::Unknown
    {
        return ty.clone();
    }
    JavaType::Unknown
}

/// Get the name of a local variable, preferring debug info.
pub fn get_local_name(
    env: &TypeEnvironment,
    index: u16,
    param_names: &[String],
    is_static: bool,
    offset: usize,
) -> String {
    // Check debug info first
    if let Some(meta_vec) = env.local_meta.get(&index)
        && let Some(meta) = meta_vec.iter().find(|m| {
            offset >= m.start_pc as usize && offset < (m.start_pc as usize + m.length as usize)
        })
        && let Some(ref name) = meta.name
        && !name.is_empty()
    {
        return name.clone();
    }
    // Check if it's a parameter
    let start = if is_static { 0 } else { 1 };
    let param_idx = index as usize;
    if param_idx >= start && param_idx < start + param_names.len() {
        return param_names[param_idx - start].clone();
    }
    // `this` for instance methods
    if index == 0 && !is_static {
        return "this".to_string();
    }
    format!("local{index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_code(max_locals: u16) -> CodeAttribute {
        CodeAttribute {
            max_stack: 4,
            max_locals,
            raw_code: Vec::new(),
            instructions: Vec::new(),
            exception_table: Vec::new(),
            local_variable_table: None,
            local_variable_type_table: None,
            stack_map_table: None,
        }
    }

    #[test]
    fn seeds_param_types_from_descriptor() {
        let pool: Vec<Recoverable<ConstantPoolEntry>> = vec![Recoverable::Missing];
        let code = empty_code(3);
        let env = infer_types(&[], &code, "(ZLjava/lang/String;)V", true, &pool);
        assert_eq!(env.locals[0], JavaType::Boolean);
        assert_eq!(
            env.locals[1],
            JavaType::Object("java.lang.String".to_string())
        );
    }

    #[test]
    fn seeds_stackmap_object_type_for_this() {
        // full_frame locals: [Object #1] with #1 = com/Foo
        let pool: Vec<Recoverable<ConstantPoolEntry>> = vec![
            Recoverable::Missing,
            Recoverable::Present(ConstantPoolEntry::Utf8("com/Foo".to_string())),
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }),
        ];
        let code = CodeAttribute {
            max_stack: 4,
            max_locals: 2,
            raw_code: Vec::new(),
            instructions: Vec::new(),
            exception_table: Vec::new(),
            local_variable_table: None,
            local_variable_type_table: None,
            stack_map_table: Some(vec![crate::classfile::StackMapFrame {
                offset_delta: 0,
                locals: vec![VerificationType::Object("#2".to_string())],
                stack: Vec::new(),
            }]),
        };
        let env = infer_types(&[], &code, "()V", false, &pool);
        // `this` (slot 0) resolves through the frame to com.Foo
        assert_eq!(env.locals[0], JavaType::Object("com.Foo".to_string()));
    }
}
