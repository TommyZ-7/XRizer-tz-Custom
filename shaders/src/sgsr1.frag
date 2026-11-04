// Snapdragon Game Super Resolution 1 (SGSR 1).
// Copyright (c) 2025 Qualcomm Innovation Center, Inc.
// SPDX-License-Identifier: BSD-3-Clause
// Full notice: shaders/src/SGSR-LICENSE.txt
//
// Adapted to Vulkan GLSL and XRizer's continuous radial weight field. The
// 12-tap Lanczos-like base reconstruction is always applied; only the extra
// adaptive sharpening is scaled by the weight field.
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
    uvec2 ml_origin;
    uvec2 ml_size;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 out_color;

const float EDGE_THRESHOLD = 8.0 / 255.0;
const float EDGE_SHARPNESS = 2.0;

float fast_lanczos2(float x) {
    float wA = x - 4.0;
    float wB = x * wA - wA;
    wA *= wA;
    return wB * wA;
}

vec2 weight_y(float dx, float dy, float c, float stddev) {
    float x = ((dx * dx) + (dy * dy)) * 0.55 + clamp(abs(c) * stddev, 0.0, 1.0);
    float w = fast_lanczos2(x);
    return vec2(w, w * c);
}

vec4 encode_linear(vec3 linear, float alpha) {
    if ((pc.flags & 16u) != 0u) {
        return vec4(sqrt(max(linear, vec3(0.0))), alpha);
    }
    return vec4(linear, alpha);
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

vec4 gather_green(vec2 uv) {
    return sqrt(max(textureGather(source_tex, uv, 1), vec4(0.0)));
}

void main() {
    vec2 source_uv = mix(pc.src_bounds.xy, pc.src_bounds.zw, v_uv);
    vec4 raw_center = texture(source_tex, source_uv);
    vec4 color = vec4(sqrt(max(raw_center.rgb, vec3(0.0))), raw_center.a);

    if ((pc.flags & 1u) != 0u && raw_center.a < 0.999) {
        out_color = encode_linear(raw_center.rgb, raw_center.a);
        return;
    }

    vec2 size = vec2(textureSize(source_tex, 0));
    vec2 texel = 1.0 / size;
    vec2 img_coord = source_uv * size + vec2(-0.5, 0.5);
    vec2 img_coord_pixel = floor(img_coord);
    vec2 coord = img_coord_pixel * texel;
    vec2 pl = img_coord - img_coord_pixel;

    vec4 left = gather_green(coord);
    float edge_vote = abs(left.z - left.y) + abs(color.g - left.y) + abs(color.g - left.z);

    if (edge_vote > EDGE_THRESHOLD) {
        coord.x += texel.x;
        vec4 right = gather_green(coord + vec2(texel.x, 0.0));
        vec4 up_down;
        up_down.xy = gather_green(coord + vec2(0.0, -texel.y)).wz;
        up_down.zw = gather_green(coord + vec2(0.0, texel.y)).yx;

        float mean = (left.y + left.z + right.x + right.w) * 0.25;
        left -= vec4(mean);
        right -= vec4(mean);
        up_down -= vec4(mean);
        float center_y = color.g - mean;

        float sum = abs(left.x) + abs(left.y) + abs(left.z) + abs(left.w)
            + abs(right.x) + abs(right.y) + abs(right.z) + abs(right.w)
            + abs(up_down.x) + abs(up_down.y) + abs(up_down.z) + abs(up_down.w);
        float stddev = 2.181818 / sum;

        vec2 aWY = weight_y(pl.x, pl.y + 1.0, up_down.x, stddev);
        aWY += weight_y(pl.x - 1.0, pl.y + 1.0, up_down.y, stddev);
        aWY += weight_y(pl.x - 1.0, pl.y - 2.0, up_down.z, stddev);
        aWY += weight_y(pl.x, pl.y - 2.0, up_down.w, stddev);
        aWY += weight_y(pl.x + 1.0, pl.y - 1.0, left.x, stddev);
        aWY += weight_y(pl.x, pl.y - 1.0, left.y, stddev);
        aWY += weight_y(pl.x, pl.y, left.z, stddev);
        aWY += weight_y(pl.x + 1.0, pl.y, left.w, stddev);
        aWY += weight_y(pl.x - 1.0, pl.y - 1.0, right.x, stddev);
        aWY += weight_y(pl.x - 2.0, pl.y - 1.0, right.y, stddev);
        aWY += weight_y(pl.x - 2.0, pl.y, right.z, stddev);
        aWY += weight_y(pl.x - 1.0, pl.y, right.w, stddev);

        float weight = field_weight(v_uv);
        float enhancement = ((pc.flags & 4u) != 0u) ? (pc.strength * weight) : 0.0;
        float sharpness = 1.0 + (EDGE_SHARPNESS - 1.0) * enhancement;

        float final_y = aWY.y / aWY.x;
        float max_y = max(max(left.y, left.z), max(right.x, right.w));
        float min_y = min(min(left.y, left.z), min(right.x, right.w));
        final_y = clamp(sharpness * final_y, min_y, max_y);

        float delta_y = final_y - center_y;
        delta_y = clamp(delta_y, -23.0 / 255.0, 23.0 / 255.0);
        color.rgb = clamp(color.rgb + delta_y, 0.0, 1.0);
    }

    if ((pc.flags & 2u) != 0u) {
        float aspect = float(pc.dst_width) / float(pc.dst_height);
        float distance = length((v_uv - pc.center) * vec2(aspect, 1.0));
        float ring = 1.0 - smoothstep(0.0, 0.0025, abs(distance - pc.radius));
        color.rgb = mix(color.rgb, vec3(0.0, 1.0, 0.0), ring * 0.6);
    }

    if ((pc.flags & 16u) == 0u) {
        color.rgb = color.rgb * color.rgb;
    }
    out_color = vec4(color.rgb, raw_center.a);
}
