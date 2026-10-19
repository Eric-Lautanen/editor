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
    /// Text the parser could not describe, with what it said about it. Only
    /// the patch path builds one: an edit that leaves a document unparseable
    /// keeps its tree, and the node the edit landed in becomes this leaf
    /// instead of the whole document being re-parsed. A full parse never
    /// produces it, so the tree a file opens with cannot contain one.
    Broken(String),
}

impl Value {
    /// How many children a container holds (0 for a scalar).
    pub fn child_count(&self) -> usize {
        match self {
            Value::Array(items) => items.len(),
            Value::Object(pairs) => pairs.len(),
            _ => 0,
        }
    }

    /// The node a path of child ordinals points at — an array's index, an
    /// object's member position — or `None` if the path leaves the tree.
    pub fn get_at(&self, path: &[usize]) -> Option<&Value> {
        let mut at = self;
        for &ord in path {
            at = match at {
                Value::Array(items) => items.get(ord)?,
                Value::Object(pairs) => &pairs.get(ord)?.1,
                _ => return None,
            };
        }
        Some(at)
    }

    /// [`Value::get_at`], mutable: this is how an edit's re-parse is put back in
    /// place of the node it replaced without rebuilding the tree around it.
    pub fn get_mut_at(&mut self, path: &[usize]) -> Option<&mut Value> {
        let mut at = self;
        for &ord in path {
            at = match at {
                Value::Array(items) => items.get_mut(ord)?,
                Value::Object(pairs) => &mut pairs.get_mut(ord)?.1,
                _ => return None,
            };
        }
        Some(at)
    }
}
