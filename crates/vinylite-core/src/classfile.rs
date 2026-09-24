use crate::bytecode::{Instruction, decode_method_code};
use crate::diagnostic::Diagnostic;
use crate::recovery::Recoverable;

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
    pub annotations: Vec<Annotation>,
    pub deprecated: bool,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub access_flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub signature: Option<String>,
    pub annotations: Vec<Annotation>,
    pub deprecated: bool,
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
    pub annotations: Vec<Annotation>,
    pub deprecated: bool,
}

/// A parsed `Runtime[In]VisibleAnnotations` entry (JVMS §4.7.16/17).
/// Kept structured so rendering (short names) and import collection
/// (dotted names) can each take what they need without a constant pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// Dotted type name, e.g. `java.lang.Deprecated`.
    pub type_name: String,
    pub pairs: Vec<(String, AnnotationValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationValue {
    Int { value: i64, long: bool },
    Boolean(bool),
    Char(char),
    FloatBits(u32),
    DoubleBits(u64),
    Str(String),
    Enum { type_name: String, constant: String },
    Class(String),
    Nested(Annotation),
    Array(Vec<AnnotationValue>),
}

fn short_annotation_name(dotted: &str) -> String {
    dotted
        .rsplit(['.', '$'])
        .next()
        .unwrap_or(dotted)
        .to_string()
}

fn escape_annotation_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl AnnotationValue {
    fn render_short(&self) -> String {
        match self {
            AnnotationValue::Int { value, long } => {
                if *long {
                    format!("{value}L")
                } else {
                    format!("{value}")
                }
            }
            AnnotationValue::Boolean(b) => b.to_string(),
            AnnotationValue::Char(c) => match c {
                '\'' => "'\\''".to_string(),
                '\\' => "'\\\\'".to_string(),
                '\n' => "'\\n'".to_string(),
                '\r' => "'\\r'".to_string(),
                '\t' => "'\\t'".to_string(),
                c if c.is_control() => format!("'\\u{:04x}'", *c as u32),
                c => format!("'{c}'"),
            },
            AnnotationValue::FloatBits(bits) => {
                let v = f32::from_bits(*bits);
                if v.is_nan() {
                    "Float.NaN".to_string()
                } else if v.is_infinite() {
                    if v.is_sign_positive() {
                        "Float.POSITIVE_INFINITY".to_string()
                    } else {
                        "Float.NEGATIVE_INFINITY".to_string()
                    }
                } else {
                    format!("{v:?}f")
                }
            }
            AnnotationValue::DoubleBits(bits) => {
                let v = f64::from_bits(*bits);
                if v.is_nan() {
                    "Double.NaN".to_string()
                } else if v.is_infinite() {
                    if v.is_sign_positive() {
                        "Double.POSITIVE_INFINITY".to_string()
                    } else {
                        "Double.NEGATIVE_INFINITY".to_string()
                    }
                } else {
                    format!("{v:?}")
                }
            }
            AnnotationValue::Str(s) => format!("\"{}\"", escape_annotation_string(s)),
            AnnotationValue::Enum {
                type_name,
                constant,
            } => format!("{}.{constant}", short_annotation_name(type_name)),
            AnnotationValue::Class(name) => {
                let dims = name.matches("[]").count();
                let base = name.replace("[]", "");
                format!(
                    "{}{}.class",
                    short_annotation_name(&base),
                    "[]".repeat(dims)
                )
            }
            AnnotationValue::Nested(annotation) => annotation.render_short(),
            AnnotationValue::Array(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.render_short()).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    /// Dotted class names this value references (for import collection).
    fn referenced_types(&self) -> Vec<String> {
        match self {
            AnnotationValue::Enum { type_name, .. } => vec![type_name.clone()],
            AnnotationValue::Class(name) => {
                vec![name.replace("[]", "")]
            }
            AnnotationValue::Nested(annotation) => annotation.referenced_types(),
            AnnotationValue::Array(items) => {
                items.iter().flat_map(|v| v.referenced_types()).collect()
            }
            _ => Vec::new(),
        }
    }
}

impl Annotation {
    /// `@Type`, `@Type(value)` or `@Type(k=v, ...)` with short names.
    pub fn render_short(&self) -> String {
        let head = short_annotation_name(&self.type_name);
        if self.pairs.is_empty() {
            return head;
        }
        if self.pairs.len() == 1 && self.pairs[0].0 == "value" {
            return format!("{}({})", head, self.pairs[0].1.render_short());
        }
        let inner: Vec<String> = self
            .pairs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.render_short()))
            .collect();
        format!("{}({})", head, inner.join(", "))
    }

    /// Dotted type names for import collection: the annotation type plus
    /// any enum/class value types.
    pub fn referenced_types(&self) -> Vec<String> {
        let mut out = vec![self.type_name.clone()];
        for (_, v) in &self.pairs {
            out.extend(v.referenced_types());
        }
        out.sort();
        out.dedup();
        out
    }
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
    pub line_number_table: Option<Vec<LineNumberEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineNumberEntry {
    pub start_pc: u16,
    pub line_number: u16,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Fatal,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Fatal => write!(f, "fatal classfile parse failure"),
        }
    }
}

