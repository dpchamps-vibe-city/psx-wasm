//! Mods loaded at run time into a moddable base (plans/M11 phase 2).
//!
//! A mod module (`psxw mod build --modules`) is checked against the base
//! (the game's identity, each hooked function's slot and type), given a
//! slice of the mod area and a range of synthetic addresses, and wired
//! wasm-to-wasm: the base's tables and globals are its imports, so no host
//! frame sits between the game and a mod. Chains live in the base's
//! `psxmod_chain` table: a hooked function's slot holds the outermost
//! mod's layer, and each layer's next link (`original.<k>`, an index into
//! the same table) holds the layer below, the bottom one the function's
//! own entry (`psxmod_raw`). Loading, swapping or removing a mod rewires
//! every chain from the mods' order.

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use wasmtime::{AsContextMut, Extern, Func, Global, GlobalType, Instance, Memory, Module, Mutability, Ref, Table, Val, ValType};

/// The custom sections a moddable base and a mod module carry.
const SLOTS: &str = "psxmod.slots";
const GAME: &str = "psxmod.game";
const DECL: &str = "psxmod.decl";

#[derive(Debug, Clone, Deserialize)]
struct Target {
    name: String,
    container: String,
    address: String,
    flavour: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Decl {
    id: String,
    data_size: u32,
    stack_size: u32,
    functions: u32,
    layout_hash: String,
    targets: Vec<Target>,
    calls: Vec<Target>,
}

fn sections(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for p in wasmparser::Parser::new(0).parse_all(bytes) {
        if let wasmparser::Payload::CustomSection(r) = p? {
            out.insert(r.name().to_string(), r.data().to_vec());
        }
    }
    Ok(out)
}

fn addr(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).with_context(|| format!("bad address {s}"))
}

/// What a mod module needs of the base.
#[derive(Clone)]
struct Game {
    memory: Memory,
    dispatch: Table,
    chain: Table,
    entries: Table,
    raw: Table,
    on_init: Table,
    on_loaded: Table,
    n_init: Global,
    n_loaded: Global,
    sp: Global,
    funcs: BTreeMap<String, Func>,
}

/// A loaded mod.
struct Loaded {
    id: String,
    decl: Decl,
    instance: Instance,
    /// Each hooked function's slot, and the chain index of its next link.
    slots: Vec<u32>,
    originals: Vec<u32>,
    data_base: u32,
    code_base: u32,
    /// Bytes of the area it was given (image and stack).
    area: u32,
}

/// The mods of one run, in load order.
pub struct Loader {
    game: Game,
    slots: BTreeMap<(String, u32), (u32, String)>,
    identity: Vec<u8>,
    mods: Vec<Loaded>,
    /// Slots any mod ever hooked (a removed mod's must be cleared).
    touched: BTreeSet<u32>,
    data_next: u32,
    code_next: u32,
    /// The game has booted: a mod loaded now is initialized at once.
    pub booted: bool,
}

const AREA_START: u32 = 0x8000_0000 | psxw_abi::phys::MOD_DATA.start;
const AREA_END: u32 = 0x8000_0000 | psxw_abi::phys::MOD_DATA.end;

impl Loader {
    /// For a moddable base (`None` for any other game.wasm).
    pub fn new(mut store: impl AsContextMut, instance: &Instance, base: &[u8]) -> Result<Option<Self>> {
        let s = sections(base)?;
        let Some(lines) = s.get(SLOTS) else { return Ok(None) };
        let mut slots = BTreeMap::new();
        for (i, l) in String::from_utf8_lossy(lines).lines().enumerate() {
            let mut w = l.splitn(3, ' ');
            let (c, a, f) = (w.next().unwrap_or_default(), w.next().unwrap_or_default(), w.next().unwrap_or_default());
            slots.insert((c.to_string(), addr(a)?), (i as u32, f.to_string()));
        }
        let mut store = store.as_context_mut();
        let table = |s: &mut wasmtime::StoreContextMut<'_, _>, n: &str| instance.get_table(&mut *s, n).with_context(|| format!("the base exports no {n}"));
        let global = |s: &mut wasmtime::StoreContextMut<'_, _>, n: &str| instance.get_global(&mut *s, n).with_context(|| format!("the base exports no {n}"));
        let mut funcs = BTreeMap::new();
        for e in instance.exports(&mut store).map(|e| (e.name().to_string(), e.into_extern())).collect::<Vec<_>>() {
            if let (n, Extern::Func(f)) = e {
                funcs.insert(n, f);
            }
        }
        let game = Game {
            memory: instance.get_memory(&mut store, "memory").context("no memory")?,
            dispatch: table(&mut store, "psxmod_dispatch")?,
            chain: table(&mut store, "psxmod_chain")?,
            entries: table(&mut store, "psxmod_entries")?,
            raw: table(&mut store, "psxmod_raw")?,
            on_init: table(&mut store, "psxmod_on_init")?,
            on_loaded: table(&mut store, "psxmod_on_loaded")?,
            n_init: global(&mut store, "psxmod_n_init")?,
            n_loaded: global(&mut store, "psxmod_n_loaded")?,
            sp: global(&mut store, psxw_abi::guest::SP)?,
            funcs,
        };
        Ok(Some(Self {
            game,
            slots,
            identity: s.get(GAME).cloned().unwrap_or_default(),
            mods: Vec::new(),
            touched: BTreeSet::new(),
            data_next: AREA_START,
            code_next: psxw_abi::SYNTHETIC_CODE.start,
            booted: false,
        }))
    }

