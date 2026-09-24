#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    RealJava,
    PartialJava,
    Pseudocode,
    BytecodeFallback,
    Fatal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recoverable<T> {
    Present(T),
    Missing,
    Malformed { reason: String },
}

impl<T> Recoverable<T> {
    pub fn is_present(&self) -> bool {
        matches!(self, Recoverable::Present(_))
    }

    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> Recoverable<U> {
        match self {
            Recoverable::Present(value) => Recoverable::Present(transform(value)),
            Recoverable::Missing => Recoverable::Missing,
            Recoverable::Malformed { reason } => Recoverable::Malformed { reason },
        }
    }
}
