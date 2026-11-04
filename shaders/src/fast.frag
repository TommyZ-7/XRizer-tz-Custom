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
    vec2 source_uv = mix(pc.src_bounds.xy, pc.src_bounds.zw, v_uv);
    vec4 center = texture(source_tex, source_uv);

    if ((pc.flags & 1u) != 0u && center.a < 0.999) {
        out_color = center;
        return;
    }
    if ((pc.flags & 4u) == 0u) {
        out_color = center;
        return;
    }

    vec2 bounds_size = abs(pc.src_bounds.zw - pc.src_bounds.xy);
    vec2 texel = bounds_size / vec2(textureSize(source_tex, 0));
    vec3 north = texture(source_tex, source_uv + vec2(0.0, texel.y)).rgb;
    vec3 south = texture(source_tex, source_uv - vec2(0.0, texel.y)).rgb;
    vec3 east = texture(source_tex, source_uv + vec2(texel.x, 0.0)).rgb;
    vec3 west = texture(source_tex, source_uv - vec2(texel.x, 0.0)).rgb;

    vec3 blur = (north + south + east + west) * 0.25;
    vec3 detail = center.rgb - blur;
    float weight = field_weight(v_uv);

    // RCAS-like limiting: never push a channel outside the local 5-tap range.
    vec3 low = min(min(north, south), min(east, west));
    vec3 high = max(max(north, south), max(east, west));
    vec3 color = center.rgb + detail * (pc.strength * weight * 0.6);
    color = clamp(color, min(center.rgb, low), max(center.rgb, high));

    if ((pc.flags & 2u) != 0u) {
        float aspect = float(pc.dst_width) / float(pc.dst_height);
        float distance = length((v_uv - pc.center) * vec2(aspect, 1.0));
        float ring = 1.0 - smoothstep(0.0, 0.0025, abs(distance - pc.radius));
        color = mix(color, vec3(0.0, 1.0, 0.0), ring * 0.6);
    }

    out_color = vec4(color, center.a);
}
