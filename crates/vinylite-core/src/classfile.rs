use crate::bytecode::{Instruction, decode_method_code};
use crate::diagnostic::Diagnostic;
use crate::recovery::Recoverable;
use thiserror::Error;

const CLASS_MAGIC: u32 = 0xcafebabe;

#[derive(Debug, Clone)]
pub struct ClassFile {
    pub minor_version: u16,
    pub major_version: u16,
    pub this_class: u16,
    pub super_class: u16,
    pub constant_pool: Vec<Recoverable<ConstantPoolEntry>>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub inner_classes: Vec<InnerClassInfo>,
    pub bootstrap_methods: Vec<BootstrapMethodInfo>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub access_flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BootstrapMethodInfo {
    pub method_handle_index: u16,
    pub arguments: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct InnerClassInfo {
    pub inner_class_index: u16,
    pub outer_class_index: u16,
    pub inner_name_index: u16,
    pub inner_class_access_flags: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstantPoolEntry {
    Utf8(String),
    Integer(i32),
    Float(u32),
    Long(i64),
    Double(u64),
    Class {
        name_index: u16,
    },
    String {
        string_index: u16,
    },
    FieldRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    MethodRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    InterfaceMethodRef {
        class_index: u16,
        name_and_type_index: u16,
    },
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    MethodType {
        descriptor_index: u16,
    },
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    Module {
        name_index: u16,
    },
    Package {
        name_index: u16,
    },
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub access_flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub signature: Option<String>,
    pub code: Option<CodeAttribute>,
}

#[derive(Debug, Clone)]
pub struct CodeAttribute {
    pub max_stack: u16,
    pub max_locals: u16,
    pub raw_code: Vec<u8>,
    pub instructions: Vec<Instruction>,
    pub exception_table: Vec<ExceptionEntry>,
    pub local_variable_table: Option<Vec<LocalVariableInfo>>,
    pub local_variable_type_table: Option<Vec<LocalVariableTypeEntry>>,
    pub stack_map_table: Option<Vec<StackMapFrame>>,
}

/// A StackMapTable frame (JVMS §4.7.4). Only the data needed for
/// type inference is retained: the offset delta and the locals.
#[derive(Debug, Clone)]
pub struct StackMapFrame {
    pub offset_delta: u16,
    pub locals: Vec<VerificationType>,
    pub stack: Vec<VerificationType>,
}

/// A `LocalVariableTypeTable` entry: like `LocalVariableInfo` but the
/// descriptor is a generic signature (`signature_index`).
#[derive(Debug, Clone)]
pub struct LocalVariableTypeEntry {
    pub start_pc: u16,
    pub length: u16,
    pub signature_index: u16,
    pub index: u16,
}

/// StackMapTable verification types (JVMS §4.7.4). `Object` stores the
/// constant-pool class index encoded as `"#N"`, matching what the type
/// inference pass expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationType {
    Top,
    Integer,
    Float,
    Long,
    Double,
    Null,
    UninitializedThis,
    Object(String),
    Uninitialized(u16),
}

#[derive(Debug, Clone)]
pub struct LocalVariableInfo {
    pub start_pc: u16,
    pub length: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub index: u16,
}

#[derive(Debug, Clone)]
pub struct ExceptionEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    pub catch_type: u16, // 0 = catch-all (finally)
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("fatal classfile parse failure")]
    Fatal,
}

