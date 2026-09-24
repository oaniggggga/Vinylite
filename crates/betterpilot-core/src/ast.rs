#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Assign {
        target: String,
        value: Expression,
    },
    VarDecl {
        target: String,
        var_type: Option<String>,
        value: Option<Expression>,
    },
    Expression(Expression),
    If {
        condition: Expression,
        then_body: Vec<Statement>,
        else_body: Option<Vec<Statement>>,
    },
    While {
        condition: Expression,
        body: Vec<Statement>,
    },
    ForEach {
        var_type: Option<String>,
        var_name: String,
        collection: Expression,
        body: Vec<Statement>,
    },
    Return(Option<Expression>),
    TryCatch {
        try_body: Vec<Statement>,
        catch_type: String,
        catch_var: String,
        catch_body: Vec<Statement>,
    },
    Switch {
        discriminant: Expression,
        arms: Vec<SwitchArm>,
    },
    Unknown(String),
}

/// One `switch` arm: a (possibly multi-label) `case` plus optionally the
/// `default` label jumping to the same body.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchArm {
    pub keys: Vec<i32>,
    pub is_default: bool,
    pub body: Vec<Statement>,
    /// True when control leaves the switch after this arm (a forward jump
    /// out in bytecode). False means intentional fallthrough into the next
    /// arm — no `break` is emitted.
    pub breaks: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    ConstInt(i32),
    ConstLong(i64),
    ConstFloat(f32),
    ConstDouble(f64),
    ConstString(String),
    ConstNull,
    Local(String),
    FieldAccess {
        object: Box<Expression>,
        field: String,
    },
    Binary {
        left: Box<Expression>,
        op: BinaryOp,
        right: Box<Expression>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expression>,
    },
    New {
        class: String,
        args: Vec<Expression>,
    },
    NewArray {
        element_type: String,
        size: Box<Expression>,
    },
    ArrayAccess {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    Cast {
        target_type: String,
        expr: Box<Expression>,
    },
    InstanceOf {
        expr: Box<Expression>,
        target_type: String,
    },
    Ternary {
        condition: Box<Expression>,
        then_expr: Box<Expression>,
        else_expr: Box<Expression>,
    },
    This,
    Super,
    Invoke {
        target: String,
        args: Vec<Expression>,
    },
    /// An in-progress `StringBuilder`/`StringBuffer` append chain:
    /// `new StringBuilder().append(a).append(b)`. Folded to `a + b` when
    /// `.toString()` terminates it; rendered back as builder code if it
    /// escapes unfolded (rare).
    Concat {
        builder: String,
        parts: Vec<Expression>,
    },
    Lambda {
        params: Vec<(String, String)>,
        body: Vec<Statement>,
    },
    MethodRef(String, String),
    Unknown(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BoolAnd,
    BoolOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub field_type: String,
    pub access_flags: u16,
    pub initial_value: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDecl {
    pub name: String,
    pub statements: Vec<Statement>,
    pub access_flags: u16,
    pub return_type: String,
    pub param_types: Vec<String>,
    pub param_names: Vec<String>,
    pub class_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    pub name: String,
    pub package: Option<String>,
    pub imports: Vec<String>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<MethodDecl>,
    pub access_flags: u16,
    pub super_name: Option<String>,
    pub is_enum: bool,
    pub enum_constants: Vec<String>,
}

impl ClassDecl {
    pub fn render(&self) -> String {
        let mut out = String::new();

        // Package
        if let Some(pkg) = &self.package {
            out.push_str("package ");
            out.push_str(pkg);
            out.push_str(";\n\n");
        }

        // Render body first (to filter imports by actual usage)
        let mut body = String::new();

        // Class header
        if self.is_enum {
            body.push_str("public enum ");
            body.push_str(&self.name);
            body.push_str(" {\n");
            for (i, constant) in self.enum_constants.iter().enumerate() {
                body.push_str("    ");
                body.push_str(constant);
                if i + 1 < self.enum_constants.len() {
                    body.push_str(",\n");
                } else {
                    body.push_str(";\n");
                }
            }
        } else {
            body.push_str("public class ");
            body.push_str(&self.name);
            if let Some(super_name) = &self.super_name
                && super_name.split('<').next().unwrap_or(super_name) != "java.lang.Object"
            {
                body.push_str(" extends ");
                body.push_str(&short_type(super_name));
            }
            body.push_str(" {\n");
        }

        // Render fields
        let mut rendered_field_count = 0;
        for field in &self.fields {
            if self.is_enum
                && (field.name.starts_with('$')
                    || field.name == "ENUM$VALUES"
                    || field.name == "INSTANCE")
            {
                continue;
            }
            body.push_str("    ");
            if field.access_flags & 0x0001 != 0 {
                body.push_str("public ");
            } else if field.access_flags & 0x0002 != 0 {
                body.push_str("private ");
            } else if field.access_flags & 0x0004 != 0 {
                body.push_str("protected ");
            }
            if field.access_flags & 0x0008 != 0 {
                body.push_str("static ");
            }
            if field.access_flags & 0x0010 != 0 {
                body.push_str("final ");
            }
            body.push_str(&short_type(&field.field_type));
            body.push(' ');
            body.push_str(&field.name);
            if let Some(init) = &field.initial_value {
                body.push_str(" = ");
                body.push_str(&render_expression(init));
            }
            body.push_str(";\n");
            rendered_field_count += 1;
        }
        if rendered_field_count > 0 {
            body.push('\n');
        }

        for method in &self.methods {
            if self.is_enum && is_enum_synthetic_method(&method.name) {
                continue;
            }
            body.push('\n');
            body.push_str(&render_method(method));
        }

        body.push_str("}\n");

        // Filter imports: only keep those whose short name appears in the body
        if !self.imports.is_empty() {
            let mut sorted: Vec<&str> = self.imports.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            sorted.dedup();
            for imp in &sorted {
                let short = imp.rsplit('.').next().unwrap_or(imp);
                if body.contains(short) {
                    out.push_str("import ");
                    out.push_str(imp);
                    out.push_str(";\n");
                }
            }
            out.push('\n');
        }

        out.push_str(&body);
        out
    }
}

fn render_method(method: &MethodDecl) -> String {
    let mut out = String::new();
    let pad = "    ";

    // Access modifiers
    out.push_str(pad);
    let is_clinit = method.name == "<clinit>";
    let is_init = method.name == "<init>";

    if !is_clinit && !is_init {
        if method.access_flags & 0x0001 != 0 {
            out.push_str("public ");
        } else if method.access_flags & 0x0002 != 0 {
            out.push_str("private ");
        } else if method.access_flags & 0x0004 != 0 {
            out.push_str("protected ");
        }
    }

    if method.access_flags & 0x0008 != 0 && !is_clinit {
        out.push_str("static ");
    }
    if method.access_flags & 0x0040 != 0 {
        out.push_str("native ");
    }
    if method.access_flags & 0x0800 != 0 {
        out.push_str("abstract ");
    }
    if method.access_flags & 0x1000 != 0 && !is_clinit {
        out.push_str("final ");
    }

    // Constructor or normal method
    if is_init {
        out.push_str(&short_type(&method.class_name));
        out.push('(');
        let params: Vec<String> = method
            .param_types
            .iter()
            .zip(method.param_names.iter())
            .map(|(ty, name)| format!("{} {}", short_type(ty), name))
            .collect();
        out.push_str(&params.join(", "));
        out.push_str(") {\n");
    } else if is_clinit {
        out.push_str("static {\n");
    } else {
        // Return type and name
        out.push_str(&short_type(&method.return_type));
        out.push(' ');
        out.push_str(&method.name);
        out.push('(');
        // Params
        let params: Vec<String> = method
            .param_types
            .iter()
            .zip(method.param_names.iter())
            .map(|(ty, name)| format!("{} {}", short_type(ty), name))
            .collect();
        out.push_str(&params.join(", "));
        out.push_str(") {\n");
    }

    // Body
    for stmt in &method.statements {
        out.push_str(&render_statement(stmt, 2));
    }
    out.push_str(pad);
    out.push_str("}\n");

    out
}

fn render_statement(stmt: &Statement, indent: usize) -> String {
    let mut out = String::new();
    let pad = "    ".repeat(indent);

    match stmt {
        Statement::Assign { target, value } => {
            // Skip synthetic enum assignments like $VALUES = ...
            if target.starts_with('$') {
                return out;
            }
            out.push_str(&pad);
            out.push_str(target);
            out.push_str(" = ");
            out.push_str(&render_expression_at(value, indent));
            out.push_str(";\n");
        }
        Statement::VarDecl {
            target,
            var_type,
            value,
        } => {
            if target.starts_with('$') {
                return out;
            }
            out.push_str(&pad);
            if let Some(ty) = var_type {
                out.push_str(&short_type(ty));
                out.push(' ');
            } else {
                out.push_str("var ");
            }
            out.push_str(target);
            if let Some(val) = value {
                out.push_str(" = ");
                out.push_str(&render_expression_at(val, indent));
            }
            out.push_str(";\n");
        }
        Statement::Expression(expr) => {
            out.push_str(&pad);
            out.push_str(&render_expression_at(expr, indent));
            out.push_str(";\n");
        }
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            out.push_str(&pad);
            out.push_str("if (");
            out.push_str(&render_expression_at(condition, indent));
            out.push_str(") {\n");
            for s in then_body {
                out.push_str(&render_statement(s, indent + 1));
            }
            out.push_str(&pad);
            out.push_str("}\n");
            if let Some(else_body) = else_body {
                out.push_str(&pad);
                out.push_str("else {\n");
                for s in else_body {
                    out.push_str(&render_statement(s, indent + 1));
                }
                out.push_str(&pad);
                out.push_str("}\n");
            }
        }
        Statement::While { condition, body } => {
            out.push_str(&pad);
            out.push_str("while (");
            out.push_str(&render_expression_at(condition, indent));
            out.push_str(") {\n");
            for s in body {
                out.push_str(&render_statement(s, indent + 1));
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Statement::ForEach {
            var_type,
            var_name,
            collection,
            body,
        } => {
            out.push_str(&pad);
            out.push_str("for (");
            if let Some(ty) = var_type {
                out.push_str(&short_type(ty));
                out.push(' ');
            } else {
                out.push_str("var ");
            }
            out.push_str(var_name);
            out.push_str(" : ");
            out.push_str(&render_expression_at(collection, indent));
            out.push_str(") {\n");
            for s in body {
                out.push_str(&render_statement(s, indent + 1));
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Statement::Return(expr) => {
            out.push_str(&pad);
            out.push_str("return");
            if let Some(e) = expr {
                out.push(' ');
                out.push_str(&render_expression_at(e, indent));
            }
            out.push(';');
            out.push('\n');
        }
        Statement::TryCatch {
            try_body,
            catch_type,
            catch_var,
            catch_body,
        } => {
            out.push_str(&pad);
            out.push_str("try {\n");
            for s in try_body {
                out.push_str(&render_statement(s, indent + 1));
            }
            out.push_str(&pad);
            out.push_str("} catch (");
            out.push_str(catch_type);
            out.push(' ');
            out.push_str(catch_var);
            out.push_str(") {\n");
            for s in catch_body {
                out.push_str(&render_statement(s, indent + 1));
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Statement::Switch { discriminant, arms } => {
            out.push_str(&pad);
            out.push_str("switch (");
            out.push_str(&render_expression_at(discriminant, indent));
            out.push_str(") {\n");
            for (n, arm) in arms.iter().enumerate() {
                let is_last = n + 1 == arms.len();
                if !arm.keys.is_empty() {
                    let keys: Vec<String> = arm.keys.iter().map(|k| k.to_string()).collect();
                    // Java 14+ multi-label case; stacked with `default:`
                    // when both jump to this body.
                    out.push_str(&pad);
                    out.push_str("case ");
                    out.push_str(&keys.join(", "));
                    out.push_str(":\n");
                }
                if arm.is_default {
                    out.push_str(&pad);
                    out.push_str("default:\n");
                }
                for s in &arm.body {
                    out.push_str(&render_statement(s, indent + 1));
                }
                // Close the arm unless it already diverges. An arm that
                // falls through into the next one carries no `break`
                // (fallthrough is intentional and preserved); the trailing
                // arm needs none either — it falls off the end.
                if needs_break(arm, is_last) {
                    out.push_str(&pad);
                    out.push_str("    break;\n");
                }
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Statement::Unknown(text) => {
            out.push_str(&pad);
            out.push_str("// ");
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

/// Strip package prefixes from a type name: "moscow.xenon.Xenon" → "Xenon".
/// Generic-aware: each qualified segment inside `<...>`, after `&`/`, `/`?` etc.
/// is shortened too, so `java.util.List<java.lang.String>` → `List<String>`
/// (CFR/Vineflower print generics with short names + imports).
fn short_type(ty: &str) -> String {
    let mut out = String::with_capacity(ty.len());
    let mut run_start: Option<usize> = None;

    let bytes_len = ty.len();
    let flush = |out: &mut String, run: &str| match run.rsplit('.').next() {
        Some(short) => out.push_str(short),
        None => out.push_str(run),
    };

    let mut i = 0;
    while i < bytes_len {
        let c = ty[i..].chars().next().unwrap();
        let is_word = c.is_alphanumeric() || c == '_' || c == '.' || c == '$';
        if is_word {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(start) = run_start.take() {
            flush(&mut out, &ty[start..i]);
            out.push(c);
        } else {
            out.push(c);
        }
        i += c.len_utf8();
    }
    if let Some(start) = run_start {
        flush(&mut out, &ty[start..]);
    }
    out
}

fn render_expression(expr: &Expression) -> String {
    render_expression_at(expr, 0)
}

fn render_expression_at(expr: &Expression, indent: usize) -> String {
    let re = |e: &Expression| render_expression_at(e, indent);
    match expr {
        Expression::ConstInt(v) => v.to_string(),
        Expression::ConstLong(v) => format!("{v}L"),
        Expression::ConstFloat(v) => format!("{v}f"),
        Expression::ConstDouble(v) => format!("{v}d"),
        Expression::ConstString(s) => {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        Expression::ConstNull => "null".to_string(),
        Expression::This => "this".to_string(),
        Expression::Super => "super".to_string(),
        Expression::Local(name) => name.clone(),
        Expression::FieldAccess { object, field } => {
            format!("{}.{}", re(object), field)
        }
        Expression::Binary { left, op, right } => {
            let op_str = match op {
                BinaryOp::Add => "+",
                BinaryOp::Sub => "-",
                BinaryOp::Mul => "*",
                BinaryOp::Div => "/",
                BinaryOp::Rem => "%",
                BinaryOp::And => "&",
                BinaryOp::Or => "|",
                BinaryOp::Xor => "^",
                BinaryOp::Shl => "<<",
                BinaryOp::Shr => ">>",
                BinaryOp::Ushr => ">>>",
                BinaryOp::Lt => "<",
                BinaryOp::Le => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::Ge => ">=",
                BinaryOp::Eq => "==",
                BinaryOp::Ne => "!=",
                BinaryOp::BoolAnd => "&&",
                BinaryOp::BoolOr => "||",
            };
            let left_str = re(left);
            let right_str = re(right);
            // Parenthesize nested binaries (precedence safety) as well as
            // ternary/lambda operands; unary operands (`!x`, `-y`) never
            // need parens of their own.
            let needs_parens = |e: &Expression| {
                matches!(
                    e,
                    Expression::Binary { .. }
                        | Expression::Ternary { .. }
                        | Expression::Lambda { .. }
                )
            };
            let left_str = if needs_parens(left) {
                format!("({left_str})")
            } else {
                left_str
            };
            let right_str = if needs_parens(right) {
                format!("({right_str})")
            } else {
                right_str
            };
            format!("{left_str} {op_str} {right_str}")
        }
        Expression::Unary { op, operand } => {
            let op_str = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Not => "!",
                UnaryOp::BitNot => "~",
            };
            let rendered = re(operand);
            let rendered = if *op == UnaryOp::Not {
                match operand.as_ref() {
                    Expression::Binary {
                        left,
                        op: BinaryOp::Ne,
                        right,
                    } if matches!(right.as_ref(), Expression::ConstNull) => {
                        return format!("{} == null", re(left));
                    }
                    Expression::InstanceOf { .. } => {
                        format!("({rendered})")
                    }
                    Expression::Binary { .. } => format!("({rendered})"),
                    _ => rendered,
                }
            } else {
                if matches!(operand.as_ref(), Expression::Binary { .. }) {
                    format!("({rendered})")
                } else {
                    rendered
                }
            };
            format!("{op_str}{rendered}")
        }
        Expression::New { class, args } => {
            let rendered_args: Vec<String> = args.iter().map(&re).collect();
            format!("new {}({})", class, rendered_args.join(", "))
        }
        Expression::NewArray { element_type, size } => {
            // Multi-dim element (`float[]` from `[[F`) renders size-first:
            // `new float[n][]`, not `new float[][n]`.
            let mut brackets = 0usize;
            let mut base = element_type.as_str();
            while let Some(stripped) = base.strip_suffix("[]") {
                brackets += 1;
                base = stripped;
            }
            format!("new {base}[{}]{}", re(size), "[]".repeat(brackets))
        }
        Expression::Concat { builder, parts } => {
            // Unfolded escape hatch (normally consumed by `.toString()`).
            if parts.is_empty() {
                format!("new {builder}()")
            } else {
                let mut acc = re(&parts[0]);
                for part in &parts[1..] {
                    acc = format!("({acc} + {})", re(part));
                }
                format!("new {builder}({acc})")
            }
        }
        Expression::ArrayAccess { array, index } => {
            format!("{}[{}]", re(array), re(index))
        }
        Expression::Cast { target_type, expr } => {
            format!("({}) {}", target_type, re(expr))
        }
        Expression::InstanceOf { expr, target_type } => {
            format!("({} instanceof {})", re(expr), target_type)
        }
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            format!(
                "({} ? {} : {})",
                re(condition),
                re(then_expr),
                re(else_expr)
            )
        }
        Expression::Invoke { target, args } => {
            let rendered_args: Vec<String> = args.iter().map(&re).collect();
            format!("{}({})", target, rendered_args.join(", "))
        }
        Expression::Lambda { params, body } => {
            let param_strs: Vec<String> = params
                .iter()
                .map(|(ty, name)| format!("{} {}", short_type(ty), name))
                .collect();
            let param_list = param_strs.join(", ");
            let inner_indent = indent + 1;
            let pad = "    ".repeat(indent);
            if body.len() == 1 {
                if let Statement::Return(Some(expr)) = &body[0] {
                    format!("({param_list}) -> {}", re(expr))
                } else {
                    let body_strs: Vec<String> = body
                        .iter()
                        .map(|s| render_statement(s, inner_indent))
                        .collect();
                    format!("({param_list}) -> {{\n{}{}}}", body_strs.join(""), pad)
                }
            } else {
                let body_strs: Vec<String> = body
                    .iter()
                    .map(|s| render_statement(s, inner_indent))
                    .collect();
                format!("({param_list}) -> {{\n{}{}}}", body_strs.join(""), pad)
            }
        }
        Expression::MethodRef(class, method) => {
            if class.is_empty() {
                format!("this::{}", method)
            } else {
                format!("{}::{}", class, method)
            }
        }
        Expression::Unknown(text) => text.clone(),
    }
}

/// Public wrapper for render_expression, used by the lowering module.
pub fn render_expression_pub(expr: &Expression) -> String {
    render_expression(expr)
}

/// Public wrapper with indent control.
pub fn render_expression_at_pub(expr: &Expression, indent: usize) -> String {
    render_expression_at(expr, indent)
}

/// An arm needs an explicit `break` unless it falls through, diverges
/// (return/throw), is empty (shares the next arm's body), or is trailing.
fn needs_break(arm: &SwitchArm, is_last: bool) -> bool {
    if is_last || !arm.breaks {
        return false;
    }
    match arm.body.last() {
        None => false,
        Some(Statement::Return(_)) => false,
        Some(Statement::Expression(Expression::Unknown(text)))
            if text.trim_start().starts_with("throw ") =>
        {
            false
        }
        _ => true,
    }
}

/// Check if a method is a synthetic enum method that should be hidden
fn is_enum_synthetic_method(name: &str) -> bool {
    matches!(name, "$values" | "values" | "valueOf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_simple_class() {
        let class = ClassDecl {
            name: "Example".to_string(),
            package: Some("com.example".to_string()),
            imports: vec!["java.util.List".to_string(), "unused.Import".to_string()],
            fields: vec![],
            methods: vec![MethodDecl {
                name: "getItems".to_string(),
                statements: vec![Statement::Return(Some(Expression::New {
                    class: "List".to_string(),
                    args: vec![],
                }))],
                access_flags: 0x0001,
                return_type: "int".to_string(),
                param_types: vec![],
                param_names: vec![],
                class_name: "Example".to_string(),
            }],
            access_flags: 0x0001,
            super_name: Some("java.lang.Object".to_string()),
            is_enum: false,
            enum_constants: vec![],
        };

        let rendered = class.render();
        assert!(rendered.contains("package com.example;"));
        // List is used in body → import kept; unused.Import not used → filtered
        assert!(rendered.contains("import java.util.List;"));
        assert!(!rendered.contains("unused.Import"));
        assert!(rendered.contains("public class Example"));
        assert!(rendered.contains("new List()"));
    }

    #[test]
    fn shortens_generic_types_in_fields_and_extends() {
        let class = ClassDecl {
            name: "Repo".to_string(),
            package: None,
            imports: vec!["java.util.List".to_string(), "java.lang.Enum".to_string()],
            fields: vec![FieldDecl {
                name: "items".to_string(),
                field_type: "java.util.List<java.lang.String>".to_string(),
                access_flags: 0x0002,
                initial_value: None,
            }],
            methods: vec![],
            access_flags: 0x0001,
            super_name: Some("java.lang.Enum<com.example.Repo>".to_string()),
            is_enum: false,
            enum_constants: vec![],
        };

        let rendered = class.render();
        assert!(rendered.contains("private List<String> items;"));
        assert!(rendered.contains("extends Enum<Repo>"));
        // No fully-qualified names leak into the body (imports carry them).
        assert!(!rendered.contains("java.lang.String"));
        assert!(!rendered.contains("com.example.Repo"));
    }

    #[test]
    fn renders_switch_with_breaks_and_default() {
        let switch = Statement::Switch {
            discriminant: Expression::Local("mode".to_string()),
            arms: vec![
                SwitchArm {
                    keys: vec![1, 2],
                    is_default: false,
                    body: vec![Statement::Expression(Expression::Invoke {
                        target: "run".to_string(),
                        args: vec![],
                    })],
                    breaks: true,
                },
                SwitchArm {
                    keys: vec![],
                    is_default: true,
                    body: vec![Statement::Return(None)],
                    breaks: true,
                },
            ],
        };
        let class = ClassDecl {
            name: "S".to_string(),
            package: None,
            imports: vec![],
            fields: vec![],
            methods: vec![MethodDecl {
                name: "go".to_string(),
                statements: vec![switch],
                access_flags: 0x0001,
                return_type: "void".to_string(),
                param_types: vec![],
                param_names: vec![],
                class_name: "S".to_string(),
            }],
            access_flags: 0x0001,
            super_name: None,
            is_enum: false,
            enum_constants: vec![],
        };
        let rendered = class.render();
        assert!(rendered.contains("switch (mode) {"));
        assert!(rendered.contains("case 1, 2:"));
        assert!(rendered.contains("break;"));
        assert!(rendered.contains("default:"));
        assert!(rendered.contains("return;"));
    }
}
