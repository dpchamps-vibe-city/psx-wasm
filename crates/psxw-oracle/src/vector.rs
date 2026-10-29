//! Vectors: one recorded device input, a kind tag plus a canonical payload.
//!
//! A vector's id is the sha256 of its canonical encoding ([`Vector::encode`]).
//! Large state (VRAM rectangles, SPU RAM) is stored once in the blob store
//! and referenced by hash, so a VRAM image shared by many vectors is stored
//! once. The encoding is versioned by [`VECTOR_FORMAT_VERSION`]; the kind
//! tags are the `VK_*` wire constants.
//!
//! Each kind defines its result: the bytes a device target (or psx-wasm's
//! implementation) produces for it, compared byte for byte.

use crate::hash::Hash;
use crate::wire;
use anyhow::{Result, bail, ensure};
use std::fmt;

/// Version of the canonical vector encoding. Bumped on any change; old
/// vectors stay readable only if the decoder keeps their version.
pub const VECTOR_FORMAT_VERSION: u32 = 1;

/// A vector's identity: sha256 of its canonical encoding.
pub type VectorId = Hash;

/// A blob (VRAM pixels as u16 LE, SPU RAM bytes) by content hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlobRef(pub Hash);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Kind {
    Echo,
    GpuDraw,
    GpuTransfer,
    GteOp,
    SpuRun,
    BiosCall,
    CdXa,
    MdecRun,
    CdCmds,
    BiosThreads,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Echo,
        Kind::GpuDraw,
        Kind::GpuTransfer,
        Kind::GteOp,
        Kind::SpuRun,
        Kind::BiosCall,
        Kind::CdXa,
        Kind::MdecRun,
        Kind::CdCmds,
        Kind::BiosThreads,
    ];

    pub fn tag(self) -> u32 {
        match self {
            Kind::Echo => wire::VK_ECHO,
            Kind::GpuDraw => wire::VK_GPU_DRAW,
            Kind::GpuTransfer => wire::VK_GPU_TRANSFER,
            Kind::GteOp => wire::VK_GTE_OP,
            Kind::SpuRun => wire::VK_SPU_RUN,
            Kind::BiosCall => wire::VK_BIOS_CALL,
            Kind::CdXa => wire::VK_CD_XA,
            Kind::MdecRun => wire::VK_MDEC_RUN,
            Kind::CdCmds => wire::VK_CD_CMDS,
            Kind::BiosThreads => wire::VK_BIOS_THREADS,
        }
    }

    pub fn from_tag(tag: u32) -> Result<Self> {
        Kind::ALL.into_iter().find(|k| k.tag() == tag).ok_or_else(|| anyhow::anyhow!("unknown vector kind {tag}"))
    }

    /// CLI and report name.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Echo => "echo",
            Kind::GpuDraw => "gpu_draw",
            Kind::GpuTransfer => "gpu_transfer",
            Kind::GteOp => "gte_op",
            Kind::SpuRun => "spu_run",
            Kind::BiosCall => "bios_call",
            Kind::CdXa => "cd_xa",
            Kind::MdecRun => "mdec_run",
            Kind::CdCmds => "cd_cmds",
            Kind::BiosThreads => "bios_threads",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Kind::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .ok_or_else(|| anyhow::anyhow!("unknown vector kind {s} (one of {})", Kind::ALL.map(|k| k.name()).join(", ")))
    }

    /// The psx-wasm device a kind exercises (`psxw oracle check --device`).
    pub fn device(self) -> &'static str {
        match self {
            Kind::Echo => "harness",
            Kind::GpuDraw | Kind::GpuTransfer => "gpu",
            Kind::GteOp => "gte",
            Kind::SpuRun => "spu",
            Kind::BiosCall => "bios",
            Kind::CdXa => "xa",
            Kind::MdecRun => "mdec",
            Kind::CdCmds => "cdc",
            Kind::BiosThreads => "threads",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A VRAM rectangle in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Rect { x, y, w, h }
    }

    pub fn pixels(&self) -> usize {
        self.w as usize * self.h as usize
    }

    /// Non-empty and inside VRAM, without wrapping.
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.w > 0
                && self.h > 0
                && self.x as u32 + self.w as u32 <= wire::VRAM_WIDTH
                && self.y as u32 + self.h as u32 <= wire::VRAM_HEIGHT,
            "rect {self:?} is empty or outside VRAM"
        );
        Ok(())
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && y >= self.y && (x - self.x) < self.w && (y - self.y) < self.h
    }

    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// A VRAM rectangle's contents, w*h u16 pixels in a blob.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VramRect {
    pub rect: Rect,
    pub pixels: BlobRef,
}

