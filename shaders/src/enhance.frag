// FidelityFX Super Resolution 1.0 - RCAS-style weighted sharpening.
// Copyright (c) 2021 Advanced Micro Devices, Inc.
// MIT License. See shaders/src/ffx_fsr1.h and ffx_a.h for the full notice.
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
    uvec4 con0;
    uvec4 con1;
    uvec4 con2;
    uvec4 con3;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 out_color;

const float FSR_RCAS_LIMIT = 0.25 - (1.0 / 16.0);

vec3 to_perceptual(vec3 color) {
    return sqrt(max(color, vec3(0.0)));
}

vec3 to_linear(vec3 color) {
    return color * color;
}

float field_weight(vec2 uv) {
    float aspect = float(pc.dst_width) / float(pc.dst_height);
    vec2 delta = (uv - pc.center) * vec2(aspect, 1.0);
    float distance = length(delta);
    if (pc.feather <= 0.0) {
        return distance <= pc.radius ? 1.0 : 0.0;
    }
    return 1.0 - smoothstep(pc.radius, pc.radius + pc.feather, distance);
}

void main() {
    vec2 uv = mix(pc.src_bounds.xy, pc.src_bounds.zw, v_uv);
    vec2 texel = 1.0 / vec2(textureSize(source_tex, 0));
    vec4 center = texture(source_tex, uv);

    if ((pc.flags & 1u) != 0u && center.a < 0.999) {
        out_color = center;
        return;
    }

    vec3 b = to_perceptual(texture(source_tex, uv + vec2(0.0, -texel.y)).rgb);
    vec3 d = to_perceptual(texture(source_tex, uv + vec2(-texel.x, 0.0)).rgb);
    vec3 e = to_perceptual(center.rgb);
    vec3 f = to_perceptual(texture(source_tex, uv + vec2(texel.x, 0.0)).rgb);
    vec3 h = to_perceptual(texture(source_tex, uv + vec2(0.0, texel.y)).rgb);

    float bL = b.b * 0.5 + (b.r * 0.5 + b.g);
    float dL = d.b * 0.5 + (d.r * 0.5 + d.g);
    float eL = e.b * 0.5 + (e.r * 0.5 + e.g);
    float fL = f.b * 0.5 + (f.r * 0.5 + f.g);
    float hL = h.b * 0.5 + (h.r * 0.5 + h.g);
    float nz = 0.25 * bL + 0.25 * dL + 0.25 * fL + 0.25 * hL - eL;
    nz = clamp(abs(nz) / max(max(max(bL, dL), max(eL, fL)), hL)
        - min(min(min(bL, dL), min(eL, fL)), hL), 0.0, 1.0);
    nz = -0.5 * nz + 1.0;

    vec3 mn4 = min(min(b, d), min(f, h));
    vec3 mx4 = max(max(b, d), max(f, h));
    vec3 hit_min = min(mn4, e) / (4.0 * mx4);
    vec3 hit_max = (1.0 - max(mx4, e)) / (4.0 * mn4 - 4.0);
    vec3 lobes = max(-hit_min, hit_max);
    float lobe = max(-FSR_RCAS_LIMIT, min(max(max(lobes.r, lobes.g), lobes.b), 0.0));

    float weight = field_weight(v_uv);
    lobe *= pc.strength * weight * nz;

    vec3 result = (lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0);
    result = to_linear(result);

    if ((pc.flags & 2u) != 0u) {
        float aspect = float(pc.dst_width) / float(pc.dst_height);
        float distance = length((v_uv - pc.center) * vec2(aspect, 1.0));
        float ring = 1.0 - smoothstep(0.0, 0.0025, abs(distance - pc.radius));
        result = mix(result, vec3(0.0, 1.0, 0.0), ring * 0.6);
    }

    out_color = vec4(result, center.a);
}
