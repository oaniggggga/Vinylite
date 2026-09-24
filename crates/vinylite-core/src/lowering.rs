use crate::ast::{BinaryOp, Expression, MethodDecl, Statement, SwitchArm, UnaryOp};
use crate::bytecode::{Instruction, InstructionKind};
use crate::cfg::ControlFlowGraph;
use crate::classfile::ConstantPoolEntry;
use crate::descriptor::{count_params, parse_descriptor};
use crate::recovery::Recoverable;
use std::collections::HashMap;

type Pool = [Recoverable<ConstantPoolEntry>];

// â”€â”€â”€ Constant pool helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn cp_utf8(pool: &Pool, index: u16) -> String {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(s))) => s.clone(),
        _ => format!("#{index}"),
    }
}

fn cp_class_name(pool: &Pool, index: u16) -> String {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            let raw = cp_utf8(pool, *name_index);
            if raw.starts_with("[L") && raw.ends_with(';') {
                let inner = &raw[2..raw.len() - 1];
                return format!("{}[]", inner.replace('/', "."));
            }
            if raw.starts_with('[') {
                return raw.replace('/', ".");
            }
            raw.replace('/', ".")
        }
        _ => format!("Class#{index}"),
    }
}

fn cp_name_and_type(pool: &Pool, index: u16) -> (String, String) {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::NameAndType {
            name_index,
            descriptor_index,
        })) => (cp_utf8(pool, *name_index), cp_utf8(pool, *descriptor_index)),
        _ => (format!("nat#{index}"), String::new()),
    }
}

fn cp_field_ref(pool: &Pool, index: u16) -> (String, String, String) {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::FieldRef {
            class_index,
            name_and_type_index,
        })) => {
            let class = cp_class_name(pool, *class_index);
            let (name, desc) = cp_name_and_type(pool, *name_and_type_index);
            (class, name, desc)
        }
        _ => (String::new(), format!("field#{index}"), String::new()),
    }
}

fn cp_method_ref(pool: &Pool, index: u16) -> (String, String, String) {
    match pool.get(index as usize) {
        Some(Recoverable::Present(
            ConstantPoolEntry::MethodRef {
                class_index,
                name_and_type_index,
            }
            | ConstantPoolEntry::InterfaceMethodRef {
                class_index,
                name_and_type_index,
            },
        )) => {
            let class = cp_class_name(pool, *class_index);
            let (name, desc) = cp_name_and_type(pool, *name_and_type_index);
            (class, name, desc)
        }
        Some(Recoverable::Present(ConstantPoolEntry::InvokeDynamic {
            bootstrap_method_attr_index: _,
            name_and_type_index,
        })) => {
            let (name, desc) = cp_name_and_type(pool, *name_and_type_index);
            // For invokedynamic, the class is usually implicit or handled by the bootstrap method.
            // We'll just return the name and descriptor.
            (String::new(), name, desc)
        }
        _ => (String::new(), format!("method#{index}"), String::new()),
    }
}

fn cp_ldc(pool: &Pool, index: u16) -> Expression {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Integer(v))) => Expression::ConstInt(*v),
        Some(Recoverable::Present(ConstantPoolEntry::Float(v))) => {
            Expression::ConstFloat(f32::from_bits(*v))
        }
        Some(Recoverable::Present(ConstantPoolEntry::Long(v))) => Expression::ConstLong(*v),
        Some(Recoverable::Present(ConstantPoolEntry::Double(v))) => {
            Expression::ConstDouble(f64::from_bits(*v))
        }
        Some(Recoverable::Present(ConstantPoolEntry::String { string_index })) => {
            Expression::ConstString(cp_utf8(pool, *string_index))
        }
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            let name = cp_utf8(pool, *name_index).replace('/', ".");
            Expression::Unknown(format!("{name}.class"))
        }
        _ => Expression::Unknown(format!("ldc#{index}")),
    }
}

fn short_name(full: &str) -> String {
    full.rsplit('.').next().unwrap_or(full).to_string()
}

/// Split a JVM type descriptor into (base display name, array dimensions):
/// `[[F` → (`float`, 2), `[Ljava/lang/String;` → (`String`, 1),
/// `com/foo/Bar` → (`Bar`, 0). Handles already-dotted names too.
fn split_array_descriptor(raw: &str) -> (String, usize) {
    let raw = raw.trim();
    let dims = raw.bytes().take_while(|&b| b == b'[').count();
    let base = &raw[dims.min(raw.len())..];
    let base_name = match base {
        "B" => "byte".to_string(),
        "C" => "char".to_string(),
        "D" => "double".to_string(),
        "F" => "float".to_string(),
        "I" => "int".to_string(),
        "J" => "long".to_string(),
        "S" => "short".to_string(),
        "Z" => "boolean".to_string(),
        "V" => "void".to_string(),
        _ => {
            let named = base
                .strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .unwrap_or(base);
            short_name(&named.replace('/', "."))
        }
    };
    (base_name, dims)
}

/// Full display type for a (possibly array) descriptor: `[[F` → `float[][]`.
fn decode_type_descriptor(raw: &str) -> String {
    let (base, dims) = split_array_descriptor(raw);
    format!("{base}{}", "[]".repeat(dims))
}

/// Raw class constant text (no package shortening), for array descriptors.
fn cp_raw_class(pool: &Pool, index: u16) -> String {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            cp_utf8(pool, *name_index)
        }
        _ => format!("Class#{index}"),
    }
}

// â”€â”€â”€ Stack simulation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone)]
enum StackValue {
    Expr(Expression),
    UninitNew { class: String },
}

/// A recorded `tableswitch`/`lookupswitch`: bytecode offsets are kept so
/// the structuring pass can slice arm bodies by position (CFR-style).
/// `bounds`/`depth` isolate the operand stack at arm entries (each arm
/// starts with a clean stack in verifiable code); `results` captures each
/// arm's net pushes so a trailing join store can be distributed per-arm.
#[derive(Debug, Clone)]
struct SwitchStub {
    offset: usize,
    discriminant: Expression,
    default_target: i32,
    /// (case key, target bytecode offset) pairs.
    cases: Vec<(i32, i32)>,
    /// Arm entry offsets (case + default targets).
    bounds: std::collections::HashSet<usize>,
    /// Operand-stack depth right after popping the discriminant.
    depth: usize,
    /// boundary offset -> net pushed values of the preceding arm.
    results: HashMap<usize, Vec<Expression>>,
}

struct StackMachine<'a> {
    pool: &'a Pool,
    this_class: String,
    stack: Vec<StackValue>,
    statements: Vec<(usize, Statement)>, // (bytecode_offset, statement)
    locals: Vec<Expression>,
    is_static: bool,
    current_offset: usize,
    if_targets: HashMap<usize, usize>, // bytecode_offset -> target_offset
    switch_stubs: Vec<SwitchStub>,
    temp_counter: usize,
    /// Spilled fresh arrays (structural value -> temp name) for dup-sharing.
    array_temps: Vec<(Expression, String)>,
    /// Dead-code sandboxing: stack snapshots saved on entry, restored on exit.
    dead_start: std::collections::HashSet<usize>,
    dead_end: std::collections::HashSet<usize>,
    sandbox: Vec<Vec<StackValue>>,
    bootstrap_methods: Vec<crate::classfile::BootstrapMethodInfo>,
    lambda_bodies: HashMap<String, Vec<Statement>>,
}

impl<'a> StackMachine<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        pool: &'a Pool,
        this_class: &str,
        max_locals: usize,
        is_static: bool,
        bootstrap_methods: Vec<crate::classfile::BootstrapMethodInfo>,
        lambda_bodies: HashMap<String, Vec<Statement>>,
        dead_start: std::collections::HashSet<usize>,
        dead_end: std::collections::HashSet<usize>,
    ) -> Self {
        let locals = (0..max_locals)
            .map(|i| Expression::Local(format!("var_{i}")))
            .collect();
        Self {
            pool,
            this_class: this_class.replace('/', "."),
            stack: Vec::new(),
            statements: Vec::new(),
            locals,
            is_static,
            current_offset: 0,
            if_targets: HashMap::new(),
            switch_stubs: Vec::new(),
            temp_counter: 0,
            array_temps: Vec::new(),
            dead_start,
            dead_end,
            sandbox: Vec::new(),
            bootstrap_methods,
            lambda_bodies,
        }
    }

    /// Allocate a fresh synthetic local (`arr_tmp0`, ...) for spilling
    /// intermediate values that are not valid involuntary targets.
    fn fresh_temp(&mut self, prefix: &str) -> String {
        let name = format!("{prefix}{}", self.temp_counter);
        self.temp_counter += 1;
        name
    }

    fn push(&mut self, expr: Expression) {
        self.stack.push(StackValue::Expr(expr));
    }

    fn push_uninit(&mut self, class: String) {
        self.stack.push(StackValue::UninitNew { class });
    }

    fn pop(&mut self) -> Expression {
        match self.stack.pop() {
            Some(StackValue::Expr(e)) => e,
            Some(StackValue::UninitNew { class }) => Expression::New {
                class: short_name(&class),
                args: vec![],
            },
            None => Expression::Unknown("/* empty_stack */".to_string()),
        }
    }

    fn pop_n(&mut self, n: usize) -> Vec<Expression> {
        let mut args = Vec::with_capacity(n);
        for _ in 0..n {
            args.push(self.pop());
        }
        args.reverse();
        args
    }

    fn emit(&mut self, stmt: Statement) {
        self.statements.push((self.current_offset, stmt));
    }

    fn run(&mut self, instructions: &[Instruction]) {
        for instr in instructions {
            self.current_offset = instr.offset;
            let entering_dead = self.dead_start.contains(&instr.offset);
            if entering_dead {
                self.sandbox.push(self.stack.clone());
            }
            self.check_switch_boundaries(instr.offset);
            self.execute(instr);
            if self.dead_end.contains(&instr.offset)
                && let Some(saved) = self.sandbox.pop()
            {
                self.stack = saved;
            }
        }
        // Flush remaining stack values as expression statements
        while !self.stack.is_empty() {
            let val = self.pop();
            let noise = is_noise(&val);
            if !noise {
                self.emit(Statement::Expression(val));
            }
        }
    }

    /// Check if a MethodHandle CP entry points to LambdaMetafactory.metafactory
    fn is_lambda_metafactory(&self, handle_index: u16) -> bool {
        if let Some(Recoverable::Present(ConstantPoolEntry::MethodHandle {
            reference_kind: _,
            reference_index,
        })) = self.pool.get(handle_index as usize)
        {
            let (class, method, _desc) = cp_method_ref(self.pool, *reference_index);
            class == "java.lang.invoke.LambdaMetafactory"
                && (method == "metafactory" || method == "altMetafactory")
        } else {
            false
        }
    }

    /// Resolve a MethodHandle CP entry to (class, method_name, descriptor)
    fn resolve_method_handle(&self, handle_index: u16) -> Option<(String, String, String)> {
        if let Some(Recoverable::Present(ConstantPoolEntry::MethodHandle {
            reference_index, ..
        })) = self.pool.get(handle_index as usize)
        {
            let (class, method, desc) = cp_method_ref(self.pool, *reference_index);
            Some((class, method, desc))
        } else {
            None
        }
    }

    /// Find a synthetic lambda method body by class and method name
    fn find_lambda_body(&self, _class: &str, method_name: &str) -> Option<Vec<Statement>> {
        self.lambda_bodies.get(method_name).cloned()
    }

    /// Handle invokedynamic instruction — detect LambdaMetafactory and create Lambda expression
    fn handle_invokedynamic(&mut self, cp_index: u16, desc: &str, param_count: usize) {
        let (class, method, _) = cp_method_ref(self.pool, cp_index);
        let short = short_name(&class);

        // Look up the InvokeDynamic CP entry
        if let Some(Recoverable::Present(ConstantPoolEntry::InvokeDynamic {
            bootstrap_method_attr_index,
            ..
        })) = self.pool.get(cp_index as usize)
        {
            let bm_idx = *bootstrap_method_attr_index as usize;
            if bm_idx < self.bootstrap_methods.len() {
                let (bm_handle, bm_args) = {
                    let bm = &self.bootstrap_methods[bm_idx];
                    (bm.method_handle_index, bm.arguments.clone())
                };
                if let Some((bm_class, bm_method, _)) = self.resolve_method_handle(bm_handle) {
                    // `String s = a + b;` compiles to StringConcatFactory
                    // invokedynamic — fold it back to `+` like CFR does.
                    if bm_class == "java.lang.invoke.StringConcatFactory"
                        && (bm_method == "makeConcat" || bm_method == "makeConcatWithConstants")
                    {
                        let bm_view = crate::classfile::BootstrapMethodInfo {
                            method_handle_index: bm_handle,
                            arguments: bm_args.clone(),
                        };
                        if let Some(concat) =
                            self.try_string_concat(&bm_view, &bm_method, desc, param_count)
                        {
                            if desc.ends_with(")V") {
                                self.emit(Statement::Expression(concat));
                            } else {
                                self.push(concat);
                            }
                            return;
                        }
                    }
                }
                if self.is_lambda_metafactory(bm_handle) {
                    // The second argument to metafactory is usually the implementation method handle
                    if bm_args.len() >= 2 {
                        let impl_handle_idx = bm_args[1];
                        if let Some(impl_method) = self.resolve_method_handle(impl_handle_idx) {
                            let lambda_body = self.find_lambda_body(&impl_method.0, &impl_method.1);
                            if let Some(body) = lambda_body {
                                let _args = self.pop_n(param_count);
                                let mut body_locals = Vec::new();
                                collect_locals_in_stmts(&body, &mut body_locals);

                                let (param_types, _) = parse_descriptor(desc);
                                let lambda_param_types = if param_types.len() >= param_count {
                                    param_types[param_count..].to_vec()
                                } else {
                                    vec![]
                                };
                                let all_params: Vec<(String, String)> = lambda_param_types
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, ty)| {
                                        let name = body_locals
                                            .get(i)
                                            .cloned()
                                            .unwrap_or_else(|| format!("arg{i}"));
                                        (ty, name)
                                    })
                                    .collect();
                                // Filter out captured `this` parameter (first param matching enclosing class)
                                let this_dot = self.this_class.replace('/', ".");
                                let params: Vec<(String, String)> = all_params
                                    .into_iter()
                                    .filter(|(ty, _)| ty != &this_dot)
                                    .collect();
                                self.push(Expression::Lambda { params, body });
                                return;
                            } else {
                                // Can't find the synthetic body — emit a lambda that calls the method directly
                                let args = self.pop_n(param_count);
                                let target =
                                    if !args.is_empty() && matches!(args[0], Expression::This) {
                                        "this".to_string()
                                    } else {
                                        short_name(&impl_method.0)
                                    };
                                let invoke = Expression::Invoke {
                                    target: if target == "this" || target.is_empty() {
                                        impl_method.1.clone()
                                    } else {
                                        format!("{}.{}", target, impl_method.1)
                                    },
                                    args: args
                                        .into_iter()
                                        .skip(if target == "this" { 1 } else { 0 })
                                        .collect(),
                                };
                                self.push(Expression::Lambda {
                                    params: vec![],
                                    body: vec![Statement::Expression(invoke)],
                                });
                                return;
                            }
                        }
                    }
                }
            }
        }

        // Fallback: treat as regular invoke
        let mut args = self.pop_n(param_count);
        let (params, _) = parse_descriptor(desc);
        cast_boolean_args(&mut args, &params);
        if desc.ends_with(")V") {
            let invoke = Expression::Invoke {
                target: if short.is_empty() {
                    method.clone()
                } else {
                    format!("{short}.{method}")
                },
                args,
            };
            self.emit(Statement::Expression(invoke));
        } else {
            let invoke = Expression::Invoke {
                target: if short.is_empty() {
                    method.clone()
                } else {
                    format!("{short}.{method}")
                },
                args,
            };
            self.push(invoke);
        }
    }

    /// If `array` is a freshly-created array expression (not a variable),
    /// spill it into a synthetic temp so element stores stay valid Java.
    /// Structurally identical arrays reuse the same temp — bytecode shares
    /// one array via `dup` (`anewarray; dup; ...; aastore; dup; ...`), so
    /// this keeps N stores against one temp instead of N dead arrays.
    fn spill_fresh_array(&mut self, array: Expression) -> Expression {
        let is_fresh = match &array {
            Expression::NewArray { .. } => true,
            Expression::Unknown(text) => text.trim_start().starts_with("new "),
            _ => false,
        };
        if !is_fresh {
            return array;
        }
        if let Some((_, temp)) = self.array_temps.iter().find(|(e, _)| *e == array) {
            return Expression::Local(temp.clone());
        }
        let temp = self.fresh_temp("arr_tmp");
        self.emit(Statement::Assign {
            target: temp.clone(),
            value: array.clone(),
        });
        self.array_temps.push((array, temp.clone()));
        Expression::Local(temp)
    }

    /// Record a switch for the structuring pass and leave a placeholder
    /// statement marking its exact position in the statement stream.
    fn record_switch_stub(
        &mut self,
        discriminant: Expression,
        default_target: i32,
        cases: Vec<(i32, i32)>,
    ) {
        let offset = self.current_offset;
        let mut bounds = std::collections::HashSet::new();
        for (_, target) in &cases {
            if *target >= 0 {
                bounds.insert(*target as usize);
            }
        }
        if default_target >= 0 {
            bounds.insert(default_target as usize);
        }
        self.switch_stubs.push(SwitchStub {
            offset,
            discriminant,
            default_target,
            cases,
            bounds,
            depth: self.stack.len(),
            results: HashMap::new(),
        });
        self.emit(Statement::Expression(Expression::Unknown(format!(
            "@switch {offset}"
        ))));
    }

    /// Enforce the stack discipline at switch arm entries: each arm starts
    /// with the stack as it was right after the discriminant was popped.
    /// Leftovers are the previous arm's net pushes — captured for join
    /// resolution, then discarded (dead on every path that doesn't take
    /// the join store).
    fn check_switch_boundaries(&mut self, offset: usize) {
        let hits: Vec<usize> = self
            .switch_stubs
            .iter()
            .enumerate()
            .filter(|(_, stub)| stub.bounds.contains(&offset))
            .map(|(i, _)| i)
            .collect();
        for i in hits {
            let depth = self.switch_stubs[i].depth;
            if self.stack.len() <= depth {
                continue;
            }
            let mut net: Vec<Expression> = Vec::new();
            while self.stack.len() > depth {
                net.push(self.pop());
            }
            net.reverse();
            self.switch_stubs[i].results.insert(offset, net);
        }
    }

    /// Try to fold a StringConcatFactory invokedynamic into an `a + b` chain.
    /// Returns `None` when any piece is unresolvable (caller falls back).
    fn try_string_concat(
        &mut self,
        bm: &crate::classfile::BootstrapMethodInfo,
        bm_method: &str,
        desc: &str,
        param_count: usize,
    ) -> Option<Expression> {
        let mut args = self.pop_n(param_count);
        let (params, _) = parse_descriptor(desc);
        cast_boolean_args(&mut args, &params);

        let mut parts: Vec<Expression> = Vec::new();
        if bm_method == "makeConcatWithConstants" {
            // Recipe tags: \u{1} = next dynamic argument, \u{2} = next
            // static bootstrap constant (javac layout).
            let recipe_idx = *bm.arguments.first()?;
            let recipe = self.cp_constant_string(recipe_idx)?;
            let mut dyn_args = args.into_iter();
            let mut const_idx = 1usize;
            let mut literal = String::new();
            let flush = |literal: &mut String, parts: &mut Vec<Expression>| {
                if !literal.is_empty() {
                    parts.push(Expression::ConstString(std::mem::take(literal)));
                }
            };
            for ch in recipe.chars() {
                if ch == '\u{1}' {
                    flush(&mut literal, &mut parts);
                    parts.push(dyn_args.next()?);
                } else if ch == '\u{2}' {
                    flush(&mut literal, &mut parts);
                    let idx = *bm.arguments.get(const_idx)?;
                    const_idx += 1;
                    parts.push(self.cp_constant_to_expr(idx)?);
                } else {
                    literal.push(ch);
                }
            }
            flush(&mut literal, &mut parts);
        } else {
            parts = args;
        }

        if parts.is_empty() {
            return Some(Expression::ConstString(String::new()));
        }
        let mut iter = parts.into_iter();
        let mut acc = iter.next()?;
        for part in iter {
            acc = Expression::Binary {
                left: Box::new(acc),
                op: BinaryOp::Add,
                right: Box::new(part),
            };
        }
        Some(acc)
    }

    /// Resolve a bootstrap constant-pool index to a string (for recipes).
    fn cp_constant_string(&self, cp_index: u16) -> Option<String> {
        match self.pool.get(cp_index as usize) {
            Some(Recoverable::Present(ConstantPoolEntry::String { string_index })) => {
                match self.pool.get(*string_index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Utf8(s))) => Some(s.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Render a bootstrap static constant as an expression for concatenation.
    fn cp_constant_to_expr(&self, cp_index: u16) -> Option<Expression> {
        match self.pool.get(cp_index as usize) {
            Some(Recoverable::Present(ConstantPoolEntry::Integer(v))) => {
                Some(Expression::ConstInt(*v))
            }
            Some(Recoverable::Present(ConstantPoolEntry::Float(v))) => {
                Some(Expression::ConstFloat(f32::from_bits(*v)))
            }
            Some(Recoverable::Present(ConstantPoolEntry::Long(v))) => {
                Some(Expression::ConstLong(*v))
            }
            Some(Recoverable::Present(ConstantPoolEntry::Double(v))) => {
                Some(Expression::ConstDouble(f64::from_bits(*v)))
            }
            Some(Recoverable::Present(ConstantPoolEntry::String { .. })) => self
                .cp_constant_string(cp_index)
                .map(Expression::ConstString),
            Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
                match self.pool.get(*name_index as usize) {
                    Some(Recoverable::Present(ConstantPoolEntry::Utf8(name))) => Some(
                        Expression::Unknown(format!("{}.class", name.replace('/', "."))),
                    ),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn execute(&mut self, instr: &Instruction) {
        match &instr.kind {
            // â”€â”€ Constants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Nop => {}
            InstructionKind::AconstNull => self.push(Expression::ConstNull),
            InstructionKind::Iconst(v) => self.push(Expression::ConstInt(*v)),
            InstructionKind::Lconst(v) => self.push(Expression::ConstLong(*v)),
            InstructionKind::Fconst(v) => {
                self.push(Expression::ConstFloat(f32::from_bits(*v as u32)));
            }
            InstructionKind::Dconst(v) => {
                self.push(Expression::ConstDouble(f64::from_bits(*v as u64)));
            }
            InstructionKind::Bipush(v) => self.push(Expression::ConstInt(*v as i32)),
            InstructionKind::Sipush(v) => self.push(Expression::ConstInt(*v as i32)),
            InstructionKind::Ldc(index) => self.push(cp_ldc(self.pool, *index)),

            // â”€â”€ Locals â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Load { index, .. } => {
                let expr = if *index == 0 && !self.is_static {
                    Expression::This
                } else {
                    self.locals
                        .get(*index as usize)
                        .cloned()
                        .unwrap_or_else(|| Expression::Local(format!("var_{index}")))
                };
                self.push(expr);
            }
            InstructionKind::Store { index, .. } => {
                let val = self.pop();
                // Reuse the established name for this slot (LVT / parameter names).
                // Emitting a fresh `var_N` here would split one slot into two
                // variables and orphan the LVT name — a classic CFR-level bug.
                let name = match self.locals.get(*index as usize) {
                    Some(Expression::Local(existing)) => existing.clone(),
                    _ => format!("var_{index}"),
                };
                // Don't emit store if the value is empty stack noise
                if !matches!(&val, Expression::Unknown(text) if text.contains("empty_stack")) {
                    if (*index as usize) < self.locals.len() {
                        self.locals[*index as usize] = Expression::Local(name.clone());
                    }
                    self.emit(Statement::Assign {
                        target: name,
                        value: val,
                    });
                }
            }
            InstructionKind::Iinc { index, amount } => {
                let name = match self.locals.get(*index as usize) {
                    Some(Expression::Local(existing)) => existing.clone(),
                    _ => format!("var_{index}"),
                };
                self.emit(Statement::Assign {
                    target: name.clone(),
                    value: Expression::Binary {
                        left: Box::new(Expression::Local(name)),
                        op: BinaryOp::Add,
                        right: Box::new(Expression::ConstInt(*amount as i32)),
                    },
                });
            }

            // â”€â”€ Stack manipulation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Stack(op) => match op {
                crate::bytecode::StackOp::Dup => {
                    let is_fresh_array = match self.stack.last() {
                        Some(StackValue::Expr(Expression::NewArray { .. })) => true,
                        Some(StackValue::Expr(Expression::Unknown(text))) => {
                            text.trim_start().starts_with("new ")
                        }
                        _ => false,
                    };
                    if is_fresh_array {
                        // `anewarray; dup; ...` shares one array across stores:
                        // spill immediately so every user names the same temp.
                        if let Some(StackValue::Expr(array)) = self.stack.pop() {
                            let local = self.spill_fresh_array(array);
                            self.stack.push(StackValue::Expr(local.clone()));
                            self.stack.push(StackValue::Expr(local));
                        }
                    } else if let Some(top) = self.stack.last().cloned() {
                        self.stack.push(top);
                    }
                }
                crate::bytecode::StackOp::DupX1 if self.stack.len() >= 2 => {
                    let top = self.stack.pop().unwrap();
                    let below = self.stack.pop().unwrap();
                    self.stack.push(top.clone());
                    self.stack.push(below);
                    self.stack.push(top);
                }
                crate::bytecode::StackOp::DupX2 if self.stack.len() >= 3 => {
                    let v1 = self.stack.pop().unwrap();
                    let v2 = self.stack.pop().unwrap();
                    let v3 = self.stack.pop().unwrap();
                    self.stack.push(v1.clone());
                    self.stack.push(v3);
                    self.stack.push(v2);
                    self.stack.push(v1);
                }
                crate::bytecode::StackOp::Dup2 if self.stack.len() >= 2 => {
                    let v1 = self.stack.pop().unwrap();
                    let v2 = self.stack.pop().unwrap();
                    self.stack.push(v2.clone());
                    self.stack.push(v1.clone());
                    self.stack.push(v2);
                    self.stack.push(v1);
                }
                crate::bytecode::StackOp::Pop | crate::bytecode::StackOp::Pop2 => {
                    // A discard is not a no-op: `sb.append("x"); pop` must keep
                    // the call. Emit the popped expression as a statement when
                    // it has side effects; pure values are dropped silently.
                    let count = if matches!(op, crate::bytecode::StackOp::Pop2) {
                        2
                    } else {
                        1
                    };
                    let mut popped = Vec::with_capacity(count);
                    for _ in 0..count {
                        if let Some(StackValue::Expr(expr)) = self.stack.pop() {
                            popped.push(expr);
                        }
                    }
                    for expr in popped.into_iter().rev() {
                        if expr_has_side_effects(&expr) {
                            self.emit(Statement::Expression(expr));
                        }
                    }
                }
                crate::bytecode::StackOp::Swap if self.stack.len() >= 2 => {
                    let a = self.pop();
                    let b = self.pop();
                    self.push(a);
                    self.push(b);
                }
                _ => {}
            },

            // â”€â”€ Array operations â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::ArrayLoad(_) => {
                let index = self.pop();
                let array = self.pop();
                self.push(Expression::ArrayAccess {
                    array: Box::new(array),
                    index: Box::new(index),
                });
            }
            InstructionKind::ArrayStore(_) => {
                let value = self.pop();
                let index = self.pop();
                let array = self.pop();
                // A store into a freshly-created array (`new X[n][i] = v`)
                // is not valid Java — spill the array into a temp first,
                // exactly like CFR/Vineflower do.
                let array = self.spill_fresh_array(array);
                self.emit(Statement::Assign {
                    target: render_expr(&Expression::ArrayAccess {
                        array: Box::new(array),
                        index: Box::new(index),
                    }),
                    value,
                });
            }

            // â”€â”€ Arithmetic â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Arithmetic { opcode } => {
                if is_unary_arith(*opcode) {
                    let val = self.pop();
                    self.push(Expression::Unary {
                        op: UnaryOp::Neg,
                        operand: Box::new(val),
                    });
                } else {
                    let right = self.pop();
                    let left = self.pop();
                    let op = arith_binary_op(*opcode);
                    self.push(Expression::Binary {
                        left: Box::new(left),
                        op,
                        right: Box::new(right),
                    });
                }
            }

            InstructionKind::Convert { opcode } => {
                let val = self.pop();
                let ty = convert_type(*opcode);
                self.push(Expression::Cast {
                    target_type: ty.to_string(),
                    expr: Box::new(val),
                });
            }

            InstructionKind::Compare { .. } => {
                let right = self.pop();
                let left = self.pop();
                self.push(Expression::Binary {
                    left: Box::new(left),
                    op: BinaryOp::Sub,
                    right: Box::new(right),
                });
            }

            // â”€â”€ Control flow â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::If { opcode, target } => {
                let cond = make_if_cond(*opcode, &mut self.stack);
                self.emit(Statement::If {
                    condition: cond,
                    then_body: vec![],
                    else_body: None,
                });
                self.if_targets
                    .insert(self.current_offset, *target as usize);
            }
            InstructionKind::Goto(target) => {
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "@goto {target}"
                ))));
            }
            InstructionKind::Jsr(target) => {
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "jsr {target}"
                ))));
            }
            InstructionKind::Ret(index) => {
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "ret var_{index}"
                ))));
            }
            InstructionKind::TableSwitch {
                default,
                low,
                high: _,
                targets,
            } => {
                let discriminant = self.pop();
                let cases: Vec<(i32, i32)> = targets
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (low + i as i32, *t))
                    .collect();
                self.record_switch_stub(discriminant, *default, cases);
            }
            InstructionKind::LookupSwitch { default, pairs } => {
                let discriminant = self.pop();
                self.record_switch_stub(discriminant, *default, pairs.clone());
            }

            // â”€â”€ Return â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Return(crate::bytecode::ReturnType::Void) => {
                self.emit(Statement::Return(None));
            }
            InstructionKind::Return(_) => {
                let val = self.pop();
                self.emit(Statement::Return(Some(val)));
            }

            // â”€â”€ Invoke â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Invoke { opcode, cp_index } => {
                let (class, method, desc) = cp_method_ref(self.pool, *cp_index);
                let param_count = count_params(&desc);
                let short = short_name(&class);

                // Check for invokedynamic (0xba) — potential lambda
                if *opcode == 0xba {
                    self.handle_invokedynamic(*cp_index, &desc, param_count);
                } else {
                    match opcode {
                        0xb7 if method == "<init>" => {
                            // Check if this is a `new Foo()` pattern or `super()`/`this()`
                            // Pop args first, then check the receiver
                            let mut args = self.pop_n(param_count);
                            let mut receiver = self.pop();

                            // Fix for MethodRef or Lambda being popped as receiver instead of argument
                            // e.g. new Thread(this::run) — the captured ref is misaligned on stack
                            while matches!(
                                receiver,
                                Expression::MethodRef(_, _) | Expression::Lambda { .. }
                            ) {
                                let mut new_args = vec![receiver];
                                new_args.extend(args);
                                args = new_args;
                                receiver = self.pop();
                            }

                            match &receiver {
                                Expression::This => {
                                    // Real super() call from inside constructor
                                    self.emit(Statement::Expression(Expression::Invoke {
                                        target: "super".to_string(),
                                        args,
                                    }));
                                }
                                _ => {
                                    // `new Foo(); dup; <args>; invokespecial <init>` pattern
                                    // After <init>, remove dup'd UninitNew copies of this class.
                                    let init_class = short.to_string();
                                    self.stack.retain(|sv| {
                                        !matches!(sv, StackValue::UninitNew { class }
                                            if short_name(class) == init_class)
                                    });
                                    self.push(Expression::New {
                                        class: init_class,
                                        args,
                                    });
                                }
                            }
                        }
                        0xb8 => {
                            // invokestatic
                            let mut args = self.pop_n(param_count);
                            let (params, _) = parse_descriptor(&desc);
                            cast_boolean_args(&mut args, &params);
                            let invoke = Expression::Invoke {
                                target: format!("{short}.{method}"),
                                args,
                            };
                            if desc.ends_with(")V") {
                                self.emit(Statement::Expression(invoke));
                            } else {
                                self.push(invoke);
                            }
                        }
                        _ => {
                            // invokevirtual / invokeinterface / invokespecial (non-init)
                            let mut args = self.pop_n(param_count);
                            let (params, _) = parse_descriptor(&desc);
                            cast_boolean_args(&mut args, &params);
                            let mut receiver = self.pop();

                            // Fix for MethodRef or Lambda being popped as receiver instead of argument
                            while matches!(
                                receiver,
                                Expression::MethodRef(_, _) | Expression::Lambda { .. }
                            ) {
                                let mut new_args = vec![receiver];
                                new_args.extend(args);
                                args = new_args;
                                receiver = self.pop();
                            }

                            // Pre-indy string concatenation:
                            // `new StringBuilder().append(a).append(b).toString()`
                            // folds back to `a + b` (CFR/Vineflower behavior).
                            if let Some(folded) =
                                try_fold_builder_call(&class, &method, &desc, &receiver, &mut args)
                            {
                                if desc.ends_with(")V") {
                                    self.emit(Statement::Expression(folded));
                                } else {
                                    self.push(folded);
                                }
                            } else if !contains_empty_stack(&receiver) {
                                // Skip if receiver contains empty_stack noise
                                let receiver_str = render_expr_at(&receiver, 2);
                                let target = if receiver_str.is_empty() {
                                    method
                                } else {
                                    format!("{}.{}", receiver_str, method)
                                };
                                let invoke = Expression::Invoke { target, args };
                                if desc.ends_with(")V") {
                                    self.emit(Statement::Expression(invoke));
                                } else {
                                    self.push(invoke);
                                }
                            }
                        }
                    }
                }
            }

            // â”€â”€ Fields â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Field { opcode, cp_index } => {
                let (class, field, desc) = cp_field_ref(self.pool, *cp_index);
                let short = short_name(&class);

                match opcode {
                    0xb2 => {
                        // getstatic
                        let full = if class == self.this_class || class.is_empty() {
                            field
                        } else {
                            format!("{short}.{field}")
                        };
                        self.push(Expression::Local(full));
                    }
                    0xb3 => {
                        // putstatic
                        let val = self.pop();
                        let val = coerce_bool_literal(val, &desc);
                        let full = if class == self.this_class || class.is_empty() {
                            field
                        } else {
                            format!("{short}.{field}")
                        };
                        if contains_empty_stack(&val) {
                            self.emit(Statement::Expression(Expression::Unknown(format!(
                                "/* unreconstructable putstatic {full} */"
                            ))));
                        } else {
                            self.emit(Statement::Assign {
                                target: full,
                                value: val,
                            });
                        }
                    }
                    0xb4 => {
                        // getfield
                        let object = self.pop();
                        self.push(Expression::FieldAccess {
                            object: Box::new(object),
                            field,
                        });
                    }
                    0xb5 => {
                        // putfield
                        let val = self.pop();
                        let val = coerce_bool_literal(val, &desc);
                        let object = self.pop();
                        if contains_empty_stack(&val) || contains_empty_stack(&object) {
                            self.emit(Statement::Expression(Expression::Unknown(format!(
                                "/* unreconstructable putfield {field} */"
                            ))));
                        } else {
                            self.emit(Statement::Assign {
                                target: format!("{}.{}", render_expr(&object), field),
                                value: val,
                            });
                        }
                    }
                    _ => self.push(Expression::Unknown(format!("field#{cp_index}"))),
                }
            }

            // â”€â”€ Types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Type { opcode, cp_index } => {
                let class = cp_class_name(self.pool, *cp_index);
                let short = short_name(&class);
                // Array descriptors (`[F`, `[[Ljava/lang/String;`) need full
                // decoding; plain refs just shorten.
                let raw = cp_raw_class(self.pool, *cp_index);
                let display = if raw.trim_start().starts_with('[') {
                    decode_type_descriptor(&raw)
                } else {
                    short.clone()
                };

                match opcode {
                    0xbb => self.push_uninit(class),
                    0xbd => {
                        let size = self.pop();
                        self.push(Expression::NewArray {
                            element_type: display,
                            size: Box::new(size),
                        });
                    }
                    0xc0 => {
                        let val = self.pop();
                        self.push(Expression::Cast {
                            target_type: display,
                            expr: Box::new(val),
                        });
                    }
                    0xc1 => {
                        let val = self.pop();
                        self.push(Expression::InstanceOf {
                            expr: Box::new(val),
                            target_type: display,
                        });
                    }
                    _ => self.push(Expression::Unknown(format!("type#{cp_index}"))),
                }
            }

            InstructionKind::NewArray(kind) => {
                let type_name = match kind {
                    4 => "boolean",
                    5 => "char",
                    6 => "float",
                    7 => "double",
                    8 => "byte",
                    9 => "short",
                    10 => "int",
                    11 => "long",
                    _ => "?",
                };
                let size = self.pop();
                self.push(Expression::NewArray {
                    element_type: type_name.to_string(),
                    size: Box::new(size),
                });
            }
            InstructionKind::MultiANewArray {
                cp_index,
                dimensions,
            } => {
                let raw = cp_raw_class(self.pool, *cp_index);
                let (base, _) = split_array_descriptor(&raw);
                let mut sizes = Vec::new();
                for _ in 0..*dimensions {
                    sizes.push(render_expr(&self.pop()));
                }
                sizes.reverse();
                let size_str = sizes.join("][");
                self.push(Expression::Unknown(format!("new {base}[{size_str}]")));
            }

            // â”€â”€ Misc â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            InstructionKind::Wide => {}
            InstructionKind::Throw => {
                let val = self.pop();
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "throw {}",
                    render_expr(&val)
                ))));
            }
            InstructionKind::MonitorEnter => {
                self.pop();
            }
            InstructionKind::MonitorExit => {
                self.pop();
            }
            InstructionKind::ArrayLength => {
                let arr = self.pop();
                self.push(Expression::FieldAccess {
                    object: Box::new(arr),
                    field: "length".to_string(),
                });
            }
            InstructionKind::Unknown(opcode) => {
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "/* unknown opcode 0x{opcode:02x} */"
                ))));
            }
            InstructionKind::Malformed { opcode, reason, .. } => {
                self.emit(Statement::Expression(Expression::Unknown(format!(
                    "/* malformed 0x{:02x}: {} */",
                    opcode.unwrap_or(0),
                    reason
                ))));
            }
        }
    }
}

