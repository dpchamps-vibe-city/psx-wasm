//! Generated files (plans/M11 "Generated files"): each mod's `mod.h`, the
//! mod library's `psxmod.h`, and `compile_commands.json`.

use crate::decl::{Kind, Mod};
use crate::headers::DeclParam;
use crate::plan::{Plan, TargetType};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const PSXMOD_H: &str = r#"/* psxmod.h: the mod library (psx-wasm). */
#pragma once

/* Log a line, printf-style (%d %i %u %x %X %o %c %s %p %%, with flags,
 * width and precision). A run's log shows it as "mod: <line>"; a test's
 * `log` expectation matches it. */
void psxmod_log(const char *fmt, ...);
"#;

/// A declaration of `name` with type `ty` (a type name such as `u8 *` or
/// `void (*)(int)`).
pub fn spell(ty: &str, name: &str) -> String {
    let ty = ty.trim();
    if ty.contains('(') || ty.contains('[') {
        format!("__typeof__({ty}) {name}")
    } else if ty.ends_with('*') {
        format!("{ty}{name}")
    } else {
        format!("{ty} {name}")
    }
}

fn param_list(params: &[DeclParam], variadic: bool, lead: Option<String>) -> String {
    let mut v: Vec<String> = lead.into_iter().collect();
    for (i, p) in params.iter().enumerate() {
        v.push(match &p.name {
            Some(_) => p.text.clone(),
            None => spell(&p.ty, &format!("p{i}")),
        });
    }
    if variadic {
        v.push("...".into());
    }
    if v.is_empty() { "void".into() } else { v.join(", ") }
}

/// The macro a mod function's prototype starts with: `extern` unless the
/// build defines it (`static __attribute__((used))` for a `static` one).
pub fn linkage_macro(function: &str) -> String {
    format!("PSXW_LINKAGE_{function}")
}

/// `mod.h` for mod `mi` of `mods`. `prelude`: the game's prelude headers.
pub fn mod_h(plan: &Plan, mods: &[Mod], mi: usize, prelude: &[String]) -> String {
    let m = &mods[mi];
    let mut includes: Vec<String> = Vec::new();
    let mut add = |h: &str| {
        if !includes.iter().any(|x| x == h) {
            includes.push(h.to_string());
        }
    };
    for p in prelude {
        add(p);
    }
    let types: Vec<(usize, &TargetType)> = plan.types.iter().filter(|((k, _), _)| *k == mi).map(|((_, h), t)| (*h, t)).collect();
    let mut declares: Vec<String> = Vec::new();
    for (_, t) in &types {
        match &t.header {
            Some(d) => {
                for c in &d.context {
                    add(c);
                }
                add(&d.header);
            }
            None => {
                let decl = format!("{}({});", spell(&t.returns, &t.cname), param_list(&t.params, t.variadic, None));
                if !declares.contains(&decl) {
                    declares.push(decl);
                }
            }
        }
    }
    let mut out = String::new();
    let _ = writeln!(out, "/* mod.h: generated from {}/mod.json by psxw mod build. Do not edit. */", m.id());
    let _ = writeln!(out, "#pragma once");
    for h in &includes {
        let why = types
            .iter()
            .filter_map(|(_, t)| t.header.as_ref().filter(|d| d.header == *h).map(|_| t.cname.as_str()))
            .collect::<BTreeSet<_>>();
        if why.is_empty() {
            let _ = writeln!(out, "#include \"{h}\"");
        } else {
            let _ = writeln!(out, "#include \"{h}\" /* declares {} */", why.into_iter().collect::<Vec<_>>().join(", "));
        }
    }
    let _ = writeln!(out, "#include \"psxmod.h\"");
    if !declares.is_empty() {
        let _ = writeln!(out, "\n/* Declared by the hooks' signatures. */");
        for d in &declares {
            let _ = writeln!(out, "{d}");
        }
    }
    let mut functions: BTreeSet<String> = BTreeSet::new();
    let mut protos = String::new();
    for (hi, t) in &types {
        let h = &m.decl.hooks[*hi];
        let f = &h.imp.function;
        functions.insert(f.clone());
        let point = plan.resolved.get(&(mi, *hi)).and_then(|(r, _)| r.point.clone());
        let from = h.from.as_ref().map(|c| format!(" from {c}")).unwrap_or_default();
        let _ = writeln!(protos, "\n/* {} {}{from} <- {}: {f} */", h.kind, h.target, h.imp.file);
        let link = linkage_macro(f);
        match (h.kind, point) {
            (_, Some(_)) => {
                let _ = writeln!(protos, "{link} void {f}(void);");
            }
            (Kind::Patch, None) => {
                let _ = writeln!(protos, "{link} __typeof__({}) {f};", t.cname);
                if let Some(o) = &h.original {
                    let _ = writeln!(protos, "extern __typeof__({}) {o}; /* the next link in the chain */", t.cname);
                }
            }
            (Kind::Before, None) => {
                let _ = writeln!(protos, "{link} void {f}({});", param_list(&t.params, t.variadic, None));
            }
            (Kind::After, None) => {
                let lead = (t.returns != "void").then(|| {
                    let taken = t.params.iter().any(|p| p.name.as_deref() == Some("result"));
                    spell(&t.returns, if taken { "result_" } else { "result" })
                });
                let _ = writeln!(protos, "{link} void {f}({});", param_list(&t.params, t.variadic, lead));
            }
        }
    }
    // fnptr data patches name mod functions too.
    for d in &m.decl.data {
        if let Ok(crate::decl::DataValue::FnPtr(imp)) = d.value() {
            functions.insert(imp.function.clone());
        }
    }
    let _ = writeln!(out, "\n/* Each implementation's linkage: `extern`, or `static` where its file");
    let _ = writeln!(out, " * defines it so (the build defines the macro). */");
    let _ = writeln!(out, "#ifndef PSXW_SCAN");
    for f in &functions {
        let l = linkage_macro(f);
        let _ = writeln!(out, "#ifndef {l}\n#define {l} extern\n#endif");
    }
    out.push_str(&protos);
    let _ = writeln!(out, "#endif");
    out
}

