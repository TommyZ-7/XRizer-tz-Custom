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
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 out_color;

void main() {
    vec2 uv = mix(pc.src_bounds.xy, pc.src_bounds.zw, v_uv);
    out_color = texture(source_tex, uv);
}