    /// Instantiate a mod module, checked against the base. `reuse`: a
    /// swapped-out mod whose area and addresses it takes over.
    fn instantiate(&mut self, mut store: impl AsContextMut, bytes: &[u8], reuse: Option<&Loaded>) -> Result<Loaded> {
        let mut store = store.as_context_mut();
        let s = sections(bytes)?;
        let decl: Decl = serde_json::from_slice(s.get(DECL).context("not a mod module (no psxmod.decl)")?)?;
        ensure!(s.get(GAME).is_some_and(|g| *g == self.identity), "{}: built for another game build than this one", decl.id);
        let mut slots = Vec::new();
        for t in &decl.targets {
            let (slot, flavour) = self.slots.get(&(t.container.clone(), addr(&t.address)?)).with_context(|| format!("{}: {} has no hook slot in this base", decl.id, t.name))?;
            ensure!(*flavour == t.flavour, "{}: {} is `{}` here, the mod was built for `{}`", decl.id, t.name, flavour, t.flavour);
            slots.push(*slot);
        }
        let mut calls = Vec::new();
        for c in &decl.calls {
            let (slot, flavour) = self.slots.get(&(c.container.clone(), addr(&c.address)?)).with_context(|| format!("{}: calls {}, which has no entry in this base", decl.id, c.name))?;
            ensure!(*flavour == c.flavour, "{}: calls {} as `{}`, the base has `{}`", decl.id, c.name, c.flavour, flavour);
            calls.push(*slot);
        }
        // Its area and its functions' addresses.
        let area = (decl.data_size.div_ceil(16) * 16) + decl.stack_size;
        let (data_base, code_base) = match reuse {
            Some(r) if r.area >= area && r.decl.functions >= decl.functions => (r.data_base, r.code_base),
            _ => {
                let d = self.data_next;
                ensure!(d + area <= AREA_END, "{}: the mod area is full", decl.id);
                self.data_next += area;
                let c = self.code_next;
                ensure!(c + 4 * decl.functions <= psxw_abi::SYNTHETIC_CODE.end, "{}: out of synthetic addresses", decl.id);
                self.code_next += 4 * decl.functions;
                (d, c)
            }
        };
        let stack_top = data_base + area;
        let mut originals = Vec::new();
        for _ in &decl.targets {
            let i = self.game.chain.grow(&mut store, 1, Ref::Func(None))?;
            originals.push(i as u32);
        }
        let module = Module::new(store.engine(), bytes)?;
        let konst = |store: &mut wasmtime::StoreContextMut<'_, _>, v: u32| Global::new(&mut *store, GlobalType::new(ValType::I32, Mutability::Const), Val::I32(v as i32));
        let mut imports = Vec::new();
        for imp in module.imports() {
            let e: Extern = match (imp.module(), imp.name()) {
                ("psx", "memory") => self.game.memory.into(),
                ("psx", "dispatch") => self.game.dispatch.into(),
                ("psx", "sp") => self.game.sp.into(),
                ("psxmod", "chain") => self.game.chain.into(),
                ("psxmod", "data_base") => konst(&mut store, data_base)?.into(),
                ("psxmod", "code_index") => konst(&mut store, psxw_abi::table_index(code_base))?.into(),
                ("psxmod", "code_base") => konst(&mut store, code_base)?.into(),
                ("psxmod", "stack_top") => konst(&mut store, stack_top)?.into(),
                ("original", k) => konst(&mut store, originals[k.parse::<usize>()?])?.into(),
                ("psx", f) => (*self.game.funcs.get(f).with_context(|| format!("{}: the base exports no {f}", decl.id))?).into(),
                (m, n) => bail!("{}: unknown import {m}.{n}", decl.id),
            };
            imports.push(e);
        }
        let instance = Instance::new(&mut store, &module, &imports).map_err(|e| anyhow::anyhow!("instantiating {}: {e:?}", decl.id))?;
        // Its calls into the game, through the functions' entries.
        if !calls.is_empty() {
            let t = instance.get_table(&mut store, "psxmod_calls").context("no psxmod_calls")?;
            for (k, slot) in calls.iter().enumerate() {
                let r = self.game.entries.get(&mut store, *slot as u64).context("entry")?;
                t.set(&mut store, k as u64, r)?;
            }
        }
        for k in 0..decl.targets.len() {
            ensure!(instance.get_func(&mut store, &format!("hook.{k}")).is_some(), "{}: exports no hook.{k}", decl.id);
        }
        Ok(Loaded { id: decl.id.clone(), decl, instance, slots, originals, data_base, code_base, area })
    }

