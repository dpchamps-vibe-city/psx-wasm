//! Rust mods (plans/M11 task 8): the game's declarations as Rust, by
//! bindgen (with the pinned toolchain's libclang) over the mod's
//! `mod_flat.h` for wasm32, and `mod.rs`, the Rust counterpart of `mod.h`:
//! each implementation's type checked against its hook's, and each
//! `original` declared. A Rust mod includes it:
//! `include!(concat!(env!("PSXW_MOD_DIR"), "/mod.rs"));`.

use crate::decl::{Kind, Mod};
use crate::genfiles::Flags;
use crate::plan::Plan;
use anyhow::{Context as _, Result};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

/// A Rust mod: one with a `Cargo.toml`.
pub fn is_rust(m: &Mod) -> bool {
    m.decl.wasm.is_some() && m.dir.join("Cargo.toml").is_file()
}

/// The declaration of function `name` in bindgen's output, one line.
fn declaration(bindings: &str, name: &str) -> Option<String> {
    let at = bindings.find(&format!("pub fn {name}("))?;
    let end = at + bindings[at..].find(';')?;
    Some(bindings[at..=end].split_whitespace().collect::<Vec<_>>().join(" "))
}

/// C keeps struct tags apart from typedef names; Rust doesn't. An anonymous
/// `typedef struct { … } X;` where some `struct X` also exists gets a tag
/// of its own (`struct __psxw_anon_X`), so bindgen names the two apart.
fn untagged_collisions(text: &str) -> String {
    let tree = psxw_cc::ast::parse(text);
    let root = tree.tree.root_node();
    let tags: BTreeSet<String> = tree.records().into_iter().filter_map(|r| r.ty.strip_prefix("struct ").map(str::to_string)).collect();
    let mut edits = Vec::new();
    let mut c = root.walk();
    for node in root.named_children(&mut c) {
        if node.kind() != "type_definition" {
            continue;
        }
        let Some(ty) = node.child_by_field_name("type").filter(|t| t.kind() == "struct_specifier" && t.child_by_field_name("name").is_none()) else { continue };
        let Some(name) = node.child_by_field_name("declarator").and_then(|d| tree.declarator_name(d)) else { continue };
        if tags.contains(&name) {
            // After the `struct` keyword.
            edits.push(psxw_cc::edit::Edit::insert(ty.start_byte() + "struct".len(), format!(" __psxw_anon_{name}")));
        }
    }
    psxw_cc::edit::apply(text, edits)
}

/// bindgen's `pub type psxwfn_F = Option<unsafe extern "C" fn(…) -> R>;`
/// made back into `pub fn F(…) -> R;`, in one extern block.
fn declare_functions(raw: &str) -> String {
    let mut out = String::new();
    let mut decls = String::new();
    let mut rest = raw;
    const KEY: &str = "pub type psxwfn_";
    while let Some(at) = rest.find(KEY) {
        out.push_str(&rest[..at]);
        let tail = &rest[at + KEY.len()..];
        // The item's `;`: outside brackets (array types hold one).
        let mut depth = 0i32;
        let end = tail.char_indices().find_map(|(i, c)| {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ';' if depth == 0 => return Some(i),
                _ => {}
            }
            None
        });
        let Some(end) = end else { break };
        let item = tail[..end].split_whitespace().collect::<Vec<_>>().join(" ");
        rest = &tail[end + 1..];
        let Some((name, ty)) = item.split_once(" = ") else { continue };
        let ty = ty.trim();
        let inner = ty
            .strip_prefix(":: std :: option :: Option <")
            .or_else(|| ty.strip_prefix("::std::option::Option<"))
            .or_else(|| ty.strip_prefix("core::option::Option<"))
            .or_else(|| ty.strip_prefix("::core::option::Option<"))
            .and_then(|t| t.trim_end().strip_suffix('>'))
            // A long type is formatted with a trailing comma.
            .map(|t| t.trim().trim_end_matches(',').trim());
        let Some(sig) = inner.and_then(|t| t.strip_prefix("unsafe extern \"C\" fn")) else { continue };
        decls.push_str(&format!("    pub fn {name}{};\n", sig.trim()));
    }
    out.push_str(rest);
    if !decls.is_empty() {
        out.push_str(&format!("unsafe extern \"C\" {{\n{decls}}}\n"));
    }
    out
}

