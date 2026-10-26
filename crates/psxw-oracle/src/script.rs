//! Compiling vectors into runner operations, packing operations into
//! mailbox batches, and splitting batch results back into vector results.
//!
//! A vector's semantics live here, not in the runner: GPU_DRAW becomes
//! GPU_RESET, VRAM_WRITE chunks, GPU_EXEC and VRAM_READ chunks. Large state
//! is chunked so every op fits one mailbox exchange; device state persists
//! between exchanges, so chunking does not change results. Every vector
//! starts with its device's reset op, so results do not depend on which
//! vectors ran before it in the same batch.

use crate::store::Store;
use crate::vector::{GpuMode, Rect, TransferStep, Vector, VramRect, check_spu_write};
use crate::wire::{self, pad4};
use anyhow::{Result, bail, ensure};

/// Largest op payload: the mailbox minus the batch count and op header.
pub const MAX_OP_PAYLOAD: usize = (wire::MBX_CAPACITY - 4 - wire::BATCH_OP_HDR_SIZE) as usize;
/// Largest op result: the mailbox minus the result record header.
pub const MAX_OP_RESULT: usize = (wire::MBX_CAPACITY - wire::BATCH_RES_HDR_SIZE) as usize;

/// One runner operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    pub code: u32,
    pub payload: Vec<u8>,
    /// Exact result length the op produces.
    pub result_len: usize,
    /// Whether the result is part of the vector's result.
    pub keep: bool,
}

impl Op {
    fn new(code: u32, payload: Vec<u8>) -> Self {
        Op { code, payload, result_len: 0, keep: false }
    }
    fn keeping(code: u32, payload: Vec<u8>, result_len: usize) -> Self {
        Op { code, payload, result_len, keep: true }
    }
}

