/*
 * Sprite reconstruction adapted from Hyllian's xBR-lv2 shader (corner C,
 * small-detail preservation), https://github.com/libretro/glsl-shaders/blob/master/xbr/shaders/xbr-lv2.glsl
 *
 * Copyright (C) 2011-2016 Hyllian - sergiogdb@gmail.com
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 *
 * Original incorporates ideas from Joshua Street's SABR shader.
 * PSXRevamp adaptations: decoded VRAM/CLUT taps, bounded atlas footprint,
 * transparent shape detection, premultiplied coverage, separate PSX STP class,
 * and analytic sampling footprint instead of a fixed output scale.
 */

float xbr_key(uint p) {
    // Transparency must differ from opaque black (0x8000). Keep it outside
    // the entire luminance range and equality threshold of opaque colours.
    return p == 0u ? -64.0 : dot(rgb5(p) * (8.0 / 255.0), vec3(14.352, 28.176, 5.472));
}
float xbr_detail_key(uint p) {
    // Upstream's small-details path uses Rec.709 for the outer ring, not
    // the inner ring's Rec.601 coefficients. Both are scaled by Y weight 48.
    return p == 0u ? -64.0 : dot(rgb5(p) * (8.0 / 255.0), 48.0 * vec3(0.2126, 0.7152, 0.0722));
}
// Match the reference's normalized colour arithmetic, including its choice
// between equally weighted corner reconstructions. RGB8-scale arithmetic can
// round that comparison differently and select a different small-feature shape.
vec4 xbr_rgba(uint p) { return p == 0u ? vec4(0.0) : vec4(rgb5(p) * (8.0 / 255.0), 1.0); }
vec4 xbr_eq(vec4 a, vec4 b) { return step(abs(a - b), vec4(15.0)); }
vec4 xbr_neq(vec4 a, vec4 b) { return vec4(1.0) - xbr_eq(a, b); }
vec4 xbr_diff(vec4 a, vec4 b) { return vec4(notEqual(a, b)); }
vec4 xbr_distance(vec4 a, vec4 b, vec4 c, vec4 d, vec4 e, vec4 f,
                  vec4 g, vec4 h, vec4 i, vec4 j, vec4 k, vec4 l) {
    return abs(a-b) + abs(a-c) + abs(d-e) + abs(d-f) + abs(i-j) + abs(k-l) + 2.0*abs(g-h);
}

vec4 xbr_mix(vec4 a, uint b, float amount, uint flags, inout uint source) {
    // STP changes colour blending only on SEMI primitives. On opaque draws,
    // equal colours must reconstruct identically regardless of their mask bit.
    // Transparent taps have no black colour to bleed into an opaque edge.
    if ((flags & SEMI) != 0u && a.a > 0.0 && b != 0u && ((source ^ b) & 0x8000u) != 0u) return a;
    if (b != 0u && (a.a == 0.0 || amount > 0.5)) source = b;
    return mix(a, xbr_rgba(b), amount);
}
float xbr_change(vec4 a, vec4 b) {
    vec4 d = abs(a-b);
    return d.r + d.g + d.b + 3.0*d.a;
}

