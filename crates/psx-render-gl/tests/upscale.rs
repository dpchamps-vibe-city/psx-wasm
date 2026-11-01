#![cfg(feature = "headless")]

use psx_render_gl::{Frame, Renderer, headless::Headless, visual::{ColourPrecision, TextureFilter, VisualPreset}};
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
    for (scale, precision) in [
        (1, ColourPrecision::Native), (2, ColourPrecision::Native), (4, ColourPrecision::Native),
        (1, ColourPrecision::High), (2, ColourPrecision::High), (4, ColourPrecision::High),
    ] {
        r.set_visual_preset(VisualPreset::CleanPolygons { scale, textures: TextureFilter::Nearest, dithering: true, precision }).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 1024, 512);
        if let Some(want) = &baseline {
            assert_eq!(&native, want);
        } else {
            baseline = Some(native.clone());
        }
        let f = present(&mut r, PresentDesc::ENABLED);
        let half = if precision == ColourPrecision::High { 128 } else { 120 };
        assert_eq!(pixel(&f, 3 * scale, 3 * scale), &[half, 0, half, 255]);
        assert_eq!(pixel(&f, 19 * scale, 3 * scale), pixel(&f, 3 * scale, 3 * scale));
        if scale > 1 {
            assert_eq!(pixel(&f, 25 * scale - 1, 8 * scale - 1), &[0, 0, 0, 255], "COPY retains enhanced edge detail");
        }
        assert_eq!(pixel(&f, 0, 0), &[0, 0, 248, 255], "wrapping masked uploads");
        assert_eq!(pixel(&f, 24 * scale, 24 * scale), &[0, half, 0, 255], "quad seam and polygon masks");
        assert_eq!(pixel(&f, 24 * scale + 1, 24 * scale), &[0, half, 0, 255]);
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

