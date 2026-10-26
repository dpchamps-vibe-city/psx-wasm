//! The backend's present path in the pinned render image: a 15-bit and a
//! 24-bit display area become the RGBA8 frame the host shows. Not an
//! oracle (no device target sees presentation); it checks the conversion
//! against its definition. Skips without Docker.

use psx_devices::gpu::Gpu;
use psxw_abi::render::{PresentDesc, header, op};
use psxw_inputs::Workspace;

fn docker_ready() -> bool {
    psxw_oracle::images::docker_available()
}

/// GP0 words uploading `w`×`h` pixels from `f` at (x, y).
fn upload(gpu: &mut Gpu, x: u32, y: u32, w: u32, h: u32, f: impl Fn(u32, u32) -> u16) {
    gpu.gp0_words(&[0xA000_0000, y << 16 | x, h << 16 | w]);
    let px: Vec<u16> = (0..h).flat_map(|j| (0..w).map(move |i| (i, j))).map(|(i, j)| f(i, j)).collect();
    for pair in px.chunks(2) {
        gpu.gp0(pair[0] as u32 | (pair.get(1).copied().unwrap_or(0) as u32) << 16);
    }
}

#[test]
fn present_converts_15_and_24_bit_display_areas() {
    if !docker_ready() {
        eprintln!("skipping: docker is not available");
        return;
    }
    let ws = Workspace::discover().expect("workspace");
    let mut dev = psxw_gpu_oracle::GpuDevice::new(&ws);
    dev.reset().expect("render server");

    let pattern = |i: u32, j: u32| ((i * 7 + j * 13) as u16 & 0x7FFF) | ((i ^ j) as u16 & 1) << 15;
    let mut gpu = Gpu::new();
    gpu.gp1(0x0300_0000); // display on
    gpu.gp1(0x0500_0000 | 16 << 10 | 64); // display at (64, 16)
    gpu.gp1(0x0600_0000 | (0x260 + 256 * 10) << 12 | 0x260); // 256 dots
    gpu.gp1(0x0700_0000 | (0x10 + 32) << 10 | 0x10); // 32 lines
    gpu.gp1(0x0800_0000); // 256x240, 15-bit
    upload(&mut gpu, 64, 16, 256, 32, pattern);
    let mut stream = gpu.take_commands();
    let desc = gpu.present_desc();
    assert_eq!(desc, PresentDesc { flags: PresentDesc::ENABLED, x: 64, y: 16, width: 256, height: 32 });
    stream.push(header(op::PRESENT, 5));
    stream.extend_from_slice(&desc.to_words());
    dev.execute(&stream).expect("present");
    let (w, h, rgba) = dev.frame().expect("frame");
    assert_eq!((w, h), (256, 32));
    for j in 0..h {
        for i in 0..w {
            let p = pattern(i, j) as u32;
            let want = [((p & 31) << 3) as u8, (((p >> 5) & 31) << 3) as u8, (((p >> 10) & 31) << 3) as u8, 255];
            let at = ((j * w + i) * 4) as usize;
            assert_eq!(&rgba[at..at + 4], &want, "15-bit pixel ({i}, {j})");
        }
    }

    // 24-bit: 3 bytes per pixel along the row, from halfwords.
    gpu.gp1(0x0800_0010);
    let bytes = |i: u32, j: u32| (i * 31 + j * 17 + 5) as u8;
    upload(&mut gpu, 64, 16, 384, 32, |i, j| bytes(2 * i, j) as u16 | (bytes(2 * i + 1, j) as u16) << 8);
    let mut stream = gpu.take_commands();
    let desc = gpu.present_desc();
    assert_eq!(desc.flags, PresentDesc::ENABLED | PresentDesc::DEPTH24);
    stream.push(header(op::PRESENT, 5));
    stream.extend_from_slice(&desc.to_words());
    dev.execute(&stream).expect("present 24-bit");
    let (w, h, rgba) = dev.frame().expect("frame");
    assert_eq!((w, h), (256, 32));
    for j in 0..h {
        for i in 0..w {
            let want = [bytes(3 * i, j), bytes(3 * i + 1, j), bytes(3 * i + 2, j), 255];
            let at = ((j * w + i) * 4) as usize;
            assert_eq!(&rgba[at..at + 4], &want, "24-bit pixel ({i}, {j})");
        }
    }
}
