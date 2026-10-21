#version 460
#extension GL_GOOGLE_include_directive : require
#include "cell_graphics.glsl"
layout(set = 0, binding = 3) uniform sampler2D atlas;
layout(set = 0, binding = 4) uniform sampler2D color_atlas;
layout(location = 0) in vec2 uv;
layout(location = 1) flat in uint fg;
layout(location = 2) flat in uint visible;
layout(location = 3) flat in uint color_flag;
layout(location = 4) flat in vec4 cell_bounds;
layout(location = 5) flat in uint graphic;
layout(location = 6) flat in float dim_factor;
layout(location = 0) out vec4 color;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    // Glyph overhang must not repaint a neighboring TUI cell or line.
    vec2 p = gl_FragCoord.xy;
    if (visible == 0u || any(lessThan(p, cell_bounds.xy)) || any(greaterThanEqual(p, cell_bounds.zw))) discard;
    if (graphic != 0u) {
        float alpha = graphic_coverage(graphic, p - cell_bounds.xy, cell_bounds.zw - cell_bounds.xy);
        color = vec4(unpack_color(fg).rgb, alpha);
    } else if (color_flag != 0u) {
        vec4 texel = texture(color_atlas, uv);
        color = vec4(texel.rgb * dim_factor, texel.a);
    } else {
        float alpha = texture(atlas, uv).r;
        color = vec4(unpack_color(fg).rgb, alpha);
    }
}