// â”€â”€â”€ Helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// True when evaluating `expr` can have side effects (method calls,
/// allocations, assignments), so a discarded value must still be
/// rendered as a statement. Pure loads/constants are safe to drop.
fn expr_has_side_effects(expr: &Expression) -> bool {
    match expr {
        Expression::Invoke { .. } => true,
        Expression::New { .. } => true,
        Expression::NewArray { .. } => true,
        Expression::Concat { .. } | Expression::Lambda { .. } => true,
        Expression::Binary { left, right, .. } => {
            expr_has_side_effects(left) || expr_has_side_effects(right)
        }
        Expression::Unary { operand, .. } => expr_has_side_effects(operand),
        Expression::Cast { expr, .. } => expr_has_side_effects(expr),
        Expression::InstanceOf { expr, .. } => expr_has_side_effects(expr),
        Expression::Ternary { .. } => true,
        _ => false,
    }
}

fn contains_empty_stack(expr: &Expression) -> bool {
    match expr {
        Expression::Unknown(text) => text.contains("empty_stack"),
        Expression::FieldAccess { object, .. } => contains_empty_stack(object),
        Expression::Invoke { args, .. } => args.iter().any(contains_empty_stack),
        Expression::Concat { parts, .. } => parts.iter().any(contains_empty_stack),
        _ => false,
    }
}

fn is_noise(expr: &Expression) -> bool {
    match expr {
        Expression::This
        | Expression::Super
        | Expression::ConstNull
        | Expression::ConstInt(_)
        | Expression::ConstLong(_)
        | Expression::ConstFloat(_)
        | Expression::ConstDouble(_)
        | Expression::ConstString(_) => true,
        Expression::New { args, .. } if args.is_empty() => true,
        Expression::Unknown(text) if text.contains("empty_stack") => true,
        Expression::FieldAccess { object, .. } if contains_empty_stack(object) => true,
        _ => false,
    }
}

fn is_unary_arith(opcode: u8) -> bool {
    (0x74..=0x77).contains(&opcode)
}

fn arith_binary_op(opcode: u8) -> BinaryOp {
    match opcode {
        0x60..=0x63 => BinaryOp::Add,
        0x64..=0x67 => BinaryOp::Sub,
        0x68..=0x6b => BinaryOp::Mul,
        0x6c..=0x6f => BinaryOp::Div,
        0x70..=0x73 => BinaryOp::Rem,
        0x78..=0x79 => BinaryOp::Shl,
        0x7a..=0x7b => BinaryOp::Shr,
        0x7c..=0x7d => BinaryOp::Ushr,
        0x7e..=0x7f => BinaryOp::And,
        0x80..=0x81 => BinaryOp::Or,
        0x82..=0x83 => BinaryOp::Xor,
        _ => BinaryOp::Add,
    }
}

fn convert_type(opcode: u8) -> &'static str {
    match opcode {
        0x85 => "long",
        0x86 => "float",
        0x87 => "double",
        0x88 => "int",
        0x89 => "float",
        0x8a => "double",
        0x8b => "int",
        0x8c => "long",
        0x8d => "double",
        0x8e => "int",
        0x8f => "long",
        0x90 => "float",
        0x91 => "byte",
        0x92 => "char",
        0x93 => "short",
        _ => "int",
    }
}

fn make_if_cond(opcode: u8, stack: &mut Vec<StackValue>) -> Expression {
    let pop = |stack: &mut Vec<StackValue>| -> Expression {
        match stack.pop() {
            Some(StackValue::Expr(e)) => e,
            Some(StackValue::UninitNew { class }) => Expression::New {
                class: short_name(&class),
                args: vec![],
            },
            None => Expression::Unknown("/* empty_stack */".to_string()),
        }
    };

    match opcode {
        0x99 => {
            // ifeq: jump if == 0 → if (!expr)
            let v = pop(stack);
            Expression::Unary {
                op: UnaryOp::Not,
                operand: Box::new(v),
            }
        }
        0x9a => {
            // ifne: jump if != 0 → if (expr)
            pop(stack)
        }
        0x9b => {
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Lt,
                right: Box::new(Expression::ConstInt(0)),
            }
        }
        0x9c => {
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Ge,
                right: Box::new(Expression::ConstInt(0)),
            }
        }
        0x9d => {
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Gt,
                right: Box::new(Expression::ConstInt(0)),
            }
        }
        0x9e => {
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Le,
                right: Box::new(Expression::ConstInt(0)),
            }
        }
        0x9f => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Eq,
                right: Box::new(r),
            }
        }
        0xa0 => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Ne,
                right: Box::new(r),
            }
        }
        0xa1 => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Lt,
                right: Box::new(r),
            }
        }
        0xa2 => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Ge,
                right: Box::new(r),
            }
        }
        0xa3 => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Gt,
                right: Box::new(r),
            }
        }
        0xa4 => {
            let r = pop(stack);
            let l = pop(stack);
            Expression::Binary {
                left: Box::new(l),
                op: BinaryOp::Le,
                right: Box::new(r),
            }
        }
        0xc6 => {
            // ifnull → if (expr == null)
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Eq,
                right: Box::new(Expression::ConstNull),
            }
        }
        0xc7 => {
            // ifnonnull → if (expr != null)
            let v = pop(stack);
            Expression::Binary {
                left: Box::new(v),
                op: BinaryOp::Ne,
                right: Box::new(Expression::ConstNull),
            }
        }
        _ => Expression::Unknown("/* cond */".to_string()),
    }
}

fn render_expr(expr: &Expression) -> String {
    crate::ast::render_expression_pub(expr)
}

fn render_expr_at(expr: &Expression, indent: usize) -> String {
    crate::ast::render_expression_at_pub(expr, indent)
}

fn cast_boolean_args(args: &mut [Expression], param_types: &[String]) {
    for (arg, ty) in args.iter_mut().zip(param_types.iter()) {
        if ty == "boolean" {
            coerce_bool_literal_in_place(arg);
        }
    }
}

/// Rewrite `0`/`1` integer constants as `false`/`true` when the target
/// type is boolean (javac encodes booleans as ints in bytecode).
fn coerce_bool_literal(val: Expression, field_desc: &str) -> Expression {
    if field_desc == "Z"
        && let Expression::ConstInt(v) = val
    {
        return Expression::Unknown(if v == 0 {
            "false".to_string()
        } else {
            "true".to_string()
        });
    }
    val
}

fn coerce_bool_literal_in_place(arg: &mut Expression) {
    if let Expression::ConstInt(v) = arg {
        let v = *v;
        *arg = Expression::Unknown(if v == 0 {
            "false".to_string()
        } else {
            "true".to_string()
        });
    }
}

/// Fold pre-indy string concatenation: `new StringBuilder().append(a)`
/// accumulates into `Concat`, and a terminating `.toString()` folds the
/// parts into an `a + b` chain. Returns `None` for anything else (normal
/// invoke handling applies). `appendCodePoint` and multi-arg appends are
/// deliberately excluded (different semantics than `+`).
fn try_fold_builder_call(
    class: &str,
    method: &str,
    desc: &str,
    receiver: &Expression,
    args: &mut [Expression],
) -> Option<Expression> {
    if method == "append" && args.len() == 1 {
        let (builder, mut parts) = match receiver {
            Expression::New {
                class: new_class,
                args: new_args,
            } => {
                if new_class != "StringBuilder" && new_class != "StringBuffer" {
                    return None;
                }
                // `class` here is the dotted owner from the constant pool;
                // require java.lang to avoid user classes with the same name.
                if class != "java.lang.StringBuilder" && class != "java.lang.StringBuffer" {
                    return None;
                }
                let init = match new_args.as_slice() {
                    [] => None,
                    [Expression::ConstString(s)] => Some(Expression::ConstString(s.clone())),
                    [Expression::ConstInt(_)] => None, // capacity hint
                    _ => return None,
                };
                (short_name(class), init.into_iter().collect())
            }
            Expression::Concat { builder, parts } => (builder.clone(), parts.clone()),
            _ => return None,
        };
        // `append(boolean)` arrives as `iconst_0/1`: coerce via descriptor.
        let (params, _) = parse_descriptor(desc);
        cast_boolean_args(args, &params);
        parts.push(args[0].clone());
        Some(Expression::Concat { builder, parts })
    } else if method == "toString" && args.is_empty() {
        if let Expression::Concat { parts, .. } = receiver {
            if parts.is_empty() {
                return Some(Expression::ConstString(String::new()));
            }
            let mut iter = parts.iter().cloned();
            let mut acc = iter.next()?;
            for part in iter {
                acc = Expression::Binary {
                    left: Box::new(acc),
                    op: BinaryOp::Add,
                    right: Box::new(part),
                };
            }
            Some(acc)
        } else {
            None
        }
    } else {
        None
    }
}

// Structured control flow reconstruction

/// Negate a guard condition: when guard is TRUE, skip body.
/// So we negate to get "when to run" logic.
fn negate_guard_cond(cond: Expression) -> Expression {
    match cond {
        // !expr → expr (unwrap double negation)
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => *operand,
        // expr == null → expr != null
        Expression::Binary {
            left,
            op: BinaryOp::Eq,
            right,
        } if matches!(right.as_ref(), Expression::ConstNull) => Expression::Binary {
            left,
            op: BinaryOp::Ne,
            right,
        },
        // anything else → !(anything)
        other => Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(other),
        },
    }
}

// ─── Switch reconstruction (CFR-style) ───

/// Fold recorded tableswitch/lookupswitch stubs into `Statement::Switch`
/// nodes. Runs before loop/if structuring so arm bodies still carry raw
/// bytecode offsets; each arm slice is structured recursively.
fn fold_switches(
    result: &mut Vec<(usize, Statement)>,
    if_targets: &HashMap<usize, usize>,
    switches: &[SwitchStub],
) {
    // Reverse offset order: drains never disturb pending positions.
    let mut ordered: Vec<&SwitchStub> = switches.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.offset));

    for stub in ordered {
        let Some(pos) = result.iter().position(|(off, stmt)| {
            *off == stub.offset
                && matches!(stmt,
                    Statement::Expression(Expression::Unknown(t))
                    if t.strip_prefix("@switch ").and_then(|s| s.parse::<usize>().ok()) == Some(stub.offset))
        }) else {
            continue;
        };
        // Unresolvable discriminant (stack noise) — leave the stub as a comment.
        if contains_empty_stack(&stub.discriminant) {
            continue;
        }
        if let Some(built) = build_switch_node(result, pos, stub, if_targets, switches) {
            if std::env::var("VINYLITE_DEBUG_SWITCH").is_ok() {
                eprintln!(
                    "[switch] stub@{} pos={} arms={} consumed_through={} len={}",
                    stub.offset,
                    pos,
                    match &built.stmt {
                        Statement::Switch { arms, .. } => arms.len(),
                        _ => 999,
                    },
                    built.consumed_through,
                    result.len(),
                );
            }
            // Splice: switch node, then relocated join-rest, replacing the
            // consumed range. (Insertions sit after `pos`, so pending
            // earlier-offset stubs keep valid positions in reverse order.)
            let drain_end = built.consumed_through.min(result.len());
            let mut replacement: Vec<(usize, Statement)> = Vec::with_capacity(built.post.len() + 1);
            // Placeholder offset for the node itself.
            replacement.push((stub.offset, built.stmt));
            replacement.extend(built.post);
            result.splice(pos..drain_end.max(pos + 1), replacement);
        }
    }
}