vec4 sprite_xbr(vec2 at, vec2 lo, vec2 hi, float footprint, uint flags, uvec4 h0, uvec4 tw, out uint source) {
    ivec2 p = ivec2(floor(at + 0.5));
    vec2 fp = fract(at + 0.5);
    // The 21-tap neighbourhood and rotated corner rules are those of xBR-lv2.
    uint A1 = sprite_texel(p + ivec2(-1,-2), lo, hi, flags, h0, tw);
    uint B1 = sprite_texel(p + ivec2( 0,-2), lo, hi, flags, h0, tw);
    uint C1 = sprite_texel(p + ivec2( 1,-2), lo, hi, flags, h0, tw);
    uint A  = sprite_texel(p + ivec2(-1,-1), lo, hi, flags, h0, tw);
    uint B  = sprite_texel(p + ivec2( 0,-1), lo, hi, flags, h0, tw);
    uint C  = sprite_texel(p + ivec2( 1,-1), lo, hi, flags, h0, tw);
    uint D  = sprite_texel(p + ivec2(-1, 0), lo, hi, flags, h0, tw);
    uint E  = sprite_texel(p,               lo, hi, flags, h0, tw);
    uint F  = sprite_texel(p + ivec2( 1, 0), lo, hi, flags, h0, tw);
    uint G  = sprite_texel(p + ivec2(-1, 1), lo, hi, flags, h0, tw);
    uint H  = sprite_texel(p + ivec2( 0, 1), lo, hi, flags, h0, tw);
    uint I  = sprite_texel(p + ivec2( 1, 1), lo, hi, flags, h0, tw);
    uint G5 = sprite_texel(p + ivec2(-1, 2), lo, hi, flags, h0, tw);
    uint H5 = sprite_texel(p + ivec2( 0, 2), lo, hi, flags, h0, tw);
    uint I5 = sprite_texel(p + ivec2( 1, 2), lo, hi, flags, h0, tw);
    uint A0 = sprite_texel(p + ivec2(-2,-1), lo, hi, flags, h0, tw);
    uint D0 = sprite_texel(p + ivec2(-2, 0), lo, hi, flags, h0, tw);
    uint G0 = sprite_texel(p + ivec2(-2, 1), lo, hi, flags, h0, tw);
    uint C4 = sprite_texel(p + ivec2( 2,-1), lo, hi, flags, h0, tw);
    uint F4 = sprite_texel(p + ivec2( 2, 0), lo, hi, flags, h0, tw);
    uint I4 = sprite_texel(p + ivec2( 2, 1), lo, hi, flags, h0, tw);

    vec4 b = vec4(xbr_key(B), xbr_key(D), xbr_key(H), xbr_key(F));
    vec4 c = vec4(xbr_key(C), xbr_key(A), xbr_key(G), xbr_key(I));
    vec4 d = b.yzwx, e = vec4(xbr_key(E)), f = b.wxyz;
    vec4 g = c.zwxy, h = b.zwxy, i = c.wxyz;
    vec4 i4 = vec4(xbr_detail_key(I4), xbr_detail_key(C1), xbr_detail_key(A0), xbr_detail_key(G5));
    vec4 i5 = vec4(xbr_detail_key(I5), xbr_detail_key(C4), xbr_detail_key(A1), xbr_detail_key(G0));
    vec4 h5 = vec4(xbr_detail_key(H5), xbr_detail_key(F4), xbr_detail_key(B1), xbr_detail_key(D0));
    vec4 f4 = h5.yzwx;

    vec4 irlv0 = xbr_diff(e,f) * xbr_diff(e,h);
    vec4 irlv1 = irlv0 * (xbr_neq(f,b)*xbr_neq(f,c) + xbr_neq(h,d)*xbr_neq(h,g)
        + xbr_eq(e,i)*(xbr_neq(f,f4)*xbr_neq(f,i4) + xbr_neq(h,h5)*xbr_neq(h,i5))
        + xbr_eq(e,g) + xbr_eq(e,c));
    vec4 irlv2l = xbr_diff(e,g) * xbr_diff(d,g);
    vec4 irlv2u = xbr_diff(e,c) * xbr_diff(b,c);

    vec4 ao = vec4(1,-1,-1,1), bo = vec4(1,1,-1,-1);
    vec4 co = vec4(1.5,0.5,-0.5,0.5);
    vec4 bx = vec4(0.5,2,-0.5,-2), cx = vec4(1,1,-0.5,0);
    vec4 by = vec4(2,0.5,-2,-0.5), cy = vec4(2,0,-1,0.5);
    vec4 delta = vec4(max(footprint, 0.0001));
    vec4 delta_l = delta * vec4(0.5,1,0.5,1), delta_u = delta_l.yxwz;
    vec4 fx = ao*fp.y + bo*fp.x;
    vec4 fx45i = clamp((fx + delta - co - 0.25)/(2.0*delta), 0.0, 1.0);
    vec4 fx45 = clamp((fx + delta - co)/(2.0*delta), 0.0, 1.0);
    vec4 fx30 = clamp((ao*fp.y + bx*fp.x + delta_l - cx)/(2.0*delta_l), 0.0, 1.0);
    vec4 fx60 = clamp((ao*fp.y + by*fp.x + delta_u - cy)/(2.0*delta_u), 0.0, 1.0);

    vec4 wd1 = xbr_distance(e,c,g,i,f4,h5,h,f,b,d,i4,i5);
    vec4 wd2 = xbr_distance(h,d,i5,f,b,i4,e,i,g,h5,c,f4);
    vec4 edri = step(wd1,wd2) * irlv0;
    vec4 edr = step(wd1 + 0.1,wd2) * step(vec4(0.5),irlv1);
    vec4 edr_l = step(2.0*abs(f-g),abs(h-c)) * irlv2l * edr;
    vec4 edr_u = step(2.0*abs(h-c),abs(f-g)) * irlv2u * edr;
    vec4 weights = max(max(edr_l*fx30,edr_u*fx60),max(edr*fx45,edri*fx45i));
    vec4 choose = step(abs(e-f),abs(e-h));

    uint s1 = E, s2 = E;
    vec4 centre = xbr_rgba(E);
    vec4 r1 = xbr_mix(centre, choose.x > 0.5 ? F : H, weights.x, flags, s1);
    r1 = xbr_mix(r1, choose.z > 0.5 ? D : B, weights.z, flags, s1);
    vec4 r2 = xbr_mix(centre, choose.y > 0.5 ? B : F, weights.y, flags, s2);
    r2 = xbr_mix(r2, choose.w > 0.5 ? H : D, weights.w, flags, s2);
    bool second = xbr_change(centre,r2) >= xbr_change(centre,r1);
    source = second ? s2 : s1;
    return second ? r2 : r1;
}