/// How GPU_DRAW sends its packets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GpuMode {
    /// GP0 port writes.
    Port,
    /// DMA2 block mode.
    Block,
    /// DMA2 linked list, one node per packet.
    List,
}

impl GpuMode {
    pub fn wire(self) -> u32 {
        match self {
            GpuMode::Port => wire::GPU_MODE_PORT,
            GpuMode::Block => wire::GPU_MODE_BLOCK,
            GpuMode::List => wire::GPU_MODE_LIST,
        }
    }

    fn from_wire(v: u32) -> Result<Self> {
        Ok(match v {
            wire::GPU_MODE_PORT => GpuMode::Port,
            wire::GPU_MODE_BLOCK => GpuMode::Block,
            wire::GPU_MODE_LIST => GpuMode::List,
            _ => bail!("unknown GPU mode {v}"),
        })
    }
}

/// VRAM initial state, draw state words (GP0 E1h..E6h and the like, sent
/// by port first), GP0 packets, then the rectangles read back. Result: the
/// readback rectangles' pixels, u16 LE, in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GpuDraw {
    pub init: Vec<VramRect>,
    pub state: Vec<u32>,
    pub mode: GpuMode,
    pub packets: Vec<Vec<u32>>,
    pub readback: Vec<Rect>,
}

/// One step of a GPU_TRANSFER.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TransferStep {
    /// GP0(A0h) under the current mask settings.
    CpuToVram(VramRect),
    /// GP0(C0h); the pixels join the result.
    VramToCpu(Rect),
    /// GP0(80h).
    VramToVram { src_x: u16, src_y: u16, dst_x: u16, dst_y: u16, w: u16, h: u16 },
}

/// VRAM initial state, state words (E6h mask settings), transfer steps,
/// readback rectangles. Result: every VramToCpu step's pixels, then the
/// readback rectangles' pixels.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GpuTransfer {
    pub init: Vec<VramRect>,
    pub state: Vec<u32>,
    pub steps: Vec<TransferStep>,
    pub readback: Vec<Rect>,
}

/// GTE registers in, one command word, registers out. Result: data[32],
/// ctrl[32] as u32 LE (`GTE_EXEC_RESULT_SIZE` bytes).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GteOp {
    pub data: [u32; 32],
    pub ctrl: [u32; 32],
    pub cmd: u32,
}

/// SPU RAM bytes at an address.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpuRam {
    pub addr: u32,
    pub bytes: BlobRef,
}

/// A register write at a sample index (register offset from 1F801C00h).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpuWrite {
    pub sample: u32,
    pub reg: u32,
    pub value: u16,
}

/// SPU RAM initial state, timed register writes, a run length, captures.
/// Result: the register snapshot (`SPU_SNAPSHOT_SIZE` bytes) then each
/// capture's bytes. See OP_SPU_RUN for the timing contract.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpuRun {
    pub ram: Vec<SpuRam>,
    pub writes: Vec<SpuWrite>,
    pub samples: u32,
    pub captures: Vec<(u32, u32)>,
}

/// One BIOS call. Result: v0 as u32 LE, then each capture's bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BiosCall {
    pub table: u32,
    pub func: u32,
    pub args: [u32; 4],
    pub setup: Vec<(u32, Vec<u8>)>,
    pub captures: Vec<(u32, u32)>,
}

/// XA-ADPCM played from the runner disc's test file (`crate::xa_disc`) and
/// captured from the SPU's CD input buffers. `file_lba` is where the file
/// starts on the target's disc, `start` the first sector played (within
/// the file), `mode`/`file`/`channel` the Setmode and Setfilter values.
/// Result: each capture. See OP_CD_XA.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CdXa {
    pub xa: BlobRef,
    pub file_lba: u32,
    pub start: u32,
    pub mode: u32,
    pub file: u32,
    pub channel: u32,
    pub samples: u32,
    pub captures: Vec<(u32, u32)>,
}

/// One MDEC command: its words (the command word, then its parameters)
/// and how many decoded words DMA1 reads after it (a multiple of
/// `MDEC_OUT_BLOCK`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MdecCmd {
    pub words: Vec<u32>,
    pub out: u32,
}

/// MDEC commands from a reset: tables and decodes. Result: each command's
/// decoded words, u32 LE. See OP_MDEC_RUN.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MdecRun {
    pub cmds: Vec<MdecCmd>,
}

