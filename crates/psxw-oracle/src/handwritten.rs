//! Handwritten vectors: a few of every kind, built by hand rather than
//! recorded from a game. They exercise the whole pipeline before any device
//! recorder exists, and their emulator goldens are the first provisional
//! references (GTE, SPU, BIOS, and GPU primitives that are not exact by
//! construction). They live in the `handwritten` set.

use crate::store::Store;
use crate::vector::*;
use crate::wire;
use anyhow::Result;

pub const SET: &str = "handwritten";

/// A named handwritten vector.
pub struct Handwritten {
    pub name: &'static str,
    pub vector: Vector,
}

/// Build every handwritten vector (storing the blobs they reference).
pub fn all(store: &Store) -> Result<Vec<Handwritten>> {
    let mut v = vec![Handwritten { name: "echo", vector: Vector::Echo(b"psx-wasm device oracle".to_vec()) }];
    v.extend(gte());
    v.extend(spu(store)?);
    v.extend(bios());
    v.extend(gpu(store)?);
    Ok(v)
}

/// Store every handwritten vector and add them to the `handwritten` set.
pub fn install(store: &Store) -> Result<Vec<(VectorId, &'static str)>> {
    let mut ids = Vec::new();
    for h in all(store)? {
        ids.push((store.put_vector(&h.vector)?, h.name));
    }
    store.add_to_set(SET, ids.iter().map(|(id, _)| *id))?;
    Ok(ids)
}

fn pair(lo: i32, hi: i32) -> u32 {
    (lo as u32 & 0xFFFF) | (hi as u32) << 16
}

/// A plausible GTE state: rotation, translation, light and colour matrices,
/// projection, depth cueing, three vertices and the FIFOs.
fn gte_state() -> ([u32; 32], [u32; 32]) {
    let mut c = [0u32; 32];
    c[0] = pair(0x0F00, 0x0200); // R11 R12
    c[1] = pair(-0x0100, -0x0200); // R13 R21
    c[2] = pair(0x0E00, 0x0300); // R22 R23
    c[3] = pair(0x0100, -0x0300); // R31 R32
    c[4] = 0x0F80; // R33
    c[5] = 100; // TRX
    c[6] = (-200i32) as u32; // TRY
    c[7] = 3000; // TRZ
    c[8] = pair(0x0800, -0x0400); // L11 L12
    c[9] = pair(0x0200, 0x0100); // L13 L21
    c[10] = pair(0x0C00, -0x0100); // L22 L23
    c[11] = pair(-0x0600, 0x0400); // L31 L32
    c[12] = 0x0900; // L33
    c[13] = 0x0100; // RBK
    c[14] = 0x0200; // GBK
    c[15] = 0x0300; // BBK
    c[16] = pair(0x1000, 0x0000); // LR1 LR2
    c[17] = pair(0x0000, 0x0000); // LR3 LG1
    c[18] = pair(0x1000, 0x0000); // LG2 LG3
    c[19] = pair(0x0000, 0x0000); // LB1 LB2
    c[20] = 0x1000; // LB3
    c[21] = 0x0800; // RFC
    c[22] = 0x0400; // GFC
    c[23] = 0x0200; // BFC
    c[24] = 160 << 16; // OFX
    c[25] = 120 << 16; // OFY
    c[26] = 300; // H
    c[27] = (-0x010A_i32) as u32; // DQA
    c[28] = 0x0140_0000; // DQB
    c[29] = 0x0155; // ZSF3
    c[30] = 0x0100; // ZSF4
    let mut d = [0u32; 32];
    d[0] = pair(100, -50); // VXY0
    d[1] = 2000; // VZ0
    d[2] = pair(-300, 120); // VXY1
    d[3] = 2500; // VZ1
    d[4] = pair(40, 260); // VXY2
    d[5] = 1800; // VZ2
    d[6] = 0x3060_8040; // RGBC: code 30h, R 40h, G 80h, B 60h
    d[8] = 0x0800; // IR0
    d[9] = 0x0123; // IR1
    d[10] = (-0x0456i32) as u32 & 0xFFFF; // IR2 (sign-extended on read)
    d[11] = 0x0789; // IR3
    d[12] = pair(10, 20); // SXY0
    d[13] = pair(200, 30); // SXY1
    d[14] = pair(120, 180); // SXY2
    d[16] = 1000; // SZ0
    d[17] = 1100; // SZ1
    d[18] = 1200; // SZ2
    d[19] = 1300; // SZ3
    d[20] = 0x0010_2030; // RGB0
    d[21] = 0x0040_5060; // RGB1
    d[22] = 0x0070_8090; // RGB2
    d[30] = 0x0000_F000; // LZCS
    (d, c)
}

