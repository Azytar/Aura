#version 460
layout(location = 0) out vec4 color;
struct Cell { uint glyph; uint fg; uint bg; uint flags; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
// Push-constant members must stay in the exact order and type of the Rust
// `PushGrid` and of the vertex stages; every stage declares the whole block so
// field offsets cannot drift between stages. `offset_x`/`offset_y` are unused
// here but are part of the shared layout.
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; float ascent; int offset_x; int offset_y; uint cursor; uint cursor_visible; uint selection_start; uint selection_end; uint cursor_style; uint selection_color; uint padding_x; uint padding_y; } grid;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    uvec2 p = uvec2(gl_FragCoord.xy - vec2(grid.padding_x, grid.padding_y));
    uint col = min(uint(float(p.x) / grid.cell_width), grid.cols - 1u);
    uint row = min(uint(float(p.y) / grid.cell_height), grid.rows - 1u);
    Cell cell = cells[row * grid.cols + col];
    uint fg = cell.fg;
    uint bg = cell.bg;
    if ((cell.flags & 64u) != 0u) { uint swap = fg; fg = bg; bg = swap; }
    uint index = row * grid.cols + col;
    if (grid.selection_start != 0xffffffffu && index >= grid.selection_start && index <= grid.selection_end) {
        bg = grid.selection_color;
    }
    uint ch = uint(grid.cell_height);
    // Mirrors underline_rows/strikethrough_rows/cursor_span in the renderer:
    // one decoration row per 16 cell rows, at least one; thin cursors are double.
    uint thick = max(ch >> 4u, 1u);
    bool is_cursor = grid.cursor_visible != 0u && index == grid.cursor;
    if (is_cursor && grid.cursor_style == 0u) bg = fg;
    color = unpack_color(bg);
    float local_y = float(p.y) - float(row) * grid.cell_height;
    float local_x = float(p.x) - float(col) * grid.cell_width;
    bool paint = false;
    if ((cell.flags & 16u) != 0u && local_y >= float(ch - thick)) paint = true;
    if ((cell.flags & 32u) != 0u && local_y >= float(ch / 2u - thick / 2u) && local_y < float(ch / 2u - thick / 2u + thick)) paint = true;
    if (is_cursor && grid.cursor_style == 1u && local_y >= float(ch - 2u * thick)) paint = true;
    if (is_cursor && grid.cursor_style == 2u && local_x < float(2u * thick)) paint = true;
    if (paint) color = vec4(unpack_color(fg).rgb, 1.0);
}