/// One CD controller command: the command byte, its parameters, how many
/// interrupts to collect, and the bytes to read from the data FIFO at each
/// INT1 (0, 800h or 924h).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CdCmd {
    pub cmd: u32,
    pub params: Vec<u8>,
    pub ints: u32,
    pub data: u32,
}

/// CD controller commands on the runner disc (`crate::cdc`). `disc` holds
/// the raw 2352-byte sectors from `first_lba`, as the disc image has them
/// (the sectors a model of the disc reads); `end_lba` is the disc's size
/// in sectors and `region` GetID's region letters. Result: see
/// OP_CD_CMDS.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CdCmds {
    pub disc: BlobRef,
    pub first_lba: u32,
    pub end_lba: u32,
    pub region: [u8; 4],
    pub cmds: Vec<CdCmd>,
}

/// BIOS threads (`crate::threads`): the thread entry's address, the TCB
/// count SYSTEM.CNF gives the runner disc, and the steps (u32 op, a, b,
/// c). Result: see OP_BIOS_THREADS.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BiosThreads {
    pub pc: u32,
    pub tcbs: u32,
    pub steps: Vec<[u32; 4]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Vector {
    Echo(Vec<u8>),
    GpuDraw(GpuDraw),
    GpuTransfer(GpuTransfer),
    GteOp(GteOp),
    SpuRun(SpuRun),
    BiosCall(BiosCall),
    CdXa(CdXa),
    MdecRun(MdecRun),
    CdCmds(CdCmds),
    BiosThreads(BiosThreads),
}

/// Result words of a BIOS_THREADS step (`None` for an unknown op).
pub fn thread_step_words(op: u32) -> Option<u32> {
    Some(match op {
        wire::TS_TABLES => wire::TS_TABLES_WORDS,
        wire::TS_FILL => wire::TS_FILL_WORDS,
        wire::TS_OPEN => wire::TS_OPEN_WORDS,
        wire::TS_CHANGE => wire::TS_CHANGE_WORDS,
        wire::TS_CLOSE => wire::TS_CLOSE_WORDS,
        _ => return None,
    })
}

/// SPU RAM the runner reserves (its silent loop block).
pub const SPU_RESERVED: std::ops::Range<u32> = 0x1000..0x1010;

impl Vector {
    pub fn kind(&self) -> Kind {
        match self {
            Vector::Echo(_) => Kind::Echo,
            Vector::GpuDraw(_) => Kind::GpuDraw,
            Vector::GpuTransfer(_) => Kind::GpuTransfer,
            Vector::GteOp(_) => Kind::GteOp,
            Vector::SpuRun(_) => Kind::SpuRun,
            Vector::BiosCall(_) => Kind::BiosCall,
            Vector::CdXa(_) => Kind::CdXa,
            Vector::MdecRun(_) => Kind::MdecRun,
            Vector::CdCmds(_) => Kind::CdCmds,
            Vector::BiosThreads(_) => Kind::BiosThreads,
        }
    }

    pub fn id(&self) -> VectorId {
        Hash::of(&self.encode())
    }

    /// Every blob the vector references.
    pub fn blobs(&self) -> Vec<BlobRef> {
        match self {
            Vector::GpuDraw(d) => d.init.iter().map(|r| r.pixels).collect(),
            Vector::GpuTransfer(t) => t
                .init
                .iter()
                .map(|r| r.pixels)
                .chain(t.steps.iter().filter_map(|s| match s {
                    TransferStep::CpuToVram(r) => Some(r.pixels),
                    _ => None,
                }))
                .collect(),
            Vector::SpuRun(s) => s.ram.iter().map(|r| r.bytes).collect(),
            Vector::CdXa(x) => vec![x.xa],
            Vector::CdCmds(c) => vec![c.disc],
            _ => Vec::new(),
        }
    }