/// GTE command words as the SDK encodes them.
fn gte() -> Vec<Handwritten> {
    let (d, c) = gte_state();
    let op = |name, cmd| Handwritten { name, vector: Vector::GteOp(GteOp { data: d, ctrl: c, cmd }) };
    let mut v = vec![
        op("gte_rtps", 0x0018_0001),
        op("gte_rtpt", 0x0028_0030),
        op("gte_nclip", 0x0140_0006),
        op("gte_avsz3", 0x0158_002D),
        op("gte_avsz4", 0x0168_002E),
        op("gte_mvmva_rt_v0_tr", 0x0048_6012),
        op("gte_ncds", 0x00E8_0413),
        op("gte_nccs", 0x0108_041B),
        op("gte_sqr", 0x00A8_0428),
        op("gte_op", 0x0178_000C),
        op("gte_gpf", 0x0198_003D),
        op("gte_dpcs", 0x0078_0010),
    ];
    // Saturation: a far-off vertex overflows MAC and SXY, setting FLAG bits.
    let mut big = d;
    big[0] = pair(0x7FFF, -0x8000);
    big[1] = 0x7FFF;
    v.push(Handwritten { name: "gte_rtps_saturate", vector: Vector::GteOp(GteOp { data: big, ctrl: c, cmd: 0x0018_0001 }) });
    v
}

/// Four ADPCM blocks of a square wave; the last loops to the first.
fn adpcm_square() -> Vec<u8> {
    let mut out = Vec::new();
    for block in 0..4 {
        out.push(0x00); // shift 0, filter 0
        out.push(match block {
            0 => 0x04, // loop start
            3 => 0x03, // loop end + repeat
            _ => 0x00,
        });
        for i in 0..14u32 {
            let nib = |k: u32| if (2 * i + k) % 28 < 14 { 0x7u8 } else { 0x8u8 };
            out.push(nib(0) | nib(1) << 4);
        }
    }
    out
}

fn spu(store: &Store) -> Result<Vec<Handwritten>> {
    let sample = store.put_blob(&adpcm_square())?;
    let addr = 0x2000u32;
    let mut writes = vec![
        SpuWrite { sample: 0, reg: 0x1AA, value: 0xC000 }, // SPU on, unmuted
        SpuWrite { sample: 0, reg: 0x180, value: 0x3FFF }, // main volume L
        SpuWrite { sample: 0, reg: 0x182, value: 0x3FFF }, // main volume R
    ];
    for (voice, pitch) in [(1u32, 0x1000u16), (3, 0x0800)] {
        let b = 16 * voice;
        for (off, value) in [(0x0, 0x3FFF), (0x2, 0x3FFF), (0x4, pitch), (0x6, (addr >> 3) as u16), (0x8, 0x00FF), (0xA, 0x0000)] {
            writes.push(SpuWrite { sample: 0, reg: b + off, value });
        }
    }
    let mut noise = writes.clone();
    writes.push(SpuWrite { sample: 2, reg: 0x188, value: 0x000A }); // KON voices 1, 3
    writes.push(SpuWrite { sample: 400, reg: 0x18C, value: 0x000A }); // KOFF
    noise.push(SpuWrite { sample: 1, reg: 0x1AA, value: 0xC000 | 0x0A << 8 }); // noise clock
    noise.push(SpuWrite { sample: 1, reg: 0x194, value: 0x0008 }); // NON: voice 3 plays noise
    noise.push(SpuWrite { sample: 2, reg: 0x188, value: 0x000A });
    let captures = vec![(0x0000, 0x1000), (addr, 0x40)];
    Ok(vec![
        Handwritten {
            name: "spu_square_voices_1_3",
            vector: Vector::SpuRun(SpuRun { ram: vec![SpuRam { addr, bytes: sample }], writes, samples: 512, captures: captures.clone() }),
        },
        Handwritten {
            name: "spu_noise_voice_3",
            vector: Vector::SpuRun(SpuRun { ram: vec![SpuRam { addr, bytes: sample }], writes: noise, samples: 300, captures }),
        },
    ])
}