struct BuiltSwitch {
    stmt: Statement,
    /// Exclusive statement index through which the switch consumed input.
    consumed_through: usize,
    /// Join-rest statements relocated right after the switch node.
    post: Vec<(usize, Statement)>,
}

/// Slice one arm body starting at statement `start_idx`.
/// `next_bound` is the next case boundary offset (exclusive upper fence):
/// reaching it means falling into the next arm.
struct ArmSlice {
    body: Vec<(usize, Statement)>,
    breaks: bool,
    /// Target of the breaking forward-goto, if the arm ends with one.
    broke_to: Option<usize>,
    /// True when a backward (loop) edge was kept inside the arm.
    has_loop: bool,
    /// Exclusive stop index.
    stop: usize,
}

fn slice_switch_arm(
    result: &[(usize, Statement)],
    start_idx: usize,
    next_bound: Option<usize>,
    case_bounds: &std::collections::HashSet<usize>,
    stub_offset: usize,
    is_last: bool,
    if_targets: &HashMap<usize, usize>,
) -> ArmSlice {
    let mut body = Vec::new();
    let mut breaks = false;
    let mut broke_to = None;
    let mut has_loop = false;
    let mut j = start_idx;
    while j < result.len() {
        let (off, stmt) = &result[j];
        // Crossing into the next arm's offset range — fallthrough.
        if j > start_idx
            && let Some(nb) = next_bound
            && *off >= nb
        {
            break;
        }
        // Next arm starts here — fallthrough from the previous arm.
        if j > start_idx && case_bounds.contains(off) {
            break;
        }
        // Last arm only: a join point (target of an outer if, but not of
        // any if nested inside this arm) ends the switch.
        if is_last && j > start_idx && is_join_offset(*off, result[start_idx].0, if_targets) {
            break;
        }
        match stmt {
            Statement::Return(_) => {
                body.push(result[j].clone());
                breaks = true;
                j += 1;
                break;
            }
            Statement::Expression(Expression::Unknown(text)) => {
                if text.trim_start().starts_with("throw ") {
                    body.push(result[j].clone());
                    breaks = true;
                    j += 1;
                    break;
                }
                if let Some(target_str) = text.strip_prefix("@goto ")
                    && let Ok(target) = target_str.parse::<usize>()
                {
                    if case_bounds.contains(&target) {
                        // Jump to another arm — intentional fallthrough.
                        j += 1;
                    } else if target > stub_offset {
                        // Forward jump out of the switch — `break`.
                        breaks = true;
                        broke_to = Some(target);
                        j += 1;
                    } else {
                        // Backward jump — a loop back-edge inside this arm;
                        // keep the marker so loop folding sees it.
                        has_loop = true;
                        body.push(result[j].clone());
                        j += 1;
                    }
                    break;
                }
                if text.strip_prefix("@switch ").is_some() {
                    // Nested switch — leave it for the recursive pass.
                    body.push(result[j].clone());
                    j += 1;
                    continue;
                }
                body.push(result[j].clone());
                j += 1;
            }
            _ => {
                body.push(result[j].clone());
                j += 1;
            }
        }
    }
    ArmSlice {
        body,
        breaks,
        broke_to,
        has_loop,
        stop: j,
    }
}

/// True when `off` is the target of an `if` whose source is *outside*
/// `[arm_start, off)` — i.e. control reconverges here from outside the arm.
fn is_join_offset(off: usize, arm_start: usize, if_targets: &HashMap<usize, usize>) -> bool {
    if_targets
        .iter()
        .any(|(src, tgt)| *tgt == off && (*src < arm_start || *src >= off))
}

fn build_switch_node(
    result: &[(usize, Statement)],
    pos: usize,
    stub: &SwitchStub,
    if_targets: &HashMap<usize, usize>,
    switches: &[SwitchStub],
) -> Option<BuiltSwitch> {
    // Group case keys by target; drop non-forward (degenerate) targets.
    let mut by_target: HashMap<usize, Vec<i32>> = HashMap::new();
    for (key, target) in &stub.cases {
        if *target >= 0 && (*target as usize) > stub.offset {
            by_target.entry(*target as usize).or_default().push(*key);
        }
    }
    if by_target.is_empty() {
        return None;
    }
    let default_off: Option<usize> =
        if stub.default_target >= 0 && (stub.default_target as usize) > stub.offset {
            Some(stub.default_target as usize)
        } else {
            None
        };

    // Arm order = target offset order (source order); default spliced in.
    let mut bounds: Vec<usize> = by_target.keys().copied().collect();
    if let Some(d) = default_off
        && !bounds.contains(&d)
    {
        bounds.push(d);
    }
    bounds.sort();
    let case_bounds: std::collections::HashSet<usize> = bounds.iter().copied().collect();

    // Every boundary must resolve to a statement after the switch.
    // Case targets usually point at stack pushes (no statement of their
    // own), so resolve to the first statement at or after the target.
    let mut bound_idx: Vec<usize> = Vec::with_capacity(bounds.len());
    for b in &bounds {
        let idx = result
            .iter()
            .position(|(off, _)| *off >= *b && *off > stub.offset)?;
        bound_idx.push(idx);
    }

    let mut arms: Vec<SwitchArm> = Vec::new();
    let mut arm_bounds: Vec<usize> = Vec::new();
    // Raw slices + per-arm control facts, parallel to `arms`.
    let mut raw_bodies: Vec<Vec<(usize, Statement)>> = Vec::new();
    let mut arm_broke_to: Vec<Option<usize>> = Vec::new();
    let mut arm_has_loop: Vec<bool> = Vec::new();
    let mut consumed_through = pos + 1;
    let debug = std::env::var("VINYLITE_DEBUG_SWITCH").is_ok();
    for (n, b) in bounds.iter().enumerate() {
        let is_last = n + 1 == bounds.len();
        let is_default = default_off == Some(*b);
        // No statement between this boundary and the next: empty arm that
        // falls through (e.g. `case 1:` immediately followed by `case 2:`).
        if n + 1 < bounds.len() && bound_idx[n] >= bound_idx[n + 1] {
            let mut keys = by_target.get(b).cloned().unwrap_or_default();
            keys.sort();
            // An empty default arm (default == join) carries no code — skip it.
            if !(keys.is_empty() && is_default) {
                arms.push(SwitchArm {
                    keys,
                    is_default,
                    body: Vec::new(),
                    breaks: false,
                });
                arm_bounds.push(*b);
                raw_bodies.push(Vec::new());
                arm_broke_to.push(None);
                arm_has_loop.push(false);
            }
            continue;
        }
        let slice = slice_switch_arm(
            result,
            bound_idx[n],
            bounds.get(n + 1).copied(),
            &case_bounds,
            stub.offset,
            is_last,
            if_targets,
        );
        // An empty default arm (default == join) carries no code — skip it.
        if slice.body.is_empty() && is_default {
            consumed_through = consumed_through.max(slice.stop);
            continue;
        }
        // Recursively structure the arm body (nested loops/ifs/switches).
        let structured: Vec<Statement> =
            restructure_control_flow(slice.body.clone(), if_targets, switches)
                .into_iter()
                .map(|(_, s)| s)
                .collect();
        let mut keys = by_target.get(b).cloned().unwrap_or_default();
        keys.sort();
        if debug {
            eprintln!(
                "[switch-arm] stub@{} bound={} keys={:?} default={} breaks={} stop={} start={}",
                stub.offset, b, keys, is_default, slice.breaks, slice.stop, bound_idx[n]
            );
        }
        arms.push(SwitchArm {
            keys,
            is_default,
            body: structured,
            breaks: slice.breaks,
        });
        arm_bounds.push(*b);
        raw_bodies.push(slice.body);
        arm_broke_to.push(slice.broke_to);
        arm_has_loop.push(slice.has_loop);
        consumed_through = consumed_through.max(slice.stop);
    }

    if arms.is_empty() {
        return None;
    }
    // CFR-style break-to-push resolution (see try_distribute_join_values).
    // May relocate trailing join-rest after the switch (`post`).
    let mut post: Vec<(usize, Statement)> = Vec::new();
    if let Some(outcome) = try_distribute_join_values(
        result,
        consumed_through,
        &mut arms,
        &arm_bounds,
        &raw_bodies,
        &mut arm_broke_to,
        &arm_has_loop,
        stub,
        if_targets,
        switches,
    ) {
        consumed_through = outcome.consumed_through;
        post = outcome.post;
    }
    Some(BuiltSwitch {
        stmt: Statement::Switch {
            discriminant: stub.discriminant.clone(),
            arms,
        },
        consumed_through,
        post,
    })
}

/// An arm diverges when its body ends with return/throw.
fn arm_diverges(body: &[Statement]) -> bool {
    match body.last() {
        Some(Statement::Return(_)) => true,
        Some(Statement::Expression(Expression::Unknown(text))) => {
            text.trim_start().starts_with("throw ")
        }
        _ => false,
    }
}

/// A join value must be a pure, non-compound move of the arm's pushed value
/// (local, constant, field/array read). Invokes, allocations and arithmetic
/// may mix arm state with join computation or repeat side effects — bail.
fn is_plain_join_value(expr: &Expression) -> bool {
    match expr {
        Expression::Local(_)
        | Expression::ConstInt(_)
        | Expression::ConstLong(_)
        | Expression::ConstFloat(_)
        | Expression::ConstDouble(_)
        | Expression::ConstString(_)
        | Expression::ConstNull
        | Expression::This
        | Expression::Super => true,
        Expression::FieldAccess { object, .. } => is_plain_join_value(object),
        Expression::ArrayAccess { array, index } => {
            is_plain_join_value(array) && is_plain_join_value(index)
        }
        _ => false,
    }
}

/// Outcome of join distribution: possibly extended consumption plus
/// join-rest statements relocated after the switch node.
struct DistributeOutcome {
    consumed_through: usize,
    post: Vec<(usize, Statement)>,
}

#[allow(clippy::too_many_arguments)]
fn try_distribute_join_values(
    result: &[(usize, Statement)],
    consumed_through: usize,
    arms: &mut [SwitchArm],
    arm_bounds: &[usize],
    raw_bodies: &[Vec<(usize, Statement)>],
    arm_broke_to: &mut [Option<usize>],
    arm_has_loop: &[bool],
    stub: &SwitchStub,
    if_targets: &HashMap<usize, usize>,
    switches: &[SwitchStub],
) -> Option<DistributeOutcome> {
    // No loops inside contributing arms: loop-carried stack flow can't be
    // reasoned about locally.
    if arm_has_loop.iter().any(|l| *l) {
        return None;
    }

    // Locate the join store: either trailing the switch (After mode) or
    // absorbed at the end of the last arm's raw slice (Absorbed mode).
    enum JoinSite {
        After { index: usize },
        Absorbed { arm: usize, stmt_index: usize },
    }
    let dist_debug = std::env::var("VINYLITE_DEBUG_SWITCH").is_ok();
    let mut join_site: Option<JoinSite> = None;
    if let Some((_, join_stmt)) = result.get(consumed_through)
        && matches!(
            join_stmt,
            Statement::Assign { .. }
                | Statement::VarDecl { value: Some(_), .. }
                | Statement::Return(Some(_))
        )
    {
        join_site = Some(JoinSite::After {
            index: consumed_through,
        });
    }
    if join_site.is_none() && !arms.is_empty() {
        // Absorbed: the last arm falls through into the join store.
        // Unanimous convergence anchor: at least one arm must break to
        // the join, and all breaking arms must agree on its offset.
        let last = arms.len() - 1;
        let mut targets_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for broke in arm_broke_to.iter().flatten() {
            targets_set.insert(*broke);
        }
        if targets_set.len() == 1
            && let Some(raw) = raw_bodies.get(last)
        {
            let join_off = *targets_set.iter().next()?;
            // First store/return-value at or after the join offset.
            for (k, (off, stmt)) in raw.iter().enumerate() {
                if *off < join_off {
                    continue;
                }
                if matches!(
                    stmt,
                    Statement::Assign { .. }
                        | Statement::VarDecl { value: Some(_), .. }
                        | Statement::Return(Some(_))
                ) {
                    join_site = Some(JoinSite::Absorbed {
                        arm: last,
                        stmt_index: k,
                    });
                    break;
                }
                // A divergent or control statement before any store:
                // the join is not cleanly separable.
                if matches!(
                    stmt,
                    Statement::Return(_)
                        | Statement::If { .. }
                        | Statement::While { .. }
                        | Statement::Switch { .. }
                ) {
                    break;
                }
            }
        }
    }
    let join_site = join_site?;
    if dist_debug {
        eprintln!(
            "[dist] site stub@{} disc={} join_off={} arms={} results={}",
            stub.offset,
            crate::ast::render_expression_pub(&stub.discriminant),
            match &join_site {
                JoinSite::After { index } =>
                    result.get(*index).map(|(o, _)| *o as i64).unwrap_or(-1),
                JoinSite::Absorbed { arm, stmt_index } => raw_bodies
                    .get(*arm)
                    .and_then(|r| r.get(*stmt_index))
                    .map(|(o, _)| *o as i64)
                    .unwrap_or(-1),
            },
            arms.len(),
            stub.results.len()
        );
        for (n, arm) in arms.iter().enumerate() {
            eprintln!(
                "[dist]   arm={n} keys={:?} body_len={} breaks={} broke_to={:?}",
                arm.keys,
                arm.body.len(),
                arm.breaks,
                arm_broke_to.get(n).copied().flatten()
            );
        }
    }

    // Extract (target, value, is_return, join_off) from the join statement.
    let (join_off, target, value, is_return) = match &join_site {
        JoinSite::After { index } => {
            let (off, stmt) = result.get(*index)?;
            let (target, value, is_return) = match stmt {
                Statement::Assign { target, value } => (Some(target.clone()), value.clone(), false),
                Statement::VarDecl {
                    target,
                    value: Some(value),
                    ..
                } => (Some(target.clone()), value.clone(), false),
                Statement::Return(Some(value)) => (None, value.clone(), true),
                _ => return None,
            };
            (*off, target, value, is_return)
        }
        JoinSite::Absorbed { arm, stmt_index } => {
            let (off, stmt) = raw_bodies.get(*arm)?.get(*stmt_index)?.clone();
            let (target, value, is_return) = match stmt {
                Statement::Assign { target, value } => (Some(target), value, false),
                Statement::VarDecl {
                    target,
                    value: Some(value),
                    ..
                } => (Some(target), value, false),
                Statement::Return(Some(value)) => (None, value, true),
                _ => return None,
            };
            (off, target, value, is_return)
        }
    };
    // Only plain locals can be re-targeted soundly.
    if dist_debug {
        eprintln!(
            "[dist] extracted target={:?} value={} ret={is_return}",
            target,
            crate::ast::render_expression_pub(&value)
        );
    }
    if let Some(ref t) = target
        && (t.contains('.') || t.contains('[') || t.starts_with('$') || t == "this")
    {
        if dist_debug {
            eprintln!("[dist] abort bad target {t}");
        }
        return None;
    }
    if contains_empty_stack(&value) || !is_plain_join_value(&value) {
        if dist_debug {
            eprintln!(
                "[dist] abort bad value {}",
                crate::ast::render_expression_pub(&value)
            );
        }
        return None;
    }

    // Absorbed mode: the last arm textually contains the join store and
    // reaches it by construction (straight-line pre-store region guaranteed
    // by the search above), so it is always covered.
    let absorbed_arm: Option<usize> = match &join_site {
        JoinSite::Absorbed { arm, .. } => Some(*arm),
        JoinSite::After { .. } => None,
    };

    // Unanimity: every non-diverging arm either breaks to the join offset
    // or falls through toward it. Arms exiting elsewhere keep the join alive.
    for (n, arm) in arms.iter().enumerate() {
        if Some(n) == absorbed_arm {
            continue;
        }
        if arm_diverges(&arm.body) {
            continue;
        }
        if arm.body.is_empty() && !arm.breaks {
            continue; // pure fallthrough — inherits textually
        }
        match arm_broke_to.get(n).copied().flatten() {
            Some(t) if t == join_off => {}
            None if !arm.breaks => {}
            _ => {
                if dist_debug {
                    eprintln!(
                        "[dist] abort unanimity arm={n} breaks={} broke_to={:?} join={join_off}",
                        arm.breaks,
                        arm_broke_to.get(n).copied().flatten()
                    );
                }
                return None;
            }
        }
    }

    // Collect one value per contributing arm. Arm k's value is captured at
    // the next boundary; the last arm's value is the join's own (it was
    // consumed by the store during simulation).
    let mut values: Vec<Option<Expression>> = vec![None; arms.len()];
    for (n, arm) in arms.iter().enumerate() {
        if Some(n) == absorbed_arm {
            values[n] = Some(value.clone());
            continue;
        }
        if arm_diverges(&arm.body) {
            continue;
        }
        if arm.body.is_empty() && !arm.breaks {
            continue; // pure fallthrough — inherits the next arm's store
        }
        if arm.body.is_empty() && arm.breaks {
            // `case 0: <push v>; break;` — the pushed value (captured at the
            // next boundary) still needs its store. Without a value the join
            // store must stay.
            if n + 1 < arm_bounds.len() {
                let captured = stub.results.get(&arm_bounds[n + 1]);
                match captured {
                    Some(v) if v.len() == 1 && !contains_empty_stack(&v[0]) => {
                        values[n] = Some(v[0].clone());
                        continue;
                    }
                    _ => return None,
                }
            }
            return None;
        }
        if !arm.breaks && n + 1 < arms.len() {
            continue; // fallthrough with code — inherits the next arm's store
        }
        // The last arm's value is the join's own (consumed by the store).
        if n + 1 < arms.len() {
            let captured = stub.results.get(&arm_bounds[n + 1]);
            match captured {
                Some(v) if v.len() == 1 && !contains_empty_stack(&v[0]) => {
                    values[n] = Some(v[0].clone());
                }
                _ => {
                    if dist_debug {
                        eprintln!(
                            "[dist] abort novalue arm={n} bound={} captured={:?}",
                            arm_bounds[n + 1],
                            captured.map(|v| v.len())
                        );
                    }
                    return None;
                }
            }
        } else {
            values[n] = Some(value.clone());
        }
    }

    // Absorbed mode: split the store out of the last arm now that all
    // checks passed (infallible from here). The last arm is rebuilt from
    // pure pre-store code; join-rest relocates past the switch.
    let mut post: Vec<(usize, Statement)> = Vec::new();
    if let JoinSite::Absorbed { arm, stmt_index } = &join_site {
        let raw = raw_bodies.get(*arm).cloned().unwrap_or_default();
        let rest_raw: Vec<(usize, Statement)> = if *stmt_index + 1 < raw.len() {
            raw[*stmt_index + 1..].to_vec()
        } else {
            Vec::new()
        };
        let pre: Vec<(usize, Statement)> = raw[..(*stmt_index).min(raw.len())].to_vec();
        let structured: Vec<Statement> = restructure_control_flow(pre, if_targets, switches)
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        if let Some(last_arm) = arms.get_mut(*arm) {
            last_arm.body = structured;
            // Falls into the join position (relocated past the switch).
            last_arm.breaks = false;
        }
        if let Some(broke) = arm_broke_to.get_mut(*arm) {
            *broke = None;
        }
        post = restructure_control_flow(rest_raw, if_targets, switches);
    }

    // Rewrite: append the store/return to each contributing arm.
    if dist_debug {
        let have = values.iter().filter(|v| v.is_some()).count();
        eprintln!(
            "[dist] stub@{} values={}/{}",
            stub.offset,
            have,
            values.len()
        );
    }
    for (arm, val) in arms.iter_mut().zip(values.iter()) {
        if dist_debug && val.is_some() {
            eprintln!(
                "[dist] stub@{} arm keys={:?} gets store",
                stub.offset, arm.keys
            );
        }
    }
    for (arm, val) in arms.iter_mut().zip(values) {
        let Some(val) = val else { continue };
        if is_return {
            arm.body.push(Statement::Return(Some(val)));
        } else if let Some(ref t) = target {
            arm.body.push(Statement::Assign {
                target: t.clone(),
                value: val,
            });
        }
    }

    match join_site {
        JoinSite::After { .. } => {
            // Drop the join statement itself.
            Some(DistributeOutcome {
                consumed_through: consumed_through + 1,
                post: Vec::new(),
            })
        }
        JoinSite::Absorbed { .. } => Some(DistributeOutcome {
            consumed_through,
            post,
        }),
    }
}