pub struct ClassFileParser<'a> {
    bytes: &'a [u8],
    offset: usize,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> ClassFileParser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            diagnostics: Vec::new(),
        }
    }

    pub fn parse(&mut self) -> Result<ClassFile, ParseError> {
        let magic = self.read_u32_fatal("classfile magic")?;
        if magic != CLASS_MAGIC {
            self.diagnostics.push(Diagnostic::fatal(
                0,
                format!("invalid classfile magic 0x{magic:08x}"),
            ));
            return Err(ParseError::Fatal);
        }

        let minor_version = self.read_u16_fatal("minor version")?;
        let major_version = self.read_u16_fatal("major version")?;
        let constant_pool = self.parse_constant_pool()?;

        let _access_flags = self.read_u16_fatal("access flags")?;
        let this_class = self.read_u16_fatal("this class")?;
        let super_class = self.read_u16_fatal("super class")?;
        self.skip_table("interfaces")?;
        let fields = self.parse_fields(&constant_pool)?;
        let methods = self.parse_methods(&constant_pool)?;
        let (inner_classes, bootstrap_methods, signature) =
            self.parse_class_attributes(&constant_pool);

        Ok(ClassFile {
            minor_version,
            major_version,
            this_class,
            super_class,
            constant_pool,
            fields,
            methods,
            inner_classes,
            bootstrap_methods,
            signature,
        })
    }

    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    fn parse_constant_pool(&mut self) -> Result<Vec<Recoverable<ConstantPoolEntry>>, ParseError> {
        let count = self.read_u16_fatal("constant pool count")? as usize;
        let mut entries = Vec::with_capacity(count);
        entries.push(Recoverable::Missing);

        let mut index = 1;
        while index < count {
            let entry_offset = self.offset;
            let Some(tag) = self.read_u8_recoverable("constant pool tag") else {
                entries.push(Recoverable::Malformed {
                    reason: "truncated constant pool tag".to_string(),
                });
                self.fill_missing_constant_pool(&mut entries, count);
                break;
            };

            let entry = self.parse_constant_pool_entry(tag, entry_offset);
            let is_wide = matches!(
                entry,
                Recoverable::Present(ConstantPoolEntry::Long(_))
                    | Recoverable::Present(ConstantPoolEntry::Double(_))
            );
            entries.push(entry);

            if is_wide && index + 1 < count {
                entries.push(Recoverable::Missing);
                index += 1;
            }

            index += 1;
        }

        Ok(entries)
    }

    fn parse_constant_pool_entry(
        &mut self,
        tag: u8,
        entry_offset: usize,
    ) -> Recoverable<ConstantPoolEntry> {
        match tag {
            1 => self.parse_utf8_entry(entry_offset),
            3 => self
                .read_u32_entry(entry_offset, "integer")
                .map(|value| ConstantPoolEntry::Integer(value as i32)),
            4 => self
                .read_u32_entry(entry_offset, "float")
                .map(ConstantPoolEntry::Float),
            5 => self
                .read_u64_entry(entry_offset, "long")
                .map(|value| ConstantPoolEntry::Long(value as i64)),
            6 => self
                .read_u64_entry(entry_offset, "double")
                .map(ConstantPoolEntry::Double),
            7 => self
                .read_u16_entry(entry_offset, "class name index")
                .map(|name_index| ConstantPoolEntry::Class { name_index }),
            8 => self
                .read_u16_entry(entry_offset, "string index")
                .map(|string_index| ConstantPoolEntry::String { string_index }),
            9 => self.parse_ref_entry(entry_offset, |class_index, name_and_type_index| {
                ConstantPoolEntry::FieldRef {
                    class_index,
                    name_and_type_index,
                }
            }),
            10 => self.parse_ref_entry(entry_offset, |class_index, name_and_type_index| {
                ConstantPoolEntry::MethodRef {
                    class_index,
                    name_and_type_index,
                }
            }),
            11 => self.parse_ref_entry(entry_offset, |class_index, name_and_type_index| {
                ConstantPoolEntry::InterfaceMethodRef {
                    class_index,
                    name_and_type_index,
                }
            }),
            12 => match (self.read_u16_recoverable(), self.read_u16_recoverable()) {
                (Some(name_index), Some(descriptor_index)) => {
                    Recoverable::Present(ConstantPoolEntry::NameAndType {
                        name_index,
                        descriptor_index,
                    })
                }
                _ => self.malformed_cp(entry_offset, "truncated name-and-type entry"),
            },
            15 => match (
                self.read_u8_recoverable("method handle kind"),
                self.read_u16_recoverable(),
            ) {
                (Some(reference_kind), Some(reference_index)) => {
                    Recoverable::Present(ConstantPoolEntry::MethodHandle {
                        reference_kind,
                        reference_index,
                    })
                }
                _ => self.malformed_cp(entry_offset, "truncated method handle entry"),
            },
            16 => self
                .read_u16_entry(entry_offset, "method type descriptor index")
                .map(|descriptor_index| ConstantPoolEntry::MethodType { descriptor_index }),
            17 => self.parse_bootstrap_entry(
                entry_offset,
                |bootstrap_method_attr_index, name_and_type_index| ConstantPoolEntry::Dynamic {
                    bootstrap_method_attr_index,
                    name_and_type_index,
                },
            ),
            18 => self.parse_bootstrap_entry(
                entry_offset,
                |bootstrap_method_attr_index, name_and_type_index| {
                    ConstantPoolEntry::InvokeDynamic {
                        bootstrap_method_attr_index,
                        name_and_type_index,
                    }
                },
            ),
            19 => self
                .read_u16_entry(entry_offset, "module name index")
                .map(|name_index| ConstantPoolEntry::Module { name_index }),
            20 => self
                .read_u16_entry(entry_offset, "package name index")
                .map(|name_index| ConstantPoolEntry::Package { name_index }),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    entry_offset,
                    format!("unknown constant pool tag {tag}; remaining classfile offsets may be unreliable"),
                ));
                Recoverable::Malformed {
                    reason: format!("unknown tag {tag}"),
                }
            }
        }
    }

    fn parse_utf8_entry(&mut self, entry_offset: usize) -> Recoverable<ConstantPoolEntry> {
        let Some(length) = self.read_u16_recoverable() else {
            return self.malformed_cp(entry_offset, "truncated UTF-8 length");
        };
        let length = length as usize;
        let Some(bytes) = self.read_bytes_recoverable(length) else {
            return self.malformed_cp(entry_offset, "truncated UTF-8 bytes");
        };

        match std::str::from_utf8(bytes) {
            Ok(value) => Recoverable::Present(ConstantPoolEntry::Utf8(value.to_string())),
            Err(error) => {
                self.diagnostics.push(Diagnostic::warning(
                    entry_offset,
                    format!("malformed UTF-8 in constant pool entry: {error}"),
                ));
                Recoverable::Malformed {
                    reason: "malformed UTF-8".to_string(),
                }
            }
        }
    }

    fn parse_ref_entry(
        &mut self,
        entry_offset: usize,
        build: impl FnOnce(u16, u16) -> ConstantPoolEntry,
    ) -> Recoverable<ConstantPoolEntry> {
        match (self.read_u16_recoverable(), self.read_u16_recoverable()) {
            (Some(class_index), Some(name_and_type_index)) => {
                Recoverable::Present(build(class_index, name_and_type_index))
            }
            _ => self.malformed_cp(entry_offset, "truncated reference entry"),
        }
    }

    fn parse_bootstrap_entry(
        &mut self,
        entry_offset: usize,
        build: impl FnOnce(u16, u16) -> ConstantPoolEntry,
    ) -> Recoverable<ConstantPoolEntry> {
        match (self.read_u16_recoverable(), self.read_u16_recoverable()) {
            (Some(bootstrap_method_attr_index), Some(name_and_type_index)) => {
                Recoverable::Present(build(bootstrap_method_attr_index, name_and_type_index))
            }
            _ => self.malformed_cp(entry_offset, "truncated bootstrap entry"),
        }
    }

    fn parse_methods(
        &mut self,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<Vec<MethodInfo>, ParseError> {
        let method_count = self.read_u16_fatal("methods count")?;
        let mut methods = Vec::with_capacity(method_count as usize);

        for _ in 0..method_count {
            let access_flags = self.read_u16_fatal("method access flags")?;
            let name_index = self.read_u16_fatal("method name index")?;
            let descriptor_index = self.read_u16_fatal("method descriptor index")?;
            let attribute_count = self.read_u16_fatal("method attributes count")?;
            let mut code = None;
            let mut signature = None;

            for _ in 0..attribute_count {
                let attribute_name_index = self.read_u16_fatal("method attribute name index")?;
                let attribute_length = self.read_u32_fatal("method attribute length")? as usize;
                let attribute_offset = self.offset;
                let attribute_name = cp_utf8(constant_pool, attribute_name_index);

                if attribute_name.as_deref() == Some("Code") {
                    code = self.parse_code_attribute(attribute_length, constant_pool);
                } else if attribute_name.as_deref() == Some("Signature") {
                    signature = self.parse_signature_attribute(attribute_length, constant_pool);
                } else {
                    self.skip_bytes(attribute_length, "method attribute body")?;
                }

                if self.offset < attribute_offset + attribute_length {
                    let remaining = attribute_offset + attribute_length - self.offset;
                    self.skip_bytes(remaining, "method attribute padding")?;
                }
            }

            methods.push(MethodInfo {
                access_flags,
                name_index,
                descriptor_index,
                signature,
                code,
            });
        }

        Ok(methods)
    }

    fn parse_code_attribute(
        &mut self,
        attribute_length: usize,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Option<CodeAttribute> {
        let start = self.offset;
        let max_stack = self.read_u16_recoverable()?;
        let max_locals = self.read_u16_recoverable()?;
        let code_length = self.read_u32_recoverable()? as usize;
        let Some(raw_code) = self
            .read_bytes_recoverable(code_length)
            .map(ToOwned::to_owned)
        else {
            self.diagnostics.push(Diagnostic::error(
                start,
                "truncated Code attribute bytecode",
            ));
            return None;
        };

        let mut bytecode_diagnostics = Vec::new();
        let instructions = decode_method_code(&raw_code, &mut bytecode_diagnostics);
        self.diagnostics.extend(bytecode_diagnostics);

        let mut exception_table = Vec::new();
        if let Some(exception_table_length) = self.read_u16_recoverable() {
            for _ in 0..exception_table_length {
                let start_pc = self.read_u16_recoverable().unwrap_or(0);
                let end_pc = self.read_u16_recoverable().unwrap_or(0);
                let handler_pc = self.read_u16_recoverable().unwrap_or(0);
                let catch_type = self.read_u16_recoverable().unwrap_or(0);
                exception_table.push(ExceptionEntry {
                    start_pc,
                    end_pc,
                    handler_pc,
                    catch_type,
                });
            }
        }

        let mut local_variable_table = None;
        let mut local_variable_type_table = None;
        let mut stack_map_table = None;

        if let Some(attr_count) = self.read_u16_recoverable() {
            for _ in 0..attr_count {
                let attr_name_index = self.read_u16_recoverable().unwrap_or(0);
                let attr_length = self.read_u32_recoverable().unwrap_or(0) as usize;
                let attr_start = self.offset;
                let attr_name = cp_utf8(constant_pool, attr_name_index);

                match attr_name.as_deref() {
                    Some("LocalVariableTable") => {
                        local_variable_table = self.parse_local_variable_table(attr_length);
                    }
                    Some("LocalVariableTypeTable") => {
                        local_variable_type_table = self.parse_local_variable_type_table();
                    }
                    Some("StackMapTable") => {
                        stack_map_table = self.parse_stack_map_table(attr_length);
                    }
                    _ => {
                        let _ = self.skip_bytes(attr_length, "Code nested attribute body");
                    }
                }

                if self.offset < attr_start + attr_length {
                    let remaining = attr_start + attr_length - self.offset;
                    let _ = self.skip_bytes(remaining, "Code nested attribute padding");
                }
            }
        }

        let consumed = self.offset.saturating_sub(start);
        if consumed > attribute_length {
            self.diagnostics.push(Diagnostic::warning(
                start,
                "Code attribute consumed more bytes than declared length",
            ));
        }

        Some(CodeAttribute {
            max_stack,
            max_locals,
            raw_code,
            instructions,
            exception_table,
            local_variable_table,
            local_variable_type_table,
            stack_map_table,
        })
    }

    fn parse_local_variable_table(
        &mut self,
        _attr_length: usize,
    ) -> Option<Vec<LocalVariableInfo>> {
        let table_length = self.read_u16_recoverable()? as usize;
        let mut table = Vec::with_capacity(table_length);

        for _ in 0..table_length {
            let start_pc = self.read_u16_recoverable()?;
            let length = self.read_u16_recoverable()?;
            let name_index = self.read_u16_recoverable()?;
            let descriptor_index = self.read_u16_recoverable()?;
            let index = self.read_u16_recoverable()?;

            table.push(LocalVariableInfo {
                start_pc,
                length,
                name_index,
                descriptor_index,
                index,
            });
        }

        Some(table)
    }

    fn parse_local_variable_type_table(&mut self) -> Option<Vec<LocalVariableTypeEntry>> {
        let table_length = self.read_u16_recoverable()? as usize;
        let mut table = Vec::with_capacity(table_length);

        for _ in 0..table_length {
            let start_pc = self.read_u16_recoverable()?;
            let length = self.read_u16_recoverable()?;
            let _name_index = self.read_u16_recoverable()?;
            let signature_index = self.read_u16_recoverable()?;
            let index = self.read_u16_recoverable()?;

            table.push(LocalVariableTypeEntry {
                start_pc,
                length,
                signature_index,
                index,
            });
        }

        Some(table)
    }

    /// Best-effort StackMapTable parser (JVMS §4.7.4). On any truncation
    /// the whole table is discarded (`None`) with a diagnostic — type
    /// inference still works from descriptors + LVT, just without frame seeds.
    fn parse_stack_map_table(&mut self, attr_length: usize) -> Option<Vec<StackMapFrame>> {
        let attr_start = self.offset;
        let end = attr_start.saturating_add(attr_length);
        let count = self.read_u16_recoverable()?;
        let mut frames = Vec::with_capacity(count as usize);
        let mut locals: Vec<VerificationType> = Vec::new();

        for _ in 0..count {
            if self.offset >= end {
                self.diagnostics.push(Diagnostic::warning(
                    self.offset,
                    "truncated StackMapTable frame",
                ));
                return None;
            }
            let tag = self.read_u8_recoverable("stackmap frame tag")?;
            let (offset_delta, new_locals, stack) = match tag {
                0..=63 => (tag as u16, None, Vec::new()),
                64..=127 => {
                    let stack = vec![self.read_verification_type()?];
                    ((tag - 64) as u16, None, stack)
                }
                247 => {
                    let delta = self.read_u16_recoverable()?;
                    let stack = vec![self.read_verification_type()?];
                    (delta, None, stack)
                }
                248..=250 => {
                    let delta = self.read_u16_recoverable()?;
                    let chop = (251 - tag) as usize;
                    if chop > locals.len() {
                        return None;
                    }
                    let mut kept = locals.clone();
                    kept.truncate(locals.len() - chop);
                    (delta, Some(kept), Vec::new())
                }
                251 => {
                    let delta = self.read_u16_recoverable()?;
                    (delta, Some(locals.clone()), Vec::new())
                }
                252..=254 => {
                    let delta = self.read_u16_recoverable()?;
                    let mut extended = locals.clone();
                    for _ in 0..(tag - 251) {
                        extended.push(self.read_verification_type()?);
                    }
                    (delta, Some(extended), Vec::new())
                }
                255 => {
                    let delta = self.read_u16_recoverable()?;
                    let n_locals = self.read_u16_recoverable()? as usize;
                    let mut full_locals = Vec::with_capacity(n_locals);
                    for _ in 0..n_locals {
                        full_locals.push(self.read_verification_type()?);
                    }
                    let n_stack = self.read_u16_recoverable()? as usize;
                    let mut full_stack = Vec::with_capacity(n_stack);
                    for _ in 0..n_stack {
                        full_stack.push(self.read_verification_type()?);
                    }
                    (delta, Some(full_locals), full_stack)
                }
                // Tags 128-246 are reserved by JVMS §4.7.4 — treat as malformed.
                _ => {
                    self.diagnostics.push(Diagnostic::warning(
                        self.offset,
                        format!("reserved stackmap frame tag {tag}"),
                    ));
                    return None;
                }
            };
            if let Some(nl) = new_locals {
                locals = nl;
            }
            frames.push(StackMapFrame {
                offset_delta,
                locals: locals.clone(),
                stack,
            });
        }

        Some(frames)
    }

    fn read_verification_type(&mut self) -> Option<VerificationType> {
        let tag = self.read_u8_recoverable("verification type tag")?;
        match tag {
            0 => Some(VerificationType::Top),
            1 => Some(VerificationType::Integer),
            2 => Some(VerificationType::Float),
            3 => Some(VerificationType::Double),
            4 => Some(VerificationType::Long),
            5 => Some(VerificationType::Null),
            6 => Some(VerificationType::UninitializedThis),
            7 => {
                let index = self.read_u16_recoverable()?;
                Some(VerificationType::Object(format!("#{index}")))
            }
            8 => {
                let offset = self.read_u16_recoverable()?;
                Some(VerificationType::Uninitialized(offset))
            }
            _ => {
                self.diagnostics.push(Diagnostic::warning(
                    self.offset,
                    format!("unknown verification type tag {tag}"),
                ));
                None
            }
        }
    }

    fn parse_signature_attribute(
        &mut self,
        attr_length: usize,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Option<String> {
        if attr_length < 2 {
            return None;
        }
        let index = self.read_u16_recoverable()?;
        cp_utf8(constant_pool, index)
    }

    /// Like `skip_attributes` but captures a `Signature` attribute value.
    fn skip_attributes_collect_signature(
        &mut self,
        label: impl AsRef<str>,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<Option<String>, ParseError> {
        let count = self.read_u16_fatal(format!("{} count", label.as_ref()))?;
        let mut signature = None;
        for _ in 0..count {
            let name_index = self.read_u16_fatal(format!("{} name index", label.as_ref()))?;
            let length = self.read_u32_fatal(format!("{} length", label.as_ref()))? as usize;
            let attr_start = self.offset;
            let attr_name = cp_utf8(constant_pool, name_index);
            if attr_name.as_deref() == Some("Signature") {
                signature = self.parse_signature_attribute(length, constant_pool);
            } else {
                self.skip_bytes(length, format!("{} body", label.as_ref()))?;
            }
            if self.offset < attr_start + length {
                let remaining = attr_start + length - self.offset;
                self.skip_bytes(remaining, format!("{} padding", label.as_ref()))?;
            }
        }
        Ok(signature)
    }

    fn parse_fields(
        &mut self,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<Vec<FieldInfo>, ParseError> {
        let count = self.read_u16_fatal("fields count")?;
        let mut fields = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let access_flags = self.read_u16_fatal("field access flags")?;
            let name_index = self.read_u16_fatal("field name index")?;
            let descriptor_index = self.read_u16_fatal("field descriptor index")?;
            let signature =
                self.skip_attributes_collect_signature("field attributes", constant_pool)?;
            fields.push(FieldInfo {
                access_flags,
                name_index,
                descriptor_index,
                signature,
            });
        }
        Ok(fields)
    }

    #[allow(dead_code)]
    fn skip_members(
        &mut self,
        label: &str,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<(), ParseError> {
        let count = self.read_u16_fatal(format!("{label} count"))?;
        for _ in 0..count {
            self.skip_u16(format!("{label} access flags"))?;
            self.skip_u16(format!("{label} name index"))?;
            self.skip_u16(format!("{label} descriptor index"))?;
            self.skip_attributes(format!("{label} attributes"), constant_pool)?;
        }
        Ok(())
    }

    fn skip_attributes(
        &mut self,
        label: impl AsRef<str>,
        _constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<(), ParseError> {
        let count = self.read_u16_fatal(format!("{} count", label.as_ref()))?;
        for _ in 0..count {
            self.skip_u16(format!("{} name index", label.as_ref()))?;
            let length = self.read_u32_fatal(format!("{} length", label.as_ref()))? as usize;
            self.skip_bytes(length, format!("{} body", label.as_ref()))?;
        }
        Ok(())
    }

    fn skip_table(&mut self, label: &str) -> Result<(), ParseError> {
        let count = self.read_u16_fatal(format!("{label} count"))?;
        self.skip_bytes(count as usize * 2, label)
    }

    fn fill_missing_constant_pool(
        &mut self,
        entries: &mut Vec<Recoverable<ConstantPoolEntry>>,
        count: usize,
    ) {
        while entries.len() < count {
            entries.push(Recoverable::Missing);
        }
    }

    fn malformed_cp<T>(&mut self, offset: usize, reason: impl Into<String>) -> Recoverable<T> {
        let reason = reason.into();
        self.diagnostics
            .push(Diagnostic::error(offset, reason.clone()));
        Recoverable::Malformed { reason }
    }

    /// Single pass over class-level attributes, collecting BootstrapMethods,
    /// Signature and InnerClasses. Unknown attributes are skipped.
    fn parse_class_attributes(
        &mut self,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> (
        Vec<InnerClassInfo>,
        Vec<BootstrapMethodInfo>,
        Option<String>,
    ) {
        // After methods, the class attributes section follows:
        //   attributes_count: u16
        //   attributes[attributes_count]: each has name_index(u16), length(u32), info(bytes)
        let attr_count = match self.read_u16_recoverable() {
            Some(c) => c,
            None => return (Vec::new(), Vec::new(), None),
        };

        let mut inner_classes = Vec::new();
        let mut bootstrap_methods = Vec::new();
        let mut signature = None;

        for _ in 0..attr_count {
            let attr_name_index = match self.read_u16_recoverable() {
                Some(i) => i,
                None => break,
            };
            let attr_length = match self.read_u32_recoverable() {
                Some(l) => l as usize,
                None => break,
            };
            let attr_start = self.offset;

            let attr_name = cp_utf8(constant_pool, attr_name_index);

            match attr_name.as_deref() {
                Some("BootstrapMethods") => {
                    // Parse the BootstrapMethods attribute body
                    bootstrap_methods = self.parse_bootstrap_methods_body();
                }
                Some("InnerClasses") => {
                    if let Some(parsed) = self.parse_inner_classes_body() {
                        inner_classes = parsed;
                    }
                }
                Some("Signature") => {
                    signature = self.parse_signature_attribute(attr_length, constant_pool);
                }
                _ => {}
            }

            // Skip to the end of this attribute
            let consumed = self.offset - attr_start;
            if consumed < attr_length {
                let remaining = attr_length - consumed;
                let _ = self.skip_bytes(remaining, "skip class attribute");
            }
        }
        (inner_classes, bootstrap_methods, signature)
    }

    fn parse_inner_classes_body(&mut self) -> Option<Vec<InnerClassInfo>> {
        let count = self.read_u16_recoverable()? as usize;
        let mut infos = Vec::with_capacity(count);
        for _ in 0..count {
            let inner_class_index = self.read_u16_recoverable()?;
            let outer_class_index = self.read_u16_recoverable()?;
            let inner_name_index = self.read_u16_recoverable()?;
            let inner_class_access_flags = self.read_u16_recoverable()?;
            infos.push(InnerClassInfo {
                inner_class_index,
                outer_class_index,
                inner_name_index,
                inner_class_access_flags,
            });
        }
        Some(infos)
    }

    fn parse_bootstrap_methods_body(&mut self) -> Vec<BootstrapMethodInfo> {
        let mut bootstrap_methods = Vec::new();
        let count = match self.read_u16_recoverable() {
            Some(c) => c,
            None => return bootstrap_methods,
        };
        for _ in 0..count {
            let method_handle_index = match self.read_u16_recoverable() {
                Some(i) => i,
                None => break,
            };
            let num_args = match self.read_u16_recoverable() {
                Some(n) => n,
                None => break,
            };
            let mut arguments = Vec::new();
            for _ in 0..num_args {
                if let Some(arg_index) = self.read_u16_recoverable() {
                    arguments.push(arg_index);
                }
            }
            bootstrap_methods.push(BootstrapMethodInfo {
                method_handle_index,
                arguments,
            });
        }
        bootstrap_methods
    }

    fn read_u16_entry(&mut self, offset: usize, label: &str) -> Recoverable<u16> {
        match self.read_u16_recoverable() {
            Some(value) => Recoverable::Present(value),
            None => self.malformed_cp(offset, format!("truncated {label}")),
        }
    }

    fn read_u32_entry(&mut self, offset: usize, label: &str) -> Recoverable<u32> {
        match self.read_u32_recoverable() {
            Some(value) => Recoverable::Present(value),
            None => self.malformed_cp(offset, format!("truncated {label}")),
        }
    }

    fn read_u64_entry(&mut self, offset: usize, label: &str) -> Recoverable<u64> {
        match (self.read_u32_recoverable(), self.read_u32_recoverable()) {
            (Some(high), Some(low)) => Recoverable::Present(((high as u64) << 32) | low as u64),
            _ => self.malformed_cp(offset, format!("truncated {label}")),
        }
    }

    fn read_u8_recoverable(&mut self, label: impl AsRef<str>) -> Option<u8> {
        let value = self.bytes.get(self.offset).copied();
        if value.is_none() {
            self.diagnostics.push(Diagnostic::error(
                self.offset,
                format!("truncated {}", label.as_ref()),
            ));
        } else {
            self.offset += 1;
        }
        value
    }

    fn read_u16_recoverable(&mut self) -> Option<u16> {
        let bytes = self.read_bytes_recoverable(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32_recoverable(&mut self) -> Option<u32> {
        let bytes = self.read_bytes_recoverable(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u16_fatal(&mut self, label: impl AsRef<str>) -> Result<u16, ParseError> {
        self.read_u16_recoverable().ok_or_else(|| {
            self.diagnostics.push(Diagnostic::fatal(
                self.offset,
                format!("cannot read {}", label.as_ref()),
            ));
            ParseError::Fatal
        })
    }

    fn read_u32_fatal(&mut self, label: impl AsRef<str>) -> Result<u32, ParseError> {
        self.read_u32_recoverable().ok_or_else(|| {
            self.diagnostics.push(Diagnostic::fatal(
                self.offset,
                format!("cannot read {}", label.as_ref()),
            ));
            ParseError::Fatal
        })
    }

    fn read_bytes_recoverable(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(length)?;
        let bytes = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(bytes)
    }

    fn skip_u16(&mut self, label: impl AsRef<str>) -> Result<(), ParseError> {
        self.read_u16_fatal(label).map(|_| ())
    }

    fn skip_bytes(&mut self, length: usize, label: impl AsRef<str>) -> Result<(), ParseError> {
        if self.read_bytes_recoverable(length).is_some() {
            Ok(())
        } else {
            self.diagnostics.push(Diagnostic::fatal(
                self.offset,
                format!("cannot skip {} bytes for {}", length, label.as_ref()),
            ));
            Err(ParseError::Fatal)
        }
    }
}

fn cp_utf8(constant_pool: &[Recoverable<ConstantPoolEntry>], index: u16) -> Option<String> {
    match constant_pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(value))) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_magic_as_fatal() {
        let bytes = [0, 0, 0, 0];
        let mut parser = ClassFileParser::new(&bytes);

        assert!(parser.parse().is_err());
        assert!(
            parser
                .into_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.severity == crate::Severity::Fatal)
        );
    }

    #[test]
    fn tolerates_malformed_utf8_constant_pool_entry() {
        let bytes = [
            0xca, 0xfe, 0xba, 0xbe, 0x00, 0x00, 0x00, 0x34, 0x00, 0x02, 0x01, 0x00, 0x01, 0xff,
            0x00, 0x21, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut parser = ClassFileParser::new(&bytes);
        let parsed = parser
            .parse()
            .expect("class should remain structurally parseable");

        assert!(matches!(
            parsed.constant_pool[1],
            Recoverable::Malformed { .. }
        ));
        assert!(!parser.into_diagnostics().is_empty());
    }
}
