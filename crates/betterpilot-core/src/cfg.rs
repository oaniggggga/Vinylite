use crate::bytecode::{Instruction, InstructionKind};

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: usize,
    pub start_offset: usize,
    pub end_offset: usize,
    pub instructions: Vec<Instruction>,
    pub successors: Vec<usize>,
    pub reachable: bool,
}

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    pub blocks: Vec<BasicBlock>,
}

impl ControlFlowGraph {
    /// Bytecode offsets reachable from the entry block plus any additional
    /// entry points (exception handler pcs). Used to drop statically dead
    /// trap code (e.g. unreachable `athrow`s obfuscators plant between
    /// switch arms) before lowering — while keeping handler bodies alive.
    pub fn reachable_offsets(
        &self,
        extra_entry_offsets: &[usize],
    ) -> std::collections::HashSet<usize> {
        use std::collections::HashSet;
        let mut seen_blocks = vec![false; self.blocks.len()];
        let mut worklist: Vec<usize> = Vec::new();
        if !self.blocks.is_empty() {
            worklist.push(0);
        }
        for pc in extra_entry_offsets {
            if let Some(block) = self
                .blocks
                .iter()
                .find(|b| b.start_offset <= *pc && *pc < b.end_offset)
            {
                worklist.push(block.id);
            }
        }
        let mut offsets = HashSet::new();
        while let Some(bid) = worklist.pop() {
            if bid >= self.blocks.len() || seen_blocks[bid] {
                continue;
            }
            seen_blocks[bid] = true;
            let block = &self.blocks[bid];
            for ins in &block.instructions {
                offsets.insert(ins.offset);
            }
            for successor in &block.successors {
                worklist.push(*successor);
            }
        }
        offsets
    }
    pub fn build_from_instructions(instructions: &[Instruction]) -> Self {
        use std::collections::{BTreeSet, HashMap};

        let mut starts = BTreeSet::new();
        if let Some(first) = instructions.first() {
            starts.insert(first.offset);
        }

        for instr in instructions.iter() {
            for target in instr.kind.branch_targets() {
                if target >= 0 {
                    starts.insert(target as usize);
                }
            }

            if matches!(
                instr.kind,
                InstructionKind::If { .. } | InstructionKind::Malformed { .. }
            ) && let Some(off) = next_offset(instructions, instr.offset)
            {
                starts.insert(off);
            }
        }

        let mut start_list: Vec<usize> = starts.into_iter().collect();
        start_list.sort();

        let mut blocks: Vec<BasicBlock> = Vec::new();

        for (i, &start) in start_list.iter().enumerate() {
            let end = if i + 1 < start_list.len() {
                start_list[i + 1]
            } else {
                instructions
                    .last()
                    .map(|ins| ins.offset + ins.length)
                    .unwrap_or(start)
            };

            let mut instrs = Vec::new();
            for ins in instructions
                .iter()
                .filter(|ins| ins.offset >= start && ins.offset < end)
            {
                instrs.push(ins.clone());
            }

            let block = BasicBlock {
                id: blocks.len(),
                start_offset: start,
                end_offset: end,
                instructions: instrs,
                successors: Vec::new(),
                reachable: false,
            };
            blocks.push(block);
        }

        let offset_to_block = {
            let mut m = HashMap::new();
            for b in &blocks {
                m.insert(b.start_offset, b.id);
            }
            m
        };

        // compute successors
        for b in blocks.iter_mut() {
            if let Some(last) = b.instructions.last() {
                for target in last.kind.branch_targets() {
                    if target >= 0
                        && let Some(&bid) = offset_to_block.get(&(target as usize))
                    {
                        b.successors.push(bid);
                    }
                }

                if last.kind.has_fallthrough()
                    && let Some(&fid) = offset_to_block.get(&b.end_offset)
                    && !b.successors.contains(&fid)
                {
                    b.successors.push(fid);
                }
            }
        }

        // reachability from entry block (start at first block)
        if !blocks.is_empty() {
            let mut stack = vec![0usize];
            let mut visited = vec![false; blocks.len()];
            while let Some(bid) = stack.pop() {
                if visited[bid] {
                    continue;
                }
                visited[bid] = true;
                blocks[bid].reachable = true;
                for &s in &blocks[bid].successors {
                    if !visited[s] {
                        stack.push(s);
                    }
                }
            }
        }

        ControlFlowGraph { blocks }
    }
}

fn next_offset(instructions: &[Instruction], offset: usize) -> Option<usize> {
    for ins in instructions.iter() {
        if ins.offset > offset {
            return Some(ins.offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Instruction, InstructionKind};

    #[test]
    fn builds_cfg_and_marks_reachable_blocks() {
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Iconst(0),
            },
            Instruction {
                offset: 1,
                length: 3,
                kind: InstructionKind::If {
                    opcode: 0x99,
                    target: 4,
                },
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::Iconst(2),
            },
            Instruction {
                offset: 5,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
        ];

        let cfg = ControlFlowGraph::build_from_instructions(&instructions);
        assert!(!cfg.blocks.is_empty());
        assert!(cfg.blocks[0].reachable);
        let has_if_successors = cfg.blocks.iter().any(|b| !b.successors.is_empty());
        assert!(has_if_successors);
    }
}