    /// Every chain from the mods' order, and the init and loaded lists.
    fn rewire(&mut self, mut store: impl AsContextMut) -> Result<()> {
        let mut store = store.as_context_mut();
        let g = self.game.clone();
        let mut top: BTreeMap<u32, Ref> = BTreeMap::new();
        for m in &self.mods {
            for (k, &slot) in m.slots.iter().enumerate() {
                let below = match top.get(&slot) {
                    Some(r) => r.clone(),
                    None => g.raw.get(&mut store, slot as u64).context("raw entry")?,
                };
                g.chain.set(&mut store, m.originals[k] as u64, below)?;
                let hook = m.instance.get_func(&mut store, &format!("hook.{k}")).context("hook")?;
                top.insert(slot, Ref::Func(Some(hook)));
                self.touched.insert(slot);
            }
        }
        for &slot in &self.touched {
            let r = top.remove(&slot).unwrap_or(Ref::Func(None));
            g.chain.set(&mut store, slot as u64, r)?;
        }
        for (i, m) in self.mods.iter().enumerate() {
            let init = m.instance.get_func(&mut store, "psxmod_init").context("psxmod_init")?;
            let loaded = m.instance.get_func(&mut store, "psxmod_loaded").context("psxmod_loaded")?;
            g.on_init.set(&mut store, i as u64, Ref::Func(Some(init)))?;
            g.on_loaded.set(&mut store, i as u64, Ref::Func(Some(loaded)))?;
        }
        g.n_init.set(&mut store, Val::I32(self.mods.len() as i32))?;
        g.n_loaded.set(&mut store, Val::I32(self.mods.len() as i32))?;
        Ok(())
    }

    /// Load a mod after the others.
    pub fn load(&mut self, mut store: impl AsContextMut, bytes: &[u8]) -> Result<String> {
        let m = self.instantiate(&mut store, bytes, None)?;
        let id = m.id.clone();
        let init = m.instance.get_func(&mut store, "psxmod_init").context("psxmod_init")?;
        self.mods.push(m);
        self.rewire(&mut store)?;
        if self.booted {
            init.call(&mut store, &[], &mut [])?;
        }
        Ok(id)
    }

    /// Replace mod `index` with another module (the same mod rebuilt): it
    /// takes over the area and keeps the state when the data layout is the
    /// same. Frames already running in the old one finish there.
    pub fn swap(&mut self, mut store: impl AsContextMut, index: usize, bytes: &[u8]) -> Result<String> {
        ensure!(index < self.mods.len(), "no mod {index} to swap");
        let old = self.mods.remove(index);
        let m = match self.instantiate(&mut store, bytes, Some(&old)) {
            Ok(m) => m,
            Err(e) => {
                self.mods.insert(index, old);
                return Err(e);
            }
        };
        let same = m.decl.layout_hash == old.decl.layout_hash && m.data_base == old.data_base;
        let id = m.id.clone();
        let f = m.instance.get_func(&mut store, if same && self.booted { "psxmod_resume" } else { "psxmod_init" }).context("init")?;
        self.mods.insert(index, m);
        self.rewire(&mut store)?;
        if self.booted {
            f.call(&mut store, &[], &mut [])?;
        }
        Ok(id)
    }

    /// Unload mod `index`.
    pub fn remove(&mut self, store: impl AsContextMut, index: usize) -> Result<()> {
        ensure!(index < self.mods.len(), "no mod {index}");
        self.mods.remove(index);
        self.rewire(store)
    }

    pub fn ids(&self) -> Vec<String> {
        self.mods.iter().map(|m| m.id.clone()).collect()
    }
}
