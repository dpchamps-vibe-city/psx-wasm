//! The shaders, GLSL ES 3.00 (WebGL2), integer-only for every pixel
//! decision. See [`crate::setup`] for the record layout they read.
//!
//! One program draws every kind: each instance is one setup record,
//! drawn as its covering rectangle; the fragment shader decides coverage
//! exactly as the GPU does and discards what it would not plot. The
//! destination is read from the read copy of VRAM; a quad's two triangles
//! are applied in order within one invocation, so a pixel both cover is
//! blended twice, as on the GPU.

use crate::setup::STRIDE;

/// Width of the setup-record texture in texels.
pub const PRIM_TEX_WIDTH: usize = 1024;

const COMMON: &str = r#"#version 300 es
precision highp float;
precision highp int;
precision highp usampler2D;
"#;

pub fn draw_vs() -> String {
    format!(
        r#"{COMMON}
uniform usampler2D u_prims;
uniform int u_first;
flat out int v_prim;
void main() {{
    int p = u_first + gl_InstanceID;
    int t = p * {STRIDE} + 2;
    uvec4 bb = texelFetch(u_prims, ivec2(t & {mask}, t >> {shift}), 0);
    v_prim = p;
    int x0 = int(bb.x), y0 = int(bb.y), x1 = int(bb.z) + 1, y1 = int(bb.w) + 1;
    if (x1 <= x0 || y1 <= y0) {{
        gl_Position = vec4(-2.0, -2.0, 0.0, 1.0);
        return;
    }}
    int c = gl_VertexID;
    float x = float((c & 1) != 0 ? x1 : x0);
    float y = float((c & 2) != 0 ? y1 : y0);
    gl_Position = vec4(x / 512.0 - 1.0, y / 256.0 - 1.0, 0.0, 1.0);
}}
"#,
        mask = PRIM_TEX_WIDTH - 1,
        shift = PRIM_TEX_WIDTH.trailing_zeros(),
    )
}

