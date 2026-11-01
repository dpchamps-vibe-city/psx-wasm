#![cfg(feature = "headless")]

use psx_render_gl::{Frame, Renderer, headless::Headless, visual::{ColourPrecision, SpriteReconstruction, TextureFilter, VisualPreset}};
use psxw_abi::render::{PresentDesc, header, op, pack_xy, prim};

fn command(out: &mut Vec<u32>, opcode: u8, payload: &[u32]) {
    out.push(header(opcode, payload.len() as u32));
    out.extend_from_slice(payload);
}

fn scene(depth: u32, foreground: u16, flags: u32, background_masked: bool) -> Vec<u32> {
    let mut stream = Vec::new();
    command(&mut stream, op::SET_STATE, &[0, pack_xy(1023, 511), 0]);
    command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x7c00]);
    if background_masked {
        let mut upload = vec![0, 32 | 32 << 16, 1];
        upload.extend(std::iter::repeat_n(0x7c007c00, 512));
        command(&mut stream, op::UPLOAD, &upload);
    }
    if depth < 2 {
        let mut palette = vec![0u16; if depth == 0 { 16 } else { 256 }];
        palette[1] = foreground;
        palette[2] = 0x7c1f; // An incompatible atlas gutter must never leak in.
        palette[3] = 0x03e0;
        let mut upload = vec![pack_xy(0, 128), palette.len() as u32 | 1 << 16, 0];
        upload.extend(palette.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
        command(&mut stream, op::UPLOAD, &upload);
        command(&mut stream, op::SYNC, &[1, pack_xy(0, 128), palette.len() as u32 | 1 << 16]);
        command(&mut stream, op::CLUT_LOAD, &[0, pack_xy(0, 128), palette.len() as u32]);
    }
    let texels: Vec<u16> = (0..16).flat_map(|y| (0..16).map(move |x| {
        let index = if y >= 8 { if x % 2 == 0 { 1 } else { 3 } }
            else if x >= 8 { 2 } else if x >= y { 1 } else { 0 };
        if depth < 2 { index } else { [0, foreground, 0x7c1f, 0x03e0][index as usize] }
    })).collect();
    let shift = 2 - depth;
    let packed: Vec<u16> = texels.chunks(1 << shift).map(|p| {
        p.iter().enumerate().fold(0, |word, (i, v)| word | v << (i * (16 >> shift)))
    }).collect();
    let mut upload = vec![pack_xy(16 >> shift, 64), (16 >> shift) | 16 << 16, 0];
    upload.extend(packed.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
    command(&mut stream, op::UPLOAD, &upload);
    command(&mut stream, op::SYNC, &[1, 0, 1024 | 512 << 16]);
    let mut records = Vec::new();
    // Normal, mirrored U and rotated UV quads; normal, flipped U and flipped V
    // rectangles. Both kinds use the same native texel-centred footprint.
    for i in 0..6 {
        let (x, y) = (2 + (i % 3) * 10, 2 + (i / 3) * 12);
        let mut rec = [0; 16];
        rec[0] = prim::TEXTURED | prim::RAW | depth << prim::DEPTH_SHIFT | flags;
        if i < 3 {
            rec[0] |= prim::QUAD;
            for (n, (dx, dy)) in [(0, 0), (8, 0), (0, 8), (8, 8)].into_iter().enumerate() {
                rec[3 + 3*n] = pack_xy(x + dx, y + dy);
                let (u, v) = match i {
                    1 => (23 - dx, 64 + dy),
                    2 => (16 + dy, 64 + dx),
                    _ => (16 + dx, 64 + dy),
                };
                rec[5 + 3*n] = u as u32 | (v as u32) << 8;
            }
        } else {
            rec[0] |= prim::RECT;
            if i == 4 { rec[0] |= prim::FLIP_X; }
            if i == 5 { rec[0] |= prim::FLIP_Y; }
            rec[3] = pack_xy(x, y);
            rec[4] = 8 | 8 << 16;
            rec[6] = if i == 4 { 23 } else { 16 } | if i == 5 { 71 << 8 } else { 64 << 8 };
        }
        records.extend_from_slice(&rec);
    }
    // Ordinary textured geometry must keep its independent surface filter.
    let mut world = [0; 16];
    world[0] = prim::TRIANGLE | prim::TEXTURED | prim::RAW | depth << prim::DEPTH_SHIFT;
    for (i, (x, y, u, v)) in [(2, 24, 16, 72), (14, 24, 19, 72), (2, 32, 16, 75)].into_iter().enumerate() {
        world[3 + 3*i] = pack_xy(x, y);
        world[5 + 3*i] = u | v << 8;
    }
    records.extend_from_slice(&world);
    let mut draw = vec![7];
    draw.extend(records);
    command(&mut stream, op::DRAW, &draw);
    stream
}

fn present(r: &mut Renderer) -> Frame {
    let mut stream = Vec::new();
    command(&mut stream, op::PRESENT, &PresentDesc { flags: PresentDesc::ENABLED, x: 0, y: 0, width: 32, height: 32 }.to_words());
    r.execute(&stream).unwrap();
    r.frame().unwrap()
}

fn pixel(f: &Frame, x: u32, y: u32) -> &[u8] {
    let at = ((y*f.width + x)*4) as usize;
    &f.rgba[at..at+4]
}

#[test]
fn mmpx_matches_reference_preserves_palette_and_tiny_features_without_changing_native_vram() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let pixels: Vec<u16> = include_bytes!("fixtures/mmpx-input.u16").chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    let reference = include_bytes!("fixtures/mmpx-2x.rgb");
    for depth in 0..3 {
        let mut stream = Vec::new();
        command(&mut stream, op::SET_STATE, &[0, pack_xy(1023, 511), 0]);
        command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x7c00]);
        let mut palette = pixels.clone();
        palette.sort_unstable();
        palette.dedup();
        let indices: Vec<u16> = pixels.iter().map(|p| palette.iter().position(|c| c == p).unwrap() as u16).collect();
        if depth < 2 {
            palette.resize(if depth == 0 { 16 } else { 256 }, 0);
            let mut upload = vec![pack_xy(0, 128), palette.len() as u32 | 1 << 16, 0];
            upload.extend(palette.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
            command(&mut stream, op::UPLOAD, &upload);
            command(&mut stream, op::SYNC, &[1, pack_xy(0, 128), palette.len() as u32 | 1 << 16]);
            command(&mut stream, op::CLUT_LOAD, &[0, pack_xy(0, 128), palette.len() as u32]);
        }
        let shift = 2 - depth;
        let source = if depth == 2 { &pixels } else { &indices };
        let packed: Vec<u16> = source.chunks(1 << shift).map(|p| {
            p.iter().enumerate().fold(0, |word, (i, v)| word | v << (i * (16 >> shift)))
        }).collect();
        let mut upload = vec![pack_xy(16 >> shift, 64), (16 >> shift) | 16 << 16, 0];
        upload.extend(packed.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
        command(&mut stream, op::UPLOAD, &upload);
        command(&mut stream, op::SYNC, &[1, 0, 1024 | 512 << 16]);
        let mut quad = [0; 16];
        quad[0] = prim::QUAD | prim::TEXTURED | prim::RAW | depth << prim::DEPTH_SHIFT;
        for (i, (x, y, u, v)) in [(0, 0, 16, 64), (16, 0, 32, 64), (0, 16, 16, 80), (16, 16, 32, 80)].into_iter().enumerate() {
            quad[3 + 3*i] = pack_xy(x, y);
            quad[5 + 3*i] = u | v << 8;
        }
        let mut rect = [0; 16];
        rect[0] = (quad[0] & !prim::KIND_MASK) | prim::RECT;
        rect[3] = pack_xy(16, 0);
        rect[4] = 16 | 16 << 16;
        rect[6] = 16 | 64 << 8;
        let mut draw = vec![2];
        draw.extend(quad);
        draw.extend(rect);
        command(&mut stream, op::DRAW, &draw);
        r.set_resolution_scale(1).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 1024, 512);
        let original = present(&mut r);
        for precision in [ColourPrecision::Native, ColourPrecision::High] {
            for scale in [1, 2, 4] {
                r.set_visual_preset(VisualPreset::PsxRevamp {
                    scale, textures: TextureFilter::Smooth3D, sprites: SpriteReconstruction::Mmpx,
                    dithering: false, precision,
                }).unwrap();
                assert_eq!(r.read_vram(0, 0, 1024, 512), native);
                r.reset_vram();
                r.execute(&stream).unwrap();
                assert_eq!(r.read_vram(0, 0, 1024, 512), native);
                let frame = present(&mut r);
                for y in 0..16*scale {
                    for x in 0..16*scale {
                        let got = pixel(&frame, x, y);
                        assert_eq!(got, pixel(&frame, 16*scale+x, y), "MMPX quad/rectangle mismatch");
                        if scale == 1 {
                            assert_eq!(got, pixel(&original, x, y));
                        } else {
                            let at = (((y*2/scale)*32 + x*2/scale)*3) as usize;
                            assert_eq!(&got[..3], &reference[at..at+3], "MMPX reference mismatch at {x}/{y}, {scale}x, depth {depth}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn interpolating_sprite_filters_reconstruct_transparency_without_halos_atlas_bleed_or_native_changes() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    for depth in 0..3 {
        let stream = scene(depth, 0x001f, 0, false);
        r.set_resolution_scale(1).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let native = r.read_vram(0, 0, 1024, 512);
        let original = present(&mut r);
        for (precision, sprites) in [
            (ColourPrecision::Native, SpriteReconstruction::Xbr),
            (ColourPrecision::High, SpriteReconstruction::Xbr),
            (ColourPrecision::Native, SpriteReconstruction::Bicubic),
            (ColourPrecision::High, SpriteReconstruction::Bicubic),
            (ColourPrecision::Native, SpriteReconstruction::SuperXbr3p),
            (ColourPrecision::High, SpriteReconstruction::SuperXbr3p),
        ] {
            for scale in [1, 2, 4] {
                for textures in [TextureFilter::Nearest, TextureFilter::Smooth3D] {
                    r.set_visual_preset(VisualPreset::PsxRevamp {
                        scale, textures, sprites: SpriteReconstruction::Original, dithering: false, precision,
                    }).unwrap();
                    r.reset_vram();
                    r.execute(&stream).unwrap();
                    let surface_reference = present(&mut r);
                    r.set_visual_preset(VisualPreset::PsxRevamp {
                        scale, textures, sprites, dithering: false, precision,
                    }).unwrap();
                    assert_eq!(r.read_vram(0, 0, 1024, 512), native, "switching renderer changed VRAM");
                    r.reset_vram();
                    r.execute(&stream).unwrap();
                    assert_eq!(r.read_vram(0, 0, 1024, 512), native);
                    let f = present(&mut r);
                    for y in 24*scale..32*scale {
                        for x in 0..32*scale {
                            assert_eq!(pixel(&f, x, y), pixel(&surface_reference, x, y), "sprite filter changed ordinary polygons");
                        }
                    }
                    for i in 0..6 {
                        let (ox, oy) = (2 + (i % 3)*10, 2 + (i / 3)*12);
                        let (mut partial, mut expanded) = (0, 0);
                        for y in 0..8*scale {
                            for x in 0..8*scale {
                                let p = pixel(&f, ox*scale+x, oy*scale+y);
                                let old = pixel(&original, ox+x/scale, oy+y/scale);
                                assert_eq!(p[1], 0, "atlas gutter leaked");
                                assert!((p[0] as i32 + p[2] as i32 - 248).abs() <= 8, "dark halo: {p:?}");
                                if scale == 1 { assert_eq!(p, old, "sprite filter must not blur unmagnified sprites"); }
                                if p[0] > 0 && p[0] < 248 && p[2] > 0 { partial += 1; }
                                if old[0] == 0 && p[0] > 0 { expanded += 1; }
                                if i == 3 {
                                    assert_eq!(p, pixel(&f, 2*scale+x, 2*scale+y), "quad/rectangle reconstruction differs");
                                }
                            }
                        }
                        if scale > 1 {
                            assert!(partial > 0, "no fractional silhouette coverage: sprite {i}, {scale}x");
                            assert!(expanded > 0, "nearest transparency still clips reconstructed edges: sprite {i}, {scale}x");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn super_xbr_3p_matches_the_complete_preset_and_refreshes_cached_native_inputs() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    r.set_visual_preset(VisualPreset::PsxRevamp {
        scale: 4, textures: TextureFilter::Smooth3D, sprites: SpriteReconstruction::SuperXbr3p,
        dithering: false, precision: ColourPrecision::High,
    }).unwrap();
    let pixels: Vec<u16> = include_bytes!("fixtures/mmpx-input.u16").chunks_exact(2).map(|p| {
        let p = u16::from_le_bytes([p[0],p[1]]);
        if p == 0 { 0x3e12 } else { p }
    }).collect();
    let mut setup = Vec::new();
    command(&mut setup, op::SET_STATE, &[0, pack_xy(1023,511), 0]);
    let mut upload = vec![pack_xy(16,64), 16 | 16 << 16, 0];
    upload.extend(pixels.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
    command(&mut setup, op::UPLOAD, &upload);
    command(&mut setup, op::SYNC, &[1, pack_xy(16,64), 16 | 16 << 16]);
    r.execute(&setup).unwrap();
    let mut quad = [0;16];
    quad[0] = prim::QUAD | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
    for (i,(x,y,u,v)) in [(2,2,16,64),(18,2,32,64),(2,18,16,80),(18,18,32,80)].into_iter().enumerate() {
        quad[3+3*i] = pack_xy(x,y);
        quad[5+3*i] = u | v << 8;
    }
    let mut draw = vec![2];
    draw.extend(quad);
    let mut second = quad;
    for i in 0..4 { second[3+3*i] += 32; }
    draw.extend(second);
    let mut stream = Vec::new();
    command(&mut stream, op::DRAW, &draw);
    r.take_stats();
    r.execute(&stream).unwrap();
    // One canonical batch, one visual batch and three preprocessing passes.
    assert_eq!(r.take_stats().draws, 5);
    let frame = present(&mut r);
    let reference = include_bytes!("fixtures/super-xbr-3p-4x.rgb");
    for y in 0..64 {
        for x in 0..64 {
            let got = pixel(&frame,8+x,8+y);
            let at = ((y*64+x)*3) as usize;
            for c in 0..3 {
                assert!(got[c].abs_diff(reference[at+c]) <= 2,
                    "Super-xBR 3p/Jinc preset mismatch at {x}/{y}, channel {c}: {} != {}",got[c],reference[at+c]);
            }
        }
    }
    // A later callback and a SYNC outside the tile must keep the cache warm:
    // only the canonical and visual batches remain, with zero preprocessing.
    r.take_stats();
    let mut unrelated = Vec::new();
    command(&mut unrelated, op::SYNC, &[1,0,8 | 8 << 16]);
    unrelated.extend_from_slice(&stream);
    r.execute(&unrelated).unwrap();
    assert_eq!(r.take_stats().draws, 2);
    // Reuse the same key after a native source update. Its intersecting SYNC
    // must invalidate the processed result.
    let mut update = Vec::new();
    let mut upload = vec![pack_xy(16,64),16 | 16 << 16,0];
    upload.extend(std::iter::repeat_n(0x03e003e0,128));
    command(&mut update,op::UPLOAD,&upload);
    command(&mut update,op::SYNC,&[1,pack_xy(16,64),16 | 16 << 16]);
    command(&mut update,op::DRAW,&draw);
    r.execute(&update).unwrap();
    let frame = present(&mut r);
    assert_eq!(pixel(&frame,24,24), &[0,248,0,255]);
    assert_eq!(r.read_vram(6,6,1,1), vec![0x03e0]);

    // Paletted tiles depend on CLUT_LOAD, not merely palette VRAM writes.
    quad[0] &= !prim::DEPTH_MASK;
    second[0] = quad[0];
    let mut paletted_draw = vec![2];
    paletted_draw.extend(quad);
    paletted_draw.extend(second);
    let mut paletted = Vec::new();
    let mut indices = vec![pack_xy(4,64),4 | 16 << 16,0];
    indices.extend(std::iter::repeat_n(0x11111111,32));
    command(&mut paletted,op::UPLOAD,&indices);
    command(&mut paletted,op::UPLOAD,&[pack_xy(0,128),16 | 1 << 16,0,0x001f0000,0,0,0,0,0,0,0]);
    command(&mut paletted,op::SYNC,&[1,0,1024 | 512 << 16]);
    command(&mut paletted,op::CLUT_LOAD,&[0,pack_xy(0,128),16]);
    command(&mut paletted,op::DRAW,&paletted_draw);
    r.execute(&paletted).unwrap();
    assert_eq!(pixel(&present(&mut r),24,24), &[248,0,0,255]);
    let mut palette_write = Vec::new();
    command(&mut palette_write,op::UPLOAD,&[pack_xy(1,128),1 | 1 << 16,0,0x03e0]);
    command(&mut palette_write,op::SYNC,&[1,pack_xy(1,128),1 | 1 << 16]);
    command(&mut palette_write,op::DRAW,&paletted_draw);
    r.execute(&palette_write).unwrap();
    assert_eq!(pixel(&present(&mut r),24,24), &[248,0,0,255]);
    let mut reload = Vec::new();
    command(&mut reload,op::CLUT_LOAD,&[0,pack_xy(0,128),16]);
    command(&mut reload,op::DRAW,&paletted_draw);
    r.execute(&reload).unwrap();
    assert_eq!(pixel(&present(&mut r),24,24), &[0,248,0,255]);

    // Rotating a CLUT slot must not rebuild an identical processed tile.
    quad[1] = 7 << 16;
    second[1] = quad[1];
    let mut aliased_draw = vec![2];
    aliased_draw.extend(quad);
    aliased_draw.extend(second);
    let mut alias = Vec::new();
    command(&mut alias,op::CLUT_LOAD,&[7,pack_xy(0,128),16]);
    command(&mut alias,op::DRAW,&aliased_draw);
    r.take_stats();
    r.execute(&alias).unwrap();
    assert_eq!(r.take_stats().draws,2,"unchanged palette in a new slot must reuse the image");
    assert_eq!(pixel(&present(&mut r),24,24), &[0,248,0,255]);
    // Switching A -> B -> A in one slot must recover the old A snapshot.
    let mut blue = Vec::new();
    command(&mut blue,op::UPLOAD,&[pack_xy(16,128),16 | 1 << 16,0,0x7c000000,0,0,0,0,0,0,0]);
    command(&mut blue,op::SYNC,&[1,pack_xy(16,128),16 | 1 << 16]);
    command(&mut blue,op::CLUT_LOAD,&[7,pack_xy(16,128),16]);
    command(&mut blue,op::DRAW,&aliased_draw);
    r.execute(&blue).unwrap();
    assert_eq!(pixel(&present(&mut r),24,24), &[0,0,248,255]);
    r.take_stats();
    r.execute(&alias).unwrap();
    assert_eq!(r.take_stats().draws,2);
    assert_eq!(pixel(&present(&mut r),24,24), &[0,248,0,255]);
}

#[test]
fn super_xbr_palette_snapshots_handle_existing_slots_and_partial_eight_bit_clut_loads() {
    let (_egl,gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut setup = Vec::new();
    command(&mut setup,op::SET_STATE,&[0,pack_xy(1023,511),0]);
    command(&mut setup,op::UPLOAD,&[pack_xy(512,0),1 | 1 << 16,0,17]);
    for (x,colour) in [(0,0x001f),(256,0x03e0)] {
        let mut palette = vec![0u16;256];
        palette[17] = colour;
        let mut upload = vec![pack_xy(x,128),256 | 1 << 16,0];
        upload.extend(palette.chunks_exact(2).map(|p|p[0] as u32 | (p[1] as u32) << 16));
        command(&mut setup,op::UPLOAD,&upload);
    }
    command(&mut setup,op::SYNC,&[1,0,1024 | 512 << 16]);
    command(&mut setup,op::CLUT_LOAD,&[0,pack_xy(0,128),256]);
    command(&mut setup,op::CLUT_LOAD,&[1,pack_xy(256,128),256]);
    r.execute(&setup).unwrap();
    // Install with populated slots: unknown initial palettes must not alias.
    r.set_visual_preset(VisualPreset::PsxRevamp {
        scale:4,textures:TextureFilter::Smooth3D,sprites:SpriteReconstruction::SuperXbr3p,
        dithering:false,precision:ColourPrecision::High,
    }).unwrap();
    let mut sprite = [0;16];
    sprite[0] = prim::RECT | prim::TEXTURED | prim::RAW | 1 << prim::DEPTH_SHIFT;
    sprite[1] = 8;
    sprite[3] = pack_xy(2,2);
    sprite[4] = 1 | 1 << 16;
    let mut other = sprite;
    other[1] |= 1 << 16;
    other[3] = pack_xy(6,2);
    let mut both = vec![2]; both.extend(sprite); both.extend(other);
    let mut stream = Vec::new(); command(&mut stream,op::DRAW,&both);
    r.execute(&stream).unwrap();
    let frame = present(&mut r);
    assert_eq!(pixel(&frame,8,8),&[248,0,0,255]);
    assert_eq!(pixel(&frame,24,8),&[0,248,0,255]);
    let mut draw = vec![1]; draw.extend(sprite);
    for (x,count,want) in [(0,256,[248,0,0,255]),(768,16,[248,0,0,255]),(256,256,[0,248,0,255]),(768,16,[0,248,0,255])] {
        let mut load = Vec::new();
        command(&mut load,op::CLUT_LOAD,&[0,pack_xy(x,128),count]);
        command(&mut load,op::DRAW,&draw);
        r.execute(&load).unwrap();
        assert_eq!(pixel(&present(&mut r),8,8),&want,"16-entry reload must retain the current tail");
    }
    let mut change_tail = Vec::new();
    command(&mut change_tail,op::UPLOAD,&[pack_xy(17,128),1 | 1 << 16,0,0x7c00]);
    command(&mut change_tail,op::SYNC,&[1,pack_xy(17,128),1 | 1 << 16]);
    command(&mut change_tail,op::CLUT_LOAD,&[0,pack_xy(0,128),256]);
    command(&mut change_tail,op::DRAW,&draw);
    r.execute(&change_tail).unwrap();
    assert_eq!(pixel(&present(&mut r),8,8),&[0,0,248,255]);
    assert_eq!(r.read_vram(2,2,1,1),vec![0x7c00]);
}

#[test]
fn tiled_super_xbr_backgrounds_batch_without_false_dependencies_but_real_overlaps_still_sync() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    r.set_visual_preset(VisualPreset::PsxRevamp {
        scale: 4, textures: TextureFilter::Smooth3D, sprites: SpriteReconstruction::SuperXbr3p,
        dithering: false, precision: ColourPrecision::High,
    }).unwrap();
    let mut setup = Vec::new();
    command(&mut setup,op::SET_STATE,&[0,pack_xy(1023,511),0]);
    let mut upload = vec![pack_xy(512,0),16 | 16 << 16,0];
    upload.extend(std::iter::repeat_n(0x03e003e0,128));
    command(&mut setup,op::UPLOAD,&upload);
    command(&mut setup,op::SYNC,&[1,pack_xy(512,0),16 | 16 << 16]);
    r.execute(&setup).unwrap();
    let mut sprite = [0;16];
    sprite[0] = prim::RECT | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
    sprite[1] = 8; // Source at x=512, outside the tiled destination.
    sprite[4] = 16 | 16 << 16;
    let mut tiles = vec![300];
    for y in 0..15 {
        for x in 0..20 {
            sprite[3] = pack_xy(x*16,y*16);
            tiles.extend(sprite);
        }
    }
    let mut stream = Vec::new();
    command(&mut stream,op::DRAW,&tiles);
    r.execute(&stream).unwrap(); // Warm the source cache and visual destination.
    r.take_stats();
    r.execute(&stream).unwrap();
    let stats = r.take_stats();
    assert_eq!(stats.draws,2,"disjoint tiles must remain one canonical and one visual batch");
    assert_eq!(stats.syncs,1,"only the previous frame's destination needs copying");
    assert_eq!(r.read_vram(319,239,1,1),vec![0x03e0]);
    assert_eq!(pixel(&present(&mut r),8,8),&[0,248,0,255]);

    // Genuine overlapping semitransparent draws must still see each other's
    // visual results, independently of the canonical read-copy synchronization.
    let mut overlap = Vec::new();
    let mut red = vec![pack_xy(512,0),16 | 16 << 16,0];
    red.extend(std::iter::repeat_n(0x801f801f,128));
    command(&mut overlap,op::UPLOAD,&red);
    command(&mut overlap,op::SYNC,&[2,pack_xy(512,0),16 | 16 << 16,0,16 | 16 << 16]);
    sprite[0] |= prim::SEMI;
    sprite[3] = 0;
    let mut draw = vec![1];
    draw.extend(sprite);
    command(&mut overlap,op::DRAW,&draw);
    command(&mut overlap,op::SYNC,&[1,0,16 | 16 << 16]);
    command(&mut overlap,op::DRAW,&draw);
    r.execute(&overlap).unwrap();
    assert_eq!(pixel(&present(&mut r),8,8),&[186,62,0,255]);
    assert_eq!(r.read_vram(2,2,1,1),vec![0x80f7]);
}

#[test]
fn bicubic_colours_and_coverage_match_a_bounded_catmull_rom_reference() {
    fn kernel(x: f64) -> f64 {
        let x = x.abs();
        if x <= 1.0 { 1.5*x*x*x - 2.5*x*x + 1.0 }
        else if x < 2.0 { -0.5*x*x*x + 2.5*x*x - 4.0*x + 2.0 }
        else { 0.0 }
    }
    let texels: [u16; 16] = [
        0, 0x001f, 0x03e0, 0x7c00,
        0x001f, 0x0210, 0x1084, 0,
        0x03e0, 0x4210, 0x6b5a, 0x8000,
        0x7c00, 0, 0x7fff, 0x8421,
    ];
    let sample = |x: i32, y: i32| {
        let t = texels[(y.clamp(0, 3)*4 + x.clamp(0, 3)) as usize];
        [(t & 31) as f64*8.0, ((t >> 5) & 31) as f64*8.0,
            ((t >> 10) & 31) as f64*8.0, if t == 0 { 0.0 } else { 1.0 }]
    };
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    let mut stream = Vec::new();
    command(&mut stream, op::SET_STATE, &[0, pack_xy(1023, 511), 0]);
    command(&mut stream, op::FILL, &[0, 32 | 32 << 16, 0x7c00]);
    let mut upload = vec![pack_xy(16, 64), 4 | 4 << 16, 0];
    upload.extend(texels.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
    command(&mut stream, op::UPLOAD, &upload);
    command(&mut stream, op::SYNC, &[1, pack_xy(16, 64), 4 | 4 << 16]);
    let mut quad = [0; 16];
    quad[0] = prim::QUAD | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
    for (i, (x, y, u, v)) in [(2, 2, 16, 64), (6, 2, 20, 64), (2, 6, 16, 68), (6, 6, 20, 68)].into_iter().enumerate() {
        quad[3 + 3*i] = pack_xy(x, y);
        quad[5 + 3*i] = u | v << 8;
    }
    let mut draw = vec![1];
    draw.extend(quad);
    command(&mut stream, op::DRAW, &draw);
    for scale in [2, 4] {
        r.set_visual_preset(VisualPreset::PsxRevamp {
            scale, textures: TextureFilter::Nearest, sprites: SpriteReconstruction::Bicubic,
            dithering: false, precision: ColourPrecision::High,
        }).unwrap();
        r.reset_vram();
        r.execute(&stream).unwrap();
        let frame = present(&mut r);
        for y in 0..4*scale {
            for x in 0..4*scale {
                let (u, v) = ((x as f64+0.5)/scale as f64-0.5, (y as f64+0.5)/scale as f64-0.5);
                let (bx, by) = (u.floor() as i32, v.floor() as i32);
                let (mut lower, mut upper) = ([255.0_f64; 3], [0.0_f64; 3]);
                let (mut alpha_lower, mut alpha_upper) = (1.0_f64, 0.0_f64);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let t = sample(bx+dx, by+dy);
                        alpha_lower = alpha_lower.min(t[3]);
                        alpha_upper = alpha_upper.max(t[3]);
                        if t[3] > 0.0 {
                            for c in 0..3 { lower[c] = lower[c].min(t[c]); upper[c] = upper[c].max(t[c]); }
                        }
                    }
                }
                let mut sum = [0.0; 4];
                for dy in -1..=2 {
                    for dx in -1..=2 {
                        let t = sample(bx+dx, by+dy);
                        let w = kernel(u-(bx+dx) as f64)*kernel(v-(by+dy) as f64);
                        for c in 0..4 { sum[c] += t[c]*w; }
                    }
                }
                let coverage = sum[3].clamp(alpha_lower, alpha_upper);
                let got = pixel(&frame, 2*scale+x, 2*scale+y);
                for c in 0..3 {
                    let background = if c == 2 { 248.0 } else { 0.0 };
                    let want = if coverage <= 0.0 { background } else {
                        let foreground = (sum[c]/sum[3].max(0.000001)).clamp(lower[c], upper[c]).round();
                        (background*(1.0-coverage) + foreground*coverage).round()
                    };
                    assert!((got[c] as f64-want).abs() <= 1.0, "bicubic mismatch at {x}/{y}, channel {c}: {} != {want}", got[c]);
                }
            }
        }
    }
}

#[test]
fn small_features_match_upstream_at_unequal_screen_uv_scales_and_ignore_stp_on_opaque_draws() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    r.set_visual_preset(VisualPreset::PsxRevamp {
        scale: 4, textures: TextureFilter::Smooth3D, sprites: SpriteReconstruction::Xbr,
        dithering: false, precision: ColourPrecision::High,
    }).unwrap();
    let mut baseline = None;
    for stp in [false, true] {
        let pixels: Vec<u16> = (0..256).map(|n| {
            let (x, y) = (n % 16, n / 16);
            let palette = [0x3e12, 0x314a, 0x18c6, 0x5294, 0x2529, 0x56d8, 0x044f];
            let color = if (y == 5 || y == 6) && ((x == 4 || x == 5) || (x == 10 || x == 11)) { palette[2] }
                else if y > 9 && x > 2 && x < 13 && (x+y) % 5 == 0 { palette[4] }
                else if x > y && y > 7 { palette[(x+y) as usize % palette.len()] }
                else { palette[0] };
            color | if stp && color != palette[0] { 0x8000 } else { 0 }
        }).collect();
        let mut stream = Vec::new();
        command(&mut stream, op::SET_STATE, &[0, pack_xy(1023, 511), 0]);
        let mut upload = vec![pack_xy(16, 64), 16 | 16 << 16, 0];
        upload.extend(pixels.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
        command(&mut stream, op::UPLOAD, &upload);
        command(&mut stream, op::SYNC, &[1, pack_xy(16, 64), 16 | 16 << 16]);
        let mut quad = [0; 16];
        quad[0] = prim::QUAD | prim::TEXTURED | prim::RAW | 2 << prim::DEPTH_SHIFT;
        for (i, (x, y, u, v)) in [(2, 2, 16, 64), (14, 2, 32, 64), (2, 14, 16, 80), (14, 14, 32, 80)].into_iter().enumerate() {
            quad[3 + 3*i] = pack_xy(x, y);
            quad[5 + 3*i] = u | v << 8;
        }
        let mut draw = vec![1];
        draw.extend(quad);
        command(&mut stream, op::DRAW, &draw);
        r.reset_vram();
        r.execute(&stream).unwrap();
        let frame = present(&mut r);
        let rgb: Vec<u8> = (8..56).flat_map(|y| (8..56).flat_map({
            let frame = &frame;
            move |x| pixel(frame, x, y)[..3].iter().copied()
        })).collect();
        let reference = include_bytes!("fixtures/xbr-lv2-small-details-3x.rgb");
        for (i, (got, want)) in rgb.iter().zip(reference).enumerate() {
            assert!(got.abs_diff(*want) <= 2,
                "reference interpolation differs at ({}, {}) channel {}: {got} != {want}", i/3%48, i/3/48, i%3);
        }
        if let Some(want) = &baseline { assert_eq!(&rgb, want, "opaque STP bits changed reconstructed shapes"); }
        else { baseline = Some(rgb); }
    }
}

#[test]
fn sprite_coverage_composites_over_current_background_and_respects_psx_blending_and_masks() {
    let (_egl, gl) = Headless::new().unwrap();
    let mut r = Renderer::new(gl).unwrap();
    for (precision, sprites) in [
        (ColourPrecision::Native, SpriteReconstruction::Xbr),
        (ColourPrecision::High, SpriteReconstruction::Xbr),
        (ColourPrecision::Native, SpriteReconstruction::Bicubic),
        (ColourPrecision::High, SpriteReconstruction::Bicubic),
        (ColourPrecision::Native, SpriteReconstruction::SuperXbr3p),
        (ColourPrecision::High, SpriteReconstruction::SuperXbr3p),
    ] {
        r.set_visual_preset(VisualPreset::PsxRevamp {
            scale: 4, textures: TextureFilter::Smooth3D, sprites, dithering: false, precision,
        }).unwrap();
        r.reset_vram();
        r.execute(&scene(2, 0x801f, 0, false)).unwrap();
        let opaque = present(&mut r);
        for mode in 0..4 {
            let stream = scene(2, 0x801f, prim::SEMI | mode << 7, false);
            r.reset_vram();
            r.execute(&stream).unwrap();
            let f = present(&mut r);
            for y in 8..40 {
                for x in 8..40 {
                    let p = pixel(&f, x, y);
                    let coverage = pixel(&opaque, x, y)[0] as f32 / 248.0;
                    let half = if precision == ColourPrecision::Native { 120.0 } else { 124.0 };
                    let quarter = if precision == ColourPrecision::Native { 56.0 } else { 62.0 };
                    let (red, blue) = match mode {
                        0 => (half*coverage, 248.0-(248.0-half)*coverage),
                        1 => (248.0*coverage, 248.0),
                        2 => (0.0, 248.0),
                        _ => (quarter*coverage, 248.0),
                    };
                    assert!((p[0] as f32-red).abs() <= 8.0 && (p[2] as f32-blue).abs() <= 8.0,
                        "wrong coverage/STP composite: mode {mode}, {p:?}, expected {red}/{blue}");
                }
            }
        }
        r.reset_vram();
        r.execute(&scene(2, 0x001f, prim::MASK_CHECK, true)).unwrap();
        let masked = present(&mut r);
        for y in 8..40 {
            for x in 8..40 { assert_eq!(pixel(&masked, x, y), &[0, 0, 248, 255]); }
        }
    }
}
