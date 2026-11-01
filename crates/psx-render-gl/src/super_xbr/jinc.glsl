/*
 * Hyllian's jinc windowed-jinc 2-lobe sharper with anti-ringing Shader
 * Copyright (C) 2011-2014 Hyllian/Jararaca - sergiogdb@gmail.com
 * Adapted from libretro/common-shaders/xbr/shaders/super-xbr/custom-jinc2-sharper.cg
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 2 of the License, or (at your option)
 * any later version.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 * more details. A copy is included in COPYING.GPL-2.
 *
 * PSXRevamp: final resampling is fused with primitive shading/compositing,
 * following the three offscreen Super-xBR passes. Premultiplied RGBA and
 * separate STP planes extend the original opaque RGB resampler to sprites.
 */
uniform highp sampler2D u_super;

vec4 super_fetch(ivec2 p, int plane, ivec2 size) {
    return texelFetch(u_super, clamp(p,ivec2(0),size-1)+ivec2(plane*size.x,0),0);
}
vec4 super_jinc(vec2 pc, int plane, ivec2 size) {
    // Defaults of the linked 3p-smoother preset: window .42, sinc .92, AR 1.
    const float wa=0.42*3.141592653589793, wb=0.92*3.141592653589793;
    ivec2 nearest=ivec2(floor(pc));
    vec4 centre=super_fetch(nearest,plane,size);
    vec4 a=super_fetch(nearest+ivec2(1,0),plane,size), b=super_fetch(nearest-ivec2(1,0),plane,size);
    vec4 c=super_fetch(nearest+ivec2(0,1),plane,size), d=super_fetch(nearest-ivec2(0,1),plane,size);
    vec4 lower=min(centre,min(min(a,b),min(c,d))), upper=max(centre,max(max(a,b),max(c,d)));
    // AR=1 clamps the result to this range. A constant neighbourhood (also
    // an empty STP plane) therefore needs no 16-tap fetches or trig weights.
    if (all(equal(lower,upper))) return lower;
    ivec2 base=ivec2(floor(pc-0.5));
    vec4 sum=vec4(0);
    float total=0.0;
    for (int y=-1;y<=2;y++) {
        for (int x=-1;x<=2;x++) {
            ivec2 p=base+ivec2(x,y);
            float radius=length(pc-(vec2(p)+0.5));
            float w=radius==0.0 ? wa*wb : sin(radius*wa)*sin(radius*wb)/(radius*radius);
            sum+=super_fetch(p,plane,size)*w;
            total+=w;
        }
    }
    return clamp(sum/total,lower,upper);
}
void super_surface(vec2 at, vec2 lo, vec2 hi, float footprint, uvec3 color, uint flags, uvec4 h0, uvec4 tw, int d,
                   inout uint dst, inout bool wrote) {
    ivec2 nearest=ivec2(floor(at+0.5));
    if (footprint>=1.0) {
        surface(uint(nearest.x)&255u,uint(nearest.y)&255u,color.r,color.g,color.b,flags,h0,tw,d,dst,wrote);
        return;
    }
    uint source=sprite_texel(nearest,lo,hi,flags,h0,tw);
    if (source==0u) {
        float distance=1e20;
        ivec2 base=ivec2(floor(at));
        for (int y=-1;y<=2;y++) {
            for (int x=-1;x<=2;x++) {
                ivec2 p=base+ivec2(x,y);
                uint t=sprite_texel(p,lo,hi,flags,h0,tw);
                vec2 delta=vec2(p)-at;
                float next_distance=dot(delta,delta);
                if (t!=0u && next_distance<distance) { source=t; distance=next_distance; }
            }
        }
        if (source==0u) return;
    }
    ivec2 size=ivec2(hi-lo+1.0)*2;
    vec2 pc=(at-lo+0.5)*2.0;
    bool semi=(flags&SEMI)!=0u;
    vec4 opaque=super_jinc(pc,0,size);
    vec4 stp=semi ? super_jinc(pc,1,size) : vec4(0);
    vec4 selected=semi && (source&0x8000u)!=0u ? stp : opaque;
    float coverage=clamp(opaque.a+stp.a,0.0,1.0);
    if (coverage<=0.0) return;
    vec3 rgb=selected.a>0.000001 ? selected.rgb/selected.a*255.0 : rgb5(source)*8.0;
    if (coverage<1.0) {
        // RGBA8 intermediates round colour/coverage independently. Bound
        // unpremultiplication near cutouts to compatible native colours.
        vec3 lower=rgb5(source)*8.0, upper=lower;
        ivec2 base=ivec2(floor(at));
        for (int y=-1;y<=2;y++) {
            for (int x=-1;x<=2;x++) {
                uint t=sprite_texel(base+ivec2(x,y),lo,hi,flags,h0,tw);
                if (t==0u || (semi && ((t^source)&0x8000u)!=0u)) continue;
                lower=min(lower,rgb5(t)*8.0); upper=max(upper,rgb5(t)*8.0);
            }
        }
        rgb=clamp(rgb,lower,upper);
    }
    uint t=colour_pixel(rgb,native_pixel(source)&PIXEL_MASK);
    if ((flags&RAW)==0u) t=modulate(t,color.r,color.g,color.b,(flags&DITHER)!=0u ? d : 0);
    uint background=dst;
    bool covered=false;
    plot(t,true,flags,dst,covered);
    if (!covered) return;
    dst=colour_pixel(mix(visual_rgb(background),visual_rgb(dst),coverage),dst&PIXEL_MASK);
    wrote=true;
}