/// Write `game.rs` and `mod.rs` into `<mod_include>/`.
pub fn generate(flags: &Flags, plan: &Plan, mods: &[Mod], mi: usize, mod_include: &Path) -> Result<()> {
    let m = &mods[mi];
    let flat = std::fs::read_to_string(mod_include.join("mod_flat.h")).context("mod_flat.h")?;
    // The implementations and originals are the mod's to define.
    let mut own: BTreeSet<String> = BTreeSet::new();
    for h in &m.decl.hooks {
        own.insert(h.imp.function.clone());
        if let Some(o) = &h.original {
            own.insert(o.clone());
        }
    }
    // bindgen finds libclang through LIBCLANG_PATH: the pinned toolchain's.
    let lib = flags.clang.parent().and_then(Path::parent).context("clang's toolchain")?.join("lib");
    // SAFETY: set before bindgen runs, and nothing else reads it.
    unsafe { std::env::set_var("LIBCLANG_PATH", &lib) };
    // bindgen (0.73, with libclang 23) drops function declarations but
    // keeps function pointer types: declare each function's as a typedef
    // and make it a declaration again below.
    let mut text = untagged_collisions(&flat);
    for (f, t) in crate::headers::function_typedefs(&flat, "psxwfn_") {
        if !own.contains(&f) && !f.starts_with("__psxw") && !f.starts_with("psx_gte_") && f != "psxmod_log" {
            text += &format!("{t}\n");
        }
    }
    let mut b = bindgen::Builder::default()
        .header_contents("mod_flat.h", &text)
        .c_naming(true)
        .clang_args(["--target=wasm32-unknown-unknown", "-nostdinc", "-ffreestanding", "-fno-builtin", "-std=gnu99"])
        .use_core()
        .ctypes_prefix("core::ffi")
        .layout_tests(false)
        .generate_comments(false)
        .blocklist_item("__psxw.*")
        .blocklist_item("psx_gte_.*")
        .blocklist_item("PSXW_.*");
    for f in &own {
        b = b.blocklist_function(f);
    }
    let raw = b.generate().map_err(|e| anyhow::anyhow!("{}: bindgen over mod_flat.h: {e}", m.id()))?.to_string();
    let bindings = declare_functions(&raw);
    crate::build::write_if_changed(&mod_include.join("game.rs"), &bindings)?;

    let mut out = String::new();
    let _ = writeln!(out, "// mod.rs: generated from {}/mod.json by psxw mod build. Do not edit.", m.id());
    let _ = writeln!(out, "// The Rust counterpart of mod.h: the game's declarations (game.rs, by");
    let _ = writeln!(out, "// bindgen), each implementation's type checked against its hook's, and");
    let _ = writeln!(out, "// each `original`.");
    let _ = writeln!(out, "#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, unused_imports, clippy::all)]");
    let _ = writeln!(out, "pub mod game {{\n    include!(\"game.rs\");\n}}\n");
    let mut arities: BTreeSet<(usize, &'static str)> = BTreeSet::new();
    let mut checks = String::new();
    let mut originals = String::new();
    for ((k, hi), t) in plan.types.iter().filter(|((k, _), _)| *k == mi) {
        let _ = k;
        let h = &m.decl.hooks[*hi];
        let f = &h.imp.function;
        let n = t.params.len();
        let point = plan.resolved.get(&(mi, *hi)).is_some_and(|(r, _)| r.point.is_some());
        let target = &t.cname;
        let _ = writeln!(checks, "    // {} {} <- {}: {f}", h.kind, h.target, h.imp.file);
        if t.variadic {
            let _ = writeln!(checks, "    // (variadic: Rust can't define it; not checked)");
            continue;
        }
        let void = t.returns == "void";
        match (h.kind, point) {
            (_, true) => {
                let _ = writeln!(checks, "    let _: unsafe extern \"C\" fn() = crate::{f};");
            }
            (Kind::Patch, false) => {
                let ps = vec!["_"; n].join(", ");
                let ret = if void { String::new() } else { " -> _".into() };
                let _ = writeln!(checks, "    let _: [unsafe extern \"C\" fn({ps}){ret}; 2] = [game::{target}, crate::{f}];");
                if let Some(o) = &h.original {
                    let d = declaration(&bindings, target).with_context(|| format!("{}: bindgen declared no {target}", m.id()))?;
                    let d = d.replacen(&format!("pub fn {target}("), &format!("pub fn {o}("), 1);
                    let _ = writeln!(originals, "    /// The next link in {target}'s chain.\n    #[link_name = \"{o}\"]\n    {d}");
                }
            }
            (Kind::Before, false) => {
                arities.insert((n, "before"));
                let _ = writeln!(checks, "    before{n}(game::{target}, crate::{f});");
            }
            (Kind::After, false) => {
                let which = if void { "after_void" } else { "after" };
                arities.insert((n, which));
                let _ = writeln!(checks, "    {which}{n}(game::{target}, crate::{f});");
            }
        }
    }
    if !originals.is_empty() {
        let _ = writeln!(out, "use game::*;\nextern \"C\" {{\n{originals}}}\n");
    }
    let _ = writeln!(out, "#[allow(dead_code, unused_variables, non_snake_case)]");
    let _ = writeln!(out, "fn __psxw_checks() {{");
    for (n, which) in &arities {
        let a: Vec<String> = (0..*n).map(|i| format!("A{i}")).collect();
        let g = a.join(", ");
        let ga = if a.is_empty() { String::new() } else { format!("{g}, ") };
        let _ = match *which {
            "before" => writeln!(out, "    fn before{n}<{ga}R>(_: unsafe extern \"C\" fn({g}) -> R, _: unsafe extern \"C\" fn({g})) {{}}"),
            "after" => writeln!(out, "    fn after{n}<{ga}R>(_: unsafe extern \"C\" fn({g}) -> R, _: unsafe extern \"C\" fn(R{})) {{}}", a.iter().map(|x| format!(", {x}")).collect::<String>()),
            _ => writeln!(out, "    fn after_void{n}<{g}>(_: unsafe extern \"C\" fn({g}), _: unsafe extern \"C\" fn({g})) {{}}"),
        };
    }
    out.push_str(&checks);
    let _ = writeln!(out, "}}");
    crate::build::write_if_changed(&mod_include.join("mod.rs"), &out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn functions_from_pointer_types() {
        let raw = "pub type s32 = core::ffi::c_int;\npub type psxwfn_Rand =\n    ::core::option::Option<unsafe extern \"C\" fn(limit: s32) -> u32_>;\npub type psxwfn_Log = ::core::option::Option<unsafe extern \"C\" fn(fmt: *const core::ffi::c_char, ...)>;\npub type psxwfn_Get = ::core::option::Option<unsafe extern \"C\" fn(a: *mut [u8; 2usize]) -> s32>;\npub type psxwfn_M =\n    ::core::option::Option<\n        unsafe extern \"C\" fn(m0: *mut MATRIX) -> *mut MATRIX,\n    >;\n";
        let out = super::declare_functions(raw);
        assert!(out.contains("pub type s32 = core::ffi::c_int;"));
        assert!(out.contains("    pub fn Rand(limit: s32) -> u32_;\n"));
        assert!(out.contains("    pub fn Log(fmt: *const core::ffi::c_char, ...);\n"));
        assert!(out.contains("    pub fn Get(a: *mut [u8; 2usize]) -> s32;\n"));
        assert!(out.contains("    pub fn M(m0: *mut MATRIX) -> *mut MATRIX;\n"));
        assert!(!out.contains("psxwfn_"));
    }
}
