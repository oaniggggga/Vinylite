/// Parsed generic type signature.
#[derive(Debug, Clone, PartialEq)]
pub enum GenericSignature {
    /// Simple class reference: `Ljava/lang/String;`
    Class {
        name: String,
        args: Vec<GenericSignature>,
    },
    /// Type variable: `TT;`
    TypeVar(String),
    /// Array type: `[I`
    Array(Box<GenericSignature>),
    /// Primitive type: `I`, `J`, etc.
    Primitive(String),
    /// Wildcard: `*`
    Wildcard,
    /// Extends bound: `+TT;`
    Extends(Box<GenericSignature>),
    /// Super bound: `-TT;`
    Super(Box<GenericSignature>),
}

/// Parsed method signature.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSignature {
    pub type_params: Vec<TypeParam>,
    pub params: Vec<GenericSignature>,
    pub return_type: GenericSignature,
    pub throws: Vec<GenericSignature>,
}

/// Parsed class signature.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassSignature {
    pub type_params: Vec<TypeParam>,
    pub super_class: GenericSignature,
    pub interfaces: Vec<GenericSignature>,
}

/// Type parameter with optional bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<GenericSignature>,
    pub interface_bounds: Vec<GenericSignature>,
}

/// Parse a JVM generic signature string.
pub fn parse_signature(sig: &str) -> Option<GenericSignature> {
    let mut parser = SignatureParser::new(sig);
    parser.parse_type()
}

/// Parse a method signature.
pub fn parse_method_signature(sig: &str) -> Option<MethodSignature> {
    let mut parser = SignatureParser::new(sig);
    parser.parse_method_sig()
}

/// Parse a class signature.
pub fn parse_class_signature(sig: &str) -> Option<ClassSignature> {
    let mut parser = SignatureParser::new(sig);
    parser.parse_class_sig()
}

struct SignatureParser {
    bytes: Vec<u8>,
    pos: usize,
}

impl SignatureParser {
    fn new(sig: &str) -> Self {
        Self {
            bytes: sig.bytes().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.pos).copied();
        self.pos += 1;
        b
    }

