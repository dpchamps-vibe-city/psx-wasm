#![cfg(feature = "headless")]

use psx_render_gl::{Frame, Renderer, headless::Headless};
use psxw_abi::render::{self, PresentDesc, header, op, pack_xy, prim};

fn command(out: &mut Vec<u32>, opcode: u8, payload: &[u32]) {
    out.push(header(opcode, payload.len() as u32));
    out.extend_from_slice(payload);
}

fn triangle(color: u32) -> [u32; 16] {
    let mut rec = [0; 16];
    rec[0] = prim::TRIANGLE;
    for (i, (x, y)) in [(2, 2), (14, 2), (2, 14)].into_iter().enumerate() {
        rec[3 + 3 * i] = pack_xy(x, y);
        rec[4 + 3 * i] = color;
    }
    rec
}

fn draw(out: &mut Vec<u32>, records: &[[u32; 16]]) {
    let mut payload = vec![records.len() as u32];
    payload.extend(records.iter().flatten());
    command(out, op::DRAW, &payload);
}

fn present(r: &mut Renderer, flags: u32) -> Frame {
    let mut stream = Vec::new();
    command(&mut stream, op::PRESENT, &PresentDesc { flags, x: 0, y: 0, width: 32, height: 32 }.to_words());
    r.execute(&stream).unwrap();
    r.frame().unwrap()
}

fn pixel(f: &Frame, x: u32, y: u32) -> &[u8] {
    let at = ((y * f.width + x) * 4) as usize;
    &f.rgba[at..at + 4]
}

fn state(out: &mut Vec<u32>) {
    command(out, op::SET_STATE, &[0, pack_xy(1023, 511), 0]);
}

#[test]
fn polygons_gain_subpixel_edges_and_interpolation_but_native_vram_does_not_change() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut stream = Vec::new();
    state(&mut stream);
    draw(&mut stream, &[triangle(0x0000ff)]);
    let mut baseline = None;
    for scale in [1, 2, 4] {
        r.set_resolution_scale(scale).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 32, 32);
        if let Some(want) = &baseline {
            assert_eq!(&native, want);
        } else {
            baseline = Some(native);
        }
        let f = present(&mut r, PresentDesc::ENABLED);
        assert_eq!((f.width, f.height), (32 * scale, 32 * scale));
        assert_eq!(pixel(&f, 8 * scale, 7 * scale), &[248, 0, 0, 255]);
        if scale > 1 {
            assert_eq!(
                pixel(&f, 9 * scale - 1, 8 * scale - 1),
                &[0, 0, 0, 255],
                "a native pixel must contain genuinely different subpixels"
            );
        }

        // A one-native-pixel colour/UV gradient must interpolate inside it,
        // rather than repeating the native shader's rounded attributes.
        r.reset_vram();
        let mut gradient = triangle(0);
        gradient[0] |= prim::GOURAUD;
        gradient[4] = 0;
        gradient[7] = 0x0000ff;
        gradient[10] = 0;
        let mut stream = Vec::new();
        state(&mut stream);
        draw(&mut stream, &[gradient]);
        r.execute(&stream).unwrap();
        let f = present(&mut r, PresentDesc::ENABLED);
        if scale > 1 {
            assert_ne!(pixel(&f, 5 * scale, 3 * scale), pixel(&f, 6 * scale - 1, 3 * scale));
        }

        // Native 15-bit texels, but subpixel affine UV interpolation.
        r.reset_vram();
        gradient[0] = prim::TRIANGLE | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
        gradient[5] = 0;
        gradient[8] = 24;
        gradient[11] = 0;
        let mut stream = Vec::new();
        state(&mut stream);
        let mut upload = vec![pack_xy(0, 64), 32 | 1 << 16, 0];
        upload.extend(std::iter::repeat_n(0x03e0001f, 16));
        command(&mut stream, op::UPLOAD, &upload);
        // Page Y=0, so UV v=64 addresses the uploaded row.
        for i in [5, 8, 11] {
            gradient[i] |= 64 << 8;
        }
        command(&mut stream, op::SYNC, &[1, pack_xy(0, 64), 32 | 1 << 16]);
        draw(&mut stream, &[gradient]);
        r.execute(&stream).unwrap();
        let f = present(&mut r, PresentDesc::ENABLED);
        if scale > 1 {
            assert_ne!(pixel(&f, 5 * scale, 3 * scale), pixel(&f, 5 * scale + 1, 3 * scale));
        }
    }
}

