#version 460
layout(set = 0, binding = 3) uniform sampler2D atlas;
layout(location = 0) in vec2 uv;
layout(location = 1) flat in uint fg;
layout(location = 2) flat in uint visible;
layout(location = 0) out vec4 color;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    if (visible == 0u) discard;
    float alpha = texture(atlas, uv).r;
    color = vec4(unpack_color(fg).rgb, alpha);
}
