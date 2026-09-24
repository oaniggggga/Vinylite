use std::collections::HashSet;

use crate::cfg::ControlFlowGraph;

/// Represents a detected loop in the control flow graph.
#[derive(Debug, Clone)]
pub struct Loop {
    /// The header block (entry point of the loop)
    pub header: usize,
    /// The latch block (the block with the back edge)
    pub latch: usize,
    /// All block IDs in the loop (including header and latch)
    pub blocks: HashSet<usize>,
    /// The block that exits the loop (first block after the loop)
    pub exit: Option<usize>,
    /// The condition block (block containing the loop condition)
    pub condition_block: Option<usize>,
    /// The body blocks (blocks in the loop excluding header and latch)
    pub body: HashSet<usize>,
    /// Whether the condition is at the top (while) or bottom (do-while)
    pub condition_at_top: bool,
    /// The update block (for loops - block that increments the counter)
    pub update_block: Option<usize>,
    /// The initializer block (for loops - block before the loop that sets up the counter)
    pub init_block: Option<usize>,
}

/// Detect natural loops in the control flow graph.
pub fn detect_loops(cfg: &ControlFlowGraph) -> Vec<Loop> {
    let mut loops = Vec::new();

    // Find back edges using DFS
    let back_edges = find_back_edges(cfg);

    for (latch, header) in back_edges {
        let loop_blocks = find_natural_loop(cfg, header, latch);

        // Find exit block
        let exit = find_exit_block(cfg, &loop_blocks);

        // Classify the loop
        let condition_at_top = is_condition_at_top(cfg, header, &loop_blocks);

        // Find condition block
        let condition_block = if condition_at_top {
            Some(header)
        } else {
            find_condition_block(cfg, &loop_blocks, latch)
        };

        // Find body blocks (everything except header and latch)
        let body: HashSet<usize> = loop_blocks
            .iter()
            .copied()
            .filter(|&b| b != header && b != latch)
            .collect();

        // Detect for-loop pattern
        let (init_block, update_block) = detect_for_loop_pattern(cfg, header, latch, &loop_blocks);

        loops.push(Loop {
            header,
            latch,
            blocks: loop_blocks,
            exit,
            condition_block,
            body,
            condition_at_top,
            update_block,
            init_block,
        });
    }

    // Sort loops by depth (innermost first)
    loops.sort_by(|a, b| b.blocks.len().cmp(&a.blocks.len()));

    loops
}

/// Find all back edges in the CFG using DFS.
fn find_back_edges(cfg: &ControlFlowGraph) -> Vec<(usize, usize)> {
    let mut back_edges = Vec::new();
    let mut visited = vec![false; cfg.blocks.len()];
    let mut in_stack = vec![false; cfg.blocks.len()];

    for i in 0..cfg.blocks.len() {
        if !visited[i] {
            dfs_find_back_edges(cfg, i, &mut visited, &mut in_stack, &mut back_edges);
        }
    }

    back_edges
}

fn dfs_find_back_edges(
    cfg: &ControlFlowGraph,
    block: usize,
    visited: &mut [bool],
    in_stack: &mut [bool],
    back_edges: &mut Vec<(usize, usize)>,
) {
    visited[block] = true;
    in_stack[block] = true;

    for &successor in &cfg.blocks[block].successors {
        if !visited[successor] {
            dfs_find_back_edges(cfg, successor, visited, in_stack, back_edges);
        } else if in_stack[successor] {
            // Back edge found
            back_edges.push((block, successor));
        }
    }

    in_stack[block] = false;
}

/// Find all blocks in the natural loop defined by the back edge (latch -> header).
fn find_natural_loop(cfg: &ControlFlowGraph, header: usize, latch: usize) -> HashSet<usize> {
    let mut loop_blocks = HashSet::new();
    loop_blocks.insert(header);

    if header == latch {
        return loop_blocks;
    }

    loop_blocks.insert(latch);

    // Worklist algorithm: start from latch, find all predecessors that can reach header
    let mut worklist = vec![latch];
    let mut visited = HashSet::new();
    visited.insert(latch);

    while let Some(block) = worklist.pop() {
        for pred in get_predecessors(cfg, block) {
            if !visited.contains(&pred) {
                visited.insert(pred);
                loop_blocks.insert(pred);
                worklist.push(pred);
            }
        }
    }

    loop_blocks
}