#[test]
fn smooth_polygon_textures_decode_palettes_preserve_transparency_and_leave_sprites_and_vram_alone() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    for depth in 0..3 {
        for green in [0x03e0u16, 0, 0x83e0] {
            for window in [false, true] {
                let mut stream = Vec::new();
                state(&mut stream);
                command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x7c00]);
                if depth < 2 {
                    let mut palette = vec![0u16; if depth == 0 { 16 } else { 256 }];
                    palette[0] = 0x001f;
                    palette[1] = green;
                    let mut upload = vec![pack_xy(0, 80), palette.len() as u32 | 1 << 16, 0];
                    upload.extend(palette.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
                    command(&mut stream, op::UPLOAD, &upload);
                    command(&mut stream, op::SYNC, &[1, pack_xy(0, 80), palette.len() as u32 | 1 << 16]);
                    command(&mut stream, op::CLUT_LOAD, &[0, pack_xy(0, 80), palette.len() as u32]);
                }
                let row: Vec<u16> = match depth {
                    0 => vec![0x1010; 2],
                    1 => vec![0x0100; 4],
                    _ => [0x001f, green].repeat(4),
                };
                let x = if window { 8 >> (2 - depth) } else { 0 };
                let mut upload = vec![pack_xy(x, 64), row.len() as u32 | 4 << 16, 0];
                let pixels = row.repeat(4);
                upload.extend(pixels.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
                command(&mut stream, op::UPLOAD, &upload);
                command(&mut stream, op::SYNC, &[1, pack_xy(x, 64), row.len() as u32 | 4 << 16]);
                let mut polygon = triangle(0x808080);
                polygon[0] |= prim::TEXTURED | prim::RAW | prim::SEMI | depth << prim::DEPTH_SHIFT;
                polygon[2] = if window { 1 | 1 << 10 } else { 0 };
                polygon[5] = 64 << 8;
                polygon[8] = 3 | 64 << 8;
                polygon[11] = 67 << 8;
                let mut sprite = [0; 16];
                sprite[0] = (polygon[0] & !prim::KIND_MASK) | prim::RECT;
                sprite[2] = polygon[2];
                sprite[3] = pack_xy(18, 2);
                sprite[4] = 8 | 4 << 16;
                sprite[5] = 0x808080;
                sprite[6] = 64 << 8;
                draw(&mut stream, &[polygon, sprite]);
                r.set_resolution_scale(1).unwrap();
                r.reset_vram();
                r.execute(&stream).unwrap();
                let native = r.read_vram(0, 0, 1024, 512);
                let green_pixel = match green {
                    0 => [0, 0, 248, 255],
                    0x83e0 => [0, 120, 120, 255],
                    _ => [0, 248, 0, 255],
                };
                for scale in [1, 2, 4] {
                    let mut nearest = None;
                    for textures in [TextureFilter::Nearest, TextureFilter::Smooth3D] {
                        let preset = VisualPreset::CleanPolygons { scale, textures, dithering: true, precision: ColourPrecision::Native };
                        r.set_visual_preset(preset).unwrap();
                        assert_eq!(r.visual_preset(), Some(preset));
                        // Reconfiguration must not reset native memory.
                        assert_eq!(r.read_vram(0, 0, 1024, 512), native);
                        r.reset_vram();
                        r.execute(&stream).unwrap();
                        assert_eq!(r.read_vram(0, 0, 1024, 512), native);
                        let f = present(&mut r, PresentDesc::ENABLED);
                        let smoothing = textures == TextureFilter::Smooth3D && green == 0x03e0;
                        assert_eq!(pixel(&f, 5 * scale, 3 * scale), if smoothing { &[64, 184, 0, 255] } else { &green_pixel });
                        assert_eq!(pixel(&f, 3 * scale, 3 * scale), if smoothing { &[184, 64, 0, 255] } else { &[248, 0, 0, 255] });
                        assert_eq!(pixel(&f, 19 * scale, 3 * scale), &green_pixel);
                        if let Some(want) = &nearest {
                            for y in 2 * scale..6 * scale {
                                for x in 18 * scale..26 * scale {
                                    assert_eq!(pixel(&f, x, y), pixel(want, x, y), "sprite changed with filtering");
                                }
                            }
                        } else {
                            nearest = Some(f);
                        }
                        assert!(r.set_visual_preset(VisualPreset::CleanPolygons { scale: 3, textures, dithering: true, precision: ColourPrecision::Native }).is_err());
                        assert_eq!(r.visual_preset(), Some(preset));
                    }
                }
                let video = present(&mut r, PresentDesc::ENABLED | PresentDesc::DEPTH24);
                assert_eq!((video.width, video.height), (32, 32));
            }
        }
    }
}

#[test]
fn dithering_and_colour_precision_are_independent_visual_options() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut flat = triangle(0x808080);
    flat[0] |= prim::DITHER;
    let mut gradient = triangle(0x202020);
    gradient[0] |= prim::GOURAUD | prim::DITHER;
    for (i, (x, y)) in [(18, 2), (30, 2), (18, 14)].into_iter().enumerate() {
        gradient[3 + 3 * i] = pack_xy(x, y);
    }
    gradient[7] = 0xe0e0e0;
    let mut stream = Vec::new();
    state(&mut stream);
    draw(&mut stream, &[flat, gradient]);
    command(&mut stream, op::SYNC, &[1, pack_xy(16, 0), 16 | 16 << 16]);
    command(&mut stream, op::COPY, &[pack_xy(16, 0), pack_xy(16, 16), 16 | 16 << 16, 0]);
    r.execute(&stream).unwrap();
    let native = r.read_vram(0, 0, 32, 32);
    for precision in [ColourPrecision::Native, ColourPrecision::High] {
        for dithering in [true, false] {
            r.set_visual_preset(VisualPreset::CleanPolygons { scale: 4, textures: TextureFilter::Nearest, dithering, precision }).unwrap();
            // R32UI seeding must expand native words rather than blit their bits.
            let seeded = present(&mut r, PresentDesc::ENABLED);
            for (x, y) in [(4, 4), (5, 4), (19, 3)] {
                let p = native[(y * 32 + x) as usize];
                assert_eq!(pixel(&seeded, x * 4, y * 4), &[(p as u8 & 31) << 3, ((p >> 5) as u8 & 31) << 3, ((p >> 10) as u8 & 31) << 3, 255]);
            }
            r.reset_vram();
            r.execute(&stream).unwrap();
            assert_eq!(r.read_vram(0, 0, 32, 32), native);
            let f = present(&mut r, PresentDesc::ENABLED);
            let low = if !dithering { 128 } else if precision == ColourPrecision::High { 124 } else { 120 };
            assert_eq!(pixel(&f, 4 * 4, 4 * 4), &[low, low, low, 255]);
            assert_eq!(pixel(&f, 5 * 4, 4 * 4), &[128, 128, 128, 255]);
            let shade = if precision == ColourPrecision::High { 52 - if dithering { 2 } else { 0 } } else { 48 };
            assert_eq!(pixel(&f, 19 * 4 + 1, 3 * 4), &[shade, shade, shade, 255], "high precision must contain non-RGB5 colours");
            assert_eq!(pixel(&f, 19 * 4 + 1, 19 * 4), &[shade, shade, shade, 255], "COPY must retain the extra colour bits");
        }
    }
}