fn le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn rect_bytes(out: &mut Vec<u8>, r: &Rect) {
    for v in [r.x, r.y, r.w, r.h] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn pad(out: &mut Vec<u8>) {
    out.resize(pad4(out.len()), 0);
}

/// Rows per chunk so that `rows * w * 2` bytes (padded) fit `room`.
fn rows_per_chunk(w: u16, room: usize) -> usize {
    let row = 2 * w as usize;
    ((room & !3) / row).max(1)
}

/// VRAM_WRITE ops for a rectangle, split into row bands.
fn vram_write(r: &VramRect, store: &Store, ops: &mut Vec<Op>) -> Result<()> {
    let px = store.blob(&r.pixels)?;
    ensure!(px.len() == 2 * r.rect.pixels(), "blob {} has {} bytes for rect {:?}", r.pixels.0.short(), px.len(), r.rect);
    let rows = rows_per_chunk(r.rect.w, MAX_OP_PAYLOAD - wire::RECT_SIZE as usize);
    let row_bytes = 2 * r.rect.w as usize;
    for (i, band) in px.chunks(rows * row_bytes).enumerate() {
        let sub = Rect::new(r.rect.x, r.rect.y + (i * rows) as u16, r.rect.w, (band.len() / row_bytes) as u16);
        let mut p = Vec::with_capacity(8 + band.len() + 2);
        rect_bytes(&mut p, &sub);
        p.extend_from_slice(band);
        pad(&mut p);
        ops.push(Op::new(wire::OP_VRAM_WRITE, p));
    }
    Ok(())
}

/// VRAM_READ ops for a rectangle, split into row bands; results kept.
fn vram_read(r: &Rect, ops: &mut Vec<Op>) {
    let rows = rows_per_chunk(r.w, MAX_OP_RESULT);
    let mut y = 0usize;
    while y < r.h as usize {
        let h = rows.min(r.h as usize - y);
        let sub = Rect::new(r.x, r.y + y as u16, r.w, h as u16);
        let mut p = Vec::with_capacity(8);
        rect_bytes(&mut p, &sub);
        ops.push(Op::keeping(wire::OP_VRAM_READ, p, 2 * sub.pixels()));
        y += h;
    }
}

fn gpu_exec(mode: GpuMode, state: &[u32], packets: &[Vec<u32>]) -> Result<Op> {
    let mut p = Vec::new();
    le(&mut p, mode.wire());
    le(&mut p, state.len() as u32);
    state.iter().for_each(|w| le(&mut p, *w));
    le(&mut p, packets.len() as u32);
    let mut words = 0usize;
    for pk in packets {
        le(&mut p, pk.len() as u32);
        pk.iter().for_each(|w| le(&mut p, *w));
        words += pk.len() + 1;
    }
    ensure!(p.len() <= MAX_OP_PAYLOAD, "GPU command list of {} bytes does not fit one exchange", p.len());
    ensure!(words <= wire::GPU_LIST_BUF_WORDS as usize, "GPU command list of {words} words exceeds the runner's buffer");
    Ok(Op::new(wire::OP_GPU_EXEC, p))
}

/// The ops that execute `v` on a target.
pub fn compile(v: &Vector, store: &Store) -> Result<Vec<Op>> {
    v.check()?;
    let mut ops = Vec::new();
    match v {
        Vector::Echo(b) => ops.push(Op::keeping(wire::OP_ECHO, b.clone(), b.len())),
        Vector::GpuDraw(d) => {
            ops.push(Op::new(wire::OP_GPU_RESET, Vec::new()));
            for r in &d.init {
                vram_write(r, store, &mut ops)?;
            }
            ops.push(gpu_exec(d.mode, &d.state, &d.packets)?);
            for r in &d.readback {
                vram_read(r, &mut ops);
            }
        }
        Vector::GpuTransfer(t) => {
            ops.push(Op::new(wire::OP_GPU_RESET, Vec::new()));
            for r in &t.init {
                vram_write(r, store, &mut ops)?;
            }
            ops.push(gpu_exec(GpuMode::Port, &t.state, &[])?);
            for s in &t.steps {
                match s {
                    TransferStep::CpuToVram(r) => vram_write(r, store, &mut ops)?,
                    TransferStep::VramToCpu(r) => vram_read(r, &mut ops),
                    TransferStep::VramToVram { src_x, src_y, dst_x, dst_y, w, h } => {
                        let packet = vec![
                            0x8000_0000,
                            (*src_y as u32) << 16 | *src_x as u32,
                            (*dst_y as u32) << 16 | *dst_x as u32,
                            (*h as u32) << 16 | *w as u32,
                        ];
                        ops.push(gpu_exec(GpuMode::Port, &[], &[packet])?);
                    }
                }
            }
            for r in &t.readback {
                vram_read(r, &mut ops);
            }
        }
        Vector::GteOp(g) => {
            let mut p = Vec::with_capacity(wire::GTE_EXEC_PAYLOAD_SIZE as usize);
            g.data.iter().chain(&g.ctrl).for_each(|w| le(&mut p, *w));
            le(&mut p, g.cmd);
            ops.push(Op::keeping(wire::OP_GTE_EXEC, p, wire::GTE_EXEC_RESULT_SIZE as usize));
        }
        Vector::CdXa(x) => {
            let sectors = store.blob(&x.xa)?.len() / crate::xa_disc::SECTOR;
            ensure!((x.start as usize) < sectors, "XA start sector {} past the file's {sectors}", x.start);
            ops.push(Op::new(wire::OP_SPU_RESET, Vec::new()));
            let mut p = Vec::new();
            for v in [x.file_lba + x.start, x.mode, x.file, x.channel, x.samples] {
                le(&mut p, v);
            }
            le(&mut p, x.captures.len() as u32);
            for (a, n) in &x.captures {
                le(&mut p, *a);
                le(&mut p, *n);
            }
            ensure!(v.result_len() <= MAX_OP_RESULT, "XA run captures {} bytes, more than one exchange", v.result_len());
            ops.push(Op::keeping(wire::OP_CD_XA, p, v.result_len()));
        }
        Vector::SpuRun(s) => {
            ops.push(Op::new(wire::OP_SPU_RESET, Vec::new()));
            for r in &s.ram {
                let bytes = store.blob(&r.bytes)?;
                check_spu_write(r.addr, bytes.len() as u32)?;
                let chunk = (MAX_OP_PAYLOAD - 8) & !(wire::SPU_LEN_ALIGN as usize - 1);
                for (i, part) in bytes.chunks(chunk).enumerate() {
                    let mut p = Vec::with_capacity(8 + part.len());
                    le(&mut p, r.addr + (i * chunk) as u32);
                    le(&mut p, part.len() as u32);
                    p.extend_from_slice(part);
                    ops.push(Op::new(wire::OP_SPU_RAM_WRITE, p));
                }
            }
            let mut p = Vec::new();
            le(&mut p, s.samples);
            le(&mut p, s.writes.len() as u32);
            for w in &s.writes {
                le(&mut p, w.sample);
                le(&mut p, w.reg);
                le(&mut p, w.value as u32);
            }
            le(&mut p, s.captures.len() as u32);
            for (a, n) in &s.captures {
                le(&mut p, *a);
                le(&mut p, *n);
            }
            ensure!(p.len() <= MAX_OP_PAYLOAD, "SPU run payload too large");
            ensure!(v.result_len() <= MAX_OP_RESULT, "SPU run captures {} bytes, more than one exchange", v.result_len());
            ops.push(Op::keeping(wire::OP_SPU_RUN, p, v.result_len()));
        }
        Vector::BiosCall(b) => {
            let mut p = Vec::new();
            le(&mut p, b.table);
            le(&mut p, b.func);
            b.args.iter().for_each(|a| le(&mut p, *a));
            le(&mut p, b.setup.len() as u32);
            for (addr, bytes) in &b.setup {
                le(&mut p, *addr);
                le(&mut p, bytes.len() as u32);
                p.extend_from_slice(bytes);
                pad(&mut p);
            }
            le(&mut p, b.captures.len() as u32);
            for (a, n) in &b.captures {
                le(&mut p, *a);
                le(&mut p, *n);
            }
            ensure!(p.len() <= MAX_OP_PAYLOAD, "BIOS call payload too large");
            ensure!(v.result_len() <= MAX_OP_RESULT, "BIOS call captures too much");
            ops.push(Op::keeping(wire::OP_BIOS_CALL, p, v.result_len()));
        }
    }
    for op in &ops {
        ensure!(op.payload.len() <= MAX_OP_PAYLOAD && op.result_len <= MAX_OP_RESULT, "op {:#x} too large", op.code);
    }
    Ok(ops)
}

/// Batches to send, and which (vector, op) each batch slot belongs to.
#[derive(Debug, Default)]
pub struct Plan {
    pub batches: Vec<Vec<u8>>,
    pub slots: Vec<Vec<(usize, usize)>>,
}

/// Pack ops into batches in order, as many per exchange as fit. An
/// OP_CD_XA plays up to a second or so of audio, so a batch holds at most
/// one, to keep each exchange well inside the host's time limit.
pub fn pack(ops: &[Vec<Op>]) -> Plan {
    let cap = wire::MBX_CAPACITY as usize;
    let mut plan = Plan::default();
    let mut cur: Vec<(usize, usize)> = Vec::new();
    let (mut in_len, mut out_len) = (4usize, 0usize);
    let flush = |plan: &mut Plan, cur: &mut Vec<(usize, usize)>| {
        if cur.is_empty() {
            return;
        }
        let mut b = Vec::new();
        le(&mut b, cur.len() as u32);
        for &(v, o) in cur.iter() {
            let op = &ops[v][o];
            le(&mut b, op.code);
            le(&mut b, op.payload.len() as u32);
            b.extend_from_slice(&op.payload);
            pad(&mut b);
        }
        plan.batches.push(b);
        plan.slots.push(std::mem::take(cur));
    };
    let mut xa_in_batch = false;
    for (v, vops) in ops.iter().enumerate() {
        let is_xa = vops.iter().any(|op| op.code == wire::OP_CD_XA);
        if is_xa && xa_in_batch {
            flush(&mut plan, &mut cur);
            in_len = 4;
            out_len = 0;
            xa_in_batch = false;
        }
        xa_in_batch |= is_xa;
        for (o, op) in vops.iter().enumerate() {
            let need_in = wire::BATCH_OP_HDR_SIZE as usize + pad4(op.payload.len());
            let need_out = wire::BATCH_RES_HDR_SIZE as usize + pad4(op.result_len);
            if in_len + need_in > cap || out_len + need_out > cap {
                flush(&mut plan, &mut cur);
                in_len = 4;
                out_len = 0;
                xa_in_batch = is_xa;
            }
            cur.push((v, o));
            in_len += need_in;
            out_len += need_out;
        }
    }
    flush(&mut plan, &mut cur);
    plan
}

/// One exchange's outcome as the host reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeResult {
    pub state: u32,
    pub error: u32,
    pub data: Vec<u8>,
}