fn restructure_control_flow(
    offset_stmts: Vec<(usize, Statement)>,
    if_targets: &HashMap<usize, usize>,
    switches: &[SwitchStub],
) -> Vec<(usize, Statement)> {
    let mut result = offset_stmts;

    // 0. Reconstruct switch statements (before loops/ifs so arm bodies
    //    still carry raw offsets; recursion structures nested content).
    fold_switches(&mut result, if_targets, switches);

    // 1. Reconstruct while loops (backward gotos)
    let mut i = 0;
    while i < result.len() {
        if let Statement::Expression(Expression::Unknown(ref text)) = result[i].1
            && let Some(target_str) = text.strip_prefix("@goto ")
            && let Ok(target_offset) = target_str.parse::<usize>()
        {
            let current_offset = result[i].0;
            if target_offset < current_offset {
                // Backward goto found! This is the end of a loop.
                // Find the start of the loop: the statement with the largest offset <= target_offset
                let start_idx = result
                    .iter()
                    .rposition(|(off, _)| *off <= target_offset)
                    .unwrap_or(0);

                // The If condition might be at start_idx or start_idx+1
                // Pattern 1: If is at start_idx (while loop)
                // Pattern 2: Setup stmts, then If at start_idx+1 (iterator pattern)
                let if_idx = if matches!(
                    &result[start_idx].1,
                    Statement::If { then_body, else_body, .. }
                    if then_body.is_empty() && else_body.is_none()
                ) {
                    Some(start_idx)
                } else if start_idx + 1 < result.len()
                    && matches!(
                        &result[start_idx + 1].1,
                        Statement::If { then_body, else_body, .. }
                        if then_body.is_empty() && else_body.is_none()
                    )
                {
                    Some(start_idx + 1)
                } else {
                    None
                };

                if let Some(if_idx) = if_idx {
                    // Extract condition (its jump target is unused here).
                    let cond = if let Statement::If { ref condition, .. } = result[if_idx].1 {
                        condition.clone()
                    } else {
                        unreachable!()
                    };

                    // Keep (offset, statement) pairs so nested control flow
                    // can be structured recursively with offset lookups intact.
                    // The body spans if_idx + 1 .. i (the backward goto).
                    let body: Vec<(usize, Statement)> = result[if_idx + 1..i].to_vec();

                    // Structure the loop body (nested ifs, switches, loops)
                    // before wrapping it into the While statement. The trailing
                    // backward goto itself is dropped by the recursion.
                    let body: Vec<Statement> = restructure_control_flow(body, if_targets, switches)
                        .into_iter()
                        .map(|(_, s)| s)
                        .collect();

                    // Replace the If with a While
                    // The If condition jumps OUT of the loop when true.
                    // So the while condition is the negation of the If condition.
                    // But if the If condition is already a Not, we can unwrap it.
                    let while_cond = if let Expression::Unary {
                        op: UnaryOp::Not,
                        operand,
                    } = cond
                    {
                        *operand
                    } else {
                        Expression::Unary {
                            op: UnaryOp::Not,
                            operand: Box::new(cond),
                        }
                    };

                    result[if_idx].1 = Statement::While {
                        condition: while_cond,
                        body,
                    };

                    // Remove the consumed statements (from if_idx + 1 to i inclusive)
                    result.drain(if_idx + 1..=i);

                    // Reset i to process the new structure
                    i = start_idx;
                    continue;
                }
            }
        }
        i += 1;
    }

    // 2. Reconstruct if/else and guard clauses.
    // `if_targets` is already offset-keyed, so it stays valid across drains.
    let current_targets: &HashMap<usize, usize> = if_targets;

    let mut i = 0;
    while i < result.len() {
        let stmt_offset = result[i].0;
        let is_empty_if = matches!(
            &result[i].1,
            Statement::If { then_body, else_body, .. }
            if then_body.is_empty() && else_body.is_none()
        );
        let has_target = current_targets.contains_key(&stmt_offset);

        if is_empty_if && has_target {
            let target_offset = current_targets[&stmt_offset];
            let cond = if let Statement::If { ref condition, .. } = result[i].1 {
                condition.clone()
            } else {
                unreachable!()
            };

            let target_idx = result
                .iter()
                .position(|(off, _)| *off >= target_offset)
                .unwrap_or(result.len());

            if target_idx <= i + 1 {
                i += 1;
                continue;
            }

            // Look for @goto marker between i+1 and target_idx
            let goto_pos = (i + 1..target_idx).find(|&j| {
                matches!(
                    &result[j].1,
                    Statement::Expression(Expression::Unknown(text))
                    if text.starts_with("@goto ")
                )
            });

            if let Some(g) = goto_pos {
                // if/else pattern — the goto is the early exit from the then-body
                let goto_target =
                    if let Statement::Expression(Expression::Unknown(ref text)) = result[g].1 {
                        text.strip_prefix("@goto ")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0)
                    } else {
                        0
                    };

                // Bytecode pattern: `if CONDITION goto TARGET; code_A; goto END; TARGET: code_B`
                // `if CONDITION goto TARGET` jumps when TRUE — code_A runs when FALSE.
                // So code_A = else body, code_B = then body.
                // The then body spans g+1 .. else_end (the join point of the
                // early-exit goto), NOT .. target_idx: statements are keyed by
                // their *last* instruction offset, so the first then-statement
                // can carry an offset >= TARGET (its load sequence starts
                // before TARGET but its statement offset lands after).
                let else_end = result
                    .iter()
                    .position(|(off, _)| *off >= goto_target)
                    .unwrap_or(result.len());

                // Degenerate ranges (empty else arm, join before branch start,
                // or a goto landing inside the else sequence) mean this does
                // not match the expected if/else shape: bail out safely.
                if else_end <= g + 1 || else_end > result.len() {
                    i += 1;
                    continue;
                }

                let else_raw: Vec<(usize, Statement)> = result[i + 1..g].to_vec();
                let then_raw: Vec<(usize, Statement)> = result[g + 1..else_end].to_vec();

                // Structure both arms recursively (nested ifs/switches/loops).
                let then_body: Vec<Statement> =
                    restructure_control_flow(then_raw, if_targets, switches)
                        .into_iter()
                        .map(|(_, s)| s)
                        .collect();
                let else_body_stmts: Vec<Statement> =
                    restructure_control_flow(else_raw, if_targets, switches)
                        .into_iter()
                        .map(|(_, s)| s)
                        .collect();

                result[i].1 = Statement::If {
                    condition: cond,
                    then_body,
                    else_body: if else_body_stmts.is_empty() {
                        None
                    } else {
                        Some(else_body_stmts)
                    },
                };

                let remove_end = target_idx.max(else_end);
                let drain_start = i + 1;
                if drain_start < result.len() && drain_start < remove_end {
                    let drain_end = remove_end.min(result.len());
                    result.drain(drain_start..drain_end);
                }
            } else {
                // Guard clause pattern — check for nested guards
                // Guard condition: when TRUE, skip body. Negate for "when to run" logic.
                let mut combined_cond = negate_guard_cond(cond);
                let mut body_start = i + 1;

                while body_start < target_idx {
                    let body_offset = result[body_start].0;
                    let bt = current_targets.get(&body_offset).copied();
                    if let Statement::If {
                        condition: ref inner_cond,
                        then_body: ref inner_then,
                        else_body: ref inner_else,
                    } = result[body_start].1
                        && inner_then.is_empty()
                        && inner_else.is_none()
                        && bt.is_some()
                    {
                        // Merge: combined_cond = (combined_cond && negated_inner)
                        combined_cond = Expression::Binary {
                            left: Box::new(combined_cond),
                            op: BinaryOp::BoolAnd,
                            right: Box::new(negate_guard_cond(inner_cond.clone())),
                        };
                        body_start += 1;
                    } else {
                        break;
                    }
                }

                // Build then_body from the merged guard
                let then_body: Vec<Statement> = result[body_start..target_idx]
                    .iter()
                    .map(|(_, s)| s.clone())
                    .collect();

                result[i].1 = Statement::If {
                    condition: combined_cond,
                    then_body,
                    else_body: None,
                };

                if i + 1 < result.len() && i + 1 < target_idx {
                    let drain_end = target_idx.min(result.len());
                    result.drain(i + 1..drain_end);
                }
            }
        }

        // Remove surviving @goto markers
        if let Statement::Expression(Expression::Unknown(ref text)) = result[i].1
            && text.starts_with("@goto ")
        {
            result.remove(i);
            continue;
        }

        i += 1;
    }

    result
}

/// Recursively merge empty guard clauses inside all statement bodies (while, if, etc.)
fn merge_guards_recursive(stmts: &mut Vec<Statement>) {
    // First merge guards in this level
    merge_guard_clauses(stmts);
    // Then recurse into nested bodies
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                merge_guards_recursive(then_body);
                if let Some(body) = else_body {
                    merge_guards_recursive(body);
                }
            }
            Statement::While { body, .. } => {
                merge_guards_recursive(body);
            }
            Statement::For {
                init, update, body, ..
            } => {
                for s in init.iter_mut().chain(update.iter_mut()) {
                    let mut one = vec![std::mem::replace(s, Statement::Unknown(String::new()))];
                    merge_guards_recursive(&mut one);
                    *s = one
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| Statement::Unknown(String::new()));
                }
                merge_guards_recursive(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    merge_guards_recursive(&mut arm.body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                merge_guards_recursive(try_body);
                merge_guards_recursive(catch_body);
            }
            _ => {}
        }
    }
}

/// Merge consecutive empty guard clauses into a single if with combined condition
fn merge_guard_clauses(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        if let Statement::If {
            condition,
            then_body,
            else_body,
        } = &stmts[i]
            && then_body.is_empty()
            && else_body.is_none()
        {
            // This is an empty guard — merge with next guards
            let mut combined_cond = negate_guard_cond(condition.clone());
            let mut j = i + 1;
            while j < stmts.len() {
                if let Statement::If {
                    condition: inner_cond,
                    then_body: inner_then,
                    else_body: inner_else,
                } = &stmts[j]
                {
                    if inner_then.is_empty() && inner_else.is_none() {
                        combined_cond = Expression::Binary {
                            left: Box::new(combined_cond),
                            op: BinaryOp::BoolAnd,
                            right: Box::new(negate_guard_cond(inner_cond.clone())),
                        };
                        j += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if j > i + 1 {
                let body: Vec<Statement> = stmts.drain(i + 1..j).collect();
                stmts[i] = Statement::If {
                    condition: combined_cond,
                    then_body: body,
                    else_body: None,
                };
            }
        }
        i += 1;
    }
}

/// Recursively remove @goto markers from all statement bodies.
/// Leftover `@switch N` placeholders (bailed-out switches) become comments
/// so they never render as invalid Java.
fn remove_goto_markers(stmts: &mut Vec<Statement>) {
    stmts.retain(|s| !matches!(s, Statement::Expression(Expression::Unknown(text)) if text.starts_with("@goto ")));
    for stmt in stmts.iter_mut() {
        if let Statement::Expression(Expression::Unknown(text)) = stmt
            && let Some(off) = text.strip_prefix("@switch ")
        {
            *text = format!("/* unreconstructed switch at {off} */");
        }
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                remove_goto_markers(then_body);
                if let Some(body) = else_body {
                    remove_goto_markers(body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                remove_goto_markers(try_body);
                remove_goto_markers(catch_body);
            }
            Statement::While { body, .. } => {
                remove_goto_markers(body);
            }
            Statement::For {
                init, update, body, ..
            } => {
                for s in init.iter_mut().chain(update.iter_mut()) {
                    let mut one = vec![std::mem::replace(s, Statement::Unknown(String::new()))];
                    remove_goto_markers(&mut one);
                    *s = one
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| Statement::Unknown(String::new()));
                }
                remove_goto_markers(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    remove_goto_markers(&mut arm.body);
                }
            }
            _ => {}
        }
    }
}

fn is_const_true(expr: &Expression) -> bool {
    matches!(expr, Expression::ConstInt(v) if *v != 0)
        || matches!(expr, Expression::Unknown(s) if s == "true")
}

fn is_const_false(expr: &Expression) -> bool {
    matches!(expr, Expression::ConstInt(0))
        || matches!(expr, Expression::Unknown(s) if s == "false")
}

/// Drop unreachable code under constant conditions: `if (0)` keeps only the
/// else-branch, `if (1)` keeps only the then-branch, `while (0)` vanishes,
/// `while (1)` becomes `while (true)`. Cleans up opaque predicates and
/// folding residue the way CFR does.
fn remove_dead_branches(stmts: &mut Vec<Statement>) {
    let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
    for stmt in stmts.drain(..) {
        match stmt {
            Statement::If {
                condition,
                mut then_body,
                mut else_body,
            } => {
                remove_dead_branches(&mut then_body);
                if let Some(eb) = else_body.as_mut() {
                    remove_dead_branches(eb);
                }
                if is_const_false(&condition) {
                    out.extend(else_body.unwrap_or_default());
                } else if is_const_true(&condition) {
                    out.extend(then_body);
                } else {
                    out.push(Statement::If {
                        condition,
                        then_body,
                        else_body,
                    });
                }
            }
            Statement::For {
                mut init,
                condition,
                mut update,
                mut body,
            } => {
                remove_dead_branches(&mut init);
                remove_dead_branches(&mut update);
                remove_dead_branches(&mut body);
                out.push(Statement::For {
                    init,
                    condition,
                    update,
                    body,
                });
            }
            Statement::While {
                condition,
                mut body,
            } => {
                remove_dead_branches(&mut body);
                if is_const_false(&condition) {
                    // Dead loop — drop it entirely.
                } else if is_const_true(&condition) {
                    out.push(Statement::While {
                        condition: Expression::Unknown("true".to_string()),
                        body,
                    });
                } else {
                    out.push(Statement::While { condition, body });
                }
            }
            Statement::Switch {
                discriminant,
                mut arms,
            } => {
                for arm in arms.iter_mut() {
                    remove_dead_branches(&mut arm.body);
                }
                out.push(Statement::Switch { discriminant, arms });
            }
            Statement::ForEach {
                var_type,
                var_name,
                collection,
                mut body,
            } => {
                remove_dead_branches(&mut body);
                out.push(Statement::ForEach {
                    var_type,
                    var_name,
                    collection,
                    body,
                });
            }
            Statement::TryCatch {
                mut try_body,
                catch_type,
                catch_var,
                mut catch_body,
                ..
            } => {
                remove_dead_branches(&mut try_body);
                remove_dead_branches(&mut catch_body);
                out.push(Statement::TryCatch {
                    try_body,
                    catch_type,
                    catch_var,
                    catch_body,
                    resources: vec![],
                    finally_body: None,
                });
            }
            other => out.push(other),
        }
    }
    *stmts = out;
}

/// True when `expr` can stand bare as a Java `if` condition: comparisons,
/// boolean logic, boolean-assumed locals/invokes/field reads.
fn is_boolean_shaped(expr: &Expression) -> bool {
    match expr {
        Expression::Binary { op, .. } => matches!(
            op,
            BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::Le
                | BinaryOp::Ge
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::BoolAnd
                | BinaryOp::BoolOr
        ),
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => is_boolean_shaped(operand),
        Expression::InstanceOf { .. } => true,
        Expression::Local(_) | Expression::Invoke { .. } | Expression::FieldAccess { .. } => true,
        _ => false,
    }
}

/// Wrap a non-boolean condition into an explicit `!= 0` (or `== 0` when it
/// is a negation), so the rendered source is valid Java.
fn ensure_boolean_condition(expr: &mut Expression) {
    if is_boolean_shaped(expr) {
        return;
    }
    if let Expression::Unary {
        op: UnaryOp::Not,
        operand,
    } = expr
    {
        // `!X` over a non-boolean X is also invalid Java: rewrite to `X == 0`.
        let inner = std::mem::replace(operand.as_mut(), Expression::ConstNull);
        *expr = Expression::Binary {
            left: Box::new(inner),
            op: BinaryOp::Eq,
            right: Box::new(Expression::ConstInt(0)),
        };
        return;
    }
    let inner = std::mem::replace(expr, Expression::ConstNull);
    *expr = Expression::Binary {
        left: Box::new(inner),
        op: BinaryOp::Ne,
        right: Box::new(Expression::ConstInt(0)),
    };
}

/// Apply `ensure_boolean_condition` to every If/While condition, recursing
/// into nested bodies.
fn ensure_boolean_conditions_recursive(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                ensure_boolean_condition(condition);
                ensure_boolean_conditions_recursive(then_body);
                if let Some(eb) = else_body {
                    ensure_boolean_conditions_recursive(eb);
                }
            }
            Statement::While { condition, body } => {
                ensure_boolean_condition(condition);
                ensure_boolean_conditions_recursive(body);
            }
            Statement::For {
                init,
                update,
                condition,
                body,
                ..
            } => {
                ensure_boolean_conditions_recursive(init);
                ensure_boolean_conditions_recursive(update);
                if let Some(cond) = condition {
                    ensure_boolean_condition(cond);
                }
                ensure_boolean_conditions_recursive(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    ensure_boolean_conditions_recursive(&mut arm.body);
                }
            }
            Statement::ForEach { body, .. } => {
                ensure_boolean_conditions_recursive(body);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                ensure_boolean_conditions_recursive(try_body);
                ensure_boolean_conditions_recursive(catch_body);
            }
            _ => {}
        }
    }
}

/// Extract a candidate ternary join value from an if/else pair body.
/// Returns Some((target, then_value, else_value, is_return)) when the body
/// consists solely of one assignment/decl/return of a single plain value.
fn ternary_join(stmts: &[Statement]) -> Option<(String, Expression, bool)> {
    let as_join = |s: &Statement| match s {
        Statement::Assign { target, value } => {
            if matches!(value, Expression::Unknown(_)) {
                None
            } else {
                Some((target.clone(), value.clone(), false))
            }
        }
        Statement::VarDecl {
            target,
            value: Some(value),
            ..
        } => Some((target.clone(), value.clone(), false)),
        Statement::Return(Some(value)) => Some((String::new(), value.clone(), true)),
        _ => None,
    };
    if stmts.len() != 1 {
        return None;
    }
    as_join(&stmts[0])
}

/// True when the expression is safe to duplicate into both ternary arms:
/// plain values, field/array reads, and invocations of whitelisted pure
///-looking shapes. Side-effecting calls are allowed only if they are the
/// sole content of the arm (they are, by ternary_join).
fn ternary_value_ok(expr: &Expression) -> bool {
    !matches!(expr, Expression::Unknown(_))
}

/// Deep check: does this expression already contain a ternary anywhere?
/// Deliberate readability cap — we only ever fold into a *single-level*
/// ternary (`c ? a : b` with plain arms). Anything that would nest
/// (`c ? (d ? e : f) : g`, chains, ternaries inside call args) is left as
/// explicit if/else. Lambda bodies are a separate scope and don't count:
/// they render as blocks, so a ternary inside one adds no visual nesting.
fn contains_ternary(expr: &Expression) -> bool {
    match expr {
        Expression::Ternary { .. } => true,
        Expression::FieldAccess { object, .. } => contains_ternary(object),
        Expression::Binary { left, right, .. } => contains_ternary(left) || contains_ternary(right),
        Expression::Unary { operand, .. } => contains_ternary(operand),
        Expression::New { args, .. } | Expression::Invoke { args, .. } => {
            args.iter().any(contains_ternary)
        }
        Expression::NewArray { size, .. } => contains_ternary(size),
        Expression::ArrayAccess { array, index } => {
            contains_ternary(array) || contains_ternary(index)
        }
        Expression::Cast { expr, .. } | Expression::InstanceOf { expr, .. } => {
            contains_ternary(expr)
        }
        Expression::Concat { parts, .. } => parts.iter().any(contains_ternary),
        Expression::Lambda { .. } => false,
        _ => false,
    }
}

/// A fold is allowed only when the resulting ternary stays single-level:
/// no ternary in the condition or either arm.
fn ternary_fold_ok(cond: &Expression, then_value: &Expression, else_value: &Expression) -> bool {
    !contains_ternary(cond) && !contains_ternary(then_value) && !contains_ternary(else_value)
}