#[test]
fn tiled_sprite_quads_keep_pixel_centres_and_never_sample_atlas_gutters() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    for gutter in [0u16, 0x7c1f] {
        let mut atlas = vec![gutter; 64 * 32];
        let mut quads = Vec::new();
        for (i, (x, y, u, v)) in [(2, 2, 16, 64), (10, 2, 32, 64), (2, 10, 16, 80), (10, 10, 32, 80)].into_iter().enumerate() {
            for ty in 0..8 {
                for tx in 0..8 {
                    atlas[((v - 64 + ty) * 64 + u + tx) as usize] = if tx == 4 && ty == 4 { 0 } else { 0x03e0 };
                }
            }
            let mut quad = [0; 16];
            quad[0] = prim::QUAD | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
            for (n, (dx, dy)) in [(0, 0), (8, 0), (0, 8), (8, 8)].into_iter().enumerate() {
                quad[3 + 3 * n] = pack_xy(x + dx, y + dy);
                let (tu, tv) = match i {
                    1 => (u + 7 - dx, v + dy), // mirrored U: the excluded endpoint is u-1
                    2 => (u + dx, v + 7 - dy), // mirrored V
                    3 => (u + dy, v + dx), // UV axes rotated relative to screen axes
                    _ => (u + dx, v + dy),
                };
                quad[5 + 3 * n] = tu as u32 | (tv as u32) << 8;
            }
            quads.push(quad);
        }
        let mut stream = Vec::new();
        state(&mut stream);
        command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x7c00]);
        let mut upload = vec![pack_xy(0, 64), 64 | 32 << 16, 0];
        upload.extend(atlas.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
        command(&mut stream, op::UPLOAD, &upload);
        command(&mut stream, op::SYNC, &[1, pack_xy(0, 64), 64 | 32 << 16]);
        draw(&mut stream, &quads);
        r.set_resolution_scale(1).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 32, 32);
        let reference = present(&mut r, PresentDesc::ENABLED);
        for scale in [1, 2, 4] {
            for textures in [TextureFilter::Nearest, TextureFilter::Smooth3D] {
                for precision in [ColourPrecision::Native, ColourPrecision::High] {
                    r.set_visual_preset(VisualPreset::CleanPolygons { scale, textures, dithering: false, precision }).unwrap();
                    r.reset_vram();
                    r.execute(&stream).unwrap();
                    assert_eq!(r.read_vram(0, 0, 32, 32), native);
                    let f = present(&mut r, PresentDesc::ENABLED);
                    for y in 0..f.height {
                        for x in 0..f.width {
                            assert_eq!(pixel(&f, x, y), pixel(&reference, x / scale, y / scale),
                                "sprite seam or shifted cutout at ({x}, {y}), {scale}× {textures:?} {precision:?}, gutter {gutter:#x}");
                        }
                    }
                }
            }
        }
    }
}