    fn expect(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn parse_type(&mut self) -> Option<GenericSignature> {
        match self.peek()? {
            b'B' => {
                self.advance();
                Some(GenericSignature::Primitive("byte".into()))
            }
            b'C' => {
                self.advance();
                Some(GenericSignature::Primitive("char".into()))
            }
            b'D' => {
                self.advance();
                Some(GenericSignature::Primitive("double".into()))
            }
            b'F' => {
                self.advance();
                Some(GenericSignature::Primitive("float".into()))
            }
            b'I' => {
                self.advance();
                Some(GenericSignature::Primitive("int".into()))
            }
            b'J' => {
                self.advance();
                Some(GenericSignature::Primitive("long".into()))
            }
            b'S' => {
                self.advance();
                Some(GenericSignature::Primitive("short".into()))
            }
            b'Z' => {
                self.advance();
                Some(GenericSignature::Primitive("boolean".into()))
            }
            b'V' => {
                self.advance();
                Some(GenericSignature::Primitive("void".into()))
            }
            b'L' => self.parse_class_type(),
            b'T' => self.parse_type_variable(),
            b'[' => {
                self.advance();
                let inner = self.parse_type()?;
                Some(GenericSignature::Array(Box::new(inner)))
            }
            b'*' => {
                self.advance();
                Some(GenericSignature::Wildcard)
            }
            b'+' => {
                self.advance();
                let inner = self.parse_type()?;
                Some(GenericSignature::Extends(Box::new(inner)))
            }
            b'-' => {
                self.advance();
                let inner = self.parse_type()?;
                Some(GenericSignature::Super(Box::new(inner)))
            }
            _ => None,
        }
    }

    fn parse_class_type(&mut self) -> Option<GenericSignature> {
        self.expect(b'L');
        let mut name = String::new();

        while let Some(b) = self.peek() {
            match b {
                b';' | b'<' => break,
                b'/' => {
                    self.advance();
                    name.push('.');
                }
                _ => {
                    self.advance();
                    name.push(b as char);
                }
            }
        }

        let mut args = Vec::new();
        if self.peek() == Some(b'<') {
            self.advance();
            while self.peek() != Some(b'>') && self.peek().is_some() {
                if let Some(arg) = self.parse_type() {
                    args.push(arg);
                } else {
                    break;
                }
            }
            self.expect(b'>');
        }

        self.expect(b';');

        Some(GenericSignature::Class { name, args })
    }

    fn parse_type_variable(&mut self) -> Option<GenericSignature> {
        self.expect(b'T');
        let mut name = String::new();

        while let Some(b) = self.peek() {
            if b == b';' {
                self.advance();
                break;
            }
            name.push(b as char);
            self.advance();
        }

        Some(GenericSignature::TypeVar(name))
    }

    fn parse_method_sig(&mut self) -> Option<MethodSignature> {
        let mut type_params = Vec::new();

        if self.peek() == Some(b'<') {
            self.advance();
            while self.peek() != Some(b'>') && self.peek().is_some() {
                if let Some(tp) = self.parse_type_param() {
                    type_params.push(tp);
                } else {
                    break;
                }
            }
            self.expect(b'>');
        }

        self.expect(b'(');
        let mut params = Vec::new();
        while self.peek() != Some(b')') && self.peek().is_some() {
            if let Some(param) = self.parse_type() {
                params.push(param);
            } else {
                break;
            }
        }
        self.expect(b')');

        let return_type = self.parse_type()?;

        let mut throws = Vec::new();
        while self.peek() == Some(b'^') {
            self.advance();
            if let Some(exc) = self.parse_type() {
                throws.push(exc);
            }
        }

        Some(MethodSignature {
            type_params,
            params,
            return_type,
            throws,
        })
    }

    fn parse_class_sig(&mut self) -> Option<ClassSignature> {
        let mut type_params = Vec::new();

        if self.peek() == Some(b'<') {
            self.advance();
            while self.peek() != Some(b'>') && self.peek().is_some() {
                if let Some(tp) = self.parse_type_param() {
                    type_params.push(tp);
                } else {
                    break;
                }
            }
            self.expect(b'>');
        }

        let super_class = self.parse_type()?;

        let mut interfaces = Vec::new();
        while self.peek() == Some(b'L') || self.peek() == Some(b'T') {
            if let Some(iface) = self.parse_type() {
                interfaces.push(iface);
            } else {
                break;
            }
        }

        Some(ClassSignature {
            type_params,
            super_class,
            interfaces,
        })
    }

    fn parse_type_param(&mut self) -> Option<TypeParam> {
        let mut name = String::new();
        while let Some(b) = self.peek() {
            if b == b':' || b == b'>' || b == b'<' {
                break;
            }
            name.push(b as char);
            self.advance();
        }

        let mut bound = None;
        let mut interface_bounds = Vec::new();

        if self.peek() == Some(b':') {
            self.advance();
            // First ':' is the class bound
            if self.peek() != Some(b':') {
                bound = self.parse_type();
            }

            // Additional ':' are interface bounds
            while self.peek() == Some(b':') {
                self.advance();
                if let Some(iface) = self.parse_type() {
                    interface_bounds.push(iface);
                }
            }
        }

        Some(TypeParam {
            name,
            bound,
            interface_bounds,
        })
    }
}

/// Format a generic signature as a Java source string.
pub fn format_generic(sig: &GenericSignature) -> String {
    match sig {
        GenericSignature::Class { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                let args_str: Vec<String> = args.iter().map(format_generic).collect();
                format!("{}<{}>", name, args_str.join(", "))
            }
        }
        GenericSignature::TypeVar(name) => name.clone(),
        GenericSignature::Array(inner) => format!("{}[]", format_generic(inner)),
        GenericSignature::Primitive(name) => name.clone(),
        GenericSignature::Wildcard => "?".to_string(),
        GenericSignature::Extends(inner) => format!("? extends {}", format_generic(inner)),
        GenericSignature::Super(inner) => format!("? super {}", format_generic(inner)),
    }
}

/// Format a method signature as Java source.
pub fn format_method_sig(sig: &MethodSignature) -> (String, Vec<String>, String) {
    let type_params = if sig.type_params.is_empty() {
        String::new()
    } else {
        let params: Vec<String> = sig
            .type_params
            .iter()
            .map(|tp| {
                let mut s = tp.name.clone();
                if let Some(ref bound) = tp.bound {
                    s.push_str(&format!(" extends {}", format_generic(bound)));
                }
                for ib in &tp.interface_bounds {
                    s.push_str(&format!(" & {}", format_generic(ib)));
                }
                s
            })
            .collect();
        format!("<{}>", params.join(", "))
    };

    let param_types: Vec<String> = sig.params.iter().map(format_generic).collect();
    let return_type = format_generic(&sig.return_type);

    (type_params, param_types, return_type)
}

