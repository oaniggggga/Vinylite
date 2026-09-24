use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
    Fatal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub offset: usize,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(offset: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            offset,
            message: message.into(),
        }
    }

    pub fn error(offset: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            offset,
            message: message.into(),
        }
    }

    pub fn fatal(offset: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fatal,
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Warning => formatter.write_str("warning"),
            Severity::Error => formatter.write_str("error"),
            Severity::Fatal => formatter.write_str("fatal"),
        }
    }
}
