/// Parse a JVM method descriptor into (param_types, return_type) as dot-separated names.
pub fn parse_descriptor(descriptor: &str) -> (Vec<String>, String) {
    let bytes = descriptor.as_bytes();
    let mut pos = 0;
    let mut params = Vec::new();

    // expect '('
    if pos >= bytes.len() || bytes[pos] != b'(' {
        return (params, descriptor.to_string());
    }
    pos += 1;

    // parse params until ')'
    while pos < bytes.len() && bytes[pos] != b')' {
        let (ty, new_pos) = parse_type(bytes, pos);
        params.push(ty);
        pos = new_pos;
    }

    // skip ')'
    if pos < bytes.len() {
        pos += 1;
    }

    // parse return type
    let ret = if pos < bytes.len() {
        let (ty, _) = parse_type(bytes, pos);
        ty
    } else {
        "void".to_string()
    };

    (params, ret)
}

/// Parse a JVM field descriptor into a dot-separated Java type.
pub fn parse_field_descriptor(descriptor: &str) -> String {
    if descriptor.is_empty() {
        return descriptor.to_string();
    }

    let (ty, _) = parse_type(descriptor.as_bytes(), 0);
    ty
}

fn parse_type(bytes: &[u8], pos: usize) -> (String, usize) {
    match bytes[pos] {
        b'V' => ("void".to_string(), pos + 1),
        b'I' => ("int".to_string(), pos + 1),
        b'J' => ("long".to_string(), pos + 1),
        b'F' => ("float".to_string(), pos + 1),
        b'D' => ("double".to_string(), pos + 1),
        b'B' => ("byte".to_string(), pos + 1),
        b'C' => ("char".to_string(), pos + 1),
        b'S' => ("short".to_string(), pos + 1),
        b'Z' => ("boolean".to_string(), pos + 1),
        b'L' => {
            // object type: Lpackage/ClassName;
            let start = pos + 1;
            let end = bytes[start..]
                .iter()
                .position(|&b| b == b';')
                .map(|i| start + i)
                .unwrap_or(bytes.len());
            let name = String::from_utf8_lossy(&bytes[start..end]).replace('/', ".");
            (name, end + 1)
        }
        b'[' => {
            // array type
            let (inner, next) = parse_type(bytes, pos + 1);
            (format!("{inner}[]"), next)
        }
        _ => (format!("unknown@{pos}"), pos + 1),
    }
}

/// Returns true if the descriptor represents a void method.
pub fn is_void_descriptor(descriptor: &str) -> bool {
    descriptor.ends_with(")V")
}

/// Count the number of parameters in a method descriptor without parsing types.
pub fn count_params(descriptor: &str) -> usize {
    let (params, _) = parse_descriptor(descriptor);
    params.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_method_descriptor() {
        let (params, ret) = parse_descriptor("(IILjava/lang/String;)V");
        assert_eq!(params, vec!["int", "int", "java.lang.String"]);
        assert_eq!(ret, "void");
    }

    #[test]
    fn parses_return_object() {
        let (params, ret) = parse_descriptor("()Ljava/lang/Object;");
        assert!(params.is_empty());
        assert_eq!(ret, "java.lang.Object");
    }

    #[test]
    fn parses_array_params() {
        let (params, ret) = parse_descriptor("([B[I)V");
        assert_eq!(params, vec!["byte[]", "int[]"]);
        assert_eq!(ret, "void");
    }
}