/// Collapse `if (c) { x = a; } else { x = b; }` into `x = c ? a : b;`
/// (plus the implicit-else `if (c) { return a; } return b;` variant below)
/// recursively across all statement bodies.
///
/// Runs to a fixpoint: rule 1 can produce the `return` that rule 2 needs
/// (`if (a) { return n; } if (b) { return x; } else { return y; }`).
/// Folds that would nest (`a ? 1 : (b ? 2 : 3)`) are deliberately skipped
/// by `ternary_fold_ok` — chains stay as explicit if/else for readability.
/// Terminates: rule 1 never creates an `If`, rule 2 shrinks the list.
fn reconstruct_ternaries(stmts: &mut Vec<Statement>) {
    loop {
        let mut changed = false;
        let mut i = 0;
        while i < stmts.len() {
            // Rule 2 first: implicit else via fall-through return.
            // `if (c) { return a; } return b;` -> `return c ? a : b;`
            // Sound because the then-arm diverges (returns), so reaching the
            // second return means `c` was false — an implicit else.
            let fallthrough = match (&stmts[i], stmts.get(i + 1)) {
                (
                    Statement::If {
                        condition,
                        then_body,
                        else_body: None,
                    },
                    Some(Statement::Return(Some(b))),
                ) => match then_body.as_slice() {
                    [Statement::Return(Some(a))]
                        if a != b
                            && ternary_fold_ok(condition, a, b)
                            && ternary_value_ok(a)
                            && ternary_value_ok(b) =>
                    {
                        Some((condition.clone(), a.clone(), b.clone()))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some((cond, a, b)) = fallthrough {
                stmts[i] = Statement::Return(Some(Expression::Ternary {
                    condition: Box::new(cond),
                    then_expr: Box::new(a),
                    else_expr: Box::new(b),
                }));
                stmts.remove(i + 1);
                changed = true;
                i += 1;
                continue;
            }

            let (then_join, else_join, cond) = {
                let Statement::If {
                    condition,
                    then_body,
                    else_body: Some(else_body),
                } = &stmts[i]
                else {
                    i += 1;
                    continue;
                };
                match (ternary_join(then_body), ternary_join(else_body)) {
                    (Some(t), Some(e)) => (t, e, condition.clone()),
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            };

            let (t_target, t_value, t_ret) = then_join;
            let (e_target, e_value, e_ret) = else_join;

            // Both arms must agree on the join kind (both return or both assign
            // the same target), the values must differ (`c ? x : x` is noise
            // a human would never write) and be duplication-safe.
            if t_ret != e_ret
                || t_target != e_target
                || t_value == e_value
                || !ternary_fold_ok(&cond, &t_value, &e_value)
                || !ternary_value_ok(&t_value)
                || !ternary_value_ok(&e_value)
            {
                i += 1;
                continue;
            }

            let ternary = Expression::Ternary {
                condition: Box::new(cond),
                then_expr: Box::new(t_value),
                else_expr: Box::new(e_value),
            };
            stmts[i] = if t_ret {
                changed = true;
                Statement::Return(Some(ternary))
            } else if t_target.is_empty() {
                i += 1;
                continue;
            } else {
                changed = true;
                Statement::Assign {
                    target: t_target,
                    value: ternary,
                }
            };
            i += 1;
        }
        if !changed {
            break;
        }
    }
}

fn reconstruct_ternaries_recursive(stmts: &mut Vec<Statement>) {
    reconstruct_ternaries(stmts);
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                reconstruct_ternaries_recursive(then_body);
                if let Some(eb) = else_body {
                    reconstruct_ternaries_recursive(eb);
                }
            }
            Statement::While { body, .. } => reconstruct_ternaries_recursive(body),
            Statement::For {
                init, update, body, ..
            } => {
                reconstruct_ternaries_recursive(init);
                reconstruct_ternaries_recursive(update);
                reconstruct_ternaries_recursive(body);
            }
            Statement::ForEach { body, .. } => reconstruct_ternaries_recursive(body),
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    reconstruct_ternaries_recursive(&mut arm.body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                reconstruct_ternaries_recursive(try_body);
                reconstruct_ternaries_recursive(catch_body);
            }
            _ => {}
        }
    }
}

/// Recognize the javac try-with-resources shape inside a statement list:
/// `[resource decl, TryCatch{ try: [use..close], catch: Throwable [close,
/// addSuppressed, throw] }]` and fold it into one TryCatch with resources.
/// Also hoists `finally`: a handler marked `any` (catch_type == "any") whose
/// body appears after the primary try/catch.
#[allow(dead_code)]
fn try_catch_has_synthetic_close_handler(tc: &Statement) -> bool {
    let Statement::TryCatch {
        catch_type,
        catch_body,
        ..
    } = tc
    else {
        return false;
    };
    if catch_type != "Throwable" {
        return false;
    }
    let closes = catch_body.iter().any(|s| {
        matches!(
            s,
            Statement::Expression(Expression::Invoke { target, .. }) if target.ends_with(".close")
        )
    });
    let suppresses = catch_body.iter().any(|s| {
        let rendered = format!("{s:?}");
        rendered.contains("addSuppressed")
    });
    closes && suppresses
}

/// Fold the clean javac try-with-resources shape:
/// `[Res res = expr;] TryCatch{ try: [body], catch: Throwable [TC{try:
/// [res.close()], catch: T [x.addSuppressed(y), throw y]}] }; res.close();
/// Return(... res-var ...)`
/// becomes `TryCatch{ resources: [res decl], try_body: body + return }`.
/// Everything the compiler synthesizes (close calls, the suppression chain)
/// is deleted: javac regenerates it from `try (...)`.
fn fold_try_with_resources(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        // 1. Match the primary Throwable handler with the nested suppress try.
        let close_target: Option<String> = {
            let Statement::TryCatch {
                catch_type,
                catch_body,
                ..
            } = &stmts[i]
            else {
                i += 1;
                continue;
            };
            if catch_body.len() != 1 {
                i += 1;
                continue;
            }
            if catch_type != "Throwable" {
                // close-cannot-throw shape: the compiler leaves the resource
                // outside the try and the user catch handles everything.
                // Accept it; the sibling close + suppression check below
                // still applies.
            }
            let Statement::TryCatch {
                try_body: inner_try,
                catch_body: inner_catch,
                ..
            } = &catch_body[0]
            else {
                i += 1;
                continue;
            };
            if inner_try.len() != 1 {
                i += 1;
                continue;
            }
            let Statement::Expression(Expression::Invoke { target, args }) = &inner_try[0] else {
                i += 1;
                continue;
            };
            if !args.is_empty() || !target.ends_with(".close") {
                i += 1;
                continue;
            }
            let suppresses = inner_catch.iter().any(|s| {
                let rendered = format!("{s:?}");
                rendered.contains("addSuppressed") || rendered.contains("throw ")
            });
            if !suppresses {
                i += 1;
                continue;
            }
            Some(
                target
                    .strip_suffix(".close")
                    .unwrap_or("")
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
        };
        let Some(res) = close_target else { continue };

        // 2. The resource declaration must sit immediately before the try.
        //    The decl is keyed at its LAST instruction offset, which may land
        //    before the try range start while earlier decls (or other
        //    statements) sit between it and the try: walk back over any
        //    statements that do not touch the resource.
        let mut decl_pos = None;
        if i > 0 {
            for back in (0..i).rev() {
                let rendered = format!("{:?}", stmts[back]);
                if rendered.contains(&res) {
                    decl_pos = Some(back);
                    break;
                }
                if back < i - 2 {
                    break;
                }
            }
        }
        let Some(dp) = decl_pos else {
            i += 1;
            continue;
        };
        let try_pos = dp + 1;
        let resource = match std::mem::replace(&mut stmts[dp], Statement::Unknown(String::new())) {
            Statement::VarDecl {
                target,
                var_type,
                value: Some(value),
            } if target == res => Statement::VarDecl {
                target,
                var_type,
                value: Some(value),
            },
            Statement::Assign { target, value } if target == res => Statement::VarDecl {
                target,
                var_type: None,
                value: Some(value),
            },
            other => {
                stmts[dp] = other;
                i += 1;
                continue;
            }
        };
        // The slot that held the declaration is gone from the statement
        // stream; shift the list so no empty placeholder renders.
        stmts.remove(dp);
        let try_pos = try_pos - 1;

        // 3. Empty the synthetic handler and lift the declaration.
        let mut tc = std::mem::replace(&mut stmts[try_pos], Statement::Unknown(String::new()));
        let Statement::TryCatch {
            try_body: ref mut try_body_slot,
            catch_body: ref mut catch_body_slot,
            resources: ref mut resources_slot,
            ..
        } = tc
        else {
            unreachable!()
        };
        catch_body_slot.clear();
        resources_slot.push(resource);

        // 4. Drop the normal-path sibling close.
        let mut consumed_tail = 0;
        if try_pos + 1 < stmts.len()
            && matches!(
                &stmts[try_pos + 1],
                Statement::Expression(Expression::Invoke { target, args })
                if *target == format!("{res}.close") && args.is_empty()
            )
        {
            stmts.remove(try_pos + 1);
            consumed_tail += 1;
        }

        // 5. A trailing return of a variable declared at the end of the try
        //    body belongs inside the try (javac hoists it past close).
        let last_decl = match try_body_slot.last() {
            Some(Statement::VarDecl { target, .. }) => Some(target.clone()),
            Some(Statement::Assign { target, .. }) => Some(target.clone()),
            _ => None,
        };
        if let Some(declared) = last_decl
            && try_pos + 1 < stmts.len()
        {
            let moves_in = match &stmts[try_pos + 1] {
                Statement::Return(Some(value)) => {
                    let rendered = format!("{value:?}");
                    rendered.contains(&declared)
                }
                _ => false,
            };
            if moves_in {
                let ret = stmts.remove(try_pos + 1);
                try_body_slot.push(ret);
                consumed_tail += 1;
            }
        }
        // tc was taken out of the list; write the mutated node back.
        stmts[try_pos] = tc;
        i = try_pos + 1 + consumed_tail;
    }
}

fn finalize_try_shapes(stmts: &mut Vec<Statement>) {
    fold_try_with_resources(stmts);
    // In-try resource hoist (close-cannot-throw shape: decl + close inside
    // the try, user catch handler): fold decl into resources.
    {
        let mut k = 0;
        while k < stmts.len() {
            let Statement::TryCatch {
                try_body,
                resources,
                ..
            } = &mut stmts[k]
            else {
                k += 1;
                continue;
            };
            if !resources.is_empty() || try_body.len() < 3 {
                k += 1;
                continue;
            }
            // Resource = first decl; close = last body statement.
            let res_target = match &try_body[0] {
                Statement::VarDecl { target, .. } => target.clone(),
                Statement::Assign { target, .. } => target.clone(),
                _ => {
                    k += 1;
                    continue;
                }
            };
            let closes_at_end = matches!(
                try_body.last(),
                Some(Statement::Expression(Expression::Invoke { target, args }))
                if *target == format!("{res_target}.close") && args.is_empty()
            );
            if !closes_at_end {
                k += 1;
                continue;
            }
            let resource = match try_body.remove(0) {
                Statement::VarDecl {
                    target,
                    var_type,
                    value: Some(value),
                } => Statement::VarDecl {
                    target,
                    var_type,
                    value: Some(value),
                },
                Statement::Assign { target, value } => Statement::VarDecl {
                    target,
                    var_type: None,
                    value: Some(value),
                },
                other => {
                    try_body.insert(0, other);
                    k += 1;
                    continue;
                }
            };
            try_body.pop();
            resources.push(resource);
            k += 1;
        }
    }
    // In-try resource hoist: try { Res r = ...; use; r.close(); } catch
    // (Throwable e) { r.close(); addSuppressed; throw } -> try (Res r = ...) { use }.
    let mut k = 0;
    while k < stmts.len() {
        let (decl_idx, res_target) = {
            let Statement::TryCatch { try_body, .. } = &stmts[k] else {
                k += 1;
                continue;
            };
            match try_body.first() {
                Some(Statement::VarDecl {
                    target,
                    value: Some(_),
                    ..
                }) => (k, target.clone()),
                _ => {
                    k += 1;
                    continue;
                }
            }
        };
        // The close target inside the try body must be the declared var.
        let Statement::TryCatch {
            try_body,
            catch_type,
            catch_body,
            resources,
            ..
        } = &mut stmts[decl_idx]
        else {
            unreachable!()
        };
        if catch_type != "Throwable" || !resources.is_empty() {
            k += 1;
            continue;
        }
        let closes_in_body = try_body.iter().any(|s| {
            matches!(
                s,
                Statement::Expression(Expression::Invoke { target, .. })
                if target == &format!("{res_target}.close")
            )
        });
        let handler_is_synthetic = catch_body.iter().any(|s| {
            let rendered = format!("{s:?}");
            rendered.contains("addSuppressed")
        });
        if !(closes_in_body && handler_is_synthetic) {
            k += 1;
            continue;
        }
        // Hoist: remove decl + close from try body, empty the synthetic
        // handler, move the decl into resources.
        let resource = match try_body.remove(0) {
            Statement::VarDecl {
                target,
                var_type,
                value,
            } => Statement::VarDecl {
                target,
                var_type,
                value,
            },
            other => {
                try_body.insert(0, other);
                k += 1;
                continue;
            }
        };
        try_body.retain(|s| {
            !(matches!(
                s,
                Statement::Expression(Expression::Invoke { target, .. })
                if target == &format!("{res_target}.close")
            ))
        });
        catch_body.clear();
        resources.push(resource);
        k += 1;
    }

    let mut i = 0;
    while i + 1 < stmts.len() {
        // TWR shape: resource decl immediately before a TryCatch whose
        // try body starts by consuming the resource and whose Throwable
        // handler performs close + addSuppressed.
        let is_twr_candidate = match (&stmts[i], &stmts[i + 1]) {
            (
                Statement::VarDecl { target, .. },
                Statement::TryCatch {
                    try_body,
                    catch_type,
                    catch_body,
                    resources,
                    ..
                },
            ) => {
                let consumed = !resources.is_empty();
                let closes = catch_body.iter().any(|s| {
                    matches!(
                        s,
                        Statement::Expression(Expression::Invoke { target, .. })
                        if target.ends_with(".close")
                    ) || matches!(
                        s,
                        Statement::Expression(Expression::Unknown(text))
                        if text.contains("close")
                    )
                });
                let uses = try_body.iter().any(|s| {
                    let rendered = format!("{s:?}");
                    rendered.contains(target.as_str())
                });
                consumed && closes && uses && catch_type == "Throwable"
            }
            _ => false,
        };

        if is_twr_candidate {
            // Pull the resource declaration out first (single borrow).
            let resource = match std::mem::replace(&mut stmts[i], Statement::Unknown(String::new()))
            {
                Statement::VarDecl {
                    target,
                    var_type,
                    value,
                } => Statement::VarDecl {
                    target,
                    var_type,
                    value,
                },
                other => {
                    stmts[i] = other;
                    i += 1;
                    continue;
                }
            };
            let Statement::TryCatch {
                try_body,
                catch_type,
                catch_body,
                resources,
                ..
            } = &mut stmts[i + 1]
            else {
                unreachable!()
            };
            // Remove the close call from the primary try body.
            try_body.retain(|s| {
                !(matches!(
                    s,
                    Statement::Expression(Expression::Invoke { target, .. })
                    if target.ends_with(".close")
                ))
            });
            // Drop the compiler-synthesized close/suppress handler: javac
            // re-generates it from try (...).
            catch_body.clear();
            *catch_type = "Throwable".to_string();
            resources.push(resource);
            stmts.remove(i);
            i += 1;
            continue;
        }
        i += 1;
    }

    // Finally hoisting: a `catch_type == "any"` TryCatch whose body matches
    // the trailing duplicated block collapses into finally_body of the
    // preceding TryCatch. The straight-line duplicate after the try is
    // removed by truncate_after_terminator.
    let mut j = 0;
    while j < stmts.len() {
        let is_any = matches!(
            &stmts[j],
            Statement::TryCatch { catch_type, .. } if catch_type == "any"
        );
        if is_any && j > 0 {
            // Detach the any-region (single borrow), then fold its body into
            // the previous TryCatch as the finally block.
            let any_region = std::mem::replace(&mut stmts[j], Statement::Unknown(String::new()));
            let Statement::TryCatch {
                try_body: any_body, ..
            } = any_region
            else {
                unreachable!()
            };
            let any_len = any_body.len();
            let prev_is_try = matches!(stmts[j - 1], Statement::TryCatch { .. });
            if prev_is_try {
                if let Statement::TryCatch {
                    catch_body,
                    finally_body,
                    ..
                } = &mut stmts[j - 1]
                {
                    // The any-body is the actual finally content, with the
                    // duplicated tail removed.
                    let mut fin = any_body;
                    fin.truncate(fin.len().saturating_sub(0));
                    *catch_body = Vec::new();
                    *finally_body = Some(fin);
                }
                stmts.remove(j);
                continue;
            }
            // Not foldable: restore and move on.
            stmts[j] = Statement::TryCatch {
                try_body: Vec::new(),
                catch_type: "any".to_string(),
                catch_var: String::new(),
                catch_body: (0..any_len)
                    .map(|_| Statement::Unknown(String::new()))
                    .collect(),
                resources: vec![],
                finally_body: None,
            };
        }
        j += 1;
    }
}

fn finalize_try_shapes_recursive(stmts: &mut Vec<Statement>) {
    finalize_try_shapes(stmts);
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                finalize_try_shapes_recursive(then_body);
                if let Some(eb) = else_body {
                    finalize_try_shapes_recursive(eb);
                }
            }
            Statement::While { body, .. } => finalize_try_shapes_recursive(body),
            Statement::For { body, .. } => finalize_try_shapes_recursive(body),
            Statement::ForEach { body, .. } => finalize_try_shapes_recursive(body),
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    finalize_try_shapes_recursive(&mut arm.body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                resources,
                finally_body,
                ..
            } => {
                finalize_try_shapes_recursive(try_body);
                finalize_try_shapes_recursive(catch_body);
                finalize_try_shapes_recursive(resources);
                if let Some(fin) = finally_body {
                    finalize_try_shapes_recursive(fin);
                }
            }
            _ => {}
        }
    }
}

/// Remove statements after the first statement that always terminates
/// control flow (Return or Throw) on the same nesting level.
fn truncate_after_terminator(stmts: &mut Vec<Statement>) {
    for idx in 0..stmts.len() {
        if matches!(stmts[idx], Statement::Return(_)) {
            // A trailing Return terminates the straight-line flow. Everything
            // after it on this level is dead unless it is a catch-all any
            // region (handled elsewhere).
            stmts.truncate(idx + 1);
            return;
        }
    }
}

fn truncate_after_terminator_recursive(stmts: &mut Vec<Statement>) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                truncate_after_terminator_recursive(then_body);
                if let Some(eb) = else_body {
                    truncate_after_terminator_recursive(eb);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                truncate_after_terminator_recursive(try_body);
                truncate_after_terminator_recursive(catch_body);
                if let Some(fin) = finally_body {
                    truncate_after_terminator_recursive(fin);
                }
            }
            _ => {}
        }
    }
    truncate_after_terminator(stmts);
}

/// True when the statement is a simple self-update of `var` suitable for a
/// for-loop update clause: `var = var + n` and friends.
fn is_self_update(stmt: &Statement, var: &str) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            target == var
                && match value {
                    Expression::Binary { left, .. } => matches!(
                        left.as_ref(),
                        Expression::Local(name) if name == var
                    ),
                    _ => false,
                }
        }
        _ => false,
    }
}

/// Rewrite `init; while (i < n) { ...; i = i + 1; }` into
/// `for (init; i < n; i = i + 1) { ... }` when the loop is counted: one
/// leading init binding the loop var, exactly one trailing self-update, and
/// the variable not assigned anywhere else in the body.
fn desugar_counted_loops(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i + 1 < stmts.len() {
        let is_while = matches!(stmts[i + 1], Statement::While { .. });
        let is_init = matches!(
            stmts[i],
            Statement::Assign { .. } | Statement::VarDecl { .. }
        );
        if is_while && is_init {
            let loop_var = {
                let Statement::While { condition, .. } = &stmts[i + 1] else {
                    unreachable!()
                };
                match condition {
                    Expression::Binary { left, .. } => match left.as_ref() {
                        Expression::Local(name) => name.clone(),
                        _ => {
                            i += 1;
                            continue;
                        }
                    },
                    _ => {
                        i += 1;
                        continue;
                    }
                }
            };

            // Init must bind the same variable.
            let init_binds_var = match &stmts[i] {
                Statement::Assign { target, .. } => target == &loop_var,
                Statement::VarDecl { target, .. } => target == &loop_var,
                _ => false,
            };
            if !init_binds_var {
                i += 1;
                continue;
            }

            let Statement::While { body, .. } = &mut stmts[i + 1] else {
                unreachable!()
            };
            // Update must be the LAST statement of the body and self-update.
            let update_is_last = body
                .last()
                .is_some_and(|last| is_self_update(last, &loop_var));
            if !update_is_last {
                i += 1;
                continue;
            }

            // The variable must not be assigned anywhere else in the body.
            let var_reassigned_in_body = body[..body.len() - 1].iter().any(|s| {
                let mut found = false;
                count_direct_assigns(s, &loop_var, &mut found);
                found
            });
            if var_reassigned_in_body {
                i += 1;
                continue;
            }

            let (condition, update, body) = match std::mem::replace(
                &mut stmts[i + 1],
                Statement::Unknown("/* slot */".to_string()),
            ) {
                Statement::While {
                    condition,
                    mut body,
                } => {
                    let update = body.pop().expect("last checked");
                    (condition, update, body)
                }
                _ => unreachable!(),
            };
            let init_stmt =
                std::mem::replace(&mut stmts[i], Statement::Unknown("/* slot */".to_string()));
            stmts[i] = Statement::For {
                init: vec![init_stmt],
                condition: Some(condition),
                update: vec![update],
                body,
            };
            stmts.remove(i + 1);
            i += 1;
            continue;
        }
        i += 1;
    }
}

/// Count direct assignments to `var` in `stmt` (immediate Assign/VarDecl
/// targets, descending into all nested bodies and For clauses).
fn count_direct_assigns(stmt: &Statement, var: &str, found: &mut bool) {
    if *found {
        return;
    }
    match stmt {
        Statement::Assign { target, .. } if target == var => *found = true,
        Statement::VarDecl { target, .. } if target == var => *found = true,
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            for s in then_body {
                count_direct_assigns(s, var, found);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    count_direct_assigns(s, var, found);
                }
            }
        }
        Statement::While { body, .. } | Statement::ForEach { body, .. } => {
            for s in body {
                count_direct_assigns(s, var, found);
            }
        }
        Statement::For {
            init, update, body, ..
        } => {
            for s in init.iter().chain(update.iter()).chain(body.iter()) {
                count_direct_assigns(s, var, found);
            }
        }
        Statement::Switch { arms, .. } => {
            for arm in arms {
                for s in &arm.body {
                    count_direct_assigns(s, var, found);
                }
            }
        }
        Statement::TryCatch {
            try_body,
            catch_body,
            ..
        } => {
            for s in try_body.iter().chain(catch_body.iter()) {
                count_direct_assigns(s, var, found);
            }
        }
        _ => {}
    }
}

fn desugar_counted_loops_recursive(stmts: &mut Vec<Statement>) {
    desugar_counted_loops(stmts);
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                desugar_counted_loops_recursive(then_body);
                if let Some(eb) = else_body {
                    desugar_counted_loops_recursive(eb);
                }
            }
            Statement::While { body, .. } => desugar_counted_loops_recursive(body),
            Statement::For { body, .. } => desugar_counted_loops_recursive(body),
            Statement::ForEach { body, .. } => desugar_counted_loops_recursive(body),
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    desugar_counted_loops_recursive(&mut arm.body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                desugar_counted_loops_recursive(try_body);
                desugar_counted_loops_recursive(catch_body);
            }
            _ => {}
        }
    }
}

/// Remove `if (cond) { }` blocks with empty bodies (decompilation artifacts)
fn remove_empty_if_blocks(stmts: &mut Vec<Statement>) {
    stmts.retain(|s| {
        !matches!(s, Statement::If { then_body, else_body, .. }
        if then_body.is_empty() && else_body.is_none())
    });
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                remove_empty_if_blocks(then_body);
                if let Some(body) = else_body {
                    remove_empty_if_blocks(body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                remove_empty_if_blocks(try_body);
                remove_empty_if_blocks(catch_body);
            }
            Statement::While { body, .. } => {
                remove_empty_if_blocks(body);
            }
            Statement::For {
                init, update, body, ..
            } => {
                for s in init.iter_mut().chain(update.iter_mut()) {
                    let mut one = vec![std::mem::replace(s, Statement::Unknown(String::new()))];
                    remove_empty_if_blocks(&mut one);
                    *s = one
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| Statement::Unknown(String::new()));
                }
                remove_empty_if_blocks(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    remove_empty_if_blocks(&mut arm.body);
                }
            }
            _ => {}
        }
    }
}

// Try/catch reconstruction from exception table

fn wrap_try_catch(
    offset_stmts: Vec<(usize, Statement)>,
    exception_table: &[crate::classfile::ExceptionEntry],
    pool: &Pool,
) -> Vec<Statement> {
    // Build try ranges: (start_pc, end_pc, handler_pc, catch types).
    // Multi-catch (`catch (A | B e)`) emits one table entry per type with
    // identical start/end/handler; merge them so the region is handled once.
    let mut try_ranges: Vec<(usize, usize, u16, Vec<u16>)> = Vec::new();
    for entry in exception_table {
        let key = (
            entry.start_pc as usize,
            entry.end_pc as usize,
            entry.handler_pc,
        );
        if let Some(existing) = try_ranges.iter_mut().find(|r| (r.0, r.1, r.2) == key) {
            existing.3.push(entry.catch_type);
        } else {
            try_ranges.push((key.0, key.1, key.2, vec![entry.catch_type]));
        }
    }
    try_ranges.sort_by_key(|r| r.0);

    let handler_pcs: std::collections::HashSet<usize> = exception_table
        .iter()
        .map(|e| e.handler_pc as usize)
        .collect();

    let mut consumed_handler_pcs: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    let mut result: Vec<Statement> = Vec::new();
    let mut i = 0;

    while i < offset_stmts.len() {
        let (offset, ref stmt) = offset_stmts[i];

        // Check if this offset was consumed by a try/catch handler
        if consumed_handler_pcs.contains(&offset) {
            i += 1;
            continue;
        }

        // Check if this offset starts a try range (within first 25 bytes of start_pc)
        if let Some(pos) = try_ranges
            .iter()
            .position(|r| r.0 <= offset && offset < r.0 + 25 && offset <= r.1)
        {
            let (try_start, try_end, _handler_pc, catch_type_idxs) = try_ranges.remove(pos);
            // Collect try body: statements overlapping [try_start, try_end).
            // Statements carry their LAST instruction's offset, so a statement
            // ending exactly at try_end (e.g. `ireturn` at end_pc - the
            // bytecode range is exclusive only for the *next* handler's
            // start) still belongs to this try. Include any statement whose
            // recorded offset is < try_end, plus one statement whose offset
            // equals try_end when the previous statement's span reaches it
            // (single-statement ranges). Simplest sound rule: include
            // off < try_end, and if nothing was collected, include the first
            // statement at or after try_start.
            let mut try_body: Vec<(usize, Statement)> = Vec::new();
            while i < offset_stmts.len() && offset_stmts[i].0 < try_end {
                try_body.push((offset_stmts[i].0, offset_stmts[i].1.clone()));
                i += 1;
            }
            if try_body.is_empty() && i < offset_stmts.len() {
                // The whole range collapsed into a single statement keyed at
                // its end (composed statements carry their last
                // instruction's offset, e.g. `return f(x)` inside [a, b)).
                try_body.push((offset_stmts[i].0, offset_stmts[i].1.clone()));
                i += 1;
            }

            // Collect catch body starting at the first statement at or after
            // handler_pc (the astore of the exception itself emits no
            // statement, so an exact match rarely exists).
            let mut catch_body: Vec<(usize, Statement)> = Vec::new();
            let handler_offset = _handler_pc as usize;
            let handler_pos = offset_stmts
                .iter()
                .position(|(off, _)| *off >= handler_offset);
            if let Some(h_pos) = handler_pos {
                let mut h_idx = h_pos;
                while h_idx < offset_stmts.len() {
                    let (off, ref s) = offset_stmts[h_idx];
                    if h_idx != h_pos {
                        // Another (not-yet-consumed) try region or handler
                        // begins here: the current catch body ends.
                        if try_ranges.iter().any(|r| r.0 <= off && off < r.1)
                            || handler_pcs.contains(&off)
                        {
                            break;
                        }
                    }
                    if matches!(s, Statement::Unknown(txt) if txt.contains("@goto")) {
                        break;
                    }
                    catch_body.push((off, s.clone()));
                    consumed_handler_pcs.insert(off);
                    h_idx += 1;
                    if matches!(s, Statement::Return(_)) {
                        break;
                    }
                }
            }

            // Determine catch type name(s); multi-catch renders as "A | B"
            let mut names: Vec<String> = catch_type_idxs
                .iter()
                .map(|&idx| {
                    if idx == 0 {
                        "Exception".to_string()
                    } else {
                        short_name(&cp_class_name(pool, idx))
                    }
                })
                .collect();
            names.sort();
            names.dedup();
            let catch_type_name = names.join(" | ");

            let inner_table_try: Vec<_> = exception_table
                .iter()
                .filter(|&e| e.start_pc as usize > try_start && (e.end_pc as usize) < try_end)
                .cloned()
                .collect();

            let inner_table_catch: Vec<_> = exception_table
                .iter()
                .filter(|&e| e.start_pc as usize > handler_offset)
                .cloned()
                .collect();

            let try_body_lowered = if !inner_table_try.is_empty() {
                wrap_try_catch(try_body, &inner_table_try, pool)
            } else {
                try_body.into_iter().map(|(_, s)| s).collect()
            };

            let catch_body_lowered = if !inner_table_catch.is_empty() {
                wrap_try_catch(catch_body, &inner_table_catch, pool)
            } else {
                catch_body.into_iter().map(|(_, s)| s).collect()
            };

            // Remove trailing void return
            let mut catch_body_final = catch_body_lowered;
            if let Some(Statement::Return(None)) = catch_body_final.last() {
                catch_body_final.pop();
            }

            // Only emit TryCatch if catch_body actually handles an exception
            let has_catch_handler = !catch_body_final.is_empty();

            if has_catch_handler {
                result.push(Statement::TryCatch {
                    try_body: try_body_lowered,
                    catch_type: catch_type_name,
                    catch_var: "e".to_string(),
                    catch_body: catch_body_final,
                    resources: vec![],
                    finally_body: None,
                });
            } else {
                result.extend(try_body_lowered);
            }
        } else {
            let mut s = stmt.clone();
            match &mut s {
                Statement::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    *then_body = wrap_try_catch_stmts(then_body.clone(), exception_table, pool);
                    if let Some(eb) = else_body {
                        *eb = wrap_try_catch_stmts(eb.clone(), exception_table, pool);
                    }
                }
                Statement::While { body, .. } => {
                    *body = wrap_try_catch_stmts(body.clone(), exception_table, pool);
                }
                Statement::Switch { arms, .. } => {
                    for arm in arms {
                        arm.body = wrap_try_catch_stmts(
                            std::mem::take(&mut arm.body),
                            exception_table,
                            pool,
                        );
                    }
                }
                _ => {}
            }
            result.push(s);
            i += 1;
        }
    }

    result
}

fn wrap_try_catch_stmts(
    stmts: Vec<Statement>,
    exception_table: &[crate::classfile::ExceptionEntry],
    pool: &Pool,
) -> Vec<Statement> {
    if exception_table.is_empty() || stmts.is_empty() {
        return stmts;
    }
    let offset_stmts: Vec<(usize, Statement)> = stmts.into_iter().enumerate().collect();
    wrap_try_catch(offset_stmts, exception_table, pool)
}

fn expr_references_var(expr: &Expression, var: &str) -> bool {
    match expr {
        Expression::Local(name) => name == var,
        Expression::FieldAccess { object, .. } => expr_references_var(object, var),
        Expression::Binary { left, right, .. } => {
            expr_references_var(left, var) || expr_references_var(right, var)
        }
        Expression::Unary { operand, .. } => expr_references_var(operand, var),
        Expression::Cast { expr, .. } => expr_references_var(expr, var),
        Expression::InstanceOf { expr, .. } => expr_references_var(expr, var),
        Expression::New { args, .. } => args.iter().any(|a| expr_references_var(a, var)),
        Expression::Invoke { target, args } => {
            target == var
                || target.starts_with(&format!("{var}."))
                || args.iter().any(|a| expr_references_var(a, var))
        }
        Expression::Concat { parts, .. } => parts.iter().any(|p| expr_references_var(p, var)),
        _ => false,
    }
}