void sprite_surface(vec2 at, vec2 lo, vec2 hi, float footprint, uvec3 color, uint flags, uvec4 h0, uvec4 tw, int d,
                    inout uint dst, inout bool wrote) {
    if (SUPER_SPRITES) {
        super_surface(at, lo, hi, footprint, color, flags, h0, tw, d, dst, wrote);
        return;
    }
    if (MMPX_SPRITES) {
        mmpx_surface(at, lo, hi, footprint, color, flags, h0, tw, d, dst, wrote);
        return;
    }
    if (BICUBIC_SPRITES) {
        bicubic_surface(at, lo, hi, footprint, color, flags, h0, tw, d, dst, wrote);
        return;
    }
    if (footprint >= 1.0) {
        // No magnification: retain crisp native samples rather than adding blur.
        ivec2 p = ivec2(floor(at + 0.5));
        surface(uint(p.x) & 255u, uint(p.y) & 255u, color.r, color.g, color.b, flags, h0, tw, d, dst, wrote);
        return;
    }
    uint source;
    vec4 rgba = sprite_xbr(at, lo, hi, footprint, flags, h0, tw, source);
    if (rgba.a <= 0.0) return;
    uint t = colour_pixel(rgba.rgb * (255.0 / rgba.a), native_pixel(source) & PIXEL_MASK);
    if ((flags & RAW) == 0u) t = modulate(t, color.r, color.g, color.b, (flags & DITHER) != 0u ? d : 0);
    uint background = dst;
    bool covered = false;
    plot(t, true, flags, dst, covered);
    if (!covered) return;
    // Coverage is independent of PSX semitransparency. Apply the latter first,
    // then composite over the visual background without introducing black halos.
    dst = colour_pixel(mix(visual_rgb(background), visual_rgb(dst), rgba.a), dst & PIXEL_MASK);
    wrote = true;
}

void enhanced_rect(int px, int py, uint flags, uvec4 h0, uvec4 tw, inout uint dst, inout bool wrote) {
    if (!RECONSTRUCT_SPRITES || (h0.z & 4u) == 0u) {
        rect(px, py, flags, h0, tw, dst, wrote);
        return;
    }
    uvec4 a = T(3), t = T(4), c = T(5), bounds = T(6);
    int xs = int(a.x), xb = int(a.y), ys = int(a.z), yb = int(a.w);
    if (px < xs || px >= xb) return;
    int y = ys + ((py - ys) & 511);
    if (y >= yb || skipped(y)) return;
    ivec2 step_uv = ivec2(t.zw);
    ivec2 uv = (ivec2(t.xy) + ivec2(px-xs,y-ys)*step_uv) & 255;
    vec2 sub = (vec2(ivec2(gl_FragCoord.xy) % u_scale) + 0.5) / float(u_scale) - 0.5;
    // Clamp taps, not the interpolation phase. Clamping `at` changes the
    // apparent width/shape of features touching a sprite's texel bounds.
    vec2 at = vec2(uv) + sub*vec2(step_uv);
    sprite_surface(at, vec2(bounds.xy), vec2(bounds.zw), 1.0/float(u_scale),
        uvec3(c.x & 255u, (c.x >> 8) & 255u, (c.x >> 16) & 255u), flags & ~DITHER, h0, tw, 0, dst, wrote);
}