/// Per-op status and bytes, from one batch's result.
pub fn split_batch(r: &ExchangeResult, n_ops: usize) -> Result<Vec<(u32, Vec<u8>)>> {
    if r.state != wire::MBX_STATE_DONE {
        let code = if r.error != 0 { r.error } else { wire::ERR_BAD_KIND };
        return Ok(vec![(code, Vec::new()); n_ops]);
    }
    let mut out = Vec::with_capacity(n_ops);
    let mut pos = 0usize;
    for _ in 0..n_ops {
        ensure!(r.data.len() >= pos + 8, "batch result truncated");
        let st = u32::from_le_bytes(r.data[pos..pos + 4].try_into()?);
        let len = u32::from_le_bytes(r.data[pos + 4..pos + 8].try_into()?) as usize;
        pos += 8;
        ensure!(r.data.len() >= pos + len, "batch result truncated");
        out.push((st, r.data[pos..pos + len].to_vec()));
        pos += pad4(len);
    }
    ensure!(pos == r.data.len(), "batch result is {} bytes, records cover {pos}", r.data.len());
    Ok(out)
}

/// Name of a runner error code, for reports.
pub fn error_name(code: u32) -> &'static str {
    match code {
        wire::ERR_NONE => "ok",
        wire::ERR_BAD_KIND => "bad mailbox kind",
        wire::ERR_BAD_OP => "unknown op",
        wire::ERR_BAD_LEN => "bad length",
        wire::ERR_RANGE => "out of range",
        wire::ERR_TIMEOUT => "device timeout",
        wire::ERR_SKIPPED => "skipped after an earlier failure",
        wire::ERR_RESULT_OVERFLOW => "result overflow",
        _ => "unknown error",
    }
}

