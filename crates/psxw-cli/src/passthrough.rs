//! The pass-through mod (plans/M11 task 12): a no-op `before`, `after` and
//! `patch` with `original` on every hookable function, generated from the
//! build's own facts. Linked in, it exercises every call path at once; the
//! game's replays must stay bit-identical.
//!
//! Each hook's type comes from the function itself: a C function's from
//! clang's prototype facts (structs by value as same-sized structs), any
//! other's is the o32 form `unsigned long long (unsigned int a0..a3)`. A
//! variadic function gets no patch (C can't forward its arguments).

use anyhow::Result;
use psxw_cc::protos::Kind;
use psxw_link::game::Tier;
use psxw_link::object::{NameClass, classify_name};
use psxw_inputs::Workspace;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

fn kind_type(k: &Kind, structs: &mut BTreeSet<(u32, u32)>) -> String {
    match *k {
        Kind::I8 { signed: true } => "signed char".into(),
        Kind::I8 { signed: false } => "unsigned char".into(),
        Kind::I16 { signed: true } => "short".into(),
        Kind::I16 { signed: false } => "unsigned short".into(),
        Kind::I32 => "int".into(),
        Kind::I64 => "long long".into(),
        Kind::F32 => "float".into(),
        Kind::F64 => "double".into(),
        Kind::Ptr => "void *".into(),
        Kind::ByVal { size, align } => {
            structs.insert((size, align));
            format!("struct psxw_bv_{size}_{align}")
        }
        Kind::Sret { size } => {
            structs.insert((size, 4));
            format!("struct psxw_bv_{size}_4")
        }
        Kind::Void => "void".into(),
    }
}

fn val_type(t: wasmparser::ValType) -> &'static str {
    match t {
        wasmparser::ValType::I64 => "long long",
        wasmparser::ValType::F32 => "float",
        wasmparser::ValType::F64 => "double",
        _ => "int",
    }
}

/// Write the mod into `out`; returns how many functions it hooks. `tiers`
/// limits it to functions of those tiers (`c`, `lift`, `adapter`).
pub fn generate(ws: &Workspace, game: &str, out: &Path, tiers: Option<&[&str]>) -> Result<usize> {
    let (prep, table) = psxw_modkit::build::table(ws, game)?;
    let mut hooks = Vec::new();
    let mut src = String::new();
    let mut structs: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut n = 0;
    for (k, f) in table.functions.iter().enumerate() {
        if f.expanded {
            continue;
        }
        let tier = match f.tier {
            psxw_modkit::symbols::TierName::C => "c",
            psxw_modkit::symbols::TierName::Lift => "lift",
            psxw_modkit::symbols::TierName::Adapter => "adapter",
        };
        if tiers.is_some_and(|t| !t.contains(&tier)) {
            continue;
        }
        // (return type, parameter types, variadic)
        let (ret, params, variadic) = match prep.tiers.get(&(f.container.clone(), f.addr)) {
            Some(Tier::C { input, func }) => {
                let inp = &prep.link_inputs[*input];
                let o = &inp.obj;
                let ty = &o.types[o.funcs[*func as usize].ty as usize];
                let name = o.func_symbol(func + o.num_imported_funcs()).map(|s| s.name.clone()).unwrap_or_default();
                let name = match classify_name(&name) {
                    NameClass::Guest(g) => g.to_string(),
                    _ => name,
                };
                match inp.protos.get(&name).filter(|p| p.params.len() + usize::from(p.variadic) == ty.params.len()) {
                    Some(p) => {
                        let (ret, params) = match p.params.first() {
                            Some(s @ Kind::Sret { .. }) => (kind_type(s, &mut structs), &p.params[1..]),
                            _ => (kind_type(&p.ret, &mut structs), &p.params[..]),
                        };
                        (ret, params.iter().map(|k| kind_type(k, &mut structs)).collect::<Vec<_>>(), p.variadic)
                    }
                    None => (
                        ty.results.first().map_or("void", |t| val_type(*t)).to_string(),
                        ty.params.iter().map(|t| val_type(*t).to_string()).collect(),
                        false,
                    ),
                }
            }
            Some(_) => ("unsigned long long".into(), vec!["unsigned int".to_string(); 4], false),
            None => continue,
        };
        n += 1;
        let target = serde_json::json!({ "container": f.container, "address": format!("0x{:08X}", f.addr) });
        let signature = serde_json::json!({
            "returns": ret,
            "params": params.iter().enumerate().map(|(i, t)| serde_json::json!({ "name": format!("p{i}"), "type": t })).collect::<Vec<_>>(),
            "variadic": variadic,
        });
        let file = "src/pass.c";
        hooks.push(serde_json::json!({ "kind": "before", "target": target, "impl": { "file": file, "function": format!("pt_b_{k}") }, "signature": signature }));
        hooks.push(serde_json::json!({ "kind": "after", "target": target, "impl": { "file": file, "function": format!("pt_a_{k}") }, "signature": signature }));
        if !variadic {
            hooks.push(serde_json::json!({ "kind": "patch", "target": target, "impl": { "file": file, "function": format!("pt_p_{k}") }, "original": format!("pt_o_{k}"), "signature": signature }));
        }
        let decl = |i: usize, t: &str| psxw_modkit::genfiles::spell(t, &format!("p{i}"));
        let mut list: Vec<String> = params.iter().enumerate().map(|(i, t)| decl(i, t)).collect();
        if variadic {
            list.push("...".into());
        }
        let plist = if list.is_empty() { "void".to_string() } else { list.join(", ") };
        let args: Vec<String> = (0..params.len()).map(|i| format!("p{i}")).collect();
        let _ = writeln!(src, "void pt_b_{k}({plist}) {{}}");
        if ret == "void" {
            let _ = writeln!(src, "void pt_a_{k}({plist}) {{}}");
        } else {
            let mut alist = vec![psxw_modkit::genfiles::spell(&ret, "r")];
            alist.extend(list.iter().cloned());
            let _ = writeln!(src, "void pt_a_{k}({}) {{}}", alist.join(", "));
        }
        if !variadic {
            let call = format!("pt_o_{k}({})", args.join(", "));
            if ret == "void" {
                let _ = writeln!(src, "void pt_p_{k}({plist}) {{ {call}; }}");
            } else {
                let _ = writeln!(src, "{} {{ return {call}; }}", psxw_modkit::genfiles::spell(&ret, &format!("pt_p_{k}({plist})")));
            }
        }
    }
    let mut bv = String::from("/* Structs passed by value, by size and alignment. */\n#pragma once\n");
    for (size, align) in &structs {
        // Elements as wide as the alignment, so the struct's natural
        // alignment is the original's.
        let el = match align {
            8 => "unsigned long long",
            4 => "unsigned int",
            2 => "unsigned short",
            _ => "unsigned char",
        };
        let _ = writeln!(bv, "struct psxw_bv_{size}_{align} {{ {el} b[{}]; }};", size / (*align).max(1));
    }
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out.join("src"))?;
    std::fs::write(out.join("src").join("bv.h"), bv)?;
    std::fs::write(out.join("src").join("pass.c"), format!("#include \"bv.h\"\n#include \"mod.h\"\n\n{src}"))?;
    let decl = serde_json::json!({ "id": "pass-through", "version": "0.1.0", "game": game, "hooks": hooks });
    std::fs::write(out.join("mod.json"), serde_json::to_string_pretty(&decl)?)?;
    Ok(n)
}