impl std::error::Error for ParseError {}

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

        let class_access_flags = self.read_u16_fatal("access flags")?;
        let this_class = self.read_u16_fatal("this class")?;
        let super_class = self.read_u16_fatal("super class")?;
        self.skip_table("interfaces")?;
        let fields = self.parse_fields(&constant_pool)?;
        let methods = self.parse_methods(&constant_pool)?;
        let (inner_classes, bootstrap_methods, signature, annotations, attr_deprecated) =
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
            annotations,
            deprecated: attr_deprecated || class_access_flags & 0x2000 != 0,
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
            let mut annotations = Vec::new();
            let mut deprecated = false;

            for _ in 0..attribute_count {
                let attribute_name_index = self.read_u16_fatal("method attribute name index")?;
                let attribute_length = self.read_u32_fatal("method attribute length")? as usize;
                let attribute_offset = self.offset;
                let attribute_name = cp_utf8(constant_pool, attribute_name_index);

                if attribute_name.as_deref() == Some("Code") {
                    code = self.parse_code_attribute(attribute_length, constant_pool);
                } else if attribute_name.as_deref() == Some("Signature") {
                    signature = self.parse_signature_attribute(attribute_length, constant_pool);
                } else if attribute_name.as_deref() == Some("RuntimeVisibleAnnotations")
                    || attribute_name.as_deref() == Some("RuntimeInvisibleAnnotations")
                {
                    annotations.extend(self.parse_annotations_table(constant_pool));
                } else if attribute_name.as_deref() == Some("Deprecated") {
                    deprecated = true;
                } else {
                    self.skip_bytes(attribute_length, "method attribute body")?;
                }

                if self.offset < attribute_offset + attribute_length {
                    let remaining = attribute_offset + attribute_length - self.offset;
                    self.skip_bytes(remaining, "method attribute padding")?;
                }
            }

            if access_flags & 0x2000 != 0 {
                deprecated = true;
            }
            methods.push(MethodInfo {
                access_flags,
                name_index,
                descriptor_index,
                signature,
                code,
                annotations,
                deprecated,
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
        let mut line_number_table = None;

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
                    Some("LineNumberTable") => {
                        line_number_table = self.parse_line_number_table();
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
            line_number_table,
        })
    }

    fn parse_line_number_table(&mut self) -> Option<Vec<LineNumberEntry>> {
        let table_length = self.read_u16_recoverable()? as usize;
        let mut table = Vec::with_capacity(table_length);
        for _ in 0..table_length {
            let start_pc = self.read_u16_recoverable()?;
            let line_number = self.read_u16_recoverable()?;
            table.push(LineNumberEntry {
                start_pc,
                line_number,
            });
        }
        // Keep sorted by pc so consumers can binary-search / emit in order.
        table.sort_by_key(|e| e.start_pc);
        Some(table)
    }

    /// Best-effort `Runtime[In]VisibleAnnotations` parser (JVMS §4.7.16/17).
    /// Truncation or an unknown element tag ends the table early with a
    /// diagnostic — the annotations parsed so far are still returned, and
    /// the caller resyncs via the attribute length.
    fn parse_annotations_table(
        &mut self,
        pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Vec<Annotation> {
        let count = match self.read_u16_recoverable() {
            Some(c) => c as usize,
            None => return Vec::new(),
        };
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            match self.parse_annotation(pool) {
                Some(annotation) => out.push(annotation),
                None => {
                    self.diagnostics.push(Diagnostic::warning(
                        self.offset,
                        "truncated annotation entry, rest of table skipped",
                    ));
                    break;
                }
            }
        }
        out
    }

    fn parse_annotation(&mut self, pool: &[Recoverable<ConstantPoolEntry>]) -> Option<Annotation> {
        let type_index = self.read_u16_recoverable()?;
        let descriptor = cp_utf8(pool, type_index)?;
        let pair_count = self.read_u16_recoverable()?;
        let mut pairs = Vec::with_capacity(pair_count as usize);
        for _ in 0..pair_count {
            let name_index = self.read_u16_recoverable()?;
            let name = cp_utf8(pool, name_index)?;
            let value = self.parse_annotation_element(pool)?;
            pairs.push((name, value));
        }
        Some(Annotation {
            type_name: field_descriptor_to_dotted(&descriptor),
            pairs,
        })
    }

    fn parse_annotation_element(
        &mut self,
        pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Option<AnnotationValue> {
        let tag = self.read_u8_recoverable("annotation element tag")?;
        let const_index = |parser: &mut Self| parser.read_u16_recoverable();
        match tag {
            b'B' | b'C' | b'I' | b'S' | b'Z' => {
                let index = const_index(self)?;
                let value = match pool.get(index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Integer(v))) => *v as i64,
                    _ => return None,
                };
                match tag {
                    b'Z' => Some(AnnotationValue::Boolean(value != 0)),
                    b'C' => char::from_u32(value as u32).map(AnnotationValue::Char),
                    _ => Some(AnnotationValue::Int { value, long: false }),
                }
            }
            b'J' => {
                let index = const_index(self)?;
                match pool.get(index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Long(v))) => {
                        Some(AnnotationValue::Int {
                            value: *v,
                            long: true,
                        })
                    }
                    _ => None,
                }
            }
            b'F' => {
                let index = const_index(self)?;
                match pool.get(index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Float(bits))) => {
                        Some(AnnotationValue::FloatBits(*bits))
                    }
                    _ => None,
                }
            }
            b'D' => {
                let index = const_index(self)?;
                match pool.get(index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Double(bits))) => {
                        Some(AnnotationValue::DoubleBits(*bits))
                    }
                    _ => None,
                }
            }
            b's' => {
                let index = const_index(self)?;
                match pool.get(index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::String { string_index })) => {
                        cp_utf8(pool, *string_index).map(AnnotationValue::Str)
                    }
                    _ => None,
                }
            }
            b'e' => {
                let type_index = self.read_u16_recoverable()?;
                let const_index = self.read_u16_recoverable()?;
                let descriptor = cp_utf8(pool, type_index)?;
                let constant = cp_utf8(pool, const_index)?;
                Some(AnnotationValue::Enum {
                    type_name: field_descriptor_to_dotted(&descriptor),
                    constant,
                })
            }
            b'c' => {
                let index = const_index(self)?;
                let descriptor = cp_utf8(pool, index)?;
                Some(AnnotationValue::Class(field_descriptor_to_dotted(
                    &descriptor,
                )))
            }
            b'@' => self.parse_annotation(pool).map(AnnotationValue::Nested),
            b'[' => {
                let count = self.read_u16_recoverable()? as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.parse_annotation_element(pool)?);
                }
                Some(AnnotationValue::Array(items))
            }
            _ => {
                self.diagnostics.push(Diagnostic::warning(
                    self.offset,
                    format!("unknown annotation element tag 0x{tag:02x}"),
                ));
                None
            }
        }
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

    /// Like `skip_attributes` but captures `Signature`, `Deprecated` and
    /// `Runtime[In]VisibleAnnotations` values.
    fn skip_attributes_collect_signature(
        &mut self,
        label: impl AsRef<str>,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> Result<(Option<String>, Vec<Annotation>, bool), ParseError> {
        let count = self.read_u16_fatal(format!("{} count", label.as_ref()))?;
        let mut signature = None;
        let mut annotations = Vec::new();
        let mut deprecated = false;
        for _ in 0..count {
            let name_index = self.read_u16_fatal(format!("{} name index", label.as_ref()))?;
            let length = self.read_u32_fatal(format!("{} length", label.as_ref()))? as usize;
            let attr_start = self.offset;
            let attr_name = cp_utf8(constant_pool, name_index);
            match attr_name.as_deref() {
                Some("Signature") => {
                    signature = self.parse_signature_attribute(length, constant_pool);
                }
                Some("RuntimeVisibleAnnotations") | Some("RuntimeInvisibleAnnotations") => {
                    annotations.extend(self.parse_annotations_table(constant_pool));
                }
                Some("Deprecated") => {
                    deprecated = true;
                }
                _ => {
                    self.skip_bytes(length, format!("{} body", label.as_ref()))?;
                }
            }
            if self.offset < attr_start + length {
                let remaining = attr_start + length - self.offset;
                self.skip_bytes(remaining, format!("{} padding", label.as_ref()))?;
            }
        }
        Ok((signature, annotations, deprecated))
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
            let (signature, annotations, attr_deprecated) =
                self.skip_attributes_collect_signature("field attributes", constant_pool)?;
            fields.push(FieldInfo {
                access_flags,
                name_index,
                descriptor_index,
                signature,
                annotations,
                deprecated: attr_deprecated || access_flags & 0x2000 != 0,
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
    /// Signature, InnerClasses, annotations and Deprecated. Unknown
    /// attributes are skipped.
    fn parse_class_attributes(
        &mut self,
        constant_pool: &[Recoverable<ConstantPoolEntry>],
    ) -> (
        Vec<InnerClassInfo>,
        Vec<BootstrapMethodInfo>,
        Option<String>,
        Vec<Annotation>,
        bool,
    ) {
        // After methods, the class attributes section follows:
        //   attributes_count: u16
        //   attributes[attributes_count]: each has name_index(u16), length(u32), info(bytes)
        let attr_count = match self.read_u16_recoverable() {
            Some(c) => c,
            None => return (Vec::new(), Vec::new(), None, Vec::new(), false),
        };

        let mut inner_classes = Vec::new();
        let mut bootstrap_methods = Vec::new();
        let mut signature = None;
        let mut annotations = Vec::new();
        let mut deprecated = false;

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
                Some("RuntimeVisibleAnnotations") | Some("RuntimeInvisibleAnnotations") => {
                    annotations.extend(self.parse_annotations_table(constant_pool));
                }
                Some("Deprecated") => {
                    deprecated = true;
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
        (
            inner_classes,
            bootstrap_methods,
            signature,
            annotations,
            deprecated,
        )
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

/// Map an annotation type/class descriptor to dotted form:
/// `Ljava/lang/Deprecated;` → `java.lang.Deprecated`,
/// `[Ljava/lang/String;` → `java.lang.String[]`.
fn field_descriptor_to_dotted(descriptor: &str) -> String {
    let raw = descriptor.trim();
    let dims = raw.bytes().take_while(|&b| b == b'[').count();
    let mut inner = &raw[dims..];
    if inner.starts_with('L') && inner.ends_with(';') {
        inner = &inner[1..inner.len() - 1];
    }
    let mut dotted = inner.replace('/', ".");
    for _ in 0..dims {
        dotted.push_str("[]");
    }
    dotted
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
    fn parses_marker_and_valued_annotations() {
        // Table: [@Deprecated, @Anno(text="hello", level=Level.HIGH)].
        let pool = vec![
            Recoverable::Missing,
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "Ljava/lang/Deprecated;".to_string(),
            )),
            Recoverable::Present(ConstantPoolEntry::Utf8("Lcom/example/Anno;".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("text".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("level".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("hello".to_string())),
            Recoverable::Present(ConstantPoolEntry::String { string_index: 5 }),
            Recoverable::Present(ConstantPoolEntry::Utf8("Lcom/example/Level;".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("HIGH".to_string())),
        ];
        let bytes = [
            0x00, 0x02, // count = 2
            0x00, 0x01, 0x00, 0x00, // @Deprecated, no pairs
            0x00, 0x02, 0x00, 0x02, // @Anno, 2 pairs
            0x00, 0x03, b's', 0x00, 0x06, // text = "hello"
            0x00, 0x04, b'e', 0x00, 0x07, 0x00, 0x08, // level = Level.HIGH
        ];
        let mut parser = ClassFileParser::new(&bytes);
        let annotations = parser.parse_annotations_table(&pool);
        assert_eq!(annotations.len(), 2);
        assert_eq!(annotations[0].render_short(), "Deprecated");
        assert_eq!(
            annotations[1].render_short(),
            "Anno(text=\"hello\", level=Level.HIGH)"
        );
        let refs = annotations[1].referenced_types();
        assert!(refs.contains(&"com.example.Anno".to_string()));
        assert!(refs.contains(&"com.example.Level".to_string()));
    }

    #[test]
    fn truncated_annotations_degrade_gracefully() {
        let pool: Vec<Recoverable<ConstantPoolEntry>> = vec![Recoverable::Missing];
        let bytes = [0x00, 0x01, 0x00]; // count=1, then EOF mid-entry
        let mut parser = ClassFileParser::new(&bytes);
        let annotations = parser.parse_annotations_table(&pool);
        assert!(annotations.is_empty());
        assert!(!parser.into_diagnostics().is_empty());
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