fn bios() -> Vec<Handwritten> {
    let s = wire::SCRATCH_BASE;
    let call = |name, func, args: [u32; 4], setup: Vec<(u32, Vec<u8>)>, captures: Vec<(u32, u32)>| Handwritten {
        name,
        vector: Vector::BiosCall(BiosCall { table: 0xA0, func, args, setup, captures }),
    };
    vec![
        call("bios_strlen", 0x1B, [s, 0, 0, 0], vec![(s, b"psx-wasm device oracle\0".to_vec())], vec![]),
        call("bios_toupper", 0x25, [b'q' as u32, 0, 0, 0], vec![], vec![]),
        call("bios_strcmp", 0x17, [s, s + 16, 0, 0], vec![(s, b"abc\0".to_vec()), (s + 16, b"abd\0".to_vec())], vec![]),
        call(
            "bios_memset",
            0x2B,
            [s + 4, 0x5A, 24, 0],
            vec![(s, vec![0x11; 32])],
            vec![(s, 32)],
        ),
        call(
            "bios_memcpy",
            0x2A,
            [s + 64, s, 20, 0],
            vec![(s, (0u8..32).collect()), (s + 64, vec![0xEE; 32])],
            vec![(s + 64, 32)],
        ),
    ]
}

/// GPU work that is not exact by construction: shaded, dithered and
/// semi-transparent primitives, and mask-bit transfers. Provisional.
fn gpu(store: &Store) -> Result<Vec<Handwritten>> {
    let area = Rect::new(0, 0, 64, 64);
    let mut rng = crate::exact::Rng::new(0x5053_5857);
    let base: Vec<u16> = (0..area.pixels()).map(|_| rng.u16()).collect();
    let init = VramRect { rect: area, pixels: store.put_blob(&pixels_to_bytes(&base))? };
    let state = |e1: u32, e6: u32| vec![0xE100_0000 | e1, 0xE200_0000, 0xE300_0000, 0xE400_0000 | 511 << 10 | 1023, 0xE500_0000, 0xE600_0000 | e6];
    let triangle = vec![0x3000_00FF, pair(4, 2), 0x0000_FF00, pair(60, 10), 0x00FF_0000, pair(20, 58)];
    let semi_rects: Vec<Vec<u32>> = (0..4u32)
        .map(|abr| vec![0xE100_0000 | abr << 5 | 1 << 10, 0x6200_0000 | 0x80_4020 >> abr, pair(4 + 14 * abr as i32, 4), pair(12, 40)])
        .collect();
    let mut transfer_px: Vec<u16> = (0..16 * 16).map(|_| rng.u16()).collect();
    transfer_px[0] = 0x8000;
    let transfer = store.put_blob(&pixels_to_bytes(&transfer_px))?;
    Ok(vec![
        Handwritten {
            name: "gpu_gouraud_triangle_dither",
            vector: Vector::GpuDraw(GpuDraw {
                init: vec![init.clone()],
                state: state(1 << 9 | 1 << 10, 0),
                mode: GpuMode::List,
                packets: vec![triangle],
                readback: vec![area],
            }),
        },
        Handwritten {
            name: "gpu_semi_transparent_rects",
            vector: Vector::GpuDraw(GpuDraw {
                init: vec![init.clone()],
                state: state(1 << 10, 0),
                mode: GpuMode::Block,
                packets: semi_rects,
                readback: vec![area],
            }),
        },
        Handwritten {
            name: "gpu_transfer_mask_bits",
            vector: Vector::GpuTransfer(GpuTransfer {
                init: vec![init],
                state: vec![0xE600_0003], // set mask on write, check mask
                steps: vec![
                    TransferStep::CpuToVram(VramRect { rect: Rect::new(8, 8, 16, 16), pixels: transfer }),
                    TransferStep::VramToVram { src_x: 8, src_y: 8, dst_x: 30, dst_y: 30, w: 16, h: 16 },
                ],
                readback: vec![area],
            }),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handwritten_vectors_compile() {
        let dir = std::env::temp_dir().join(format!("psxw-oracle-hw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let all = all(&store).unwrap();
        // XA vectors name sectors on the runner disc, so xa_disc makes them
        // against the built disc instead.
        for k in Kind::ALL.into_iter().filter(|k| *k != Kind::CdXa) {
            assert!(all.iter().any(|h| h.vector.kind() == k), "no handwritten {k}");
        }
        for h in &all {
            crate::script::compile(&h.vector, &store).unwrap_or_else(|e| panic!("{}: {e:#}", h.name));
        }
        assert_eq!(adpcm_square().len(), 64);
        let ids = install(&store).unwrap();
        assert_eq!(store.set(SET).unwrap().len(), ids.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
