/// JSON value. Objects keep insertion order (Vec of pairs) so the preview matches the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Kept as source text to avoid precision loss on big ints/floats.
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}