pub fn draw_fs() -> String {
    format!(
        r#"{COMMON}
uniform usampler2D u_vram;   // the read copy
uniform usampler2D u_prims;
uniform usampler2D u_clut;
uniform usampler2D u_stage;
uniform ivec4 u_clip;        // drawing area x0, y0, x1, y1 (inclusive)
uniform int u_skip;          // row parity the line skip leaves, or -1
flat in int v_prim;
out uvec4 o_pix;

const uint TRIANGLE = 1u, QUAD = 2u, RECT = 3u, LINE = 4u, FILL = 5u, UPLOAD = 6u, COPY = 7u;
const uint TEXTURED = 8u, RAW = 16u, GOURAUD = 32u, SEMI = 64u, DITHER = 512u;
const uint MASK_SET = 1024u, MASK_CHECK = 2048u;

uvec4 T(int i) {{
    int t = v_prim * {STRIDE} + i;
    return texelFetch(u_prims, ivec2(t & {mask}, t >> {shift}), 0);
}}

uint vram(int x, int y) {{ return texelFetch(u_vram, ivec2(x, y), 0).r; }}

int sext11(int v) {{ return (v << 21) >> 21; }}

// 64-bit values as (hi, lo).
uvec2 add64(uvec2 a, uvec2 b) {{
    uint lo = a.y + b.y;
    return uvec2(a.x + b.x + (lo < a.y ? 1u : 0u), lo);
}}
uvec2 neg64(uvec2 a) {{
    uint lo = ~a.y + 1u;
    return uvec2(~a.x + (lo == 0u ? 1u : 0u), lo);
}}
// a * n for 0 <= n < 65536.
uvec2 mul64(uvec2 a, uint n) {{
    uint l0 = (a.y & 0xFFFFu) * n;
    uint l1 = (a.y >> 16) * n;
    uint lo = l0 + (l1 << 16);
    return uvec2(a.x * n + (l1 >> 16) + (lo < l0 ? 1u : 0u), lo);
}}
// base + n * step, |n| < 65536.
uvec2 madd64(uvec2 base, uvec2 step, int n) {{
    uvec2 p = mul64(step, uint(n < 0 ? -n : n));
    if (n < 0) p = neg64(p);
    return add64(base, p);
}}

const int DITHER_TABLE[16] = int[16](-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2);
int dither_at(int x, int y) {{ return DITHER_TABLE[((y & 3) << 2) | (x & 3)]; }}
uint dlut(uint v, int d) {{ return uint(clamp((int(v) + d) >> 3, 0, 31)); }}

bool skipped(int y) {{ return u_skip >= 0 && (y & 1) == u_skip; }}

// The four semi-transparency modes on 15-bit pixels (blargg's
// formulas, as the GPU's arithmetic gives them).
uint blend(uint bg, uint fg, uint abr) {{
    if (abr == 0u) {{
        bg |= 0x8000u;
        return (((fg + bg) - ((fg ^ bg) & 0x0421u)) >> 1) & 0xFFFFu;
    }}
    if (abr == 2u) {{
        bg |= 0x8000u;
        fg &= 0x7FFFu;
        uint diff = bg - fg + 0x108420u;
        uint borrow = (diff - ((bg ^ fg) & 0x108420u)) & 0x108420u;
        return ((diff - borrow) & (borrow - (borrow >> 5))) & 0xFFFFu;
    }}
    bg &= 0x7FFFu;
    if (abr == 3u) fg = ((fg >> 2) & 0x1CE7u) | 0x8000u;
    uint sum = fg + bg;
    uint carry = (sum - ((fg ^ bg) & 0x8421u)) & 0x8420u;
    return ((sum - carry) | (carry - (carry >> 5))) & 0xFFFFu;
}}

// Plot `fore` over `dst`: blend, mask check, mask set.
void plot(uint fore, bool textured, uint flags, inout uint dst, inout bool wrote) {{
    if ((flags & SEMI) != 0u && (fore & 0x8000u) != 0u) fore = blend(dst, fore, (flags >> 7) & 3u);
    if ((flags & MASK_CHECK) != 0u && (dst & 0x8000u) != 0u) return;
    dst = (textured ? fore : (fore & 0x7FFFu)) | ((flags & MASK_SET) != 0u ? 0x8000u : 0u);
    wrote = true;
}}

// A texel through the texture window and colour table; 0 = transparent.
uint texel(uint u, uint v, uint flags, uvec4 tw, uint slot) {{
    uint depth = (flags >> 12) & 3u;
    uint u_ext = (u & tw.x) + tw.y;
    int fx = int((u_ext >> (2u - depth)) & 1023u);
    int fy = int(((v & tw.z) + tw.w) & 511u);
    uint word = vram(fx, fy);
    if (depth == 0u) {{
        uint idx = (word >> ((u_ext & 3u) * 4u)) & 0xFu;
        return texelFetch(u_clut, ivec2(int(idx), int(slot)), 0).r;
    }}
    if (depth == 1u) {{
        uint idx = (word >> ((u_ext & 1u) * 8u)) & 0xFFu;
        return texelFetch(u_clut, ivec2(int(idx), int(slot)), 0).r;
    }}
    return word;
}}

uint modulate(uint t, uint r, uint g, uint b, int d) {{
    uint mr = ((t & 0x1Fu) * r) >> 4;
    uint mg = (((t >> 5) & 0x1Fu) * g) >> 4;
    uint mb = (((t >> 10) & 0x1Fu) * b) >> 4;
    return (t & 0x8000u) | dlut(mr, d) | (dlut(mg, d) << 5) | (dlut(mb, d) << 10);
}}

uint shade(uint r, uint g, uint b, bool dither, int d) {{
    if (dither) return 0x8000u | dlut(r, d) | (dlut(g, d) << 5) | (dlut(b, d) << 10);
    return 0x8000u | (r >> 3) | ((g >> 3) << 5) | ((b >> 3) << 10);
}}

// The textured or shaded pixel of a polygon or rectangle.
void surface(uint u, uint v, uint r, uint g, uint b, uint flags, uvec4 h0, uvec4 tw, int d,
             inout uint dst, inout bool wrote) {{
    if ((flags & TEXTURED) != 0u) {{
        uint t = texel(u, v, flags, tw, h0.y >> 16);
        if (t == 0u) return;
        if ((flags & RAW) == 0u) t = modulate(t, r, g, b, (flags & DITHER) != 0u ? d : 0);
        plot(t, true, flags, dst, wrote);
    }} else {{
        plot(shade(r, g, b, (flags & DITHER) != 0u, d), false, flags, dst, wrote);
    }}
}}

// One triangle of a polygon at texel `b`.
void triangle(int b, int px, int py, uint flags, uvec4 h0, uvec4 tw, inout uint dst, inout bool wrote) {{
    uvec4 t0 = T(b);
    if (t0.x == 0u) return;
    int y0 = int(t0.y), y1 = int(t0.z), y2 = int(t0.w);
    int yi = y0 + ((py - y0) & 511);
    if (yi >= y2) return;
    int sy = sext11(yi);
    if (sy < u_clip.y || sy > u_clip.w || skipped(yi)) return;
    int part = b + (yi < y1 ? 1 : 4);
    uvec4 ph = T(part);
    int yref = int(ph.x);
    // Walking up, the GPU stops at the first row above the drawing area,
    // including one reached by wrapping past y = 1024.
    if (ph.y != 0u && ((yi + 1024) >> 11) != ((yref - 1 + 1024) >> 11)) return;
    uvec4 pl = T(part + 1), pr = T(part + 2);
    int n = yi - yref;
    int xs = int(madd64(pl.xy, pl.zw, n).x);
    int xb = int(madd64(pr.xy, pr.zw, n).x);
    int w = xb - xs;
    int x = sext11(xs);
    if (x < u_clip.x) {{ w -= u_clip.x - x; x = u_clip.x; }}
    if (x + w > u_clip.z + 1) w = u_clip.z + 1 - x;
    if (w <= 0 || px < x || px >= x + w) return;
    uint ix = uint(xs + (px - sext11(xs)));
    uint iy = uint(yi);
    uvec4 c0 = T(b + 7), c1 = T(b + 8), c2 = T(b + 9), c3 = T(b + 10);
    uint u = (c0.x + c1.y * ix + c2.z * iy) >> 24;
    uint v = (c0.y + c1.z * ix + c2.w * iy) >> 24;
    uint r = (c0.z + c1.w * ix + c3.x * iy) >> 24;
    uint g = (c0.w + c2.x * ix + c3.y * iy) >> 24;
    uint bl = (c1.x + c2.y * ix + c3.z * iy) >> 24;
    surface(u, v, r, g, bl, flags, h0, tw, dither_at(px, py), dst, wrote);
}}

void rect(int px, int py, uint flags, uvec4 h0, uvec4 tw, inout uint dst, inout bool wrote) {{
    uvec4 a = T(3), t = T(4), c = T(5);
    int xs = int(a.x), xb = int(a.y), ys = int(a.z), yb = int(a.w);
    if (px < xs || px >= xb) return;
    int y = ys + ((py - ys) & 511);
    if (y >= yb || skipped(y)) return;
    uint u = (t.x + uint((px - xs) * int(t.z))) & 0xFFu;
    uint v = (t.y + uint((y - ys) * int(t.w))) & 0xFFu;
    // Rectangles never dither.
    surface(u, v, c.x & 0xFFu, (c.x >> 8) & 0xFFu, (c.x >> 16) & 0xFFu, flags & ~DITHER, h0, tw, 0, dst, wrote);
}}

void line(int px, int py, uint flags, inout uint dst, inout bool wrote) {{
    if (px < u_clip.x || px > u_clip.z) return;
    uvec4 a = T(3), cx = T(4), st = T(5), c0 = T(6), dc = T(7);
    int x0 = int(a.x), y0 = int(a.y), k = int(a.z);
    int i = -1;
    int yv = 0;
    if (a.w != 0u) {{
        // x advances one pixel per step.
        i = (px - x0) & 2047;
        if (i > k) return;
        yv = int(madd64(cx.zw, st.zw, i).x) & 2047;
        if ((yv & 511) != py) return;
    }} else {{
        // y advances one pixel per step; rows plot at y & 511.
        int sgn = c0.w != 0u ? -1 : 1;
        for (int m = 0; m < 2; m++) {{
            int cand = py + 512 * m;
            int j = (sgn * (cand - y0)) & 2047;
            if (j > k) continue;
            int xv = int(madd64(cx.xy, st.xy, j).x) & 2047;
            if (xv != px) continue;
            i = j;
            yv = cand;
        }}
        if (i < 0) return;
    }}
    if (yv < u_clip.y || yv > u_clip.w || skipped(yv)) return;
    uint r, g, b;
    if ((flags & GOURAUD) != 0u) {{
        uint ui = uint(i);
        r = ((c0.x + ui * dc.x) >> 12) & 0xFFu;
        g = ((c0.y + ui * dc.y) >> 12) & 0xFFu;
        b = ((c0.z + ui * dc.z) >> 12) & 0xFFu;
    }} else {{
        r = dc.w & 0xFFu;
        g = (dc.w >> 8) & 0xFFu;
        b = (dc.w >> 16) & 0xFFu;
    }}
    plot(shade(r, g, b, (flags & DITHER) != 0u, dither_at(px, yv)), false, flags, dst, wrote);
}}

void main() {{
    int px = int(gl_FragCoord.x), py = int(gl_FragCoord.y);
    uvec4 h0 = T(0);
    uint flags = h0.x;
    uint kind = flags & 7u;
    uint dst = vram(px, py);
    bool wrote = false;
    if (kind == TRIANGLE || kind == QUAD) {{
        uvec4 tw = T(1);
        triangle(3, px, py, flags, h0, tw, dst, wrote);
        if (kind == QUAD) triangle(14, px, py, flags, h0, tw, dst, wrote);
    }} else if (kind == RECT) {{
        rect(px, py, flags, h0, T(1), dst, wrote);
    }} else if (kind == LINE) {{
        line(px, py, flags, dst, wrote);
    }} else {{
        uvec4 a = T(3), m = T(4);
        int i = (px - int(a.x)) & 1023, j = (py - int(a.y)) & 511;
        if (i >= int(a.z) || j >= int(a.w)) discard;
        if (kind == FILL) {{
            if (skipped(py)) discard;
            dst = m.x;
            wrote = true;
        }} else {{
            uint src = kind == UPLOAD
                ? texelFetch(u_stage, ivec2(int(m.x) + i, int(m.y) + j), 0).r
                : vram((int(m.x) + i) & 1023, (int(m.y) + j) & 511);
            if ((m.z & 2u) == 0u || (dst & 0x8000u) == 0u) {{
                dst = src | ((m.z & 1u) != 0u ? 0x8000u : 0u);
                wrote = true;
            }}
        }}
    }}
    if (!wrote) discard;
    o_pix = uvec4(dst, 0u, 0u, 0u);
}}
"#,
        mask = PRIM_TEX_WIDTH - 1,
        shift = PRIM_TEX_WIDTH.trailing_zeros(),
    )
}

