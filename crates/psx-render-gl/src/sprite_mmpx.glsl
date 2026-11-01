/*
 * MMPX style-preserving 2x pixel-art magnification.
 * Copyright 2020 Morgan McGuire & Mara Gagiu.
 *
 * Adapted from their MIT-licensed algorithm, via the GLSL-derived ReShade
 * transcription by Marty McFly (martymcmodding):
 * https://gist.github.com/martymcmodding/ed60af0f8b928c59ce2cabd5292ff11a
 * Paper: https://jcgt.org/published/0010/02/04/
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
 * PSXRevamp samples the virtual 2x result directly, nearest-scaled to the
 * primitive's output size. This is one MMPX pass, not recursive 4x processing.
 * Rules compare decoded colours/opacity, not palette indices. Selected texels
 * retain their original STP/mask bit for the PSX pixel pipeline.
 */

uint mmpx_key(uint p, uint flags) {
    // Distinguish transparent zero from opaque black. Ignore the STP bit for
    // colour equality on opaque primitives, but retain it on selected texels.
    return p == 0u ? 0u : 0x10000u | (p & ((flags & SEMI) != 0u ? 0xffffu : 0x7fffu));
}
uint mmpx_luma(uint p) {
    // The reference's RGB8/alpha luminance ordering, with PSX binary opacity.
    return p == 0u ? 256u : uint(dot(rgb5(p), vec3(8.0))) + 1u;
}

#define MEQ(a,b) (mmpx_key(a,flags) == mmpx_key(b,flags))
#define MNE(a,b) (!MEQ(a,b))
#define MALL2(b,a0,a1) (MEQ(b,a0) && MEQ(b,a1))
#define MALL3(b,a0,a1,a2) (MALL2(b,a0,a1) && MEQ(b,a2))
#define MALL4(b,a0,a1,a2,a3) (MALL3(b,a0,a1,a2) && MEQ(b,a3))
#define MANY3(b,a0,a1,a2) (MEQ(b,a0) || MEQ(b,a1) || MEQ(b,a2))
#define MNONE2(b,a0,a1) (MNE(b,a0) && MNE(b,a1))
#define MNONE4(b,a0,a1,a2,a3) (MNONE2(b,a0,a1) && MNONE2(b,a2,a3))
#define MS(x,y) sprite_texel(p + ivec2(x,y), lo, hi, flags, h0, tw)

