#version 460
layout(location = 0) out vec4 color;
struct Cell { uint glyph; uint fg; uint bg; uint flags; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; uint cursor; uint cursor_visible; } grid;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    uvec2 p = uvec2(gl_FragCoord.xy);
    uint col = min(uint(float(p.x) / grid.cell_width), grid.cols - 1u);
    uint row = min(uint(float(p.y) / grid.cell_height), grid.rows - 1u);
    Cell cell = cells[row * grid.cols + col];
    uint fg = cell.fg;
    uint bg = cell.bg;
    if ((cell.flags & 64u) != 0u) { uint swap = fg; fg = bg; bg = swap; }
    if (grid.cursor_visible != 0u && row * grid.cols + col == grid.cursor) bg = fg;
    color = unpack_color(bg);
}
