//! Host-local Super-xBR 3p sprite preprocessing. The final Jinc stage runs in
//! the enhanced primitive shader. Canonical VRAM/CLUT textures are read-only.

use anyhow::Result;
use glow::HasContext;
use psxw_abi::render::{prim, Rect, CLUT_SLOTS};
use std::collections::HashMap;

use crate::{Stats, framebuffer, gl_err, program, raster, shaders, texture, visual::VisualContext};

struct Plane {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    width: i32,
    height: i32,
}
impl Plane {
    fn new(gl: &glow::Context, width: i32, height: i32) -> Result<Self> {
        unsafe {
            let texture = texture(gl, glow::RGBA8, width, height)?;
            let framebuffer = match framebuffer(gl, texture) {
                Ok(f) => f,
                Err(e) => { gl.delete_texture(texture); return Err(e); }
            };
            Ok(Self { texture, framebuffer, width, height })
        }
    }
    fn ensure(slot: &mut Option<Self>, gl: &glow::Context, width: i32, height: i32) -> Result<()> {
        if !slot.as_ref().is_some_and(|p| p.width == width && p.height == height) {
            let next = Self::new(gl, width, height)?;
            if let Some(old) = slot.replace(next) { old.destroy(gl); }
        }
        Ok(())
    }
    fn destroy(self, gl: &glow::Context) {
        unsafe { gl.delete_framebuffer(self.framebuffer); gl.delete_texture(self.texture); }
    }
}
struct Stage {
    program: glow::Program,
    bounds: Option<glow::UniformLocation>,
    window: Option<glow::UniformLocation>,
    info: Option<glow::UniformLocation>,
    size: Option<glow::UniformLocation>,
}
impl Stage {
    fn new(gl: &glow::Context, pass: usize) -> Result<Self> {
        let fs = format!("#version 300 es\n#define SUPER_PASS {pass}\n{}", include_str!("super_xbr/pass.glsl"));
        let program = unsafe { program(gl, &shaders::present_vs(), &fs)? };
        unsafe {
            gl.use_program(Some(program));
            for (name, unit) in [("u_vram", 0), ("u_clut", 2), ("u_previous", 6)] {
                gl.uniform_1_i32(gl.get_uniform_location(program, name).as_ref(), unit);
            }
            Ok(Self {
                program,
                bounds: gl.get_uniform_location(program, "u_bounds"),
                window: gl.get_uniform_location(program, "u_tw"),
                info: gl.get_uniform_location(program, "u_info"),
                size: gl.get_uniform_location(program, "u_size"),
            })
        }
    }
}
struct Entry {
    key: [u32; 10],
    palette: u64,
    plane: Plane,
    valid: bool,
    used: u64,
}
impl Entry {
    fn reads(&self, rect: Rect) -> bool {
        let k = self.key;
        // The source is a Cartesian UV region, but texture-window mapping can
        // make physical VRAM addresses non-contiguous or wrapped. Test each
        // axis after that mapping rather than invalidating whole texture pages.
        let rows = (k[7]..=k[9]).any(|v| {
            let y = ((v & k[4]) + k[5]) & 511;
            (y.wrapping_sub(rect.y) & 511) < rect.h
        });
        rows && (k[6]..=k[8]).any(|u| {
            let depth = (k[0] & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT;
            let x = (((u & k[2]) + k[3]) >> (2-depth)) & 1023;
            (x.wrapping_sub(rect.x) & 1023) < rect.w
        })
    }
}

#[derive(Clone, Copy)]
struct PaletteSlot { head: u64, full: u64 }
struct PaletteSource { id: u64, current: bool }

pub(crate) struct Pipeline {
    stages: Vec<Stage>,
    vao: glow::VertexArray,
    dummy: Plane,
    scratch0: Option<Plane>,
    scratch1: Option<Plane>,
    cache: Vec<Entry>,
    cache_bytes: usize,
    palettes: Vec<PaletteSlot>,
    palette_sources: HashMap<(u32,u32,u32), PaletteSource>,
    next_palette: u64,
    clock: u64,
    draws: u32,
}
impl Pipeline {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        let mut stages: Vec<Stage> = Vec::new();
        for pass in 0..3 {
            match Stage::new(gl, pass) {
                Ok(p) => stages.push(p),
                Err(e) => {
                    for p in stages { unsafe { gl.delete_program(p.program); } }
                    return Err(e);
                }
            }
        }
        let dummy = match Plane::new(gl, 1, 1) {
            Ok(p) => p,
            Err(e) => {
                for p in stages { unsafe { gl.delete_program(p.program); } }
                return Err(e);
            }
        };
        let vao = match unsafe { gl.create_vertex_array() } {
            Ok(v) => v,
            Err(e) => {
                dummy.destroy(gl);
                for p in stages { unsafe { gl.delete_program(p.program); } }
                return Err(gl_err(e));
            }
        };
        // A factory may be installed with already populated canonical CLUT
        // slots. Their contents are initially unknown, not assumed identical.
        let palettes = (1..=CLUT_SLOTS as u64).map(|id| PaletteSlot { head: id, full: id }).collect();
        let p = Self { stages, vao, dummy, scratch0: None, scratch1: None, cache: Vec::new(), cache_bytes: 0,
            palettes, palette_sources: HashMap::new(), next_palette: CLUT_SLOTS as u64+1, clock: 0, draws: 0 };
        p.bind(gl, p.dummy());
        Ok(p)
    }
    pub fn invalidate_all(&mut self) {
        for e in &mut self.cache { e.valid = false; }
        for source in self.palette_sources.values_mut() { source.current = false; }
    }
    pub fn reset(&mut self) {
        self.invalidate_all();
        self.palette_sources.clear();
        for i in 0..self.palettes.len() {
            let id = self.fresh_palette_id();
            self.palettes[i] = PaletteSlot { head: id, full: id };
        }
    }
    fn fresh_palette_id(&mut self) -> u64 {
        let id = self.next_palette;
        self.next_palette += 1;
        id
    }
    fn palette_source(&mut self, x: u32, y: u32, count: u32) -> u64 {
        let key = (x & 1023,y & 511,count);
        if let Some(source) = self.palette_sources.get(&key) {
            if source.current { return source.id; }
        }
        let id = self.fresh_palette_id();
        // Dropping provenance only loses reuse. Loaded slot snapshots and
        // existing image identities remain valid independently of this map.
        if self.palette_sources.len() == 4096 && !self.palette_sources.contains_key(&key) { self.palette_sources.clear(); }
        self.palette_sources.insert(key, PaletteSource { id, current: true });
        id
    }
    pub fn sync(&mut self, rects: &[u32]) {
        for e in &mut self.cache {
            if e.valid && rects.chunks_exact(2).any(|r| e.reads(Rect::from_words(r[0],r[1]))) { e.valid = false; }
        }
        for (&(x,y,count), source) in &mut self.palette_sources {
            if source.current && rects.chunks_exact(2).any(|r| {
                let r = Rect::from_words(r[0],r[1]);
                (y.wrapping_sub(r.y) & 511) < r.h
                    && ((x.wrapping_sub(r.x) & 1023) < r.w || (r.x.wrapping_sub(x) & 1023) < count)
            }) { source.current = false; }
        }
        // A palette VRAM write does NOT modify already-loaded CLUT snapshots.
    }
    pub fn load_clut(&mut self, slot: u32, x: u32, y: u32, count: u32) {
        let Some(old) = self.palettes.get(slot as usize).copied() else { return };
        let head = self.palette_source(x,y,16);
        let full = if count == 256 { self.palette_source(x,y,256) }
            else if head == old.head { old.full }
            else {
                // A 16-entry load leaves entries 16..255 untouched. Do not
                // identify an 8-bit palette solely by its most recent head.
                self.fresh_palette_id()
            };
        self.palettes[slot as usize] = PaletteSlot { head, full };
    }
    pub fn key(rec: &[u32]) -> [u32; 10] {
        [rec[0] & (prim::DEPTH_MASK | prim::SEMI), rec[1], rec[4], rec[5], rec[6], rec[7], rec[24], rec[25], rec[26], rec[27]]
    }
    pub fn dummy(&self) -> glow::Texture { self.dummy.texture }
    pub fn bind(&self, gl: &glow::Context, texture: glow::Texture) {
        unsafe {
            gl.active_texture(glow::TEXTURE5);
            gl.bind_sampler(5, None);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        }
    }
    pub fn prepare(&mut self, ctx: &VisualContext<'_>, rec: &[u32]) -> Result<glow::Texture> {
        let gl = ctx.gl;
        let bounds = &rec[24..28];
        let (w, h) = ((bounds[2]-bounds[0]+1) as i32, (bounds[3]-bounds[1]+1) as i32);
        let flags = rec[0] & (prim::DEPTH_MASK | prim::SEMI);
        let planes = if flags & prim::SEMI != 0 { 2 } else { 1 };
        let mut key = Self::key(rec);
        let depth = (flags & prim::DEPTH_MASK) >> prim::DEPTH_SHIFT;
        let palette = if depth < 2 {
            self.palettes.get((rec[1] >> 16) as usize).map_or(0, |p| if depth == 0 { p.head } else { p.full })
        } else { 0 };
        // Slot numbers rotate even for unchanged palettes. Reuse processed
        // images by the loaded snapshot's identity, never by the slot number.
        // The actual slot is still used by the shaders and canonical engine.
        key[1] &= 0xffff;
        self.clock += 1;
        let index = if let Some(i) = self.cache.iter().position(|e| e.key == key && e.palette == palette) {
            self.cache[i].used = self.clock;
            if self.cache[i].valid { return Ok(self.cache[i].plane.texture); }
            i
        } else {
            // Budget bytes rather than eight entries: small glyphs must not
            // evict the entire working set on every frame.
            let bytes = (planes*2*w*2*h*4) as usize;
            while self.cache.len() >= 256 || self.cache_bytes + bytes > 16*1024*1024 {
                let i = self.cache.iter().enumerate().min_by_key(|(_, e)| e.used).unwrap().0;
                let old = self.cache.swap_remove(i);
                self.cache_bytes -= (old.plane.width*old.plane.height*4) as usize;
                old.plane.destroy(gl);
            }
            let plane = Plane::new(gl, planes*2*w, 2*h)?;
            self.cache_bytes += bytes;
            self.cache.push(Entry { key, palette, plane, valid: false, used: self.clock });
            self.cache.len()-1
        };
        Plane::ensure(&mut self.scratch0, gl, planes*w, h)?;
        Plane::ensure(&mut self.scratch1, gl, planes*2*w, 2*h)?;
        let p0 = self.scratch0.as_ref().unwrap();
        let p1 = self.scratch1.as_ref().unwrap();
        let output = &self.cache[index].plane;
        raster::prepare_draw(gl);
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            for (pass, (destination, previous)) in [(p0, self.dummy.texture), (p1, p0.texture), (output, p1.texture)].into_iter().enumerate() {
                let s = &self.stages[pass];
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(destination.framebuffer));
                gl.draw_buffers(&[glow::COLOR_ATTACHMENT0]);
                gl.viewport(0, 0, destination.width, destination.height);
                gl.use_program(Some(s.program));
                gl.uniform_4_i32(s.bounds.as_ref(), bounds[0] as i32, bounds[1] as i32, bounds[2] as i32, bounds[3] as i32);
                gl.uniform_4_u32(s.window.as_ref(), rec[4], rec[5], rec[6], rec[7]);
                gl.uniform_2_u32(s.info.as_ref(), flags, rec[1] >> 16);
                gl.uniform_2_i32(s.size.as_ref(), w, h);
                for (unit, texture) in [(0, ctx.vram.read), (2, ctx.vram.clut), (6, previous)] {
                    gl.active_texture(glow::TEXTURE0+unit);
                    gl.bind_sampler(unit, None);
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                }
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
        }
        self.draws += 3;
        self.cache[index].valid = true;
        Ok(self.cache[index].plane.texture)
    }
    pub fn take_stats(&mut self) -> Stats {
        Stats { draws: std::mem::take(&mut self.draws), ..Stats::default() }
    }
    pub fn destroy(self, gl: &glow::Context) {
        for s in self.stages { unsafe { gl.delete_program(s.program); } }
        unsafe { gl.delete_vertex_array(self.vao); }
        self.dummy.destroy(gl);
        for p in [self.scratch0, self.scratch1].into_iter().flatten() { p.destroy(gl); }
        for e in self.cache { e.plane.destroy(gl); }
    }
}
