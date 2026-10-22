#version 460
struct Cell { uint glyph; uint fg; uint bg; uint flags; uint underline_color; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; float ascent; int offset_x; int offset_y; uint cursor; uint cursor_visible; uint selection_start; uint selection_end; uint cursor_style; uint selection_color; uint padding_x; uint padding_y; uint cursor_color; uint background_color; uint background_dense; } grid;
layout(location = 0) flat out uvec4 cell_colors_flags;
layout(location = 1) flat out vec2 cell_origin;
layout(location = 2) flat out uint is_cursor_cell;
void main() {
    if (grid.background_dense != 0u) {
        const vec2 corners[4] = vec2[](vec2(-1,-1), vec2(1,-1), vec2(-1,1), vec2(1,1));
        gl_Position = vec4(corners[gl_VertexIndex], 0.0, 1.0);
        return;
    }
    uint index = gl_InstanceIndex;
    Cell cell = cells[index];
    uint fg = cell.fg;
    uint bg = cell.bg;
    if ((cell.flags & 64u) != 0u) { uint swap = fg; fg = bg; bg = swap; }
    if ((cell.flags & 128u) != 0u) fg = (fg & 0x00fefefeu) >> 1u;
    if (grid.selection_start != 0xffffffffu && index >= grid.selection_start && index <= grid.selection_end) bg = grid.selection_color;
    is_cursor_cell = uint(grid.cursor_visible != 0u && index == grid.cursor);
    cell_colors_flags = uvec4(fg, bg, cell.flags, cell.underline_color);
    vec2 cell_position = vec2(index % grid.cols, index / grid.cols);
    cell_origin = vec2(grid.padding_x, grid.padding_y) + cell_position * vec2(grid.cell_width, grid.cell_height);
    // The attachment clear already covers plain default cells. Degenerate
    // quads avoid their fragment work even when another cell needs decoration.
    bool decorated = (cell_colors_flags.z & 256u) == 0u && (cell_colors_flags.z & 48u) != 0u;
    bool visible = bg != grid.background_color || decorated || is_cursor_cell != 0u;
    vec2 corners[4] = vec2[](vec2(0,0), vec2(1,0), vec2(0,1), vec2(1,1));
    vec2 position = cell_position + (visible ? corners[gl_VertexIndex] : vec2(0));
    gl_Position = vec4(position.x * 2.0 / float(grid.cols) - 1.0,
                       position.y * 2.0 / float(grid.rows) - 1.0, 0.0, 1.0);
}