/// Get predecessors of a block.
fn get_predecessors(cfg: &ControlFlowGraph, block: usize) -> Vec<usize> {
    cfg.blocks
        .iter()
        .filter(|b| b.successors.contains(&block))
        .map(|b| b.id)
        .collect()
}

/// Find the exit block of a loop.
fn find_exit_block(cfg: &ControlFlowGraph, loop_blocks: &HashSet<usize>) -> Option<usize> {
    for &block_id in loop_blocks {
        let block = &cfg.blocks[block_id];
        for &successor in &block.successors {
            if !loop_blocks.contains(&successor) {
                return Some(successor);
            }
        }
    }
    None
}

/// Check if the loop condition is at the top (while loop) or bottom (do-while).
fn is_condition_at_top(
    cfg: &ControlFlowGraph,
    header: usize,
    loop_blocks: &HashSet<usize>,
) -> bool {
    // A while loop has the condition at the header
    // The header has two successors: one in the loop body, one is the exit
    let header_block = &cfg.blocks[header];

    if header_block.successors.len() != 2 {
        return false;
    }

    let has_exit_successor = header_block
        .successors
        .iter()
        .any(|&s| !loop_blocks.contains(&s));
    let has_body_successor = header_block
        .successors
        .iter()
        .any(|&s| loop_blocks.contains(&s));

    has_exit_successor && has_body_successor
}

/// Find the block containing the loop condition for do-while loops.
fn find_condition_block(
    cfg: &ControlFlowGraph,
    loop_blocks: &HashSet<usize>,
    latch: usize,
) -> Option<usize> {
    // In a do-while loop, the condition is at the latch
    let latch_block = &cfg.blocks[latch];

    if latch_block.successors.len() == 2 {
        let has_exit = latch_block
            .successors
            .iter()
            .any(|&s| !loop_blocks.contains(&s));

        if has_exit {
            return Some(latch);
        }
    }

    // Look for a block that has both a loop-internal successor and an exit successor
    for &block_id in loop_blocks {
        let block = &cfg.blocks[block_id];
        if block.successors.len() == 2 {
            let has_exit = block.successors.iter().any(|&s| !loop_blocks.contains(&s));
            if has_exit {
                return Some(block_id);
            }
        }
    }

    None
}

/// Detect for-loop pattern: init -> condition -> body -> update -> condition
fn detect_for_loop_pattern(
    cfg: &ControlFlowGraph,
    header: usize,
    latch: usize,
    loop_blocks: &HashSet<usize>,
) -> (Option<usize>, Option<usize>) {
    // For a for-loop, we expect:
    // - An initializer block before the loop
    // - A condition block (header)
    // - A body block
    // - An update block (latch) that increments and jumps back to condition

    // Check if latch has a single successor that is the header
    let latch_block = &cfg.blocks[latch];
    if latch_block.successors.len() != 1 || latch_block.successors[0] != header {
        return (None, None);
    }

    // Find the initializer block (predecessor of header that is not in the loop)
    let init_block = get_predecessors(cfg, header)
        .into_iter()
        .find(|&pred| !loop_blocks.contains(&pred));

    // The update block is typically the latch
    let update_block = Some(latch);

    (init_block, update_block)
}

/// Get the loop type as a string for debugging.
pub fn loop_type_string(loops: &[Loop]) -> Vec<String> {
    loops
        .iter()
        .map(|l| {
            if l.update_block.is_some() && l.init_block.is_some() {
                "for".to_string()
            } else if l.condition_at_top {
                "while".to_string()
            } else {
                "do-while".to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Instruction, InstructionKind};

    fn instr(offset: usize, kind: InstructionKind) -> Instruction {
        Instruction {
            offset,
            length: 1,
            kind,
        }
    }

    #[test]
    fn detects_while_loop_back_edge() {
        // 0: iconst_0 → 1: ifeq 4 (exit) → 2: iinc → 3: goto 1 → 4: return
        let instructions = vec![
            instr(0, InstructionKind::Iconst(0)),
            instr(1, InstructionKind::If { opcode: 0x99, target: 4 }),
            instr(2, InstructionKind::Iinc { index: 1, amount: 1 }),
            instr(3, InstructionKind::Goto(1)),
            instr(4, InstructionKind::Return(crate::bytecode::ReturnType::Void)),
        ];
        let cfg = ControlFlowGraph::build_from_instructions(&instructions);
        let loops = detect_loops(&cfg);
        assert_eq!(loops.len(), 1);
        assert!(loops[0].condition_at_top);
        assert_eq!(loop_type_string(&loops), vec!["while".to_string()]);
    }
}
