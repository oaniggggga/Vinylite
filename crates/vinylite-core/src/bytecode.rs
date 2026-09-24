use crate::diagnostic::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub offset: usize,
    pub length: usize,
    pub kind: InstructionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionKind {
    Nop,
    AconstNull,
    Iconst(i32),
    Lconst(i64),
    Fconst(u8),
    Dconst(u8),
    Bipush(i8),
    Sipush(i16),
    Ldc(u16),
    Load {
        ty: LoadStoreType,
        index: u16,
    },
    Store {
        ty: LoadStoreType,
        index: u16,
    },
    Iinc {
        index: u16,
        amount: i16,
    },
    Stack(StackOp),
    ArrayLoad(LoadStoreType),
    ArrayStore(LoadStoreType),
    Arithmetic {
        opcode: u8,
    },
    Convert {
        opcode: u8,
    },
    Compare {
        opcode: u8,
    },
    If {
        opcode: u8,
        target: i32,
    },
    Goto(i32),
    Jsr(i32),
    Ret(u16),
    TableSwitch {
        default: i32,
        low: i32,
        high: i32,
        targets: Vec<i32>,
    },
    LookupSwitch {
        default: i32,
        pairs: Vec<(i32, i32)>,
    },
    Return(ReturnType),
    Invoke {
        opcode: u8,
        cp_index: u16,
    },
    Field {
        opcode: u8,
        cp_index: u16,
    },
    Type {
        opcode: u8,
        cp_index: u16,
    },
    NewArray(u8),
    MultiANewArray {
        cp_index: u16,
        dimensions: u8,
    },
    Wide,
    Throw,
    MonitorEnter,
    MonitorExit,
    ArrayLength,
    Unknown(u8),
    Malformed {
        opcode: Option<u8>,
        bytes: Vec<u8>,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStoreType {
    Int,
    Long,
    Float,
    Double,
    Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnType {
    Void,
    Int,
    Long,
    Float,
    Double,
    Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackOp {
    Pop,
    Pop2,
    Dup,
    DupX1,
    DupX2,
    Dup2,
    Dup2X1,
    Dup2X2,
    Swap,
}

pub fn decode_method_code(code: &[u8], diagnostics: &mut Vec<Diagnostic>) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    let mut offset = 0;

    while offset < code.len() {
        let opcode = code[offset];
        let instruction_offset = offset;
        offset += 1;

        let kind = match opcode {
            0x00 => InstructionKind::Nop,
            0x01 => InstructionKind::AconstNull,
            0x02..=0x08 => InstructionKind::Iconst((opcode as i32) - 0x03),
            0x09..=0x0a => InstructionKind::Lconst((opcode - 0x09).into()),
            0x0b..=0x0d => InstructionKind::Fconst(opcode - 0x0b),
            0x0e..=0x0f => InstructionKind::Dconst(opcode - 0x0e),
            0x10 => match read_i8(code, &mut offset) {
                Some(value) => InstructionKind::Bipush(value),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x11 => match read_i16(code, &mut offset) {
                Some(value) => InstructionKind::Sipush(value),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x12 => match read_u8(code, &mut offset) {
                Some(index) => InstructionKind::Ldc(index.into()),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x13 | 0x14 => match read_u16(code, &mut offset) {
                Some(index) => InstructionKind::Ldc(index),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x15..=0x19 => match read_u8(code, &mut offset) {
                Some(index) => InstructionKind::Load {
                    ty: load_store_type(opcode),
                    index: index.into(),
                },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x1a..=0x2d => compact_load_store(opcode),
            0x36..=0x3a => match read_u8(code, &mut offset) {
                Some(index) => InstructionKind::Store {
                    ty: load_store_type(opcode),
                    index: index.into(),
                },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x3b..=0x4e => compact_load_store(opcode),
            0x2e..=0x35 => InstructionKind::ArrayLoad(array_type(opcode)),
            0x4f..=0x56 => InstructionKind::ArrayStore(array_type(opcode)),
            0x57 => InstructionKind::Stack(StackOp::Pop),
            0x58 => InstructionKind::Stack(StackOp::Pop2),
            0x59 => InstructionKind::Stack(StackOp::Dup),
            0x5a => InstructionKind::Stack(StackOp::DupX1),
            0x5b => InstructionKind::Stack(StackOp::DupX2),
            0x5c => InstructionKind::Stack(StackOp::Dup2),
            0x5d => InstructionKind::Stack(StackOp::Dup2X1),
            0x5e => InstructionKind::Stack(StackOp::Dup2X2),
            0x5f => InstructionKind::Stack(StackOp::Swap),
            0x60..=0x83 => InstructionKind::Arithmetic { opcode },
            0x84 => match (read_u8(code, &mut offset), read_i8(code, &mut offset)) {
                (Some(index), Some(amount)) => InstructionKind::Iinc {
                    index: index.into(),
                    amount: amount.into(),
                },
                _ => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0x85..=0x93 => InstructionKind::Convert { opcode },
            0x94..=0x98 => InstructionKind::Compare { opcode },
            0x99..=0xa6 | 0xc6 | 0xc7 => match read_i16(code, &mut offset) {
                Some(delta) => InstructionKind::If {
                    opcode,
                    target: instruction_offset as i32 + delta as i32,
                },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xa7 => match read_i16(code, &mut offset) {
                Some(delta) => InstructionKind::Goto(instruction_offset as i32 + delta as i32),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xa8 => match read_i16(code, &mut offset) {
                Some(delta) => InstructionKind::Jsr(instruction_offset as i32 + delta as i32),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xa9 => match read_u8(code, &mut offset) {
                Some(index) => InstructionKind::Ret(index.into()),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xaa => match read_table_switch(code, instruction_offset, &mut offset) {
                Some((default, low, high, targets)) => InstructionKind::TableSwitch {
                    default,
                    low,
                    high,
                    targets,
                },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xab => match read_lookup_switch(code, instruction_offset, &mut offset) {
                Some((default, pairs)) => InstructionKind::LookupSwitch { default, pairs },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xac => InstructionKind::Return(ReturnType::Int),
            0xad => InstructionKind::Return(ReturnType::Long),
            0xae => InstructionKind::Return(ReturnType::Float),
            0xaf => InstructionKind::Return(ReturnType::Double),
            0xb0 => InstructionKind::Return(ReturnType::Reference),
            0xb1 => InstructionKind::Return(ReturnType::Void),
            0xb2..=0xb5 => match read_u16(code, &mut offset) {
                Some(cp_index) => InstructionKind::Field { opcode, cp_index },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xb6..=0xb8 => match read_u16(code, &mut offset) {
                Some(cp_index) => InstructionKind::Invoke { opcode, cp_index },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xb9 => match read_invoke_interface(code, &mut offset) {
                Some(cp_index) => InstructionKind::Invoke { opcode, cp_index },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xba => match read_invoke_dynamic(code, &mut offset) {
                Some(cp_index) => InstructionKind::Invoke { opcode, cp_index },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xbb | 0xbd | 0xc0 | 0xc1 => match read_u16(code, &mut offset) {
                Some(cp_index) => InstructionKind::Type { opcode, cp_index },
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xbc => match read_u8(code, &mut offset) {
                Some(array_type) => InstructionKind::NewArray(array_type),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xbe => InstructionKind::ArrayLength,
            0xbf => InstructionKind::Throw,
            0xc2 => InstructionKind::MonitorEnter,
            0xc3 => InstructionKind::MonitorExit,
            0xc4 => match read_wide(code, &mut offset) {
                Some(kind) => kind,
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xc5 => match (read_u16(code, &mut offset), read_u8(code, &mut offset)) {
                (Some(cp_index), Some(dimensions)) => InstructionKind::MultiANewArray {
                    cp_index,
                    dimensions,
                },
                _ => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xc8 => match read_i32(code, &mut offset) {
                Some(delta) => InstructionKind::Goto(instruction_offset as i32 + delta),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xc9 => match read_i32(code, &mut offset) {
                Some(delta) => InstructionKind::Jsr(instruction_offset as i32 + delta),
                None => malformed_truncated(opcode, instruction_offset, code, diagnostics),
            },
            0xca | 0xfe | 0xff => InstructionKind::Unknown(opcode),
            _ => {
                diagnostics.push(Diagnostic::warning(
                    instruction_offset,
                    format!("unsupported opcode 0x{opcode:02x}; preserving as unknown instruction"),
                ));
                InstructionKind::Unknown(opcode)
            }
        };

        let length = offset.saturating_sub(instruction_offset);

        instructions.push(Instruction {
            offset: instruction_offset,
            length,
            kind,
        });
    }

    instructions
}

impl InstructionKind {
    pub fn branch_targets(&self) -> Vec<i32> {
        match self {
            InstructionKind::If { target, .. }
            | InstructionKind::Goto(target)
            | InstructionKind::Jsr(target) => vec![*target],
            InstructionKind::TableSwitch {
                default, targets, ..
            } => {
                let mut all = Vec::with_capacity(targets.len() + 1);
                all.push(*default);
                all.extend(targets.iter().copied());
                all
            }
            InstructionKind::LookupSwitch { default, pairs } => {
                let mut all = Vec::with_capacity(pairs.len() + 1);
                all.push(*default);
                all.extend(pairs.iter().map(|(_, target)| *target));
                all
            }
            _ => Vec::new(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, InstructionKind::Return(_) | InstructionKind::Throw)
    }

    pub fn has_fallthrough(&self) -> bool {
        !matches!(
            self,
            InstructionKind::Goto(_)
                | InstructionKind::Return(_)
                | InstructionKind::Throw
                | InstructionKind::TableSwitch { .. }
                | InstructionKind::LookupSwitch { .. }
        )
    }
}

fn malformed_truncated(
    opcode: u8,
    instruction_offset: usize,
    code: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
) -> InstructionKind {
    diagnostics.push(Diagnostic::error(
        instruction_offset,
        format!("truncated operands for opcode 0x{opcode:02x}"),
    ));

    InstructionKind::Malformed {
        opcode: Some(opcode),
        bytes: code[instruction_offset..].to_vec(),
        reason: "truncated operands".to_string(),
    }
}

fn compact_load_store(opcode: u8) -> InstructionKind {
    match opcode {
        0x1a..=0x1d => InstructionKind::Load {
            ty: LoadStoreType::Int,
            index: (opcode - 0x1a).into(),
        },
        0x1e..=0x21 => InstructionKind::Load {
            ty: LoadStoreType::Long,
            index: (opcode - 0x1e).into(),
        },
        0x22..=0x25 => InstructionKind::Load {
            ty: LoadStoreType::Float,
            index: (opcode - 0x22).into(),
        },
        0x26..=0x29 => InstructionKind::Load {
            ty: LoadStoreType::Double,
            index: (opcode - 0x26).into(),
        },
        0x2a..=0x2d => InstructionKind::Load {
            ty: LoadStoreType::Reference,
            index: (opcode - 0x2a).into(),
        },
        0x3b..=0x3e => InstructionKind::Store {
            ty: LoadStoreType::Int,
            index: (opcode - 0x3b).into(),
        },
        0x3f..=0x42 => InstructionKind::Store {
            ty: LoadStoreType::Long,
            index: (opcode - 0x3f).into(),
        },
        0x43..=0x46 => InstructionKind::Store {
            ty: LoadStoreType::Float,
            index: (opcode - 0x43).into(),
        },
        0x47..=0x4a => InstructionKind::Store {
            ty: LoadStoreType::Double,
            index: (opcode - 0x47).into(),
        },
        0x4b..=0x4e => InstructionKind::Store {
            ty: LoadStoreType::Reference,
            index: (opcode - 0x4b).into(),
        },
        _ => InstructionKind::Unknown(opcode),
    }
}

fn array_type(opcode: u8) -> LoadStoreType {
    match opcode {
        0x2e | 0x4f => LoadStoreType::Int,
        0x2f | 0x50 => LoadStoreType::Long,
        0x30 | 0x51 => LoadStoreType::Float,
        0x31 | 0x52 => LoadStoreType::Double,
        _ => LoadStoreType::Reference,
    }
}

fn load_store_type(opcode: u8) -> LoadStoreType {
    match opcode {
        0x15 | 0x36 => LoadStoreType::Int,
        0x16 | 0x37 => LoadStoreType::Long,
        0x17 | 0x38 => LoadStoreType::Float,
        0x18 | 0x39 => LoadStoreType::Double,
        _ => LoadStoreType::Reference,
    }
}

fn read_invoke_interface(code: &[u8], offset: &mut usize) -> Option<u16> {
    let cp_index = read_u16(code, offset)?;
    read_u8(code, offset)?;
    read_u8(code, offset)?;
    Some(cp_index)
}

fn read_invoke_dynamic(code: &[u8], offset: &mut usize) -> Option<u16> {
    let cp_index = read_u16(code, offset)?;
    read_u8(code, offset)?;
    read_u8(code, offset)?;
    Some(cp_index)
}

fn read_wide(code: &[u8], offset: &mut usize) -> Option<InstructionKind> {
    let opcode = read_u8(code, offset)?;
    match opcode {
        0x15..=0x19 => Some(InstructionKind::Load {
            ty: load_store_type(opcode),
            index: read_u16(code, offset)?,
        }),
        0x36..=0x3a => Some(InstructionKind::Store {
            ty: load_store_type(opcode),
            index: read_u16(code, offset)?,
        }),
        0x84 => Some(InstructionKind::Iinc {
            index: read_u16(code, offset)?,
            amount: read_i16(code, offset)?,
        }),
        0xa9 => Some(InstructionKind::Ret(read_u16(code, offset)?)),
        _ => Some(InstructionKind::Wide),
    }
}

fn read_table_switch(
    code: &[u8],
    instruction_offset: usize,
    offset: &mut usize,
) -> Option<(i32, i32, i32, Vec<i32>)> {
    align_switch_offset(instruction_offset, offset);
    let default_delta = read_i32(code, offset)?;
    let low = read_i32(code, offset)?;
    let high = read_i32(code, offset)?;
    if high < low {
        return None;
    }
    let count = (high - low + 1) as usize;
    let mut targets = Vec::with_capacity(count);
    for _ in 0..count {
        targets.push(instruction_offset as i32 + read_i32(code, offset)?);
    }
    Some((
        instruction_offset as i32 + default_delta,
        low,
        high,
        targets,
    ))
}

fn read_lookup_switch(
    code: &[u8],
    instruction_offset: usize,
    offset: &mut usize,
) -> Option<(i32, Vec<(i32, i32)>)> {
    align_switch_offset(instruction_offset, offset);
    let default_delta = read_i32(code, offset)?;
    let pair_count = read_i32(code, offset)?;
    if pair_count < 0 {
        return None;
    }
    let mut pairs = Vec::with_capacity(pair_count as usize);
    for _ in 0..pair_count {
        let key = read_i32(code, offset)?;
        let target = instruction_offset as i32 + read_i32(code, offset)?;
        pairs.push((key, target));
    }
    Some((instruction_offset as i32 + default_delta, pairs))
}

fn align_switch_offset(instruction_offset: usize, offset: &mut usize) {
    let padding = (4 - ((instruction_offset + 1) % 4)) % 4;
    *offset += padding;
}

fn read_u8(bytes: &[u8], offset: &mut usize) -> Option<u8> {
    let value = *bytes.get(*offset)?;
    *offset += 1;
    Some(value)
}

fn read_i8(bytes: &[u8], offset: &mut usize) -> Option<i8> {
    Some(read_u8(bytes, offset)? as i8)
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> Option<u16> {
    let high = read_u8(bytes, offset)? as u16;
    let low = read_u8(bytes, offset)? as u16;
    Some((high << 8) | low)
}

fn read_i16(bytes: &[u8], offset: &mut usize) -> Option<i16> {
    Some(read_u16(bytes, offset)? as i16)
}

fn read_i32(bytes: &[u8], offset: &mut usize) -> Option<i32> {
    let b0 = read_u8(bytes, offset)?;
    let b1 = read_u8(bytes, offset)?;
    let b2 = read_u8(bytes, offset)?;
    let b3 = read_u8(bytes, offset)?;
    Some(i32::from_be_bytes([b0, b1, b2, b3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nop_spam_without_failing() {
        let mut diagnostics = Vec::new();
        let instructions = decode_method_code(&[0x00, 0x00, 0x03, 0xac], &mut diagnostics);

        assert_eq!(instructions.len(), 4);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn emits_malformed_instruction_for_truncated_operand() {
        let mut diagnostics = Vec::new();
        let instructions = decode_method_code(&[0x10], &mut diagnostics);

        assert!(matches!(
            instructions[0].kind,
            InstructionKind::Malformed { .. }
        ));
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consumes_wide_constant_pool_operands() {
        let mut diagnostics = Vec::new();
        let instructions = decode_method_code(&[0x13, 0x01, 0x02, 0xb1], &mut diagnostics);

        assert_eq!(instructions.len(), 2);
        assert_eq!(instructions[0].length, 3);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn consumes_tableswitch_padding_and_targets() {
        let mut diagnostics = Vec::new();
        let instructions = decode_method_code(
            &[
                0xaa, 0x00, 0x00, 0x00, // opcode + padding
                0x00, 0x00, 0x00, 0x08, // default
                0x00, 0x00, 0x00, 0x01, // low
                0x00, 0x00, 0x00, 0x02, // high
                0x00, 0x00, 0x00, 0x04, // target 1
                0x00, 0x00, 0x00, 0x06, // target 2
            ],
            &mut diagnostics,
        );

        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].length, 24);
        assert!(diagnostics.is_empty());
    }
}