uint sprite_mmpx(vec2 at, vec2 lo, vec2 hi, uint flags, uvec4 h0, uvec4 tw) {
    ivec2 p = ivec2(floor(at + 0.5));
    uint A = MS(-1,-1), B = MS(0,-1), C = MS(1,-1);
    uint D = MS(-1,0),  E = MS(0,0),  F = MS(1,0);
    uint G = MS(-1,1),  H = MS(0,1),  I = MS(1,1);
    uint J = E, K = E, L = E, M = E;

    if (!(MALL4(E,A,B,C,D) && MALL4(E,F,G,H,I))) {
        uint P = MS(0,-2), S = MS(0,2), Q = MS(-2,0), R = MS(2,0);
        uint Bl = mmpx_luma(B), Dl = mmpx_luma(D), El = mmpx_luma(E);
        uint Fl = mmpx_luma(F), Hl = mmpx_luma(H);

        // 1:1 slope rules.
        if (MEQ(D,B) && MNE(D,H) && MNE(D,F) && (El >= Dl || MEQ(E,A)) && MANY3(E,A,C,G) && (El < Dl || MNE(A,D) || MNE(E,P) || MNE(E,Q))) J = D;
        if (MEQ(B,F) && MNE(B,D) && MNE(B,H) && (El >= Bl || MEQ(E,C)) && MANY3(E,A,C,I) && (El < Bl || MNE(C,B) || MNE(E,P) || MNE(E,R))) K = B;
        if (MEQ(H,D) && MNE(H,F) && MNE(H,B) && (El >= Hl || MEQ(E,G)) && MANY3(E,A,G,I) && (El < Hl || MNE(G,H) || MNE(E,S) || MNE(E,Q))) L = H;
        if (MEQ(F,H) && MNE(F,B) && MNE(F,D) && (El >= Fl || MEQ(E,I)) && MANY3(E,C,G,I) && (El < Fl || MNE(I,H) || MNE(E,R) || MNE(E,S))) M = F;

        // Intersection rules.
        if (MNE(E,F) && MALL4(E,C,I,D,Q) && MALL2(F,B,H) && MNE(F,MS(3,0))) K = M = F;
        if (MNE(E,D) && MALL4(E,A,G,F,R) && MALL2(D,B,H) && MNE(D,MS(-3,0))) J = L = D;
        if (MNE(E,H) && MALL4(E,G,I,B,P) && MALL2(H,D,F) && MNE(H,MS(0,3))) L = M = H;
        if (MNE(E,B) && MALL4(E,A,C,H,S) && MALL2(B,D,F) && MNE(B,MS(0,-3))) J = K = B;
        if (Bl < El && MALL4(E,G,H,I,S) && MNONE4(E,A,D,C,F)) J = K = B;
        if (Hl < El && MALL4(E,A,B,C,P) && MNONE4(E,D,G,I,F)) L = M = H;
        if (Fl < El && MALL4(E,A,D,G,Q) && MNONE4(E,B,C,I,H)) K = M = F;
        if (Dl < El && MALL4(E,C,F,I,R) && MNONE4(E,B,A,G,H)) J = L = D;

        // 2:1 slope rules. Assignments deliberately use the already updated
        // quadrant values, in the same sequence as the reference algorithm.
        if (MNE(H,B)) {
            if (MNONE2(H,A,E) && MNE(H,C)) {
                if (MALL3(H,G,F,R) && MNONE2(H,D,MS(2,-1))) L = M;
                if (MALL3(H,I,D,Q) && MNONE2(H,F,MS(-2,-1))) M = L;
            }
            if (MNONE2(B,I,G) && MNE(B,E)) {
                if (MALL3(B,A,F,R) && MNONE2(B,D,MS(2,1))) J = K;
                if (MALL3(B,C,D,Q) && MNONE2(B,F,MS(-2,1))) K = J;
            }
        }
        if (MNE(F,D)) {
            if (MNONE2(D,I,E) && MNE(D,C)) {
                if (MALL3(D,A,H,S) && MNONE2(D,B,MS(1,2))) J = L;
                if (MALL3(D,G,B,P) && MNONE2(D,H,MS(1,-2))) L = J;
            }
            if (MNONE2(F,E,A) && MNE(F,G)) {
                if (MALL3(F,C,H,S) && MNONE2(F,B,MS(-1,2))) K = M;
                if (MALL3(F,I,B,P) && MNONE2(F,H,MS(-1,-2))) M = K;
            }
        }
    }
    bvec2 bottom_right = greaterThanEqual(fract(at + 0.5), vec2(0.5));
    return bottom_right.y ? (bottom_right.x ? M : L) : (bottom_right.x ? K : J);
}

#undef MEQ
#undef MNE
#undef MALL2
#undef MALL3
#undef MALL4
#undef MANY3
#undef MNONE2
#undef MNONE4
#undef MS

void mmpx_surface(vec2 at, vec2 lo, vec2 hi, float footprint, uvec3 color, uint flags, uvec4 h0, uvec4 tw, int d,
                  inout uint dst, inout bool wrote) {
    uint t = footprint >= 1.0
        ? sprite_texel(ivec2(floor(at + 0.5)), lo, hi, flags, h0, tw)
        : sprite_mmpx(at, lo, hi, flags, h0, tw);
    if (t == 0u) return;
    t = native_pixel(t);
    if ((flags & RAW) == 0u) t = modulate(t, color.r, color.g, color.b, (flags & DITHER) != 0u ? d : 0);
    plot(t, true, flags, dst, wrote);
}