    /// Length of the result in bytes.
    pub fn result_len(&self) -> usize {
        match self {
            Vector::Echo(b) => b.len(),
            Vector::GpuDraw(d) => d.readback.iter().map(|r| 2 * r.pixels()).sum(),
            Vector::GpuTransfer(t) => {
                let steps: usize = t
                    .steps
                    .iter()
                    .map(|s| match s {
                        TransferStep::VramToCpu(r) => 2 * r.pixels(),
                        _ => 0,
                    })
                    .sum();
                steps + t.readback.iter().map(|r| 2 * r.pixels()).sum::<usize>()
            }
            Vector::GteOp(_) => wire::GTE_EXEC_RESULT_SIZE as usize,
            Vector::SpuRun(s) => wire::SPU_SNAPSHOT_SIZE as usize + s.captures.iter().map(|c| c.1 as usize).sum::<usize>(),
            Vector::BiosCall(b) => 4 + b.captures.iter().map(|c| c.1 as usize).sum::<usize>(),
            Vector::CdXa(x) => x.captures.iter().map(|c| c.1 as usize).sum(),
            Vector::MdecRun(m) => m.cmds.iter().map(|c| 4 * c.out as usize).sum(),
            Vector::CdCmds(c) => 8 + c.cmds.iter().map(|c| (c.ints * (wire::CD_RECORD_SIZE + c.data + 4)) as usize).sum::<usize>(),
            Vector::BiosThreads(t) => t.steps.iter().map(|s| 4 * thread_step_words(s[0]).unwrap_or(0) as usize).sum(),
        }
    }

    /// Structural checks that need no blob contents. Blob sizes are checked
    /// when the vector is compiled for a target.
    pub fn check(&self) -> Result<()> {
        match self {
            Vector::Echo(b) => ensure!(b.len() <= 0x8000, "echo payload of {} bytes", b.len()),
            Vector::GpuDraw(d) => {
                for r in &d.init {
                    r.rect.check()?;
                }
                for r in &d.readback {
                    r.check()?;
                }
                let max = match d.mode {
                    GpuMode::List => 255,
                    GpuMode::Port | GpuMode::Block => wire::GPU_PACKET_MAX_WORDS as usize,
                };
                ensure!(d.packets.iter().all(|p| p.len() <= max), "{:?}-mode packet over {max} words", d.mode);
            }
            Vector::GpuTransfer(t) => {
                for r in &t.init {
                    r.rect.check()?;
                }
                for s in &t.steps {
                    match s {
                        TransferStep::CpuToVram(r) => r.rect.check()?,
                        TransferStep::VramToCpu(r) => r.check()?,
                        TransferStep::VramToVram { src_x, src_y, dst_x, dst_y, w, h } => {
                            Rect::new(*src_x, *src_y, *w, *h).check()?;
                            Rect::new(*dst_x, *dst_y, *w, *h).check()?;
                        }
                    }
                }
                for r in &t.readback {
                    r.check()?;
                }
            }
            Vector::GteOp(_) => {}
            Vector::SpuRun(s) => {
                ensure!(s.samples <= 44_100 * 60, "SPU run of {} samples", s.samples);
                let mut prev = 0;
                for w in &s.writes {
                    ensure!(w.sample >= prev && w.sample <= s.samples, "SPU writes out of order or past the run");
                    ensure!(w.reg < 0x200 && w.reg % 2 == 0 && w.reg != 0x1A8, "SPU register {:#x} not writable", w.reg);
                    prev = w.sample;
                }
                for &(addr, len) in &s.captures {
                    check_spu_range(addr, len)?;
                }
                for r in &s.ram {
                    ensure!(r.addr % wire::SPU_ADDR_ALIGN == 0, "SPU RAM address {:#x} unaligned", r.addr);
                }
            }
            Vector::CdXa(x) => {
                ensure!(x.samples <= 44_100 * 60, "XA run of {} samples", x.samples);
                ensure!(x.mode <= 0xFF && x.file <= 0xFF && x.channel <= 0xFF, "XA mode or filter out of range");
                for &(addr, len) in &x.captures {
                    check_spu_range(addr, len)?;
                }
            }
            Vector::MdecRun(m) => {
                ensure!(!m.cmds.is_empty(), "MDEC run with no commands");
                for c in &m.cmds {
                    ensure!(!c.words.is_empty() && c.words.len() <= 0x4000, "MDEC command of {} words", c.words.len());
                    ensure!(c.out % wire::MDEC_OUT_BLOCK == 0, "MDEC output of {} words is not a multiple of {}", c.out, wire::MDEC_OUT_BLOCK);
                }
            }
            Vector::CdCmds(c) => {
                ensure!(c.end_lba >= c.first_lba, "disc ends before its sectors");
                for k in &c.cmds {
                    ensure!(k.cmd <= 0xFF && k.params.len() <= 16, "CD command {:#x} with {} parameters", k.cmd, k.params.len());
                    ensure!((1..=64).contains(&k.ints), "CD command {:#x} collects {} interrupts", k.cmd, k.ints);
                    ensure!(k.data <= wire::CD_DATA_MAX && k.data % 4 == 0, "CD data read of {} bytes", k.data);
                }
            }
            Vector::BiosThreads(t) => {
                let end = wire::SCRATCH_BASE + wire::SCRATCH_SIZE;
                ensure!(
                    t.pc % 4 == 0 && t.pc >= wire::SCRATCH_BASE && t.pc + 4 * wire::THREAD_STUB_WORDS <= end,
                    "thread entry {:#x} outside the scratch region",
                    t.pc
                );
                ensure!((1..=32).contains(&t.tcbs), "{} TCBs", t.tcbs);
                for s in &t.steps {
                    ensure!(thread_step_words(s[0]).is_some(), "thread step {}", s[0]);
                    let limit = if s[0] == wire::TS_FILL { 32 } else { wire::THREAD_SLOTS };
                    ensure!(s[1] < limit, "thread step {} operand {}", s[0], s[1]);
                }
            }
            Vector::BiosCall(b) => {
                ensure!(matches!(b.table, 0xA0 | 0xB0 | 0xC0), "BIOS table {:#x}", b.table);
                for (addr, bytes) in &b.setup {
                    check_scratch(*addr, bytes.len() as u32)?;
                }
                for &(addr, len) in &b.captures {
                    check_scratch(addr, len)?;
                    ensure!(len % 4 == 0, "BIOS capture length {len} not a multiple of 4");
                }
            }
        }
        Ok(())
    }

