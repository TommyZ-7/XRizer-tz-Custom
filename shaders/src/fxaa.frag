#version 450

layout(set = 0, binding = 0) uniform sampler2D source_tex;

layout(push_constant) uniform PushConstants {
    vec4 src_bounds;
    vec2 center;
    float radius;
    float feather;
    float strength;
    uint flags;
    uint dst_width;
    uint dst_height;
    uint weight_mode;
    uint antialias;
    uint fxaa_quality;
    float smaa_threshold;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 out_color;

const uint FLAG_PRESERVE_ALPHA = 1u;
const uint FLAG_SHOW_REGION = 2u;
const uint FLAG_ENHANCEMENT_ENABLED = 4u;
const uint FLAG_OUTPUT_UNORM = 8u;
const vec3 LUMA = vec3(0.299, 0.587, 0.114);

vec4 encode_output(vec4 color) {
    if ((pc.flags & FLAG_OUTPUT_UNORM) != 0u) {
        return vec4(sqrt(max(color.rgb, vec3(0.0))), color.a);
    }
    return color;
}

float luma(vec3 color) {
    return dot(color, LUMA);
}

vec2 source_uv(vec2 uv) {
    return mix(pc.src_bounds.xy, pc.src_bounds.zw, uv);
}

// Keep every tap inside this eye's submitted subrect. Without this, a
// double-wide texture would sample the other eye at the subrect edge.
vec4 sample_source(vec2 uv) {
    vec2 texel = 1.0 / vec2(textureSize(source_tex, 0));
    vec2 lo = min(pc.src_bounds.xy, pc.src_bounds.zw) + texel * 0.5;
    vec2 hi = max(pc.src_bounds.xy, pc.src_bounds.zw) - texel * 0.5;
    // Guard a sub-texel subrect where lo can exceed hi.
    return texture(source_tex, clamp(uv, min(lo, hi), max(lo, hi)));
}

// Mirrors src/upscaler/math.rs:weight_at_mode.
float field_weight(vec2 uv) {
    if (pc.weight_mode == 2u) {
        return 1.0;
    }

    float aspect = float(pc.dst_width) / float(pc.dst_height);
    vec2 delta = (uv - pc.center) * vec2(aspect, 1.0);
    float d = length(delta);

    if (pc.weight_mode == 0u) {
        if (pc.feather <= 0.0) {
            return d <= pc.radius ? 1.0 : 0.0;
        }
        return 1.0 - smoothstep(pc.radius, pc.radius + pc.feather, d);
    }

    if (pc.feather <= 0.0) {
        return d <= pc.radius ? 0.0 : 1.0;
    }
    return smoothstep(pc.radius, pc.radius + pc.feather, d);
}

// FXAA-style edge-directed filter. The search length is selected by
// fxaa_quality (0 = performance, 1 = balanced, 2 = quality).
vec3 fxaa(vec2 uv) {
    vec2 texel = 1.0 / vec2(textureSize(source_tex, 0));
    vec3 rgb_m = sample_source(uv).rgb;
    float luma_m = luma(rgb_m);

    float luma_n = luma(sample_source(uv + vec2(0.0, -texel.y)).rgb);
    float luma_s = luma(sample_source(uv + vec2(0.0, texel.y)).rgb);
    float luma_e = luma(sample_source(uv + vec2(texel.x, 0.0)).rgb);
    float luma_w = luma(sample_source(uv + vec2(-texel.x, 0.0)).rgb);

    float luma_min = min(luma_m, min(min(luma_n, luma_s), min(luma_e, luma_w)));
    float luma_max = max(luma_m, max(max(luma_n, luma_s), max(luma_e, luma_w)));

    // Low-contrast areas are left untouched; this is also the guard against
    // turning flat UI panels into a soft blur.
    if ((luma_max - luma_min) < max(0.0312, luma_max * 0.125)) {
        return rgb_m;
    }

    float luma_nw = luma(sample_source(uv + vec2(-texel.x, -texel.y)).rgb);
    float luma_ne = luma(sample_source(uv + vec2(texel.x, -texel.y)).rgb);
    float luma_sw = luma(sample_source(uv + vec2(-texel.x, texel.y)).rgb);
    float luma_se = luma(sample_source(uv + vec2(texel.x, texel.y)).rgb);

    float edge_h =
        abs(luma_nw + luma_ne - 2.0 * luma_n) +
        2.0 * abs(luma_w + luma_e - 2.0 * luma_m) +
        abs(luma_sw + luma_se - 2.0 * luma_s);
    float edge_v =
        abs(luma_nw + luma_sw - 2.0 * luma_w) +
        2.0 * abs(luma_n + luma_s - 2.0 * luma_m) +
        abs(luma_ne + luma_se - 2.0 * luma_e);

    bool horizontal = edge_h >= edge_v;
    vec2 dir = horizontal ? vec2(texel.x, 0.0) : vec2(0.0, texel.y);
    float gradient = horizontal ? (luma_e - luma_w) : (luma_s - luma_n);
    if (gradient < 0.0) {
        dir = -dir;
    }

    int steps = pc.fxaa_quality == 0u ? 2 : (pc.fxaa_quality == 1u ? 4 : 8);
    vec3 positive = vec3(0.0);
    vec3 negative = vec3(0.0);
    for (int i = 1; i <= steps; i++) {
        float t = float(i) / float(steps);
        positive += sample_source(uv + dir * t * 4.0).rgb;
        negative += sample_source(uv - dir * t * 4.0).rgb;
    }
    float count = float(steps);
    vec3 filtered = 0.5 * (positive + negative) / count;

    // Fall back to the centre when the search lands outside the local luma
    // range; this avoids bleeding across high-contrast silhouettes.
    float filtered_luma = luma(filtered);
    if (filtered_luma < luma_min || filtered_luma > luma_max) {
        return rgb_m;
    }
    return filtered;
}

void main() {
    vec2 uv = source_uv(v_uv);
    vec4 original = sample_source(uv);

    if ((pc.flags & FLAG_ENHANCEMENT_ENABLED) == 0u) {
        out_color = encode_output(original);
        return;
    }
    if ((pc.flags & FLAG_PRESERVE_ALPHA) != 0u && original.a < 0.999) {
        out_color = encode_output(original);
        return;
    }

    vec3 filtered = fxaa(uv);
    float weight = field_weight(v_uv);
    vec3 rgb = mix(original.rgb, filtered, clamp(pc.strength * weight, 0.0, 1.0));

    if ((pc.flags & FLAG_SHOW_REGION) != 0u && pc.weight_mode != 2u) {
        float aspect = float(pc.dst_width) / float(pc.dst_height);
        vec2 delta = (v_uv - pc.center) * vec2(aspect, 1.0);
        float d = length(delta);
        float ring = 1.0 - smoothstep(0.0, 0.0025, abs(d - pc.radius));
        rgb = mix(rgb, vec3(1.0, 0.0, 1.0), ring * 0.75);
    }

    out_color = encode_output(vec4(rgb, original.a));
}
