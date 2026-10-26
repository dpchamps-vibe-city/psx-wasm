//! Diagnostics from clang (`file:line:col: severity: message [-Wflag]`) and
//! from GCC 2.95 cc1 (`file:line: message`, warnings prefixed `warning:`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub message: String,
    pub flag: Option<String>,
}

pub fn parse_clang(stderr: &str) -> Vec<Diag> {
    stderr.lines().filter_map(clang_line).collect()
}

fn clang_line(line: &str) -> Option<Diag> {
    let (sev_at, sev, sev_len) = ["fatal error: ", "error: ", "warning: ", "note: "]
        .iter()
        .find_map(|s| line.find(&format!(": {s}")).map(|i| (i, *s, s.len() + 2)))?;
    let loc = &line[..sev_at];
    let mut parts = loc.rsplitn(3, ':');
    let _col = parts.next()?.parse::<u32>().ok()?;
    let line_no = parts.next()?.parse::<u32>().ok()?;
    let file = parts.next()?.to_string();
    let mut message = line[sev_at + sev_len..].to_string();
    let mut flag = None;
    if message.ends_with(']') {
        if let Some(i) = message.rfind(" [-W") {
            let f = &message[i + 2..message.len() - 1];
            flag = Some(f.split(',').next().unwrap_or(f).to_string());
            message.truncate(i);
        }
    }
    let severity = match sev {
        "warning: " => Severity::Warning,
        "note: " => Severity::Note,
        _ => Severity::Error,
    };
    Some(Diag { file, line: line_no, severity, message, flag })
}

pub fn parse_gcc(stderr: &str) -> Vec<Diag> {
    stderr
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let file = parts.next()?.to_string();
            let line_no = parts.next()?.trim().parse::<u32>().ok()?;
            let rest = parts.next()?.trim();
            let (severity, message) = match rest.strip_prefix("warning:") {
                Some(m) => (Severity::Warning, m.trim().to_string()),
                None => (Severity::Error, rest.to_string()),
            };
            Some(Diag { file, line: line_no, severity, message, flag: None })
        })
        .collect()
}
