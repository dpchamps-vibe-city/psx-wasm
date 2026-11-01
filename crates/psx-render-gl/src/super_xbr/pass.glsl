/*
 * Super-xBR passes 0, 1 and optional smoothing pass 2.
 * Adapted from libretro/common-shaders/xbr/shaders/super-xbr/super-xbr-pass{0,1,2}.cg
 * Copyright (c) 2015 Hyllian - sergiogdb@gmail.com
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
 * PSXRevamp: decoded read-only native textures, premultiplied coverage, bounded
 * per-sprite sampling and separate opaque/STP planes for SEMI primitives.
 * Opaque RGB uses the Cg preset's defaults: edge strength 2, weight 1, AR 1.
 */
precision highp float;
precision highp int;
precision highp usampler2D;
uniform usampler2D u_vram;
uniform usampler2D u_clut;
uniform highp sampler2D u_previous;
uniform ivec4 u_bounds;
uniform uvec4 u_tw;
uniform uvec2 u_info; // depth/SEMI flags, CLUT slot
uniform ivec2 u_size; // native sprite dimensions
out vec4 o_colour;

vec4 original(vec2 at, int plane) {
    ivec2 p = clamp(ivec2(floor(at)), ivec2(0), u_size-1) + u_bounds.xy;
    uint depth = (u_info.x >> 12) & 3u;
    uint u = (uint(p.x) & u_tw.x) + u_tw.y;
    uint v = ((uint(p.y) & u_tw.z) + u_tw.w) & 511u;
    uint t = texelFetch(u_vram, ivec2(int((u >> (2u-depth)) & 1023u), int(v)), 0).r;
    if (depth < 2u) {
        uint index = depth == 0u ? (t >> ((u & 3u)*4u)) & 15u : (t >> ((u & 1u)*8u)) & 255u;
        t = texelFetch(u_clut, ivec2(int(index), int(u_info.y)), 0).r;
    }
    if (t == 0u || ((u_info.x & 64u) != 0u && int((t >> 15) & 1u) != plane)) return vec4(0);
    return vec4(vec3(t & 31u, (t >> 5) & 31u, (t >> 10) & 31u)*(8.0/255.0), 1);
}
vec4 previous(vec2 at, int plane, ivec2 size) {
    ivec2 p = clamp(ivec2(floor(at)), ivec2(0), size-1);
    return texelFetch(u_previous, p + ivec2(plane*size.x, 0), 0);
}
float luminance(vec4 c) {
    // Identical to the reference for opaque images. Give transparency its own
    // edge distance instead of interpreting missing coverage as black pigment.
    return dot(c.rgb/max(c.a,0.000001), vec3(.2126,.7152,.0722)) + 2.0*(1.0-c.a);
}
#if SUPER_PASS == 0
const vec4 WP = vec4(2,1,-1,4);
const vec2 WQ = vec2(-1,1);
const float W1 = 1.29633/10.0, W2 = 1.75068/20.0;
#elif SUPER_PASS == 1
const vec4 WP = vec4(8,0,0,0);
const vec2 WQ = vec2(0,0);
const float W1 = 1.75068/10.0, W2 = 1.29633/20.0;
#else
const vec4 WP = vec4(1,0,2,3);
const vec2 WQ = vec2(-2,1);
const float W1 = 1.29633/10.0, W2 = 1.75068/20.0;
#endif
float diagonal(float b0,float b1,float c0,float c1,float c2,float d0,float d1,float d2,float d3,float e1,float e2,float e3,float f2,float f3) {
    return WP.x*(abs(c1-c2)+abs(c1-c0)+abs(e2-e1)+abs(e2-e3))
        + WP.y*(abs(d2-d3)+abs(d0-d1)) + WP.z*(abs(d1-d3)+abs(d0-d2))
        + WP.w*abs(d1-d2) + WQ.x*(abs(c0-c2)+abs(e1-e3)) + WQ.y*(abs(b0-b1)+abs(f2-f3));
}
float axial(float i1,float i2,float i3,float i4,float e1,float e2,float e3,float e4) {
    return WP.w*(abs(i1-i2)+abs(i3-i4)) + WP.x*(abs(i1-e1)+abs(i2-e2)+abs(i3-e3)+abs(i4-e4));
}
void main() {
#if SUPER_PASS == 0
    int plane = int(gl_FragCoord.x)/u_size.x;
    vec2 p = vec2(int(gl_FragCoord.x)%u_size.x, int(gl_FragCoord.y)) + 0.5;
    #define S(x,y) original(p+vec2(x,y),plane)
#elif SUPER_PASS == 1
    int plane = int(gl_FragCoord.x)/(2*u_size.x);
    vec2 p = (vec2(int(gl_FragCoord.x)%(2*u_size.x), int(gl_FragCoord.y))+0.5)*0.5;
    vec2 fp = fract(p), dir = fp-0.5;
    if (dir.x*dir.y > 0.0) {
        o_colour = fp.x > 0.5 ? previous(p,plane,u_size) : original(p,plane);
        return;
    }
    vec2 g1 = fp.x > 0.5 ? vec2(0.5,0) : vec2(0,0.5);
    vec2 g2 = fp.x > 0.5 ? vec2(0,0.5) : vec2(0.5,0);
#else
    ivec2 size = 2*u_size;
    int plane = int(gl_FragCoord.x)/size.x;
    vec2 p = vec2(int(gl_FragCoord.x)%size.x, int(gl_FragCoord.y)) + 0.5;
    #define S(x,y) previous(p+vec2(x,y),plane,size)
#endif
#if SUPER_PASS == 1
    vec4 P0=original(p-3.0*g1,plane), P1=previous(p-3.0*g2,plane,u_size);
    vec4 P2=previous(p+3.0*g2,plane,u_size), P3=original(p+3.0*g1,plane);
    vec4 B=previous(p-2.0*g1-g2,plane,u_size), C=original(p-g1-2.0*g2,plane);
    vec4 D=previous(p-2.0*g1+g2,plane,u_size), E=original(p-g1,plane);
    vec4 F=previous(p-g2,plane,u_size), G=original(p-g1+2.0*g2,plane);
    vec4 H=previous(p+g2,plane,u_size), I=original(p+g1,plane);
    vec4 F4=original(p+g1-2.0*g2,plane), I4=previous(p+2.0*g1-g2,plane,u_size);
    vec4 H5=original(p+g1+2.0*g2,plane), I5=previous(p+2.0*g1+g2,plane,u_size);
#else
    #if SUPER_PASS == 2
        p -= 1.0;
    #endif
    vec4 P0=S(-1,-1), P1=S(2,-1), P2=S(-1,2), P3=S(2,2);
    vec4 B=S(0,-1), C=S(1,-1), D=S(-1,0), E=S(0,0), F=S(1,0);
    vec4 G=S(-1,1), H=S(0,1), I=S(1,1), F4=S(2,0), I4=S(2,1), H5=S(0,2), I5=S(1,2);
#endif
    float b=luminance(B), c=luminance(C), d=luminance(D), e=luminance(E), f=luminance(F);
    float g=luminance(G), h=luminance(H), i=luminance(I), i4=luminance(I4), p0=luminance(P0);
    float i5=luminance(I5), p1=luminance(P1), h5=luminance(H5), p2=luminance(P2), f4=luminance(F4), p3=luminance(P3);
    float de=diagonal(d,b,g,e,c,p2,h,f,p1,h5,i,f4,i5,i4)-diagonal(c,f4,b,f,i4,p0,e,i,p3,d,h,i5,g,h5);
    float hv=axial(f,i,e,h,c,i5,b,h5)-axial(e,f,h,i,d,f4,g,i4);
    float strength=smoothstep(0.0,2.000001,abs(de));
    vec4 w1=vec4(-W1,W1+0.5,W1+0.5,-W1), w2=vec4(-W2,W2+0.25,W2+0.25,-W2);
    vec4 c1=mat4(P2,H,F,P1)*w1, c2=mat4(P0,E,I,P3)*w1;
    vec4 c3=mat4(D+G,E+H,F+I,F4+I4)*w2, c4=mat4(C+B,F+E,I+H,I5+H5)*w2;
    vec4 colour=mix(mix(c1,c2,step(0.0,de)),mix(c3,c4,step(0.0,hv)),1.0-strength);
    vec4 lower=min(min(E,F),min(H,I)), upper=max(max(E,F),max(H,I));
    colour=clamp(colour,lower,upper);
    colour.rgb=min(colour.rgb,vec3(colour.a));
    o_colour=colour;
}
