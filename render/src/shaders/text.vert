#version 460
struct Cell { uint glyph; uint fg; uint bg; uint flags; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
struct Glyph { vec4 uv; vec4 size_bearing; vec4 bearing_advance; };
layout(set = 0, binding = 2, std430) readonly buffer Glyphs { Glyph glyphs[]; };
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; float ascent; uint cursor; uint cursor_visible; uint selection_start; uint selection_end; uint cursor_style; } grid;
layout(location = 0) out vec2 uv;
layout(location = 1) flat out uint fg;
layout(location = 2) flat out uint visible;
layout(location = 3) flat out uint color_flag;
void main() {
    uint index = gl_InstanceIndex;
    uint col = index % grid.cols;
    uint row = index / grid.cols;
    Cell cell = cells[index];
    visible = cell.glyph == 0xffffffffu ? 0u : 1u;
    Glyph glyph = glyphs[cell.glyph == 0xffffffffu ? 0u : cell.glyph];
    vec2 corners[4] = vec2[](vec2(0,0), vec2(1,0), vec2(0,1), vec2(1,1));
    vec2 corner = corners[gl_VertexIndex];
    vec2 origin = vec2(float(col) * grid.cell_width + glyph.size_bearing.z,
                       float(row) * grid.cell_height + grid.ascent - glyph.size_bearing.w);
    vec2 pixel = origin + corner * glyph.size_bearing.xy;
    gl_Position = vec4(pixel.x * 2.0 / (float(grid.cols) * grid.cell_width) - 1.0,
                       pixel.y * 2.0 / (float(grid.rows) * grid.cell_height) - 1.0, 0.0, 1.0);
    uv = mix(glyph.uv.xy, glyph.uv.zw, corner);
    fg = (cell.flags & 64u) != 0u ? cell.bg : cell.fg;
    color_flag = uint(glyph.bearing_advance.w + 0.5);
    if (grid.cursor_visible != 0u && index == grid.cursor) fg = (cell.flags & 64u) != 0u ? cell.fg : cell.bg;
}