/// Format a class signature as Java source.
pub fn format_class_sig(sig: &ClassSignature) -> (String, String, Vec<String>) {    let type_params = if sig.type_params.is_empty() {
        String::new()
    } else {
        let params: Vec<String> = sig
            .type_params
            .iter()
            .map(|tp| {
                let mut s = tp.name.clone();
                if let Some(ref bound) = tp.bound {
                    s.push_str(&format!(" extends {}", format_generic(bound)));
                }
                for ib in &tp.interface_bounds {
                    s.push_str(&format!(" & {}", format_generic(ib)));
                }
                s
            })
            .collect();
        format!("<{}>", params.join(", "))
    };

    let super_class = format_generic(&sig.super_class);
    let interfaces: Vec<String> = sig.interfaces.iter().map(format_generic).collect();

    (type_params, super_class, interfaces)
}

/// Resolve method parameter/return types, preferring the generic `Signature`
/// when present (CFR/Vineflower show `List<String>`, not raw `List`).
/// Falls back to the erased descriptor on any parse failure.
pub fn generic_method_types(descriptor: &str, signature: Option<&str>) -> (Vec<String>, String) {
    if let Some(sig) = signature
        && let Some(parsed) = parse_method_signature(sig)
    {
        let (_, params, ret) = format_method_sig(&parsed);
        // Guard against arity mismatch (malformed Signature attribute).
        let (erased_params, erased_ret) = crate::descriptor::parse_descriptor(descriptor);
        if params.len() == erased_params.len() {
            return (params, ret);
        }
        return (erased_params, erased_ret);
    }
    crate::descriptor::parse_descriptor(descriptor)
}

/// Resolve a field type, preferring the generic `Signature` when present.
pub fn generic_field_type(descriptor: &str, signature: Option<&str>) -> String {
    if let Some(sig) = signature
        && let Some(parsed) = parse_signature(sig)
    {
        return format_generic(&parsed);
    }
    crate::descriptor::parse_field_descriptor(descriptor)
}

/// Resolve the superclass display name, preferring the generic class
/// `Signature` (e.g. `Enum<Xenon>`) when present.
pub fn generic_super_name(erased_dotted: Option<String>, class_signature: Option<&str>) -> Option<String> {
    if let Some(sig) = class_signature
        && let Some(parsed) = parse_class_signature(sig)
    {
        let (_, super_name, _) = format_class_sig(&parsed);
        if !super_name.is_empty() {
            return Some(super_name);
        }
    }
    erased_dotted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_class_signature() {
        let sig = "Ljava/lang/Object;";
        let result = parse_signature(sig).unwrap();
        assert_eq!(
            result,
            GenericSignature::Class {
                name: "java.lang.Object".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn parses_generic_class() {
        let sig = "Ljava/util/List<Ljava/lang/String;>;";
        let result = parse_signature(sig).unwrap();
        match result {
            GenericSignature::Class { name, args } => {
                assert_eq!(name, "java.util.List");
                assert_eq!(args.len(), 1);
                match &args[0] {
                    GenericSignature::Class { name: arg_name, .. } => {
                        assert_eq!(arg_name, "java.lang.String");
                    }
                    _ => panic!("expected class"),
                }
            }
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn parses_method_signature() {
        let sig = "(Ljava/lang/String;)V";
        let result = parse_method_signature(sig).unwrap();
        assert_eq!(result.params.len(), 1);
        assert_eq!(
            result.return_type,
            GenericSignature::Primitive("void".into())
        );
    }

    #[test]
    fn parses_type_variable() {
        let sig = "TT;";
        let result = parse_signature(sig).unwrap();
        assert_eq!(result, GenericSignature::TypeVar("T".into()));
    }

    #[test]
    fn generic_method_types_prefers_signature() {
        let (params, ret) = generic_method_types(
            "(Ljava/util/List;)Ljava/util/List;",
            Some("(Ljava/util/List<Ljava/lang/String;>;)Ljava/util/List<Ljava/lang/String;>;"),
        );
        assert_eq!(params, vec!["java.util.List<java.lang.String>"]);
        assert_eq!(ret, "java.util.List<java.lang.String>");
    }

    #[test]
    fn generic_method_types_falls_back_on_arity_mismatch() {
        let (params, ret) = generic_method_types("(I)V", Some("(Ljava/lang/String;I)V"));
        assert_eq!(params, vec!["int"]);
        assert_eq!(ret, "void");
    }

    #[test]
    fn generic_field_type_prefers_signature() {
        let ty = generic_field_type(
            "Ljava/util/List;",
            Some("Ljava/util/List<Ljava/lang/String;>;"),
        );
        assert_eq!(ty, "java.util.List<java.lang.String>");
    }

    #[test]
    fn generic_super_name_prefers_class_signature() {
        let name = generic_super_name(
            Some("java.lang.Enum".to_string()),
            Some("Ljava/lang/Enum<Lmoscow/xenon/Xenon;>;"),
        );
        assert_eq!(name, Some("java.lang.Enum<moscow.xenon.Xenon>".to_string()));
    }
}