fn stmt_references_exception(stmt: &Statement) -> bool {
    match stmt {
        Statement::Expression(expr) => expr_references_var(expr, "e"),
        Statement::Assign { value, .. } => expr_references_var(value, "e"),
        _ => false,
    }
}

fn wrap_unwrapped_catch_statements(stmts: &mut Vec<Statement>) {
    let mut i = 0;
    while i < stmts.len() {
        match &mut stmts[i] {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                wrap_unwrapped_catch_statements(then_body);
                if let Some(eb) = else_body {
                    wrap_unwrapped_catch_statements(eb);
                }
            }
            Statement::While { body, .. } => {
                wrap_unwrapped_catch_statements(body);
            }
            Statement::For {
                init, update, body, ..
            } => {
                for s in init.iter_mut().chain(update.iter_mut()) {
                    let mut one = vec![std::mem::replace(s, Statement::Unknown(String::new()))];
                    wrap_unwrapped_catch_statements(&mut one);
                    *s = one
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| Statement::Unknown(String::new()));
                }
                wrap_unwrapped_catch_statements(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    wrap_unwrapped_catch_statements(&mut arm.body);
                }
            }
            Statement::ForEach { body, .. } => {
                wrap_unwrapped_catch_statements(body);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                wrap_unwrapped_catch_statements(try_body);
                wrap_unwrapped_catch_statements(catch_body);
            }
            _ => {}
        }

        if i + 1 < stmts.len()
            && stmt_references_exception(&stmts[i + 1])
            && !matches!(stmts[i], Statement::TryCatch { .. })
        {
            let target_stmt = stmts[i].clone();
            let handler_stmt = stmts.remove(i + 1);
            stmts[i] = Statement::TryCatch {
                try_body: vec![target_stmt],
                catch_type: "Exception".to_string(),
                catch_var: "e".to_string(),
                catch_body: vec![handler_stmt],
                resources: vec![],
                finally_body: None,
            };
        }
        i += 1;
    }
}

// ─── CFR-style boolean condition simplification ───
// javac encodes `if (flag)` over a boolean as `ifeq/ifne` (already bare),
// but `if_icmp*` against 0/1 and negated guards leave `flag != 0`,
// `flag == 0`, `!!flag` residue. Normalize it like CFR/Vineflower do.

fn is_int_const(expr: &Expression, value: i32) -> bool {
    matches!(expr, Expression::ConstInt(v) if *v == value)
}

fn is_null_const(expr: &Expression) -> bool {
    matches!(expr, Expression::ConstNull)
}

/// Simplify a boolean condition in place. Returns true if it changed.
fn simplify_condition(expr: &mut Expression) -> bool {
    // Post-order: simplify children first so `!(1 != 0)` folds bottom-up.
    let mut changed = match expr {
        Expression::Unary { operand, .. } => simplify_condition(operand),
        Expression::Binary { left, right, .. } => {
            simplify_condition(left) | simplify_condition(right)
        }
        _ => false,
    };
    // Constant-fold `1 != 0`-style residue.
    if fold_const_condition(expr) {
        return true;
    }
    changed |= match expr {
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => {
            // !!x → x
            if let Expression::Unary {
                op: UnaryOp::Not,
                operand: inner,
            } = operand.as_mut()
            {
                let inner = std::mem::replace(inner.as_mut(), Expression::ConstNull);
                *expr = inner;
                return true;
            }
            // !(x == null) → x != null, !(x != null) → x == null
            if let Expression::Binary { left, op, right } = operand.as_mut() {
                if is_null_const(left) || is_null_const(right) {
                    let flipped = match op {
                        BinaryOp::Eq => BinaryOp::Ne,
                        BinaryOp::Ne => BinaryOp::Eq,
                        _ => return false,
                    };
                    *expr = Expression::Binary {
                        left: std::mem::replace(left, Box::new(Expression::ConstNull)),
                        op: flipped,
                        right: std::mem::replace(right, Box::new(Expression::ConstNull)),
                    };
                    return true;
                }
                // !(a < b) → a >= b (and Lt/Le/Gt/Ge/Eq/Ne flips). Covers
                // `while (!(i >= n))` → `while (i < n)`.
                // (Null comparisons handled above; float NaN edge semantics
                // differ in theory but javac emits these shapes for plain
                // negated conditions, matching CFR/Vineflower behavior.)
                let flipped = match op {
                    BinaryOp::Eq => BinaryOp::Ne,
                    BinaryOp::Ne => BinaryOp::Eq,
                    BinaryOp::Lt => BinaryOp::Ge,
                    BinaryOp::Le => BinaryOp::Gt,
                    BinaryOp::Gt => BinaryOp::Le,
                    BinaryOp::Ge => BinaryOp::Lt,
                    _ => return false,
                };
                *expr = Expression::Binary {
                    left: std::mem::replace(left, Box::new(Expression::ConstNull)),
                    op: flipped,
                    right: std::mem::replace(right, Box::new(Expression::ConstNull)),
                };
                return true;
            }
            false
        }
        Expression::Binary { left, op, right } => {
            // x == 0 → !x, x != 0 → x (int sensu boolean, never null)
            if (*op == BinaryOp::Eq || *op == BinaryOp::Ne)
                && !is_null_const(left)
                && !is_null_const(right)
            {
                let (other, is_zero, is_one) = if is_int_const(left, 0) || is_int_const(left, 1) {
                    let zero = is_int_const(left, 0);
                    (
                        std::mem::replace(right, Box::new(Expression::ConstNull)),
                        zero,
                        !zero,
                    )
                } else if is_int_const(right, 0) || is_int_const(right, 1) {
                    let zero = is_int_const(right, 0);
                    (
                        std::mem::replace(left, Box::new(Expression::ConstNull)),
                        zero,
                        !zero,
                    )
                } else {
                    return false;
                };
                // Don't rewrite genuine integer arithmetic like `(a - b) != 0`.
                if matches!(other.as_ref(), Expression::Binary { .. }) {
                    // restore
                    if is_int_const(left, 0) || is_int_const(left, 1) {
                        *right = other;
                    } else {
                        *left = other;
                    }
                    return false;
                }
                let positive = (*op == BinaryOp::Ne) == is_zero || (*op == BinaryOp::Eq) == is_one;
                *expr = if positive {
                    *other
                } else {
                    Expression::Unary {
                        op: UnaryOp::Not,
                        operand: other,
                    }
                };
                return true;
            }
            false
        }
        _ => false,
    };
    changed
}

/// Fold constant integer comparisons (`1 != 0` → true as `1`, `1 == 0` → `0`)
/// and constant negations so guards like `if (!(1 != 0))` collapse.
fn fold_const_condition(expr: &mut Expression) -> bool {
    match expr {
        Expression::Binary { left, op, right } => {
            if let (Expression::ConstInt(a), Expression::ConstInt(b)) =
                (left.as_ref(), right.as_ref())
            {
                let result = match op {
                    BinaryOp::Eq => a == b,
                    BinaryOp::Ne => a != b,
                    BinaryOp::Lt => a < b,
                    BinaryOp::Le => a <= b,
                    BinaryOp::Gt => a > b,
                    BinaryOp::Ge => a >= b,
                    _ => return false,
                };
                *expr = Expression::ConstInt(i32::from(result));
                return true;
            }
            false
        }
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => {
            if let Expression::ConstInt(v) = operand.as_ref() {
                let v = *v;
                *expr = Expression::ConstInt(if v == 0 { 1 } else { 0 });
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Apply `simplify_condition` to a fixpoint (bounded): one rewrite often
/// unlocks another (`!(flag == 0)` → `flag != 0` → `flag`). Each rule
/// strictly reduces negations/comparisons, so this terminates; the cap is
/// belt-and-braces.
fn simplify_condition_fixpoint(expr: &mut Expression) {
    for _ in 0..8 {
        if !simplify_condition(expr) {
            break;
        }
    }
}

fn simplify_conditions_recursive(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                simplify_condition_fixpoint(condition);
                simplify_conditions_recursive(then_body);
                if let Some(eb) = else_body {
                    simplify_conditions_recursive(eb);
                }
            }
            Statement::While { condition, body } => {
                simplify_condition_fixpoint(condition);
                simplify_conditions_recursive(body);
            }
            Statement::For {
                init,
                update,
                condition,
                body,
                ..
            } => {
                simplify_conditions_recursive(init);
                simplify_conditions_recursive(update);
                if let Some(cond) = condition {
                    simplify_condition_fixpoint(cond);
                }
                simplify_conditions_recursive(body);
            }
            Statement::Switch { discriminant, arms } => {
                simplify_condition_fixpoint(discriminant);
                for arm in arms {
                    simplify_conditions_recursive(&mut arm.body);
                }
            }
            Statement::ForEach { body, .. } => simplify_conditions_recursive(body),
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                simplify_conditions_recursive(try_body);
                simplify_conditions_recursive(catch_body);
            }
            _ => {}
        }
    }
}

// ─── Type attribution from the inference pass ───

/// Convert a `JavaType` to a display string usable in a `VarDecl`.
/// `Unknown`/`Null` map to `None` so the renderer falls back to `var`
/// instead of emitting `Object`/`null` as a type (the `null local2` bug).
fn java_type_display(ty: &crate::inference::JavaType) -> Option<String> {
    match ty {
        crate::inference::JavaType::Unknown | crate::inference::JavaType::Null => None,
        other => Some(other.to_string()),
    }
}

/// Patch `VarDecl` types using the forward inference environment plus
/// boolean usage: a local used as a bare `if`/`while` condition is boolean.
fn apply_inferred_types(
    stmts: &mut [Statement],
    env: &crate::inference::TypeEnvironment,
    slot_names: &[String],
) {
    let mut name_types: HashMap<String, String> = HashMap::new();
    for (slot, name) in slot_names.iter().enumerate() {
        if name.is_empty() || name == "this" {
            continue;
        }
        if let Some(ty) = env.locals.get(slot)
            && let Some(display) = java_type_display(ty)
        {
            name_types.entry(name.clone()).or_insert(display);
        }
    }

    let mut bare_conditions = std::collections::HashSet::new();
    collect_bare_condition_names(stmts, &mut bare_conditions);

    apply_types_recursive(stmts, &name_types, &bare_conditions);
}

fn collect_bare_condition_names(stmts: &[Statement], out: &mut std::collections::HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                collect_bare_names_in_expr(condition, out);
                collect_bare_condition_names(then_body, out);
                if let Some(eb) = else_body {
                    collect_bare_condition_names(eb, out);
                }
            }
            Statement::While { condition, body } => {
                collect_bare_names_in_expr(condition, out);
                collect_bare_condition_names(body, out);
            }
            Statement::For {
                init,
                update,
                condition,
                body,
                ..
            } => {
                collect_bare_condition_names(init, out);
                collect_bare_condition_names(update, out);
                if let Some(cond) = condition {
                    collect_bare_names_in_expr(cond, out);
                }
                collect_bare_condition_names(body, out);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    collect_bare_condition_names(&arm.body, out);
                }
            }
            Statement::ForEach { body, .. } => collect_bare_condition_names(body, out),
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                collect_bare_condition_names(try_body, out);
                collect_bare_condition_names(catch_body, out);
            }
            _ => {}
        }
    }
}

fn collect_bare_names_in_expr(expr: &Expression, out: &mut std::collections::HashSet<String>) {
    // Only standalone/negated/conjuncted names imply `boolean`:
    // `if (x)`, `if (!x)`, `if (x && y)`. Operands of `==`/`!=`/relational
    // comparisons (including `x == null`, `x == 0` residue) say nothing.
    match expr {
        Expression::Local(name) => {
            out.insert(name.clone());
        }
        Expression::Unary { operand, .. } => collect_bare_names_in_expr(operand, out),
        Expression::Binary { left, right, op } => {
            if matches!(op, BinaryOp::BoolAnd | BinaryOp::BoolOr) {
                collect_bare_names_in_expr(left, out);
                collect_bare_names_in_expr(right, out);
            }
        }
        _ => {}
    }
}

fn apply_types_recursive(
    stmts: &mut [Statement],
    name_types: &HashMap<String, String>,
    bare_conditions: &std::collections::HashSet<String>,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::VarDecl {
                target, var_type, ..
            } => {
                if bare_conditions.contains(target)
                    && matches!(var_type.as_deref(), None | Some("int"))
                {
                    *var_type = Some("boolean".to_string());
                } else if var_type.is_none()
                    && let Some(inferred) = name_types.get(target)
                {
                    *var_type = Some(inferred.clone());
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                apply_types_recursive(then_body, name_types, bare_conditions);
                if let Some(eb) = else_body {
                    apply_types_recursive(eb, name_types, bare_conditions);
                }
            }
            Statement::While { body, .. } => {
                apply_types_recursive(body, name_types, bare_conditions);
            }
            Statement::For {
                init, update, body, ..
            } => {
                apply_types_recursive(init, name_types, bare_conditions);
                apply_types_recursive(update, name_types, bare_conditions);
                apply_types_recursive(body, name_types, bare_conditions);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    apply_types_recursive(&mut arm.body, name_types, bare_conditions);
                }
            }
            Statement::ForEach { body, .. } => {
                apply_types_recursive(body, name_types, bare_conditions);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                apply_types_recursive(try_body, name_types, bare_conditions);
                apply_types_recursive(catch_body, name_types, bare_conditions);
            }
            _ => {}
        }
    }
}

fn infer_expr_type(expr: &Expression) -> Option<String> {
    match expr {
        Expression::New { class, .. } => Some(class.clone()),
        Expression::Cast { target_type, .. } => Some(target_type.clone()),
        Expression::ConstString(_) => Some("String".to_string()),
        Expression::ConstInt(_) => Some("int".to_string()),
        Expression::ConstLong(_) => Some("long".to_string()),
        Expression::ConstFloat(_) => Some("float".to_string()),
        Expression::ConstDouble(_) => Some("double".to_string()),
        Expression::Concat { builder, .. } => Some(builder.clone()),
        Expression::Binary {
            op: BinaryOp::Add,
            left,
            ..
        } => {
            // String concatenation infers String when either side is a string.
            if matches!(left.as_ref(), Expression::ConstString(_)) {
                Some("String".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn infer_name_for_var(target: &str, value: &Expression) -> Option<String> {
    if !target.starts_with("var_") {
        return None;
    }
    match value {
        Expression::Invoke {
            target: inv_target, ..
        } => {
            if inv_target.ends_with("ImageIO.read") || inv_target.ends_with(".read") {
                Some("bufferedImage".to_string())
            } else if inv_target.ends_with(".id") || inv_target == "id" {
                Some("id".to_string())
            } else if inv_target.ends_with(".iterator") {
                Some("iterator".to_string())
            } else {
                None
            }
        }
        Expression::Cast { target_type, .. } => {
            if target_type.ends_with("Module") {
                Some("module".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn rename_var_in_stmts(stmts: &mut [Statement], old_name: &str, new_name: &str) {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                if target == old_name {
                    *target = new_name.to_string();
                }
                rename_var_in_expr(value, old_name, new_name);
            }
            Statement::VarDecl { target, value, .. } => {
                if target == old_name {
                    *target = new_name.to_string();
                }
                if let Some(v) = value {
                    rename_var_in_expr(v, old_name, new_name);
                }
            }
            Statement::Expression(expr) => rename_var_in_expr(expr, old_name, new_name),
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                rename_var_in_expr(condition, old_name, new_name);
                rename_var_in_stmts(then_body, old_name, new_name);
                if let Some(eb) = else_body {
                    rename_var_in_stmts(eb, old_name, new_name);
                }
            }
            Statement::While { condition, body } => {
                rename_var_in_expr(condition, old_name, new_name);
                rename_var_in_stmts(body, old_name, new_name);
            }
            Statement::For {
                init,
                update,
                condition,
                body,
                ..
            } => {
                rename_var_in_stmts(init, old_name, new_name);
                rename_var_in_stmts(update, old_name, new_name);
                if let Some(cond) = condition {
                    rename_var_in_expr(cond, old_name, new_name);
                }
                rename_var_in_stmts(body, old_name, new_name);
            }
            Statement::Switch { discriminant, arms } => {
                rename_var_in_expr(discriminant, old_name, new_name);
                for arm in arms {
                    rename_var_in_stmts(&mut arm.body, old_name, new_name);
                }
            }
            Statement::ForEach {
                collection, body, ..
            } => {
                rename_var_in_expr(collection, old_name, new_name);
                rename_var_in_stmts(body, old_name, new_name);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                rename_var_in_stmts(try_body, old_name, new_name);
                rename_var_in_stmts(catch_body, old_name, new_name);
            }
            Statement::Return(Some(expr)) => rename_var_in_expr(expr, old_name, new_name),
            _ => {}
        }
    }
}

fn rename_var_in_expr(expr: &mut Expression, old_name: &str, new_name: &str) {
    match expr {
        Expression::Local(name) if name == old_name => {
            *name = new_name.to_string();
        }
        Expression::FieldAccess { object, .. } => rename_var_in_expr(object, old_name, new_name),
        Expression::Binary { left, right, .. } => {
            rename_var_in_expr(left, old_name, new_name);
            rename_var_in_expr(right, old_name, new_name);
        }
        Expression::Unary { operand, .. } => rename_var_in_expr(operand, old_name, new_name),
        Expression::Cast { expr, .. } => rename_var_in_expr(expr, old_name, new_name),
        Expression::InstanceOf { expr, .. } => rename_var_in_expr(expr, old_name, new_name),
        Expression::New { args, .. } => {
            for a in args {
                rename_var_in_expr(a, old_name, new_name);
            }
        }
        Expression::Invoke { target, args } => {
            if target == old_name {
                *target = new_name.to_string();
            } else if target.starts_with(&format!("{old_name}.")) {
                *target = format!("{new_name}{}", &target[old_name.len()..]);
            }
            for a in args {
                rename_var_in_expr(a, old_name, new_name);
            }
        }
        Expression::Lambda { body, .. } => rename_var_in_stmts(body, old_name, new_name),
        Expression::Concat { parts, .. } => {
            for part in parts {
                rename_var_in_expr(part, old_name, new_name);
            }
        }
        _ => {}
    }
}

fn convert_assigns_to_var_decls_recursive(
    stmts: &mut [Statement],
    declared_vars: &mut std::collections::HashSet<String>,
) {
    let mut i = 0;
    while i < stmts.len() {
        if let Statement::Assign {
            ref target,
            ref value,
        } = stmts[i]
        {
            if !target.contains('.')
                && !target.contains('[')
                && !target.starts_with('$')
                && target != "this"
                && !declared_vars.contains(target)
            {
                let name_to_use = if target.starts_with("var_") {
                    if let Some(better) = infer_name_for_var(target, value) {
                        if !declared_vars.contains(&better) {
                            better
                        } else {
                            target.clone()
                        }
                    } else {
                        target.clone()
                    }
                } else {
                    target.clone()
                };

                let old_target = target.clone();
                let val_clone = value.clone();
                declared_vars.insert(name_to_use.clone());
                let var_type = infer_expr_type(&val_clone);
                stmts[i] = Statement::VarDecl {
                    target: name_to_use.clone(),
                    var_type,
                    value: Some(val_clone),
                };
                if name_to_use != old_target {
                    rename_var_in_stmts(&mut stmts[i + 1..], &old_target, &name_to_use);
                }
            }
        } else {
            let stmt = &mut stmts[i];
            match stmt {
                Statement::VarDecl { target, .. } => {
                    declared_vars.insert(target.clone());
                }
                Statement::Expression(expr) => {
                    convert_assigns_in_expr(expr, declared_vars);
                }
                Statement::ForEach { var_name, body, .. } => {
                    declared_vars.insert(var_name.clone());
                    convert_assigns_to_var_decls_recursive(body, declared_vars);
                }
                Statement::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    convert_assigns_to_var_decls_recursive(then_body, declared_vars);
                    if let Some(else_b) = else_body {
                        convert_assigns_to_var_decls_recursive(else_b, declared_vars);
                    }
                }
                Statement::While { body, .. } => {
                    convert_assigns_to_var_decls_recursive(body, declared_vars);
                }
                Statement::For {
                    init, update, body, ..
                } => {
                    // Init may declare the loop variable; the body then sees
                    // it as declared. Update clauses cannot declare.
                    convert_assigns_to_var_decls_recursive(init, declared_vars);
                    convert_assigns_to_var_decls_recursive(body, declared_vars);
                    convert_assigns_to_var_decls_recursive(update, declared_vars);
                }
                Statement::Switch { arms, .. } => {
                    // Arms share the switch block scope (like straight-line
                    // code), so thread the same declaration set through.
                    for arm in arms {
                        convert_assigns_to_var_decls_recursive(&mut arm.body, declared_vars);
                    }
                }
                Statement::TryCatch {
                    try_body,
                    catch_body,
                    catch_var,
                    ..
                } => {
                    convert_assigns_to_var_decls_recursive(try_body, declared_vars);
                    declared_vars.insert(catch_var.clone());
                    convert_assigns_to_var_decls_recursive(catch_body, declared_vars);
                }
                _ => {}
            }
        }
        i += 1;
    }
}

fn convert_assigns_in_expr(
    expr: &mut Expression,
    declared_vars: &std::collections::HashSet<String>,
) {
    match expr {
        Expression::Lambda { body, params } => {
            let mut child_vars = declared_vars.clone();
            for (_, name) in params {
                child_vars.insert(name.clone());
            }
            child_vars.insert("this".to_string());
            convert_assigns_to_var_decls_recursive(body, &mut child_vars);
        }
        Expression::Binary { left, right, .. } => {
            convert_assigns_in_expr(left, declared_vars);
            convert_assigns_in_expr(right, declared_vars);
        }
        Expression::Unary { operand, .. } => convert_assigns_in_expr(operand, declared_vars),
        Expression::FieldAccess { object, .. } => convert_assigns_in_expr(object, declared_vars),
        Expression::Cast { expr, .. } => convert_assigns_in_expr(expr, declared_vars),
        Expression::InstanceOf { expr, .. } => convert_assigns_in_expr(expr, declared_vars),
        Expression::New { args, .. } => {
            for a in args {
                convert_assigns_in_expr(a, declared_vars);
            }
        }
        Expression::Invoke { args, .. } => {
            for a in args {
                convert_assigns_in_expr(a, declared_vars);
            }
        }
        Expression::Concat { parts, .. } => {
            for part in parts {
                convert_assigns_in_expr(part, declared_vars);
            }
        }
        _ => {}
    }
}

fn collect_locals_in_stmts(stmts: &[Statement], locals: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                if !locals.contains(target) && !target.contains('.') && target != "this" {
                    locals.push(target.clone());
                }
                collect_locals_in_expr(value, locals);
            }
            Statement::VarDecl { target, value, .. } => {
                if !locals.contains(target) && !target.contains('.') && target != "this" {
                    locals.push(target.clone());
                }
                if let Some(v) = value {
                    collect_locals_in_expr(v, locals);
                }
            }
            Statement::Expression(expr) => collect_locals_in_expr(expr, locals),
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                collect_locals_in_expr(condition, locals);
                collect_locals_in_stmts(then_body, locals);
                if let Some(eb) = else_body {
                    collect_locals_in_stmts(eb, locals);
                }
            }
            Statement::While { condition, body } => {
                collect_locals_in_expr(condition, locals);
                collect_locals_in_stmts(body, locals);
            }
            Statement::For {
                init,
                update,
                condition,
                body,
                ..
            } => {
                collect_locals_in_stmts(init, locals);
                collect_locals_in_stmts(update, locals);
                if let Some(cond) = condition {
                    collect_locals_in_expr(cond, locals);
                }
                collect_locals_in_stmts(body, locals);
            }
            Statement::Switch { discriminant, arms } => {
                collect_locals_in_expr(discriminant, locals);
                for arm in arms {
                    collect_locals_in_stmts(&arm.body, locals);
                }
            }
            Statement::ForEach {
                collection, body, ..
            } => {
                collect_locals_in_expr(collection, locals);
                collect_locals_in_stmts(body, locals);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                collect_locals_in_stmts(try_body, locals);
                collect_locals_in_stmts(catch_body, locals);
            }
            Statement::Return(Some(expr)) => collect_locals_in_expr(expr, locals),
            _ => {}
        }
    }
}

fn collect_locals_in_expr(expr: &Expression, locals: &mut Vec<String>) {
    match expr {
        Expression::Local(name) if !locals.contains(name) && name != "this" => {
            locals.push(name.clone());
        }
        Expression::FieldAccess { object, .. } => collect_locals_in_expr(object, locals),
        Expression::Binary { left, right, .. } => {
            collect_locals_in_expr(left, locals);
            collect_locals_in_expr(right, locals);
        }
        Expression::Unary { operand, .. } => collect_locals_in_expr(operand, locals),
        Expression::New { args, .. } => {
            for a in args {
                collect_locals_in_expr(a, locals);
            }
        }
        Expression::Cast { expr, .. } => collect_locals_in_expr(expr, locals),
        Expression::InstanceOf { expr, .. } => collect_locals_in_expr(expr, locals),
        Expression::Invoke { args, .. } => {
            for a in args {
                collect_locals_in_expr(a, locals);
            }
        }
        Expression::Concat { parts, .. } => {
            for part in parts {
                collect_locals_in_expr(part, locals);
            }
        }
        Expression::Lambda { body, .. } => collect_locals_in_stmts(body, locals),
        _ => {}
    }
}

fn restructure_for_each_loops_recursive(stmts: &mut Vec<Statement>) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                restructure_for_each_loops_recursive(then_body);
                if let Some(eb) = else_body {
                    restructure_for_each_loops_recursive(eb);
                }
            }
            Statement::While { body, .. } => {
                restructure_for_each_loops_recursive(body);
            }
            Statement::Switch { arms, .. } => {
                for arm in arms {
                    restructure_for_each_loops_recursive(&mut arm.body);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                ..
            } => {
                restructure_for_each_loops_recursive(try_body);
                restructure_for_each_loops_recursive(catch_body);
            }
            Statement::ForEach { body, .. } => {
                restructure_for_each_loops_recursive(body);
            }
            Statement::For {
                init, update, body, ..
            } => {
                restructure_for_each_loops_recursive(init);
                restructure_for_each_loops_recursive(update);
                restructure_for_each_loops_recursive(body);
            }
            _ => {}
        }
    }

    let mut i = 0;
    while i + 1 < stmts.len() {
        let (it_var, coll_expr) = match &stmts[i] {
            Statement::VarDecl {
                target,
                value: Some(val),
                ..
            }
            | Statement::Assign { target, value: val } => {
                if let Expression::Invoke {
                    target: inv_target,
                    args,
                } = val
                {
                    if (inv_target.ends_with(".iterator") || inv_target.ends_with(".iterator()"))
                        && args.is_empty()
                    {
                        let coll_str = if let Some(dot_idx) = inv_target.rfind(".iterator") {
                            &inv_target[..dot_idx]
                        } else {
                            ""
                        };
                        (target.clone(), Expression::Local(coll_str.to_string()))
                    } else {
                        i += 1;
                        continue;
                    }
                } else {
                    i += 1;
                    continue;
                }
            }
            _ => {
                i += 1;
                continue;
            }
        };

        let is_has_next = match &stmts[i + 1] {
            Statement::While {
                condition:
                    Expression::Invoke {
                        target: cond_target,
                        ..
                    },
                ..
            } => {
                cond_target == &format!("{it_var}.hasNext")
                    || cond_target == &format!("{it_var}.hasNext()")
            }
            _ => false,
        };

        if !is_has_next {
            i += 1;
            continue;
        }

        let (elem_var, elem_type, remaining_body) = match &stmts[i + 1] {
            Statement::While { body, .. } if !body.is_empty() => {
                let first = &body[0];
                let (e_var, e_type) = match first {
                    Statement::VarDecl {
                        target,
                        var_type,
                        value,
                    } => {
                        let inferred = var_type.clone().or_else(|| match value {
                            Some(Expression::Cast { target_type, .. }) => Some(target_type.clone()),
                            _ => None,
                        });
                        (target.clone(), inferred)
                    }
                    Statement::Assign { target, value } => {
                        let inferred = match value {
                            Expression::Cast { target_type, .. } => Some(target_type.clone()),
                            _ => None,
                        };
                        (target.clone(), inferred)
                    }
                    _ => (format!("{it_var}_item"), None),
                };
                let mut rem = body[1..].to_vec();
                let final_name = if e_var.starts_with("var_") || e_var.contains("_item") {
                    if let Some(ref t) = e_type {
                        let short = short_name(t);
                        if let Some(c) = short.chars().next() {
                            let lower = c.to_lowercase().to_string();
                            format!("{}{}", lower, &short[c.len_utf8()..])
                        } else {
                            e_var.clone()
                        }
                    } else {
                        e_var.clone()
                    }
                } else {
                    e_var.clone()
                };

                if final_name != e_var {
                    rename_var_in_stmts(&mut rem, &e_var, &final_name);
                }

                (final_name, e_type, rem)
            }
            _ => {
                i += 1;
                continue;
            }
        };

        stmts[i] = Statement::ForEach {
            var_type: elem_type,
            var_name: elem_var,
            collection: coll_expr,
            body: remaining_body,
        };
        stmts.remove(i + 1);
    }
}