/// Assemble a vector's result from its ops' results.
pub fn assemble(ops: &[Op], results: &[(u32, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (op, (st, bytes)) in ops.iter().zip(results) {
        if *st != wire::ERR_NONE {
            bail!("op {:#04x}: {} ({st:#x})", op.code, error_name(*st));
        }
        ensure!(bytes.len() == op.result_len, "op {:#04x} returned {} bytes, expected {}", op.code, bytes.len(), op.result_len);
        if op.keep {
            out.extend_from_slice(bytes);
        }
    }
    Ok(out)
}

/// Exchange input file (`PSXWXCHG`): one batch per exchange.
pub fn encode_exchanges(batches: &[Vec<u8>]) -> Vec<u8> {
    let mut out = wire::XCHG_IN_MAGIC.to_vec();
    le(&mut out, wire::XCHG_VERSION);
    le(&mut out, batches.len() as u32);
    for b in batches {
        le(&mut out, wire::MBX_KIND_BATCH);
        le(&mut out, b.len() as u32);
        out.extend_from_slice(b);
        pad(&mut out);
    }
    out
}

/// Exchange output file (`PSXWXRES`).
pub fn decode_results(bytes: &[u8]) -> Result<Vec<ExchangeResult>> {
    ensure!(bytes.len() >= 16 && &bytes[..8] == wire::XCHG_OUT_MAGIC, "not an exchange result file");
    let word = |p: usize| -> Result<u32> {
        ensure!(bytes.len() >= p + 4, "exchange result file truncated");
        Ok(u32::from_le_bytes(bytes[p..p + 4].try_into()?))
    };
    ensure!(word(8)? == wire::XCHG_VERSION, "exchange result version {}", word(8)?);
    let n = word(12)? as usize;
    let mut pos = 16;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let (state, error, len) = (word(pos)?, word(pos + 4)?, word(pos + 8)? as usize);
        pos += 12;
        ensure!(bytes.len() >= pos + len, "exchange result file truncated");
        out.push(ExchangeResult { state, error, data: bytes[pos..pos + len].to_vec() });
        pos += pad4(len);
    }
    ensure!(pos == bytes.len(), "exchange result file has trailing bytes");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{GpuDraw, Rect, VramRect};

    fn store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-script-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn big_rects_are_chunked_and_packed() {
        let s = store("chunk");
        let rect = Rect::new(0, 0, 1024, 512);
        let px = s.put_blob(&vec![0x55; 2 * rect.pixels()]).unwrap();
        let v = Vector::GpuDraw(GpuDraw {
            init: vec![VramRect { rect, pixels: px }],
            state: vec![],
            mode: GpuMode::Port,
            packets: vec![],
            readback: vec![rect],
        });
        let ops = compile(&v, &s).unwrap();
        let writes = ops.iter().filter(|o| o.code == wire::OP_VRAM_WRITE).count();
        let reads: Vec<_> = ops.iter().filter(|o| o.code == wire::OP_VRAM_READ).collect();
        assert!(writes >= 16 && reads.len() >= 16);
        assert_eq!(reads.iter().map(|o| o.result_len).sum::<usize>(), v.result_len());
        let plan = pack(&[ops.clone()]);
        assert!(plan.batches.iter().all(|b| b.len() <= wire::MBX_CAPACITY as usize));
        assert_eq!(plan.slots.iter().map(Vec::len).sum::<usize>(), ops.len());
        let _ = std::fs::remove_dir_all(s.root());
    }

    #[test]
    fn exchange_files_round_trip() {
        let enc = encode_exchanges(&[vec![1, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 9, 9, 9, 0]]);
        assert_eq!(&enc[..8], b"PSXWXCHG");
        let mut res = wire::XCHG_OUT_MAGIC.to_vec();
        for v in [1u32, 1, wire::MBX_STATE_DONE, 0, 12] {
            res.extend_from_slice(&v.to_le_bytes());
        }
        res.extend_from_slice(&[0, 0, 0, 0, 3, 0, 0, 0, 9, 9, 9, 0]);
        let r = decode_results(&res).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(split_batch(&r[0], 1).unwrap(), vec![(0, vec![9, 9, 9])]);
        assert!(split_batch(&r[0], 2).is_err());
        let failed = ExchangeResult { state: wire::MBX_STATE_ERROR, error: wire::ERR_BAD_LEN, data: vec![] };
        assert_eq!(split_batch(&failed, 2).unwrap()[1].0, wire::ERR_BAD_LEN);
    }
}