#[test]
fn enhanced_hazards_copies_masks_video_and_switching_are_host_local() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut stream = Vec::new();
    state(&mut stream);
    draw(&mut stream, &[triangle(0xff)]);
    // Native blending needs a SYNC, but the enhanced plane must independently
    // sync its own writes, not assume native coverage accounts for its hazards.
    command(&mut stream, op::SYNC, &[1, 0, 32 | 32 << 16]);
    let mut semi = triangle(0xff0000);
    semi[0] |= prim::SEMI;
    draw(&mut stream, &[semi]);
    command(&mut stream, op::SYNC, &[1, 0, 32 | 32 << 16]);
    command(&mut stream, op::COPY, &[0, pack_xy(16, 0), 16 | 16 << 16, 0]);
    // A semi-transparent quad must not double-blend its shared diagonal.
    // Its mask bit also protects it from a subsequent polygon.
    let mut quad = [0; 16];
    quad[0] = prim::QUAD | prim::SEMI | prim::MASK_SET;
    for (i, (x, y)) in [(18, 18), (30, 18), (18, 30), (30, 30)].into_iter().enumerate() {
        quad[3 + 3 * i] = pack_xy(x, y);
        quad[4 + 3 * i] = 0x00ff00;
    }
    draw(&mut stream, &[quad]);
    command(&mut stream, op::SYNC, &[1, pack_xy(18, 18), 12 | 12 << 16]);
    quad[0] = prim::QUAD | prim::MASK_CHECK;
    for i in 0..4 {
        quad[4 + 3 * i] = 0xff;
    }
    draw(&mut stream, &[quad]);
    // Upload across both VRAM boundaries, with masking on a later upload.
    command(&mut stream, op::UPLOAD, &[pack_xy(1023, 511), 2 | 2 << 16, render::mask::SET, 0x03e0001f, 0x7c00001f]);
    command(&mut stream, op::SYNC, &[1, pack_xy(1023, 511), 2 | 2 << 16]);
    command(&mut stream, op::UPLOAD, &[pack_xy(1023, 511), 2 | 2 << 16, render::mask::CHECK, 0, 0]);
    let mut baseline = None;
    for scale in [1, 2, 4] {
        r.set_resolution_scale(scale).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 1024, 512);
        if let Some(want) = &baseline {
            assert_eq!(&native, want);
        } else {
            baseline = Some(native.clone());
        }
        let f = present(&mut r, PresentDesc::ENABLED);
        assert_eq!(pixel(&f, 3 * scale, 3 * scale), &[120, 0, 120, 255]);
        assert_eq!(pixel(&f, 19 * scale, 3 * scale), pixel(&f, 3 * scale, 3 * scale));
        if scale > 1 {
            assert_eq!(pixel(&f, 25 * scale - 1, 8 * scale - 1), &[0, 0, 0, 255], "COPY retains enhanced edge detail");
        }
        assert_eq!(pixel(&f, 0, 0), &[0, 0, 248, 255], "wrapping masked uploads");
        assert_eq!(pixel(&f, 24 * scale, 24 * scale), &[0, 120, 0, 255], "quad seam and polygon masks");
        assert_eq!(pixel(&f, 24 * scale + 1, 24 * scale), &[0, 120, 0, 255]);
        assert!(r.set_resolution_scale(3).is_err());
        assert_eq!(r.resolution_scale(), scale);
        assert_eq!(r.read_vram(0, 0, 1024, 512), native);
        let video = present(&mut r, PresentDesc::ENABLED | PresentDesc::DEPTH24);
        assert_eq!((video.width, video.height), (32, 32));
    }
    let before = r.read_vram(0, 0, 1024, 512);
    for scale in [2, 1, 4, 1] {
        r.set_resolution_scale(scale).unwrap();
        assert_eq!(r.read_vram(0, 0, 1024, 512), before);
        let f = present(&mut r, PresentDesc::ENABLED);
        assert_eq!(pixel(&f, 3 * scale, 3 * scale), &[120, 0, 120, 255], "changing scale seeds from native VRAM");
    }
}
