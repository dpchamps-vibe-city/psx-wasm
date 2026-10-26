//! The wire protocol between the harness and the runner (`oracle/runner/`):
//! the RAM mailbox, the runner's operations and their payload layouts, the
//! exchange files the emulator host reads and writes, and the vector kind
//! tags.
//!
//! This module is the single source of truth. The C header
//! `oracle/include/psxw_oracle_wire.h`, which the runner and the emulator
//! host include, is generated from it ([`c_header`], `psxw oracle
//! gen-header`), and a test fails when the committed header is stale.
//!
//! Every multi-byte field is little-endian. Every variable-length item is
//! padded to a multiple of 4 bytes so the runner can DMA from and to it.

/// Declares a group of `u32` wire constants and the table the header
/// generator walks. Doc comments become C comments.
macro_rules! wire_consts {
    ($table:ident, $title:literal, { $( $(#[doc = $doc:literal])+ $name:ident = $value:expr; )* }) => {
        $( $(#[doc = $doc])+ pub const $name: u32 = $value; )*
        pub(crate) const $table: Group = Group {
            title: $title,
            items: &[ $( (stringify!($name), $value, concat!($($doc, "\n"),+)) ),* ],
        };
    };
}

pub(crate) struct Group {
    title: &'static str,
    items: &'static [(&'static str, u32, &'static str)],
}

wire_consts!(VERSIONS, "Versions", {
    /// Version of everything in this file. Bumped on any layout change.
    WIRE_VERSION = 1;
    /// Version of the runner program. Part of every golden's target id, so
    /// a runner change that can alter results must bump it.
    RUNNER_VERSION = 1;
});

wire_consts!(MAILBOX, "Mailbox: a fixed region of main RAM shared by the runner and the host", {
    /// Default mailbox address (KSEG0). The host reads it through the
    /// core's system-RAM pointer at `MBX_ADDR & 0x1FFFFF`.
    MBX_ADDR = 0x801E_0000;
    /// Total mailbox size: header plus data area.
    MBX_SIZE = 0x0001_0000;
    /// `"PSXO"` read as a little-endian u32.
    MBX_MAGIC = 0x4F58_5350;
    /// Header size; the data area starts here.
    MBX_HEADER_SIZE = 0x40;
    /// Bytes available for one exchange's payload, and for its result.
    MBX_CAPACITY = 0x0001_0000 - 0x40;
    /// Header field: `MBX_MAGIC`, written by the runner at start-up.
    MBX_OFF_MAGIC = 0x00;
    /// Header field: `WIRE_VERSION` the runner was built with.
    MBX_OFF_VERSION = 0x04;
    /// Header field: one of the `MBX_STATE_*` values.
    MBX_OFF_STATE = 0x08;
    /// Header field: one of the `MBX_KIND_*` values (host → runner).
    MBX_OFF_KIND = 0x0C;
    /// Header field: payload length in bytes (host → runner).
    MBX_OFF_PAYLOAD_LEN = 0x10;
    /// Header field: result length in bytes (runner → host).
    MBX_OFF_RESULT_LEN = 0x14;
    /// Header field: exchange sequence number (host → runner).
    MBX_OFF_SEQ = 0x18;
    /// Header field: the sequence number the runner last completed.
    MBX_OFF_ACK_SEQ = 0x1C;
    /// Header field: an `ERR_*` code when the state is `MBX_STATE_ERROR`.
    MBX_OFF_ERROR = 0x20;
    /// Header field: `RUNNER_VERSION` the runner was built with.
    MBX_OFF_RUNNER_VERSION = 0x24;
    /// Header field: `MBX_CAPACITY` as the runner sees it.
    MBX_OFF_CAPACITY = 0x28;
    /// Header field: incremented by the runner while it waits (liveness).
    MBX_OFF_HEARTBEAT = 0x2C;
    /// Header field: which device wait last timed out (a `DIAG_*` code);
    /// the host clears it before each exchange and logs it when set.
    MBX_OFF_DIAG_WHAT = 0x30;
    /// Header field: the register value that wait last read.
    MBX_OFF_DIAG_VALUE = 0x34;
    /// Start of the data area (payload in, result out).
    MBX_OFF_DATA = 0x40;
    /// State: the runner has not started (RAM still zero).
    MBX_STATE_BOOT = 0;
    /// State: the runner is waiting for an exchange.
    MBX_STATE_READY = 1;
    /// State: the host wrote a payload; the runner should execute it.
    MBX_STATE_PENDING = 2;
    /// State: the runner wrote the result. The host may write the next
    /// payload and set `MBX_STATE_PENDING` again.
    MBX_STATE_DONE = 3;
    /// State: the exchange was rejected; `MBX_OFF_ERROR` says why.
    MBX_STATE_ERROR = 4;
    /// Kind: a batch of operations (`BATCH_*`).
    MBX_KIND_BATCH = 1;
});

wire_consts!(BATCH, "Batch framing: payload = u32 n_ops, then n_ops op records; result = n_ops result records", {
    /// Op record header: u32 op (`OP_*`), u32 len; then len bytes, padded to 4.
    BATCH_OP_HDR_SIZE = 8;
    /// Result record header: u32 status (`ERR_*`, 0 = ok), u32 len; then len
    /// bytes, padded to 4. After a failed op the rest are `ERR_SKIPPED`.
    BATCH_RES_HDR_SIZE = 8;
});

wire_consts!(ERRORS, "Error codes (mailbox error field and op status) and diagnostics", {
    /// Success.
    ERR_NONE = 0;
    /// Unknown mailbox kind.
    ERR_BAD_KIND = 0x10;
    /// Unknown op.
    ERR_BAD_OP = 0x11;
    /// A length field is inconsistent or exceeds a buffer.
    ERR_BAD_LEN = 0x12;
    /// An address or rectangle is outside what the op allows.
    ERR_RANGE = 0x13;
    /// A device did not reach the awaited state.
    ERR_TIMEOUT = 0x14;
    /// Not run because an earlier op in the batch failed.
    ERR_SKIPPED = 0x15;
    /// The results would not fit in the mailbox.
    ERR_RESULT_OVERFLOW = 0x16;
    /// Diagnostic: waiting on GPUSTAT (value: GPUSTAT).
    DIAG_GPUSTAT = 1;
    /// Diagnostic: waiting for DMA channel 2 (value: its CHCR).
    DIAG_DMA2 = 2;
    /// Diagnostic: waiting for DMA channel 4 (value: its CHCR).
    DIAG_DMA4 = 3;
    /// Diagnostic: waiting on SPUSTAT (value: SPUSTAT).
    DIAG_SPUSTAT = 4;
    /// Diagnostic: waiting for the SPU capture-half edge (value: SPUSTAT).
    DIAG_SPU_SYNC = 5;
    /// Diagnostic: waiting for a CD controller interrupt or idle (value:
    /// the status register, and the interrupt flags << 8).
    DIAG_CD = 6;
});

wire_consts!(OPS, "Runner operations", {
    /// Payload: any bytes. Result: the same bytes.
    OP_ECHO = 0x01;
    /// Payload: none. Result: `INFO_*` words.
    OP_INFO = 0x02;
    /// Payload: none. Result: none. GP1(00h) and GP1(01h); DMA2 stopped.
    /// VRAM is left as it is.
    OP_GPU_RESET = 0x10;
    /// Payload: rect (u16 x, y, w, h), then w*h u16 pixels. Result: none.
    /// CPU→VRAM through GP0(A0h) under the current mask settings (E6).
    OP_VRAM_WRITE = 0x11;
    /// Payload: rect. Result: w*h u16 pixels through GP0(C0h).
    OP_VRAM_READ = 0x12;
    /// Payload: u32 mode (`GPU_MODE_*`), u32 n_state, n_state words, u32
    /// n_packets, then per packet u32 n and n words. Result: none.
    /// Clears the texture cache (GP0(01h)), sends the state words by port,
    /// then the packets in the given mode, then waits for the GPU to idle.
    OP_GPU_EXEC = 0x13;
    /// Payload: u32 data[32], u32 ctrl[32], u32 cmd. Result: u32 data[32],
    /// u32 ctrl[32]. Control registers are written first (0..31), then the
    /// data registers except those in `GTE_DATA_SKIP_MASK`; then
    /// `cop2 cmd`; then all 64 are read back.
    OP_GTE_EXEC = 0x20;
    /// Payload: none. Result: none. Keys every voice off and writes the
    /// baseline register state described in `oracle/runner/spu.c`.
    OP_SPU_RESET = 0x30;
    /// Payload: u32 addr, u32 len, len bytes. Result: none. DMA4 write;
    /// addr is a multiple of `SPU_ADDR_ALIGN`, len of `SPU_LEN_ALIGN`.
    OP_SPU_RAM_WRITE = 0x31;
    /// Payload: u32 samples, u32 n_writes, n_writes × (u32 sample, u32 reg,
    /// u32 value), u32 n_captures, n_captures × (u32 addr, u32 len).
    /// Result: `SPU_SNAPSHOT_SIZE` bytes of registers, then each capture.
    /// Time starts at a capture-buffer half boundary (SPUSTAT bit 11 edge);
    /// a write with sample s happens s × `SPU_CYCLES_PER_SAMPLE` CPU cycles
    /// later; the snapshot is taken at `samples`.
    OP_SPU_RUN = 0x32;
    /// Payload: u32 table (0xA0, 0xB0 or 0xC0), u32 func, u32 args[4], u32
    /// n_setup, n_setup × (u32 addr, u32 len, len bytes), u32 n_captures,
    /// n_captures × (u32 addr, u32 len). Result: u32 v0, then each capture.
    /// Addresses lie in the scratch region.
    OP_BIOS_CALL = 0x40;
    /// Payload: u32 lba, u32 mode, u32 file, u32 channel, u32 samples, u32
    /// n_captures, n_captures × (u32 addr, u32 len). Result: each capture
    /// (SPU RAM). XA-ADPCM streaming, after SPU_RESET: SPUCNT C001h (CD
    /// audio in), main and CD volumes up; the CD's audio matrix L→L and
    /// R→R at 80h, ADPCM unmuted; then Setmode(mode), Setfilter(file,
    /// channel), Setloc(lba), ReadS. The captures are DMA-read `samples` ×
    /// `SPU_CYCLES_PER_SAMPLE` cycles after ReadS is acknowledged, while
    /// the SPU keeps running; then Pause. CD interrupts are acknowledged
    /// as they come.
    OP_CD_XA = 0x50;
});

wire_consts!(LAYOUT, "Payload layouts and limits", {
    /// Bytes in a rect: u16 x, y, w, h.
    RECT_SIZE = 8;
    /// VRAM width in pixels.
    VRAM_WIDTH = 1024;
    /// VRAM height in pixels.
    VRAM_HEIGHT = 512;
    /// GPU_EXEC mode: GP0 port writes, waiting for FIFO space per word.
    GPU_MODE_PORT = 0;
    /// GPU_EXEC mode: DMA2 sync mode 1 (block), one word per block.
    GPU_MODE_BLOCK = 1;
    /// GPU_EXEC mode: DMA2 sync mode 2 (linked list), one node per packet.
    GPU_MODE_LIST = 2;
    /// Words the runner can hold for building a linked list (packets plus
    /// one header word each).
    GPU_LIST_BUF_WORDS = 0x8000;
    /// Largest packet in port and block modes: the GP0 FIFO depth. Each
    /// packet (and each linked-list node) must hold whole commands.
    GPU_PACKET_MAX_WORDS = 16;
    /// GTE_EXEC payload size.
    GTE_EXEC_PAYLOAD_SIZE = 260;
    /// GTE_EXEC result size.
    GTE_EXEC_RESULT_SIZE = 256;
    /// Data registers GTE_EXEC does not write: 15 (SXYP, a FIFO push),
    /// 28 (IRGB, overwrites IR1-3), 29 (ORGB, read-only), 31 (LZCR,
    /// read-only).
    GTE_DATA_SKIP_MASK = (1 << 15) | (1 << 28) | (1 << 29) | (1 << 31);
    /// First SPU register in the snapshot (physical address).
    SPU_REG_BASE = 0x1F80_1C00;
    /// Snapshot of SPU registers 1F801C00h..1F801E5Fh as u16 reads; the
    /// data port 1F801DA8h reads as zero.
    SPU_SNAPSHOT_SIZE = 0x260;
    /// SPU RAM size in bytes.
    SPU_RAM_SIZE = 0x0008_0000;
    /// SPU RAM transfer address granularity.
    SPU_ADDR_ALIGN = 8;
    /// SPU RAM transfer length granularity (DMA4 block of 4 words).
    SPU_LEN_ALIGN = 16;
    /// CPU cycles per 44.1 kHz SPU sample.
    SPU_CYCLES_PER_SAMPLE = 768;
    /// Start of the RAM region BIOS_CALL may set up and capture (KSEG0).
    SCRATCH_BASE = 0x8010_0000;
    /// Size of the scratch region; it ends below the mailbox.
    SCRATCH_SIZE = 0x000C_0000;
    /// INFO result size: u32 runner_version, wire_version, mailbox address,
    /// capacity, scratch base, scratch size, image end, COP0 SR.
    INFO_RESULT_SIZE = 32;
});

wire_consts!(VECTOR_KINDS, "Vector kinds (the type tag in a vector's canonical encoding)", {
    /// Bytes echoed through the runner (harness self-check).
    VK_ECHO = 0;
    /// VRAM initial state, draw state words, GP0 packets, readback rects.
    VK_GPU_DRAW = 1;
    /// VRAM initial state, mask state, CPU↔VRAM and VRAM→VRAM transfers.
    VK_GPU_TRANSFER = 2;
    /// GTE registers in, one command, registers out.
    VK_GTE_OP = 3;
    /// SPU RAM initial state, timed register writes, N samples, captures.
    VK_SPU_RUN = 4;
    /// One BIOS A0/B0/C0 call with memory set-up and captures.
    VK_BIOS_CALL = 5;
    /// XA-ADPCM from the runner disc's test file, captured from the SPU's
    /// CD input buffers.
    VK_CD_XA = 6;
});

wire_consts!(EXCHANGE, "Exchange files (driver ↔ psxw-emu-host)", {
    /// Version of both exchange file formats.
    XCHG_VERSION = 1;
});

/// Exchange input file: magic, u32 version, u32 count, then per exchange
/// u32 kind, u32 payload_len, payload padded to 4.
pub const XCHG_IN_MAGIC: &[u8; 8] = b"PSXWXCHG";
/// Exchange output file: magic, u32 version, u32 count, then per exchange
/// u32 state, u32 error, u32 result_len, result padded to 4.
pub const XCHG_OUT_MAGIC: &[u8; 8] = b"PSXWXRES";

const GROUPS: &[&Group] = &[&VERSIONS, &MAILBOX, &BATCH, &ERRORS, &OPS, &LAYOUT, &VECTOR_KINDS, &EXCHANGE];

/// Round up to the wire's 4-byte item alignment.
pub const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// The generated C header, byte for byte.
pub fn c_header() -> String {
    use std::fmt::Write;
    let mut s = String::new();
    s.push_str("/* psxw_oracle_wire.h: GENERATED by `psxw oracle gen-header` from\n");
    s.push_str(" * crates/psxw-oracle/src/wire.rs. Do not edit; a test fails if it is stale. */\n\n");
    s.push_str("#ifndef PSXW_ORACLE_WIRE_H\n#define PSXW_ORACLE_WIRE_H\n\n");
    for g in GROUPS {
        let _ = writeln!(s, "/* ---- {} ---- */\n", g.title);
        for (name, value, doc) in g.items {
            for line in doc.lines() {
                let _ = writeln!(s, "/*{} */", line.replace("*/", "* /"));
            }
            let _ = writeln!(s, "#define PSXW_{name} 0x{value:08X}u\n");
        }
    }
    let _ = writeln!(s, "/* ---- Exchange file magics ---- */\n");
    let _ = writeln!(s, "#define PSXW_XCHG_IN_MAGIC \"{}\"", std::str::from_utf8(XCHG_IN_MAGIC).unwrap_or(""));
    let _ = writeln!(s, "#define PSXW_XCHG_OUT_MAGIC \"{}\"\n", std::str::from_utf8(XCHG_OUT_MAGIC).unwrap_or(""));
    s.push_str("#endif /* PSXW_ORACLE_WIRE_H */\n");
    s
}

/// Path of the committed header, relative to the workspace root.
pub const C_HEADER_PATH: &str = "oracle/include/psxw_oracle_wire.h";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_matches_layout() {
        assert_eq!(MBX_CAPACITY, MBX_SIZE - MBX_HEADER_SIZE);
        assert_eq!(MBX_OFF_DATA, MBX_HEADER_SIZE);
        assert!(SCRATCH_BASE + SCRATCH_SIZE <= MBX_ADDR);
        assert_eq!(GTE_EXEC_PAYLOAD_SIZE, 64 * 4 + 4);
    }

    #[test]
    fn names_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for g in GROUPS {
            for (name, _, _) in g.items {
                assert!(seen.insert(*name), "duplicate {name}");
            }
        }
    }

    /// The committed header must be what this module generates. Regenerate
    /// with `psxw oracle gen-header`.
    #[test]
    fn committed_header_is_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join(C_HEADER_PATH);
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == c_header(),
            "{} is stale; run `psxw oracle gen-header`",
            path.display()
        );
    }
}
