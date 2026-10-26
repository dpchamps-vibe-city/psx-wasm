use std::ops::Range;

/// A byte-range replacement on a text buffer. An empty range inserts.
#[derive(Debug, Clone)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self { range, text: text.into() }
    }
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self { range: at..at, text: text.into() }
    }
}

/// Apply edits. Where edits overlap, the outermost one wins (a blanked
/// function swallows the asm edits inside it).
pub fn apply(text: &str, mut edits: Vec<Edit>) -> String {
    edits.sort_by(|a, b| a.range.start.cmp(&b.range.start).then(b.range.end.cmp(&a.range.end)));
    let mut kept: Vec<Edit> = Vec::new();
    for e in edits {
        if let Some(last) = kept.last() {
            let nested = e.range.start < last.range.end
                || (e.range.start == last.range.start && !last.range.is_empty() && e.range.is_empty());
            if nested {
                continue;
            }
        }
        kept.push(e);
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for e in kept {
        out.push_str(&text[pos..e.range.start]);
        out.push_str(&e.text);
        pos = e.range.end;
    }
    out.push_str(&text[pos..]);
    out
}