    /// The canonical encoding: u32 format version, u32 kind tag, payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        e.u32(VECTOR_FORMAT_VERSION);
        e.u32(self.kind().tag());
        match self {
            Vector::Echo(b) => e.bytes(b),
            Vector::GpuDraw(d) => {
                e.vram_rects(&d.init);
                e.words(&d.state);
                e.u32(d.mode.wire());
                e.u32(d.packets.len() as u32);
                for p in &d.packets {
                    e.words(p);
                }
                e.rects(&d.readback);
            }
            Vector::GpuTransfer(t) => {
                e.vram_rects(&t.init);
                e.words(&t.state);
                e.u32(t.steps.len() as u32);
                for s in &t.steps {
                    match s {
                        TransferStep::CpuToVram(r) => {
                            e.u32(0);
                            e.rect(&r.rect);
                            e.hash(&r.pixels.0);
                        }
                        TransferStep::VramToCpu(r) => {
                            e.u32(1);
                            e.rect(r);
                        }
                        TransferStep::VramToVram { src_x, src_y, dst_x, dst_y, w, h } => {
                            e.u32(2);
                            for v in [src_x, src_y, dst_x, dst_y, w, h] {
                                e.u16(*v);
                            }
                        }
                    }
                }
                e.rects(&t.readback);
            }
            Vector::GteOp(g) => {
                for v in g.data.iter().chain(&g.ctrl) {
                    e.u32(*v);
                }
                e.u32(g.cmd);
            }
            Vector::SpuRun(s) => {
                e.u32(s.samples);
                e.u32(s.ram.len() as u32);
                for r in &s.ram {
                    e.u32(r.addr);
                    e.hash(&r.bytes.0);
                }
                e.u32(s.writes.len() as u32);
                for w in &s.writes {
                    e.u32(w.sample);
                    e.u32(w.reg);
                    e.u32(w.value as u32);
                }
                e.pairs(&s.captures);
            }
            Vector::BiosCall(b) => {
                e.u32(b.table);
                e.u32(b.func);
                for a in b.args {
                    e.u32(a);
                }
                e.u32(b.setup.len() as u32);
                for (addr, bytes) in &b.setup {
                    e.u32(*addr);
                    e.bytes(bytes);
                }
                e.pairs(&b.captures);
            }
            Vector::CdXa(x) => {
                e.hash(&x.xa.0);
                for v in [x.file_lba, x.start, x.mode, x.file, x.channel, x.samples] {
                    e.u32(v);
                }
                e.pairs(&x.captures);
            }
            Vector::MdecRun(m) => {
                e.u32(m.cmds.len() as u32);
                for c in &m.cmds {
                    e.words(&c.words);
                    e.u32(c.out);
                }
            }
            Vector::CdCmds(c) => {
                e.hash(&c.disc.0);
                e.u32(c.first_lba);
                e.u32(c.end_lba);
                e.u32(u32::from_le_bytes(c.region));
                e.u32(c.cmds.len() as u32);
                for k in &c.cmds {
                    e.u32(k.cmd);
                    e.bytes(&k.params);
                    e.u32(k.ints);
                    e.u32(k.data);
                }
            }
            Vector::BiosThreads(t) => {
                e.u32(t.pc);
                e.u32(t.tcbs);
                e.u32(t.steps.len() as u32);
                for s in &t.steps {
                    for v in s {
                        e.u32(*v);
                    }
                }
            }
        }
        e.0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut d = Dec { b: bytes, pos: 0 };
        let version = d.u32()?;
        ensure!(version == VECTOR_FORMAT_VERSION, "vector format version {version}");
        let v = match Kind::from_tag(d.u32()?)? {
            Kind::Echo => Vector::Echo(d.bytes()?),
            Kind::GpuDraw => {
                let init = d.vram_rects()?;
                let state = d.words()?;
                let mode = GpuMode::from_wire(d.u32()?)?;
                let n = d.count()?;
                let mut packets = Vec::with_capacity(n);
                for _ in 0..n {
                    packets.push(d.words()?);
                }
                let readback = d.rects()?;
                Vector::GpuDraw(GpuDraw { init, state, mode, packets, readback })
            }
            Kind::GpuTransfer => {
                let init = d.vram_rects()?;
                let state = d.words()?;
                let n = d.count()?;
                let mut steps = Vec::with_capacity(n);
                for _ in 0..n {
                    steps.push(match d.u32()? {
                        0 => TransferStep::CpuToVram(VramRect { rect: d.rect()?, pixels: BlobRef(d.hash()?) }),
                        1 => TransferStep::VramToCpu(d.rect()?),
                        2 => TransferStep::VramToVram {
                            src_x: d.u16()?,
                            src_y: d.u16()?,
                            dst_x: d.u16()?,
                            dst_y: d.u16()?,
                            w: d.u16()?,
                            h: d.u16()?,
                        },
                        t => bail!("unknown transfer step {t}"),
                    });
                }
                let readback = d.rects()?;
                Vector::GpuTransfer(GpuTransfer { init, state, steps, readback })
            }
            Kind::GteOp => {
                let mut data = [0u32; 32];
                let mut ctrl = [0u32; 32];
                for v in data.iter_mut().chain(ctrl.iter_mut()) {
                    *v = d.u32()?;
                }
                Vector::GteOp(GteOp { data, ctrl, cmd: d.u32()? })
            }
            Kind::SpuRun => {
                let samples = d.u32()?;
                let n = d.count()?;
                let mut ram = Vec::with_capacity(n);
                for _ in 0..n {
                    ram.push(SpuRam { addr: d.u32()?, bytes: BlobRef(d.hash()?) });
                }
                let n = d.count()?;
                let mut writes = Vec::with_capacity(n);
                for _ in 0..n {
                    let (sample, reg, value) = (d.u32()?, d.u32()?, d.u32()?);
                    ensure!(value <= 0xFFFF, "SPU write value {value:#x}");
                    writes.push(SpuWrite { sample, reg, value: value as u16 });
                }
                let captures = d.pairs()?;
                Vector::SpuRun(SpuRun { ram, writes, samples, captures })
            }
            Kind::BiosCall => {
                let table = d.u32()?;
                let func = d.u32()?;
                let args = [d.u32()?, d.u32()?, d.u32()?, d.u32()?];
                let n = d.count()?;
                let mut setup = Vec::with_capacity(n);
                for _ in 0..n {
                    setup.push((d.u32()?, d.bytes()?));
                }
                let captures = d.pairs()?;
                Vector::BiosCall(BiosCall { table, func, args, setup, captures })
            }
            Kind::CdXa => {
                let xa = BlobRef(d.hash()?);
                let (file_lba, start, mode, file, channel, samples) = (d.u32()?, d.u32()?, d.u32()?, d.u32()?, d.u32()?, d.u32()?);
                let captures = d.pairs()?;
                Vector::CdXa(CdXa { xa, file_lba, start, mode, file, channel, samples, captures })
            }
            Kind::MdecRun => {
                let n = d.count()?;
                let mut cmds = Vec::with_capacity(n);
                for _ in 0..n {
                    cmds.push(MdecCmd { words: d.words()?, out: d.u32()? });
                }
                Vector::MdecRun(MdecRun { cmds })
            }
            Kind::CdCmds => {
                let disc = BlobRef(d.hash()?);
                let (first_lba, end_lba, region) = (d.u32()?, d.u32()?, d.u32()?.to_le_bytes());
                let n = d.count()?;
                let mut cmds = Vec::with_capacity(n);
                for _ in 0..n {
                    cmds.push(CdCmd { cmd: d.u32()?, params: d.bytes()?, ints: d.u32()?, data: d.u32()? });
                }
                Vector::CdCmds(CdCmds { disc, first_lba, end_lba, region, cmds })
            }
            Kind::BiosThreads => {
                let (pc, tcbs) = (d.u32()?, d.u32()?);
                let n = d.count()?;
                let mut steps = Vec::with_capacity(n);
                for _ in 0..n {
                    steps.push([d.u32()?, d.u32()?, d.u32()?, d.u32()?]);
                }
                Vector::BiosThreads(BiosThreads { pc, tcbs, steps })
            }
        };
        ensure!(d.pos == bytes.len(), "{} trailing bytes after the vector", bytes.len() - d.pos);
        Ok(v)
    }
}

