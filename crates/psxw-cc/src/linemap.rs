//! Map physical lines of a preprocessed `.i` back to presumed source
//! locations using its `# N "file"` line markers.

pub struct LineMap {
    files: Vec<String>,
    /// Per physical line (0-based): (file index, presumed 1-based line).
    lines: Vec<(u32, u32)>,
    /// Byte offset of each physical line's start.
    starts: Vec<usize>,
}

impl LineMap {
    pub fn new(text: &str) -> Self {
        let mut files: Vec<String> = Vec::new();
        let mut lines = Vec::new();
        let mut starts = Vec::new();
        let (mut file, mut next) = (u32::MAX, 1u32);
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            starts.push(offset);
            offset += line.len();
            if let Some((n, f)) = marker(line) {
                lines.push((file, 0));
                let idx = files.iter().position(|x| x == f).unwrap_or_else(|| {
                    files.push(f.to_string());
                    files.len() - 1
                });
                file = idx as u32;
                next = n;
            } else {
                lines.push((file, next));
                next += 1;
            }
        }
        Self { files, lines, starts }
    }

    /// Presumed (file, line) of a 1-based physical line.
    pub fn presumed(&self, physical: u32) -> Option<(&str, u32)> {
        let &(f, l) = self.lines.get(physical.checked_sub(1)? as usize)?;
        Some((self.files.get(f as usize)?.as_str(), l))
    }

    /// Byte offset where a 1-based physical line starts.
    pub fn offset(&self, physical: u32) -> Option<usize> {
        self.starts.get(physical.checked_sub(1)? as usize).copied()
    }

    /// 1-based physical line containing a byte offset.
    pub fn line_of(&self, offset: usize) -> u32 {
        self.starts.partition_point(|&s| s <= offset) as u32
    }
}

/// Parse `# 12 "path" flags`.
fn marker(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix('#')?.trim_start();
    let rest = rest.strip_prefix("line").map_or(rest, str::trim_start);
    let (num, rest) = rest.split_once(char::is_whitespace)?;
    let n = num.parse().ok()?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((n, &rest[..end]))
}

/// Replace every line marker with spaces (offsets preserved) so a C parser
/// sees plain C.
pub fn blank_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if marker(line).is_some() {
            let body = line.trim_end_matches('\n');
            out.extend(std::iter::repeat_n(' ', body.len()));
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    out
}
