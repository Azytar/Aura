#version 460
layout(set = 0, binding = 3) uniform sampler2D atlas;
layout(set = 0, binding = 4) uniform sampler2D color_atlas;
layout(location = 0) in vec2 uv;
layout(location = 1) flat in uint fg;
layout(location = 2) flat in uint visible;
layout(location = 3) flat in uint color_flag;
layout(location = 0) out vec4 color;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    if (visible == 0u) discard;
    if (color_flag != 0u) {
        vec4 texel = texture(color_atlas, uv);
        color = vec4(texel.rgb, texel.a);
    } else {
        float alpha = texture(atlas, uv).r;
        color = vec4(unpack_color(fg).rgb, alpha);
    }
}