fn check_spu_range(addr: u32, len: u32) -> Result<()> {
    ensure!(
        addr.is_multiple_of(wire::SPU_ADDR_ALIGN) && len.is_multiple_of(wire::SPU_LEN_ALIGN) && addr <= wire::SPU_RAM_SIZE && len <= wire::SPU_RAM_SIZE - addr,
        "SPU RAM range {addr:#x}+{len:#x} unaligned or outside SPU RAM"
    );
    Ok(())
}

/// Check an SPU RAM write: aligned, inside SPU RAM, clear of the reserved block.
pub fn check_spu_write(addr: u32, len: u32) -> Result<()> {
    check_spu_range(addr, len)?;
    ensure!(
        addr + len <= SPU_RESERVED.start || addr >= SPU_RESERVED.end,
        "SPU RAM write {addr:#x}+{len:#x} overlaps the runner's reserved block"
    );
    Ok(())
}

fn check_scratch(addr: u32, len: u32) -> Result<()> {
    let end = wire::SCRATCH_BASE + wire::SCRATCH_SIZE;
    ensure!(
        addr >= wire::SCRATCH_BASE && addr <= end && len <= end - addr,
        "BIOS memory {addr:#x}+{len:#x} outside the scratch region"
    );
    Ok(())
}

#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn hash(&mut self, h: &Hash) {
        self.0.extend_from_slice(&h.0);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn words(&mut self, w: &[u32]) {
        self.u32(w.len() as u32);
        for v in w {
            self.u32(*v);
        }
    }
    fn rect(&mut self, r: &Rect) {
        for v in [r.x, r.y, r.w, r.h] {
            self.u16(v);
        }
    }
    fn rects(&mut self, rs: &[Rect]) {
        self.u32(rs.len() as u32);
        for r in rs {
            self.rect(r);
        }
    }
    fn vram_rects(&mut self, rs: &[VramRect]) {
        self.u32(rs.len() as u32);
        for r in rs {
            self.rect(&r.rect);
            self.hash(&r.pixels.0);
        }
    }
    fn pairs(&mut self, ps: &[(u32, u32)]) {
        self.u32(ps.len() as u32);
        for (a, b) in ps {
            self.u32(*a);
            self.u32(*b);
        }
    }
}

struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Dec<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(self.b.len() - self.pos >= n, "vector truncated at byte {}", self.pos);
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    /// A count, bounded by the bytes left so a corrupt file can't allocate.
    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        ensure!(n <= self.b.len() - self.pos, "count {n} exceeds the vector");
        Ok(n)
    }
    fn hash(&mut self) -> Result<Hash> {
        Ok(Hash(self.take(32)?.try_into()?))
    }
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.count()?;
        Ok(self.take(n)?.to_vec())
    }
    fn words(&mut self) -> Result<Vec<u32>> {
        let n = self.count()?;
        (0..n).map(|_| self.u32()).collect()
    }
    fn rect(&mut self) -> Result<Rect> {
        Ok(Rect { x: self.u16()?, y: self.u16()?, w: self.u16()?, h: self.u16()? })
    }
    fn rects(&mut self) -> Result<Vec<Rect>> {
        let n = self.count()?;
        (0..n).map(|_| self.rect()).collect()
    }
    fn vram_rects(&mut self) -> Result<Vec<VramRect>> {
        let n = self.count()?;
        (0..n).map(|_| Ok(VramRect { rect: self.rect()?, pixels: BlobRef(self.hash()?) })).collect()
    }
    fn pairs(&mut self) -> Result<Vec<(u32, u32)>> {
        let n = self.count()?;
        (0..n).map(|_| Ok((self.u32()?, self.u32()?))).collect()
    }
}