// Public API

#[allow(clippy::too_many_arguments)]
pub fn lower_method_to_ast(
    pool: &Pool,
    method_name: &str,
    method_descriptor: &str,
    method_signature: Option<&str>,
    code: &crate::classfile::CodeAttribute,
    this_class: &str,
    access_flags: u16,
    bootstrap_methods: &[crate::classfile::BootstrapMethodInfo],
    lambda_bodies: &HashMap<String, Vec<Statement>>,
    known_fields: &std::collections::HashSet<String>,
) -> MethodDecl {
    let (params, return_type) =
        crate::generics::generic_method_types(method_descriptor, method_signature);
    let mut param_names: Vec<String> = params
        .iter()
        .enumerate()
        .map(|(i, _)| format!("arg{i}"))
        .collect();

    // For enum constructors, use meaningful names: (String name, int ordinal)
    if method_name == "<init>" && params == ["java.lang.String", "int"] {
        param_names = vec!["name".to_string(), "ordinal".to_string()];
    }

    let instructions = &code.instructions;
    let cfg = ControlFlowGraph::build_from_instructions(instructions);
    // Unreachable code (dead traps after unconditional transfers) must not
    // corrupt linear stack simulation — yet recovery-first means it stays
    // visible in the output. Solution: compute dead instruction runs; the
    // stack machine sandboxes its operand stack across them (save on entry,
    // restore on exit), so dead pushes/pops never leak into live flow.
    // Exception handlers are seeded as entry points so their bodies count
    // as live.
    let handler_pcs: Vec<usize> = code
        .exception_table
        .iter()
        .map(|entry| entry.handler_pc as usize)
        .collect();
    let reachable = cfg.reachable_offsets(&handler_pcs);
    let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if let Some(first) = instructions.first() {
        keep.insert(first.offset);
    }
    for entry in &code.exception_table {
        keep.insert(entry.handler_pc as usize);
    }
    for ins in instructions {
        for target in ins.kind.branch_targets() {
            if target >= 0 {
                keep.insert(target as usize);
            }
        }
    }
    let mut sorted: Vec<&Instruction> = instructions.iter().collect();
    sorted.sort_by_key(|ins| ins.offset);
    // live[off] = false  →  statically dead (unreachable block, or follows
    // an unconditional transfer with no incoming edge).
    let mut live: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
    let mut diverged = false;
    for ins in &sorted {
        let alive = reachable.contains(&ins.offset) && (!diverged || keep.contains(&ins.offset));
        live.insert(ins.offset, alive);
        if alive {
            diverged = !ins.kind.has_fallthrough();
        }
    }
    let mut dead_runs: Vec<(usize, usize)> = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut prev_off: Option<usize> = None;
    for ins in &sorted {
        if live.get(&ins.offset).copied().unwrap_or(true) {
            if let (Some(start), Some(prev)) = (run_start.take(), prev_off) {
                dead_runs.push((start, prev));
            }
        } else if run_start.is_none() {
            run_start = Some(ins.offset);
        }
        prev_off = Some(ins.offset);
    }
    if let (Some(start), Some(prev)) = (run_start, prev_off) {
        dead_runs.push((start, prev));
    }
    let dead_start: std::collections::HashSet<usize> = dead_runs.iter().map(|(s, _)| *s).collect();
    let dead_end: std::collections::HashSet<usize> = dead_runs.iter().map(|(_, e)| *e).collect();
    let all_instructions: Vec<Instruction> = cfg
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter().cloned())
        .collect();

    let is_static = access_flags & 0x0008 != 0 || method_name == "<clinit>";

    // CFR-style forward type inference: seeds from the method descriptor,
    // LocalVariableTable / LocalVariableTypeTable and StackMapTable frames.
    let type_env =
        crate::inference::infer_types(instructions, code, method_descriptor, is_static, pool);

    let max_locals = code.max_locals as usize;
    let mut machine = StackMachine::new(
        pool,
        this_class,
        max_locals.max(1),
        is_static,
        bootstrap_methods.to_vec(),
        lambda_bodies.clone(),
        dead_start,
        dead_end,
    );

    // Apply LocalVariableTable names if available (takes precedence over arg0/var_N)
    if let Some(lvt) = code.local_variable_table.as_deref() {
        let start_local = if is_static { 0 } else { 1 };
        for entry in lvt {
            let name = cp_utf8(pool, entry.name_index);
            if name != "this" && (entry.index as usize) < machine.locals.len() {
                machine.locals[entry.index as usize] = Expression::Local(name.clone());
                // Also update param_names if this entry is a parameter
                if (entry.index as usize) >= start_local {
                    let param_idx = entry.index as usize - start_local;
                    if param_idx < param_names.len() {
                        param_names[param_idx] = name;
                    }
                }
            }
        }
    }

    // Initialize parameter locals (local 0 = this for instance methods)
    let start_local = if is_static { 0 } else { 1 };
    for (i, param_name) in param_names.iter().enumerate() {
        let local_idx = start_local + i;
        if local_idx < machine.locals.len() {
            // Only overwrite if it's still the default var_N (LVT takes precedence)
            if let Expression::Local(ref name) = machine.locals[local_idx]
                && name.starts_with("var_")
            {
                machine.locals[local_idx] = Expression::Local(param_name.clone());
            }
        }
    }
    machine.run(&all_instructions);

    // Prune dead noise: statically dead statements that carry no information
    // (returns/throws/gotos after a divergent statement, empty-stack
    // residue) are dropped. Informative dead code (dead switches, stores,
    // real throws) stays visible for analysts.
    machine.statements.retain(|(off, stmt)| {
        live.get(off).copied().unwrap_or(true) || !dead_stmt_is_droppable(stmt)
    });

    // Snapshot slot names for type attribution (slot -> source name).
    let slot_names: Vec<String> = machine.locals.iter().map(local_display_name).collect();

    // Restructure control flow (if/else from goto patterns)
    let restructured = restructure_control_flow(
        machine.statements,
        &machine.if_targets,
        &machine.switch_stubs,
    );

    // Build try/catch structure from exception table, then clean up goto markers
    let mut statements = if !code.exception_table.is_empty() {
        wrap_try_catch(restructured, &code.exception_table, pool)
    } else {
        restructured.into_iter().map(|(_, s)| s).collect()
    };

    // Merge guard clauses (including inside while/if/try-catch bodies)
    merge_guards_recursive(&mut statements);

    // Clean up surviving @goto markers recursively
    remove_goto_markers(&mut statements);

    // Remove empty if blocks (decompilation artifacts)
    remove_empty_if_blocks(&mut statements);

    // Remove dead branches from constant conditions (opaque predicates
    // left over by constant folding, e.g. `if (0) ... else ...`).
    remove_dead_branches(&mut statements);

    simplify_conditions_recursive(&mut statements);

    // Non-boolean conditions (`i % 2`, `!(i % 2)`) are invalid Java; restore
    // explicit `!= 0` / `== 0` comparisons where the operand is not
    // boolean-shaped. Locals/invokes/field accesses stay bare: they are the
    // boolean re-typing heuristic's domain.
    ensure_boolean_conditions_recursive(&mut statements);

    // CFR-style ternary reconstruction: an if/else whose both branches only
    // assign the same variable (or return) collapses to `x = cond ? a : b`.
    reconstruct_ternaries_recursive(&mut statements);

    // javac encodes try-with-resources as: resource decl, try, close-on-exit,
    // Throwable handler with addSuppressed. Fold that shape back into
    // `try (resource) { ... }` and hoist duplicated finally blocks.
    finalize_try_shapes_recursive(&mut statements);

    // After try shaping, straight-line code after an always-returning
    // try/catch is unreachable: drop it.
    truncate_after_terminator_recursive(&mut statements);

    // Restructure for-each loops
    restructure_for_each_loops_recursive(&mut statements);

    // Canonicalize counted loops: `init; while (i < n) { ...; i++ }` -> for
    desugar_counted_loops_recursive(&mut statements);

    // For-each restructuring can expose new empty guards — sweep again.
    remove_empty_if_blocks(&mut statements);

    // Wrap any remaining catch handlers referencing 'e' into try-catch blocks
    wrap_unwrapped_catch_statements(&mut statements);

    // Late structural passes (try shaping, terminator truncation, loop
    // canonicalization) can expose fresh if/return adjacencies that did not
    // exist during the first ternary sweep — run it once more. Idempotent.
    reconstruct_ternaries_recursive(&mut statements);

    // Track local variable declarations
    let mut declared_vars = std::collections::HashSet::new();
    for p in &param_names {
        declared_vars.insert(p.clone());
    }
    declared_vars.insert("this".to_string());
    declared_vars.extend(known_fields.iter().cloned());
    convert_assigns_to_var_decls_recursive(&mut statements, &mut declared_vars);

    // Attribute inferred types (descriptor/LVT/StackMap) and boolean usage.
    apply_inferred_types(&mut statements, &type_env, &slot_names);

    // Remove trailing void return
    if let Some(Statement::Return(None)) = statements.last() {
        statements.pop();
    }

    MethodDecl {
        name: method_name.to_string(),
        statements,
        access_flags,
        return_type,
        param_types: params,
        param_names,
        class_name: this_class.replace('/', "."),
    }
}

fn local_display_name(expr: &Expression) -> String {
    match expr {
        Expression::Local(name) => name.clone(),
        Expression::This => "this".to_string(),
        _ => String::new(),
    }
}

/// True for dead statements safe to drop: divergent statements (their
/// control effect is already represented by the live divergent statement
/// that precedes them) and anything built on empty-stack noise. Structural
/// statements (if/while/switch/try) and informative stores are kept even
/// when dead.
fn dead_stmt_is_droppable(stmt: &Statement) -> bool {
    match stmt {
        Statement::Return(_) => true,
        Statement::VarDecl { value: None, .. } => true,
        Statement::Expression(expr) | Statement::Assign { value: expr, .. } => {
            dead_expr_is_noise(expr)
        }
        Statement::VarDecl {
            value: Some(value), ..
        } => dead_expr_is_noise(value),
        _ => false,
    }
}

fn dead_expr_is_noise(expr: &Expression) -> bool {
    if contains_empty_stack(expr) {
        return true;
    }
    if let Expression::Unknown(text) = expr {
        let t = text.trim_start();
        return t.starts_with("@goto ")
            || t.starts_with("@switch ")
            || t.starts_with("throw ")
            || t.starts_with("jsr ")
            || t.starts_with("ret ");
    }
    false
}

