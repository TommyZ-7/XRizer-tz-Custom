// FidelityFX Super Resolution 1.0 - EASU pass.
// Copyright (c) 2021 Advanced Micro Devices, Inc.
// MIT License. See shaders/src/ffx_fsr1.h and ffx_a.h for the full notice.
#version 450
#extension GL_GOOGLE_include_directive : require

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

#define A_GPU 1
#define A_GLSL 1
#include "ffx_a.h"

// EASU expects perceptual/gamma-space input. Sampling an sRGB view returns
// linear values, so approximate the display curve with gamma 2.0 (as FSR 1's
// own TEPD helper does). The intermediate is sRGB, so convert back to linear
// before writing.
vec2 map_uv(vec2 p) {
    if (pc.src_bounds.z < pc.src_bounds.x) {
        p.x = pc.src_bounds.x + pc.src_bounds.z - p.x;
    }
    if (pc.src_bounds.w < pc.src_bounds.y) {
        p.y = pc.src_bounds.y + pc.src_bounds.w - p.y;
    }
    return p;
}

AF4 FsrEasuRF(AF2 p) {
    return sqrt(max(textureGather(source_tex, map_uv(p), 0), vec4(0.0)));
}
AF4 FsrEasuGF(AF2 p) {
    return sqrt(max(textureGather(source_tex, map_uv(p), 1), vec4(0.0)));
}
AF4 FsrEasuBF(AF2 p) {
    return sqrt(max(textureGather(source_tex, map_uv(p), 2), vec4(0.0)));
}

#define FSR_EASU_F 1
#include "ffx_fsr1.h"

void main() {
    AF3 pix;
    FsrEasuF(
        pix,
        AU2(gl_FragCoord.xy),
        uvec4(pc.con0),
        uvec4(pc.con1),
        uvec4(pc.con2),
        uvec4(pc.con3)
    );

    vec2 source_uv = mix(pc.src_bounds.xy, pc.src_bounds.zw, v_uv);
    vec4 center = texture(source_tex, source_uv);
    if ((pc.flags & 1u) != 0u && center.a < 0.999) {
        out_color = center;
        return;
    }

    out_color = vec4(pix * pix, center.a);
}