/// u16 pixels to LE bytes (the blob and result format for VRAM).
pub fn pixels_to_bytes(px: &[u16]) -> Vec<u8> {
    px.iter().flat_map(|p| p.to_le_bytes()).collect()
}

pub fn bytes_to_pixels(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vector> {
        let blob = BlobRef(Hash::of(b"pixels"));
        vec![
            Vector::Echo(b"hello".to_vec()),
            Vector::GpuDraw(GpuDraw {
                init: vec![VramRect { rect: Rect::new(0, 0, 4, 2), pixels: blob }],
                state: vec![0xE100_0000, 0xE600_0000],
                mode: GpuMode::List,
                packets: vec![vec![0x6000_00FF, 0, 0x0002_0004], vec![0x0100_0000]],
                readback: vec![Rect::new(0, 0, 4, 2)],
            }),
            Vector::GpuTransfer(GpuTransfer {
                init: vec![],
                state: vec![0xE600_0003],
                steps: vec![
                    TransferStep::CpuToVram(VramRect { rect: Rect::new(1, 2, 3, 4), pixels: blob }),
                    TransferStep::VramToCpu(Rect::new(1, 2, 3, 4)),
                    TransferStep::VramToVram { src_x: 1, src_y: 2, dst_x: 100, dst_y: 200, w: 3, h: 4 },
                ],
                readback: vec![Rect::new(100, 200, 3, 4)],
            }),
            Vector::GteOp(GteOp { data: [7; 32], ctrl: [9; 32], cmd: 0x0018_0001 }),
            Vector::SpuRun(SpuRun {
                ram: vec![SpuRam { addr: 0x2000, bytes: blob }],
                writes: vec![SpuWrite { sample: 0, reg: 0x1AA, value: 0xC000 }],
                samples: 512,
                captures: vec![(0, 0x1000)],
            }),
            Vector::BiosCall(BiosCall {
                table: 0xA0,
                func: 0x1B,
                args: [0x8010_0000, 0, 0, 0],
                setup: vec![(0x8010_0000, b"abc\0".to_vec())],
                captures: vec![(0x8010_0000, 4)],
            }),
            Vector::MdecRun(MdecRun {
                cmds: vec![MdecCmd { words: vec![0x6000_0000, 1, 2], out: 0 }, MdecCmd { words: vec![0x3800_0001, 0xFE00_0400], out: 128 }],
            }),
            Vector::CdCmds(CdCmds {
                disc: blob,
                first_lba: 40,
                end_lba: 1200,
                region: *b"SCEA",
                cmds: vec![CdCmd { cmd: 0x02, params: vec![0, 2, 0x40], ints: 1, data: 0 }, CdCmd { cmd: 0x06, params: vec![], ints: 3, data: 0x800 }],
            }),
            Vector::BiosThreads(BiosThreads { pc: 0x8010_0000, tcbs: 4, steps: vec![[0, 0, 0, 0], [2, 0, 0x8010_8000, 0], [3, 0, 0, 0]] }),
        ]
    }

    #[test]
    fn round_trip_and_ids() {
        let mut ids = std::collections::BTreeSet::new();
        for v in samples() {
            v.check().unwrap();
            let bytes = v.encode();
            assert_eq!(Vector::decode(&bytes).unwrap(), v);
            assert!(ids.insert(v.id()));
            let mut longer = bytes.clone();
            longer.push(0);
            assert!(Vector::decode(&longer).is_err());
            assert!(Vector::decode(&bytes[..bytes.len() - 1]).is_err());
        }
    }

    #[test]
    fn result_lengths() {
        let v = samples();
        assert_eq!(v[1].result_len(), 16);
        assert_eq!(v[2].result_len(), 24 + 24);
        assert_eq!(v[3].result_len(), 256);
        assert_eq!(v[4].result_len(), 0x260 + 0x1000);
        assert_eq!(v[5].result_len(), 8);
        assert_eq!(v[6].result_len(), 512);
        assert_eq!(v[7].result_len(), 8 + 24 + 3 * (24 + 0x800));
        assert_eq!(v[8].result_len(), 4 * (11 + 49 + 13));
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(Rect::new(1020, 0, 8, 1).check().is_err());
        assert!(check_spu_write(0x0FF0, 0x20).is_err());
        assert!(check_spu_write(0x2000, 0x20).is_ok());
        assert!(check_scratch(0x8000_0000, 4).is_err());
    }
}