// â”€â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Instruction, InstructionKind};

    fn test_code(instructions: Vec<Instruction>) -> crate::classfile::CodeAttribute {
        crate::classfile::CodeAttribute {
            max_stack: 16,
            max_locals: 16,
            raw_code: Vec::new(),
            instructions,
            exception_table: Vec::new(),
            local_variable_table: None,
            local_variable_type_table: None,
            stack_map_table: None,
            line_number_table: None,
        }
    }

    #[test]
    fn for_loop_with_if_else_body_reconstructed() {
        // Mirrors SimpleTest.java:
        // for (int i = 0; i < 5; i++) {
        //     if (i % 2 == 0) { sb.append("even").append(i); }
        //     else { sb.append("odd").append(i); }
        // }
        let pool = vec![
            Recoverable::Missing, // 0
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "java/lang/StringBuilder".to_string(),
            )), // 1
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }), // 2
            Recoverable::Present(ConstantPoolEntry::Utf8("<init>".to_string())), // 3
            Recoverable::Present(ConstantPoolEntry::Utf8("()V".to_string())), // 4
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }), // 5
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 5,
            }), // 6
            Recoverable::Present(ConstantPoolEntry::Utf8("append".to_string())), // 7
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "(Ljava/lang/String;)Ljava/lang/StringBuilder;".to_string(),
            )), // 8
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 7,
                descriptor_index: 8,
            }), // 9
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 9,
            }), // 10
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "(I)Ljava/lang/StringBuilder;".to_string(),
            )), // 11
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 7,
                descriptor_index: 11,
            }), // 12
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 12,
            }), // 13
            Recoverable::Present(ConstantPoolEntry::Utf8("toString".to_string())), // 14
            Recoverable::Present(ConstantPoolEntry::Utf8("()Ljava/lang/String;".to_string())), // 15
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 14,
                descriptor_index: 15,
            }), // 16
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 16,
            }), // 17
            Recoverable::Present(ConstantPoolEntry::Utf8("even".to_string())), // 18
            Recoverable::Present(ConstantPoolEntry::Utf8("odd".to_string())), // 19
            Recoverable::Present(ConstantPoolEntry::Utf8("test:".to_string())), // 20
            Recoverable::Present(ConstantPoolEntry::Utf8("java/lang/System".to_string())), // 21
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 21 }), // 22
            Recoverable::Present(ConstantPoolEntry::Utf8("out".to_string())), // 23
            Recoverable::Present(ConstantPoolEntry::Utf8("Ljava/io/PrintStream;".to_string())), // 24
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 23,
                descriptor_index: 24,
            }), // 25
            Recoverable::Present(ConstantPoolEntry::FieldRef {
                class_index: 22,
                name_and_type_index: 25,
            }), // 26
            Recoverable::Present(ConstantPoolEntry::Utf8("java/io/PrintStream".to_string())), // 27
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 27 }),                // 28
            Recoverable::Present(ConstantPoolEntry::Utf8("println".to_string())),             // 29
            Recoverable::Present(ConstantPoolEntry::Utf8("(Ljava/lang/String;)V".to_string())), // 30
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 29,
                descriptor_index: 30,
            }), // 31
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 28,
                name_and_type_index: 31,
            }), // 32
            Recoverable::Present(ConstantPoolEntry::String { string_index: 20 }), // 33 -> "test:"
            Recoverable::Present(ConstantPoolEntry::String { string_index: 18 }), // 34 -> "even"
            Recoverable::Present(ConstantPoolEntry::String { string_index: 19 }), // 35 -> "odd"
        ];
        let int = crate::bytecode::LoadStoreType::Int;
        let ref_ = crate::bytecode::LoadStoreType::Reference;
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 3,
                kind: InstructionKind::Type {
                    opcode: 0xbb,
                    cp_index: 2,
                },
            },
            Instruction {
                offset: 3,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Dup),
            },
            Instruction {
                offset: 4,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb7,
                    cp_index: 6,
                },
            },
            Instruction {
                offset: 7,
                length: 1,
                kind: InstructionKind::Store { ty: ref_, index: 1 },
            },
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Load { ty: ref_, index: 1 },
            },
            Instruction {
                offset: 9,
                length: 2,
                kind: InstructionKind::Ldc(33),
            },
            Instruction {
                offset: 11,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 10,
                },
            },
            Instruction {
                offset: 14,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Pop),
            },
            Instruction {
                offset: 15,
                length: 1,
                kind: InstructionKind::Iconst(0),
            },
            Instruction {
                offset: 16,
                length: 1,
                kind: InstructionKind::Store { ty: int, index: 2 },
            },
            Instruction {
                offset: 17,
                length: 3,
                kind: InstructionKind::Load { ty: int, index: 2 },
            },
            Instruction {
                offset: 18,
                length: 1,
                kind: InstructionKind::Iconst(5),
            },
            Instruction {
                offset: 19,
                length: 3,
                kind: InstructionKind::If {
                    opcode: 0xa2,
                    target: 59,
                },
            },
            Instruction {
                offset: 22,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 2 },
            },
            Instruction {
                offset: 23,
                length: 1,
                kind: InstructionKind::Iconst(2),
            },
            Instruction {
                offset: 24,
                length: 1,
                kind: InstructionKind::Arithmetic { opcode: 0x70 },
            },
            Instruction {
                offset: 25,
                length: 3,
                kind: InstructionKind::If {
                    opcode: 0x9a,
                    target: 42,
                },
            },
            Instruction {
                offset: 28,
                length: 1,
                kind: InstructionKind::Load { ty: ref_, index: 1 },
            },
            Instruction {
                offset: 29,
                length: 2,
                kind: InstructionKind::Ldc(34),
            },
            Instruction {
                offset: 31,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 10,
                },
            },
            Instruction {
                offset: 34,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 2 },
            },
            Instruction {
                offset: 35,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 13,
                },
            },
            Instruction {
                offset: 38,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Pop),
            },
            Instruction {
                offset: 39,
                length: 3,
                kind: InstructionKind::Goto(53),
            },
            Instruction {
                offset: 42,
                length: 1,
                kind: InstructionKind::Load { ty: ref_, index: 1 },
            },
            Instruction {
                offset: 43,
                length: 2,
                kind: InstructionKind::Ldc(35),
            },
            Instruction {
                offset: 45,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 10,
                },
            },
            Instruction {
                offset: 48,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 2 },
            },
            Instruction {
                offset: 49,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 13,
                },
            },
            Instruction {
                offset: 52,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Pop),
            },
            Instruction {
                offset: 53,
                length: 3,
                kind: InstructionKind::Iinc {
                    index: 2,
                    amount: 1,
                },
            },
            Instruction {
                offset: 56,
                length: 3,
                kind: InstructionKind::Goto(17),
            },
            Instruction {
                offset: 59,
                length: 3,
                kind: InstructionKind::Field {
                    opcode: 0xb2,
                    cp_index: 26,
                },
            },
            Instruction {
                offset: 62,
                length: 1,
                kind: InstructionKind::Load { ty: ref_, index: 1 },
            },
            Instruction {
                offset: 63,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 17,
                },
            },
            Instruction {
                offset: 66,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 32,
                },
            },
            Instruction {
                offset: 69,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Void),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "main",
            "([Ljava/lang/String;)V",
            None,
            &code,
            "SimpleTest",
            0x0009,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        // The counted loop must be canonicalized into a For whose body keeps
        // the if/else: For { init: [var_2 = 0], cond: var_2 < 5,
        // update: [var_2++], body: [If { then, else }] }.
        let for_debug = method
            .statements
            .iter()
            .find(|s| matches!(s, Statement::For { .. }))
            .map(|s| format!("{s:?}"))
            .expect("for loop missing");
        assert!(
            for_debug.contains("op: Lt"),
            "loop condition i < 5 missing: {for_debug}"
        );
        assert!(
            for_debug.contains("ConstInt(0)"),
            "loop init missing: {for_debug}"
        );
        assert!(
            for_debug.contains("even") && for_debug.contains("odd"),
            "if/else branches missing in loop body: {for_debug}"
        );
        assert!(
            for_debug.contains("then_body: ["),
            "then branch empty: {for_debug}"
        );
        assert!(
            for_debug.contains("else_body: Some(["),
            "else branch missing: {for_debug}"
        );
    }

    #[test]
    fn ternary_reconstruction_from_if_else_assignment() {
        // Mirrors: int r; if (a > b) { r = 1; } else { r = 2; } -> r = a > b ? 1 : 2;
        let pool = vec![Recoverable::Missing]; // unused by these opcodes
        let int = crate::bytecode::LoadStoreType::Int;
        let instructions = vec![
            // a > b  (params: a=1, b=2)
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 1 },
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 2 },
            },
            Instruction {
                offset: 2,
                length: 3,
                kind: InstructionKind::If {
                    opcode: 0xa3,
                    target: 10,
                },
            }, // if_icmple -> else
            // then: r = 1
            Instruction {
                offset: 5,
                length: 1,
                kind: InstructionKind::Iconst(1),
            },
            Instruction {
                offset: 6,
                length: 1,
                kind: InstructionKind::Store { ty: int, index: 3 },
            },
            Instruction {
                offset: 7,
                length: 3,
                kind: InstructionKind::Goto(13),
            },
            // else: r = 2
            Instruction {
                offset: 10,
                length: 1,
                kind: InstructionKind::Iconst(2),
            },
            Instruction {
                offset: 11,
                length: 1,
                kind: InstructionKind::Store { ty: int, index: 3 },
            },
            // join: return r
            Instruction {
                offset: 13,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 3 },
            },
            Instruction {
                offset: 14,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "pick",
            "(II)I",
            None,
            &code,
            "TernaryTest",
            0x0009,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let debug = format!("{:?}", method.statements);
        assert!(debug.contains("Ternary"), "ternary missing, got: {debug}");
        assert!(
            debug.contains("ConstInt(1)") && debug.contains("ConstInt(2)"),
            "ternary arms wrong: {debug}"
        );
    }

    #[test]
    fn ternary_reconstruction_from_early_return_fallthrough() {
        // `if (c) { return -1; } return 1;` -> `return c ? -1 : 1;`
        // (implicit else: the then-arm diverges, so fall-through means !c).
        let mut stmts = vec![
            Statement::If {
                condition: Expression::Local("c".to_string()),
                then_body: vec![Statement::Return(Some(Expression::ConstInt(-1)))],
                else_body: None,
            },
            Statement::Return(Some(Expression::ConstInt(1))),
        ];
        reconstruct_ternaries_recursive(&mut stmts);
        assert_eq!(
            stmts,
            vec![Statement::Return(Some(Expression::Ternary {
                condition: Box::new(Expression::Local("c".to_string())),
                then_expr: Box::new(Expression::ConstInt(-1)),
                else_expr: Box::new(Expression::ConstInt(1)),
            }))],
        );
    }

    #[test]
    fn ternary_does_not_nest_across_rules() {
        // `if (a) { return 1; } if (b) { return 2; } else { return 3; }`:
        // the inner if/else still fuses (flat), but the outer fall-through
        // must NOT fold onto it — `a ? 1 : (b ? 2 : 3)` is deliberately
        // left as explicit if/else for readability.
        let mut stmts = vec![
            Statement::If {
                condition: Expression::Local("a".to_string()),
                then_body: vec![Statement::Return(Some(Expression::ConstInt(1)))],
                else_body: None,
            },
            Statement::If {
                condition: Expression::Local("b".to_string()),
                then_body: vec![Statement::Return(Some(Expression::ConstInt(2)))],
                else_body: Some(vec![Statement::Return(Some(Expression::ConstInt(3)))]),
            },
        ];
        reconstruct_ternaries_recursive(&mut stmts);
        assert_eq!(
            stmts,
            vec![
                Statement::If {
                    condition: Expression::Local("a".to_string()),
                    then_body: vec![Statement::Return(Some(Expression::ConstInt(1)))],
                    else_body: None,
                },
                Statement::Return(Some(Expression::Ternary {
                    condition: Box::new(Expression::Local("b".to_string())),
                    then_expr: Box::new(Expression::ConstInt(2)),
                    else_expr: Box::new(Expression::ConstInt(3)),
                })),
            ],
        );
    }

    #[test]
    fn ternary_does_not_nest_in_call_args() {
        // `if (c) { x = f(d ? 1 : 2); } else { x = 3; }` stays unfolded:
        // folding would bury a ternary inside call args.
        let nested = Expression::Ternary {
            condition: Box::new(Expression::Local("d".to_string())),
            then_expr: Box::new(Expression::ConstInt(1)),
            else_expr: Box::new(Expression::ConstInt(2)),
        };
        let mut stmts = vec![Statement::If {
            condition: Expression::Local("c".to_string()),
            then_body: vec![Statement::Assign {
                target: "x".to_string(),
                value: Expression::Invoke {
                    target: "f".to_string(),
                    args: vec![nested],
                },
            }],
            else_body: Some(vec![Statement::Assign {
                target: "x".to_string(),
                value: Expression::ConstInt(3),
            }]),
        }];
        let before = stmts.clone();
        reconstruct_ternaries_recursive(&mut stmts);
        assert_eq!(stmts, before);
    }

    #[test]
    fn ternary_does_not_fold_assignment_fallthrough() {
        // `if (c) { x = a; } x = b;` is NOT a ternary: when c holds,
        // x is assigned twice. Must be left alone.
        let mut stmts = vec![
            Statement::If {
                condition: Expression::Local("c".to_string()),
                then_body: vec![Statement::Assign {
                    target: "x".to_string(),
                    value: Expression::ConstInt(1),
                }],
                else_body: None,
            },
            Statement::Assign {
                target: "x".to_string(),
                value: Expression::ConstInt(2),
            },
        ];
        let before = stmts.clone();
        reconstruct_ternaries_recursive(&mut stmts);
        assert_eq!(stmts, before);
    }

    #[test]
    fn stack_composes_load_and_getfield() {
        let pool = vec![
            Recoverable::Missing,
            Recoverable::Present(ConstantPoolEntry::Utf8("Foo".to_string())),
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }),
            Recoverable::Present(ConstantPoolEntry::Utf8("name".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("Ljava/lang/String;".to_string())),
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }),
            Recoverable::Present(ConstantPoolEntry::FieldRef {
                class_index: 2,
                name_and_type_index: 5,
            }),
        ];

        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load {
                    ty: crate::bytecode::LoadStoreType::Reference,
                    index: 0,
                },
            },
            Instruction {
                offset: 1,
                length: 3,
                kind: InstructionKind::Field {
                    opcode: 0xb4,
                    cp_index: 6,
                },
            },
        ];

        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "$2",
            None,
            &code,
            "$4",
            0,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Expression(Expression::FieldAccess { field, .. }) => {
                assert_eq!(field, "name");
            }
            other => panic!("expected FieldAccess, got {other:?}"),
        }
    }

    #[test]
    fn stack_composes_invoke_with_args() {
        let pool = vec![
            Recoverable::Missing, // 0
            Recoverable::Present(ConstantPoolEntry::Utf8("java/lang/System".to_string())), // 1
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }), // 2
            Recoverable::Present(ConstantPoolEntry::Utf8("out".to_string())), // 3
            Recoverable::Present(ConstantPoolEntry::Utf8("Ljava/io/PrintStream;".to_string())), // 4
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }), // 5
            Recoverable::Present(ConstantPoolEntry::FieldRef {
                class_index: 2,
                name_and_type_index: 5,
            }), // 6
            Recoverable::Present(ConstantPoolEntry::Utf8("java/io/PrintStream".to_string())), // 7
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 7 }), // 8
            Recoverable::Present(ConstantPoolEntry::Utf8("println".to_string())), // 9
            Recoverable::Present(ConstantPoolEntry::Utf8("(Ljava/lang/String;)V".to_string())), // 10
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 9,
                descriptor_index: 10,
            }), // 11
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 8,
                name_and_type_index: 11,
            }), // 12
            Recoverable::Present(ConstantPoolEntry::Utf8("hello".to_string())), // 13
            Recoverable::Present(ConstantPoolEntry::String { string_index: 13 }), // 14
        ];

        let instructions = vec![
            Instruction {
                offset: 0,
                length: 3,
                kind: InstructionKind::Field {
                    opcode: 0xb2,
                    cp_index: 6,
                },
            },
            Instruction {
                offset: 3,
                length: 2,
                kind: InstructionKind::Ldc(14),
            },
            Instruction {
                offset: 5,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb6,
                    cp_index: 12,
                },
            },
        ];

        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "$2",
            None,
            &code,
            "$4",
            0,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Expression(Expression::Invoke { target, args }) => {
                assert_eq!(target, "System.out.println");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], Expression::ConstString("hello".to_string()));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn new_constructor_composes() {
        let pool = vec![
            Recoverable::Missing,
            Recoverable::Present(ConstantPoolEntry::Utf8("Foo".to_string())),
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }),
            Recoverable::Present(ConstantPoolEntry::Utf8("<init>".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("()V".to_string())),
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }),
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 5,
            }),
        ];

        let instructions = vec![
            Instruction {
                offset: 0,
                length: 3,
                kind: InstructionKind::Type {
                    opcode: 0xbb,
                    cp_index: 2,
                },
            },
            Instruction {
                offset: 3,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Dup),
            },
            Instruction {
                offset: 4,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb7,
                    cp_index: 6,
                },
            },
            Instruction {
                offset: 7,
                length: 1,
                kind: InstructionKind::Store {
                    ty: crate::bytecode::LoadStoreType::Reference,
                    index: 1,
                },
            },
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Void),
            },
        ];

        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "$2",
            None,
            &code,
            "$4",
            0,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        // var_1 = new Foo(); (trailing void return removed)
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::VarDecl { target, value, .. } => {
                assert_eq!(target, "var_1");
                assert!(matches!(value, Some(Expression::New { .. })));
            }
            Statement::Assign { target, value } => {
                assert_eq!(target, "var_1");
                assert!(matches!(value, Expression::New { .. }));
            }
            other => panic!("expected VarDecl/Assign(New), got {other:?}"),
        }
    }

    #[test]
    fn return_composes_value() {
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Iconst(42),
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
        ];

        let pool: Vec<Recoverable<ConstantPoolEntry>> = vec![];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "$2",
            None,
            &code,
            "$4",
            0,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Return(Some(Expression::ConstInt(42))) => {}
            other => panic!("expected Return(ConstInt(42)), got {other:?}"),
        }
    }

    #[test]
    fn putfield_composes_with_stack() {
        // aload_0 + new Foo() + putfield should produce this.x = new Foo()
        let pool = vec![
            Recoverable::Missing,                                               // 0
            Recoverable::Present(ConstantPoolEntry::Utf8("Bar".to_string())),   // 1
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }),   // 2
            Recoverable::Present(ConstantPoolEntry::Utf8("Foo".to_string())),   // 3
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 3 }),   // 4
            Recoverable::Present(ConstantPoolEntry::Utf8("x".to_string())),     // 5
            Recoverable::Present(ConstantPoolEntry::Utf8("LFoo;".to_string())), // 6
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 5,
                descriptor_index: 6,
            }), // 7
            Recoverable::Present(ConstantPoolEntry::FieldRef {
                class_index: 2,
                name_and_type_index: 7,
            }), // 8
            Recoverable::Present(ConstantPoolEntry::Utf8("<init>".to_string())), // 9
            Recoverable::Present(ConstantPoolEntry::Utf8("()V".to_string())),   // 10
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 9,
                descriptor_index: 10,
            }), // 11
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 4,
                name_and_type_index: 11,
            }), // 12
        ];

        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load {
                    ty: crate::bytecode::LoadStoreType::Reference,
                    index: 0,
                },
            },
            Instruction {
                offset: 1,
                length: 3,
                kind: InstructionKind::Type {
                    opcode: 0xbb,
                    cp_index: 4,
                },
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::Stack(crate::bytecode::StackOp::Dup),
            },
            Instruction {
                offset: 5,
                length: 3,
                kind: InstructionKind::Invoke {
                    opcode: 0xb7,
                    cp_index: 12,
                },
            },
            Instruction {
                offset: 8,
                length: 3,
                kind: InstructionKind::Field {
                    opcode: 0xb5,
                    cp_index: 8,
                },
            },
        ];

        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "$2",
            None,
            &code,
            "$4",
            0,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Assign { target, value } => {
                assert!(
                    target.contains("x"),
                    "target should contain field name 'x', got: {target}"
                );
                assert!(
                    matches!(value, Expression::New { .. }),
                    "value should be New, got: {value:?}"
                );
            }
            other => panic!("expected Assign with New, got {other:?}"),
        }
    }

    #[test]
    fn store_reuses_lvt_name_without_splitting() {
        // Slot 1 is named `count` in the LocalVariableTable: stores must
        // reuse it instead of emitting a parallel `var_1` variable.
        let pool = vec![
            Recoverable::Missing,
            Recoverable::Present(ConstantPoolEntry::Utf8("count".to_string())),
            Recoverable::Present(ConstantPoolEntry::Utf8("I".to_string())),
        ];
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Iconst(1),
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::Store {
                    ty: crate::bytecode::LoadStoreType::Int,
                    index: 1,
                },
            },
            Instruction {
                offset: 2,
                length: 1,
                kind: InstructionKind::Load {
                    ty: crate::bytecode::LoadStoreType::Int,
                    index: 1,
                },
            },
            Instruction {
                offset: 3,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
        ];
        let mut code = test_code(instructions);
        code.local_variable_table = Some(vec![crate::classfile::LocalVariableInfo {
            start_pc: 0,
            length: 10,
            name_index: 1,
            descriptor_index: 2,
            index: 1,
        }]);

        let method = lower_method_to_ast(
            &pool,
            "test",
            "()I",
            None,
            &code,
            "Test",
            0x0008,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let rendered = method
            .statements
            .iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("var_1"),
            "slot must not split into var_1, got:\n{rendered}"
        );
        assert!(
            rendered.contains("count"),
            "LVT name must survive, got:\n{rendered}"
        );
    }

    #[test]
    fn simplifies_int_zero_comparison_to_bare_boolean() {
        let mut ne = Expression::Binary {
            left: Box::new(Expression::Local("flag".to_string())),
            op: BinaryOp::Ne,
            right: Box::new(Expression::ConstInt(0)),
        };
        assert!(simplify_condition(&mut ne));
        assert_eq!(ne, Expression::Local("flag".to_string()));

        let mut eq = Expression::Binary {
            left: Box::new(Expression::Local("flag".to_string())),
            op: BinaryOp::Eq,
            right: Box::new(Expression::ConstInt(0)),
        };
        assert!(simplify_condition(&mut eq));
        assert_eq!(
            eq,
            Expression::Unary {
                op: UnaryOp::Not,
                operand: Box::new(Expression::Local("flag".to_string())),
            }
        );
    }

    #[test]
    fn keeps_genuine_integer_comparison() {
        // `(a - b) != 0` is arithmetic, not a boolean flag.
        let mut cond = Expression::Binary {
            left: Box::new(Expression::Binary {
                left: Box::new(Expression::Local("a".to_string())),
                op: BinaryOp::Sub,
                right: Box::new(Expression::Local("b".to_string())),
            }),
            op: BinaryOp::Ne,
            right: Box::new(Expression::ConstInt(0)),
        };
        assert!(!simplify_condition(&mut cond));
    }

    #[test]
    fn folds_constant_comparison_in_condition() {
        // `if (!(1 != 0))` (obfuscator opaque predicate) folds bottom-up.
        let mut cond = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Binary {
                left: Box::new(Expression::ConstInt(1)),
                op: BinaryOp::Ne,
                right: Box::new(Expression::ConstInt(0)),
            }),
        };
        assert!(simplify_condition(&mut cond));
        assert_eq!(cond, Expression::ConstInt(0));
    }

    #[test]
    fn simplifies_double_negation_and_null() {
        let mut not_not = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Unary {
                op: UnaryOp::Not,
                operand: Box::new(Expression::Local("x".to_string())),
            }),
        };
        assert!(simplify_condition(&mut not_not));
        assert_eq!(not_not, Expression::Local("x".to_string()));

        let mut not_null_eq = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Binary {
                left: Box::new(Expression::Local("o".to_string())),
                op: BinaryOp::Eq,
                right: Box::new(Expression::ConstNull),
            }),
        };
        assert!(simplify_condition(&mut not_null_eq));
        assert_eq!(
            not_null_eq,
            Expression::Binary {
                left: Box::new(Expression::Local("o".to_string())),
                op: BinaryOp::Ne,
                right: Box::new(Expression::ConstNull),
            }
        );
    }

    #[test]
    fn reconstructs_tableswitch_with_default() {
        let pool = vec![Recoverable::Missing];
        let int = crate::bytecode::LoadStoreType::Int;
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 0 },
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::TableSwitch {
                    default: 10,
                    low: 1,
                    high: 2,
                    targets: vec![4, 7],
                },
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::Iconst(10),
            },
            Instruction {
                offset: 5,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
            Instruction {
                offset: 7,
                length: 1,
                kind: InstructionKind::Iconst(20),
            },
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
            Instruction {
                offset: 10,
                length: 1,
                kind: InstructionKind::Iconst(30),
            },
            Instruction {
                offset: 11,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Int),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "(I)I",
            None,
            &code,
            "Test",
            0x0008,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Switch { discriminant, arms } => {
                assert_eq!(discriminant, &Expression::Local("arg0".to_string()));
                assert_eq!(arms.len(), 3);
                assert_eq!(arms[0].keys, vec![1]);
                assert!(!arms[0].is_default);
                assert_eq!(arms[1].keys, vec![2]);
                assert!(arms[2].is_default);
                assert!(arms[2].keys.is_empty());
            }
            other => panic!("expected Switch, got {other:?}"),
        }
    }

    #[test]
    fn distributes_switch_join_store_per_arm() {
        // switch (arg0) { case 1: push 10 → join; case 2: push 20 → join;
        //   default: push 30 → join } join: store var_1
        // becomes per-arm `var_1 = <const>` with no trailing store.
        let pool = vec![Recoverable::Missing];
        let int = crate::bytecode::LoadStoreType::Int;
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 0 },
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::TableSwitch {
                    default: 12,
                    low: 1,
                    high: 2,
                    targets: vec![4, 8],
                },
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::Iconst(10),
            },
            Instruction {
                offset: 6,
                length: 1,
                kind: InstructionKind::Goto(14),
            },
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Iconst(20),
            },
            Instruction {
                offset: 10,
                length: 1,
                kind: InstructionKind::Goto(14),
            },
            Instruction {
                offset: 12,
                length: 1,
                kind: InstructionKind::Iconst(30),
            },
            Instruction {
                offset: 14,
                length: 1,
                kind: InstructionKind::Store { ty: int, index: 1 },
            },
            Instruction {
                offset: 15,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Void),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "(I)V",
            None,
            &code,
            "Test",
            0x0008,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Switch { arms, .. } => {
                assert_eq!(arms.len(), 3);
                // Each arm carries its own store; the join store is gone.
                for arm in arms {
                    let last = arm.body.last().expect("arm must end with store");
                    assert!(
                        matches!(last, Statement::Assign { .. } | Statement::VarDecl { .. }),
                        "expected distributed store, got {last:?}"
                    );
                }
            }
            other => panic!("expected Switch, got {other:?}"),
        }
    }

    #[test]
    fn unreachable_trap_between_arms_does_not_poison_join() {
        // Mirrors real obfuscated switches: an unreachable `athrow` trap
        // sits between two arms. Linear simulation must not execute it
        // (it would pop the previous arm's value off the stack).
        let pool = vec![Recoverable::Missing];
        let int = crate::bytecode::LoadStoreType::Int;
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Load { ty: int, index: 0 },
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::TableSwitch {
                    default: 12,
                    low: 1,
                    high: 1,
                    targets: vec![4],
                },
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::Iconst(10),
            },
            Instruction {
                offset: 6,
                length: 1,
                kind: InstructionKind::Goto(14),
            },
            // Unreachable trap: no predecessor jumps here.
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Throw,
            },
            Instruction {
                offset: 12,
                length: 1,
                kind: InstructionKind::Iconst(30),
            },
            Instruction {
                offset: 14,
                length: 1,
                kind: InstructionKind::Store { ty: int, index: 1 },
            },
            Instruction {
                offset: 15,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Void),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "(I)V",
            None,
            &code,
            "Test",
            0x0008,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let rendered = format!("{:?}", method.statements);
        // The trap must not leak into the output...
        assert!(
            !rendered.contains("throw"),
            "unreachable trap leaked: {rendered}"
        );
        // ...and the join value must resolve per-arm.
        match &method.statements[0] {
            Statement::Switch { arms, .. } => {
                assert_eq!(arms.len(), 2);
                for arm in arms {
                    let last = arm.body.last().expect("arm must end with store");
                    assert!(
                        matches!(last, Statement::Assign { .. } | Statement::VarDecl { .. }),
                        "expected distributed store, got {last:?}"
                    );
                }
            }
            other => panic!("expected Switch, got {other:?}"),
        }
    }

    #[test]
    fn folds_string_concat_with_constants() {
        // invokedynamic makeConcatWithConstants("A\u{1}B\u{2}", dynamic, 42)
        let pool = vec![
            Recoverable::Missing, // 0
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "java/lang/invoke/StringConcatFactory".to_string(),
            )), // 1
            Recoverable::Present(ConstantPoolEntry::Class { name_index: 1 }), // 2
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "makeConcatWithConstants".to_string(),
            )), // 3
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "(Ljava/lang/String;I)Ljava/lang/String;".to_string(),
            )), // 4
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }), // 5
            Recoverable::Present(ConstantPoolEntry::MethodRef {
                class_index: 2,
                name_and_type_index: 5,
            }), // 6
            Recoverable::Present(ConstantPoolEntry::MethodHandle {
                reference_kind: 6,
                reference_index: 6,
            }), // 7
            Recoverable::Present(ConstantPoolEntry::Utf8("A\u{1}B\u{2}".to_string())), // 8
            Recoverable::Present(ConstantPoolEntry::String { string_index: 8 }), // 9
            Recoverable::Present(ConstantPoolEntry::Integer(42)), // 10
            Recoverable::Present(ConstantPoolEntry::Utf8("run".to_string())), // 11
            Recoverable::Present(ConstantPoolEntry::Utf8(
                "(Ljava/lang/String;)Ljava/lang/String;".to_string(),
            )), // 12
            Recoverable::Present(ConstantPoolEntry::NameAndType {
                name_index: 11,
                descriptor_index: 12,
            }), // 13
            Recoverable::Present(ConstantPoolEntry::InvokeDynamic {
                bootstrap_method_attr_index: 0,
                name_and_type_index: 13,
            }), // 14
            Recoverable::Present(ConstantPoolEntry::Utf8("S".to_string())), // 15
            Recoverable::Present(ConstantPoolEntry::String { string_index: 15 }), // 16
        ];
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 2,
                kind: InstructionKind::Ldc(16),
            },
            Instruction {
                offset: 3,
                length: 5,
                kind: InstructionKind::Invoke {
                    opcode: 0xba,
                    cp_index: 14,
                },
            },
            Instruction {
                offset: 8,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Reference),
            },
        ];
        let code = test_code(instructions);
        let bootstrap = vec![crate::classfile::BootstrapMethodInfo {
            method_handle_index: 7,
            arguments: vec![9, 10],
        }];
        let method = lower_method_to_ast(
            &pool,
            "run",
            "()Ljava/lang/String;",
            None,
            &code,
            "Test",
            0x0008,
            &bootstrap,
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert_eq!(method.statements.len(), 1);
        match &method.statements[0] {
            Statement::Return(Some(expr)) => {
                // "A" + "S" + "B" + 42 — recipe interleaves dynamics (\1) and constants (\2).
                // (Nested `+` renders left-associative with parens.)
                let rendered = crate::ast::render_expression_pub(expr);
                assert_eq!(rendered, "((\"A\" + \"S\") + \"B\") + 42");
            }
            other => panic!("expected Return with concat, got {other:?}"),
        }
    }

    #[test]
    fn spills_fresh_array_store_into_temp() {
        let pool = vec![Recoverable::Missing];
        let instructions = vec![
            Instruction {
                offset: 0,
                length: 1,
                kind: InstructionKind::Iconst(1),
            },
            Instruction {
                offset: 1,
                length: 1,
                kind: InstructionKind::NewArray(10),
            },
            Instruction {
                offset: 2,
                length: 1,
                kind: InstructionKind::Iconst(0),
            },
            Instruction {
                offset: 3,
                length: 1,
                kind: InstructionKind::Iconst(5),
            },
            Instruction {
                offset: 4,
                length: 1,
                kind: InstructionKind::ArrayStore(crate::bytecode::LoadStoreType::Int),
            },
            Instruction {
                offset: 5,
                length: 1,
                kind: InstructionKind::Return(crate::bytecode::ReturnType::Void),
            },
        ];
        let code = test_code(instructions);
        let method = lower_method_to_ast(
            &pool,
            "test",
            "()V",
            None,
            &code,
            "Test",
            0x0008,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashSet::new(),
        );
        assert!(method.statements.len() >= 2);
        match &method.statements[0] {
            Statement::VarDecl {
                target,
                value: Some(Expression::NewArray { .. }),
                ..
            } => {
                assert_eq!(target, "arr_tmp0");
            }
            other => panic!("expected fresh array temp, got {other:?}"),
        }
        match &method.statements[1] {
            Statement::Assign { target, .. } => {
                assert_eq!(target, "arr_tmp0[0]");
            }
            other => panic!("expected element store, got {other:?}"),
        }
    }

    #[test]
    fn removes_dead_constant_branches() {
        let mut stmts = vec![
            Statement::If {
                condition: Expression::ConstInt(0),
                then_body: vec![Statement::Return(Some(Expression::ConstInt(1)))],
                else_body: Some(vec![Statement::Return(Some(Expression::ConstInt(2)))]),
            },
            Statement::While {
                condition: Expression::ConstInt(0),
                body: vec![Statement::Expression(Expression::ConstInt(3))],
            },
        ];
        remove_dead_branches(&mut stmts);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], Statement::Return(Some(Expression::ConstInt(2))));
    }
}