/// How mod sources compile: the game's include directories and defines,
/// the mod's own include directory.
#[derive(Debug, Clone)]
pub struct Flags {
    pub clang: PathBuf,
    /// The pinned clang's prelude, force-included.
    pub prelude: PathBuf,
    pub includes: Vec<PathBuf>,
    pub defines: Vec<String>,
}

pub const STD: &str = "-std=gnu99";

impl Flags {
    /// clang arguments for one mod source (as `compile_commands.json` and
    /// the build use them).
    pub fn arguments(&self, mod_include: &Path, defines: &[String], src: &Path) -> Vec<String> {
        let mut a = vec![
            self.clang.display().to_string(),
            "--no-default-config".into(),
            "--target=wasm32-unknown-unknown".into(),
            "-nostdinc".into(),
            STD.into(),
            "-ffreestanding".into(),
            "-funsigned-char".into(),
            "-include".into(),
            self.prelude.display().to_string(),
        ];
        for i in &self.includes {
            a.push(format!("-I{}", i.display()));
        }
        a.push(format!("-I{}", mod_include.display()));
        for d in self.defines.iter().chain(defines) {
            a.push(format!("-D{d}"));
        }
        a.extend(["-c".into(), src.display().to_string()]);
        a
    }
}

/// `compile_commands.json` entries for a mod's sources.
pub fn compile_commands(flags: &Flags, m: &Mod, mod_include: &Path, defines: &dyn Fn(&Path) -> Vec<String>) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for src in m.sources()? {
        let src = std::fs::canonicalize(&src).unwrap_or(src);
        out.push(serde_json::json!({
            "directory": m.dir.display().to_string(),
            "file": src.display().to_string(),
            "arguments": flags.arguments(mod_include, &defines(&src), &src),
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::spell;

    #[test]
    fn spelling() {
        assert_eq!(spell("s32", "limit"), "s32 limit");
        assert_eq!(spell("u8 *", "p"), "u8 *p");
        assert_eq!(spell("void (*)(int)", "cb"), "__typeof__(void (*)(int)) cb");
    }
}