/// Present: the display area to RGBA8, 15-bit or 24-bit.
pub fn present_vs() -> String {
    format!(
        r#"{COMMON}
void main() {{
    int c = gl_VertexID;
    gl_Position = vec4((c & 1) != 0 ? 1.0 : -1.0, (c & 2) != 0 ? 1.0 : -1.0, 0.0, 1.0);
}}
"#
    )
}

pub fn present_fs() -> String {
    format!(
        r#"{COMMON}
uniform usampler2D u_vram;
uniform ivec4 u_area;   // x, y, width, height
uniform int u_depth24;
uniform int u_flip;     // 1: output row 0 is the picture's bottom row
out vec4 o_color;
uint px16(int x, int y) {{ return texelFetch(u_vram, ivec2(x & 1023, y & 511), 0).r; }}
void main() {{
    int ox = int(gl_FragCoord.x), oy = int(gl_FragCoord.y);
    int row = u_flip != 0 ? u_area.w - 1 - oy : oy;
    int y = u_area.y + row;
    uint r, g, b;
    if (u_depth24 != 0) {{
        int byte0 = 3 * ox;
        uint w0 = px16(u_area.x + (byte0 >> 1), y);
        uint w1 = px16(u_area.x + (byte0 >> 1) + 1, y);
        uint bytes = w0 | (w1 << 16);
        uint s = uint(byte0 & 1) * 8u;
        r = (bytes >> s) & 0xFFu;
        g = (bytes >> (s + 8u)) & 0xFFu;
        b = (bytes >> (s + 16u)) & 0xFFu;
    }} else {{
        uint p = px16(u_area.x + ox, y);
        r = (p & 0x1Fu) << 3;
        g = ((p >> 5) & 0x1Fu) << 3;
        b = ((p >> 10) & 0x1Fu) << 3;
    }}
    o_color = vec4(float(r), float(g), float(b), 255.0) / 255.0;
}}
"#
    )
}
