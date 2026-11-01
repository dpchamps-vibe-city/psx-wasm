// Host-visual Catmull–Rom interpolation. Unlike xBR/MMPX this applies the
// same separable cubic kernel everywhere, without classifying pixel patterns.
// Interpolate premultiplied RGB/coverage, then bound the result to the central
// 2x2 neighbourhood to suppress ringing and transparent black fringes.

vec4 cubic_weights(float f) {
    float f2 = f*f, f3 = f2*f;
    return vec4(-0.5*f + f2 - 0.5*f3,
                1.0 - 2.5*f2 + 1.5*f3,
                0.5*f + 2.0*f2 - 1.5*f3,
                -0.5*f2 + 0.5*f3);
}

uint cubic_texel(ivec2 p, vec2 lo, vec2 hi, uint source, uint flags, uvec4 h0, uvec4 tw) {
    uint t = sprite_texel(p, lo, hi, flags, h0, tw);
    // STP classes must not blend colours across opaque/semitransparent
    // boundaries. Substitute the reference colour, not transparent black.
    if ((flags & SEMI) != 0u && t != 0u && ((t ^ source) & 0x8000u) != 0u) return source;
    return t;
}

void bicubic_surface(vec2 at, vec2 lo, vec2 hi, float footprint, uvec3 color, uint flags, uvec4 h0, uvec4 tw, int d,
                     inout uint dst, inout bool wrote) {
    ivec2 nearest = ivec2(floor(at + 0.5));
    if (footprint >= 1.0) {
        surface(uint(nearest.x) & 255u, uint(nearest.y) & 255u, color.r, color.g, color.b, flags, h0, tw, d, dst, wrote);
        return;
    }
    ivec2 base = ivec2(floor(at));
    uint source = sprite_texel(nearest, lo, hi, flags, h0, tw);
    if (source == 0u) {
        // A reconstructed silhouette can cover a transparent nearest texel.
        // Use the closest nonzero central tap to choose its STP/mask class.
        float distance = 1e20;
        for (int y = 0; y < 2; y++) {
            for (int x = 0; x < 2; x++) {
                ivec2 p = base + ivec2(x,y);
                uint t = sprite_texel(p, lo, hi, flags, h0, tw);
                vec2 delta = vec2(p) - at;
                float next_distance = dot(delta, delta);
                if (t != 0u && next_distance < distance) { source = t; distance = next_distance; }
            }
        }
        if (source == 0u) return;
    }
    vec3 lower = vec3(255.0), upper = vec3(0.0);
    float alpha_lower = 1.0, alpha_upper = 0.0;
    for (int y = 0; y < 2; y++) {
        for (int x = 0; x < 2; x++) {
            uint t = cubic_texel(base + ivec2(x,y), lo, hi, source, flags, h0, tw);
            float alpha = t == 0u ? 0.0 : 1.0;
            alpha_lower = min(alpha_lower, alpha);
            alpha_upper = max(alpha_upper, alpha);
            if (t != 0u) {
                vec3 rgb = rgb5(t)*8.0;
                lower = min(lower, rgb);
                upper = max(upper, rgb);
            }
        }
    }
    vec4 wx = cubic_weights(fract(at.x)), wy = cubic_weights(fract(at.y));
    vec4 sum = vec4(0.0);
    for (int y = 0; y < 4; y++) {
        for (int x = 0; x < 4; x++) {
            uint t = cubic_texel(base + ivec2(x-1,y-1), lo, hi, source, flags, h0, tw);
            vec4 rgba = t == 0u ? vec4(0.0) : vec4(rgb5(t)*8.0, 1.0);
            sum += rgba * wx[x] * wy[y];
        }
    }
    float coverage = clamp(sum.a, alpha_lower, alpha_upper);
    if (coverage <= 0.0) return;
    vec3 rgb = clamp(sum.rgb / max(sum.a, 0.000001), lower, upper);
    uint t = colour_pixel(rgb, native_pixel(source) & PIXEL_MASK);
    if ((flags & RAW) == 0u) t = modulate(t, color.r, color.g, color.b, (flags & DITHER) != 0u ? d : 0);
    uint background = dst;
    bool covered = false;
    plot(t, true, flags, dst, covered);
    if (!covered) return;
    dst = colour_pixel(mix(visual_rgb(background), visual_rgb(dst), coverage), dst & PIXEL_MASK);
    wrote = true;
}
