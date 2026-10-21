#version 460
layout(location = 0) out vec4 color;
struct Cell { uint glyph; uint fg; uint bg; uint flags; uint underline_color; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
// Push-constant members must stay in the exact order and type of the Rust
// `PushGrid` and of the vertex stages; every stage declares the whole block so
// field offsets cannot drift between stages. `offset_x`/`offset_y` are unused
// here but are part of the shared layout.
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; float ascent; int offset_x; int offset_y; uint cursor; uint cursor_visible; uint selection_start; uint selection_end; uint cursor_style; uint selection_color; uint padding_x; uint padding_y; uint cursor_color; } grid;
vec4 unpack_color(uint c) { return vec4(float(c & 255u), float((c >> 8) & 255u), float((c >> 16) & 255u), 255.0) / 255.0; }
void main() {
    uvec2 p = uvec2(gl_FragCoord.xy - vec2(grid.padding_x, grid.padding_y));
    uint col = min(uint(float(p.x) / grid.cell_width), grid.cols - 1u);
    uint row = min(uint(float(p.y) / grid.cell_height), grid.rows - 1u);
    Cell cell = cells[row * grid.cols + col];
    uint fg = cell.fg;
    uint bg = cell.bg;
    if ((cell.flags & 64u) != 0u) { uint swap = fg; fg = bg; bg = swap; }
    if ((cell.flags & 128u) != 0u) fg = (fg & 0x00fefefeu) >> 1u;
    uint index = row * grid.cols + col;
    if (grid.selection_start != 0xffffffffu && index >= grid.selection_start && index <= grid.selection_end) {
        bg = grid.selection_color;
    }
    uint ch = uint(grid.cell_height);
    // Mirrors underline_rows/strikethrough_rows/cursor_span in the renderer:
    // one decoration row per 16 cell rows, at least one; thin cursors are double.
    uint thick = max(ch >> 4u, 1u);
    bool is_cursor = grid.cursor_visible != 0u && index == grid.cursor;
    uint cursor_color = grid.cursor_color == 0xffffffffu ? fg : grid.cursor_color;
    if (is_cursor && grid.cursor_style == 0u) bg = cursor_color;
    color = unpack_color(bg);
    float local_y = float(p.y) - float(row) * grid.cell_height;
    float local_x = float(p.x) - float(col) * grid.cell_width;
    bool paint = false;
    if ((cell.flags & 256u) == 0u) {
        if ((cell.flags & 16u) != 0u) {
            uint style = (cell.flags >> 9u) & 7u;
            float bottom = float(ch) - local_y;
            float band = float(thick);
            if (style == 0u) paint = bottom <= band;
            if (style == 1u) paint = bottom <= band || (bottom > 2.0 * band && bottom <= 3.0 * band);
            if (style == 2u) {
                // A triangle wave gives deterministic cell-aligned curls without an atlas.
                float phase = mod(local_x, 6.0 * band) / (3.0 * band);
                float wave = 1.0 + 2.0 * abs(phase - 1.0);
                paint = bottom > wave * band && bottom <= (wave + 1.0) * band;
            }
            if (style == 3u) paint = bottom <= band && mod(local_x, 2.0 * band) < band;
            if (style == 4u) paint = bottom <= band && mod(local_x, 6.0 * band) < 4.0 * band;
            uint underline_color = cell.underline_color == 0xffffffffu ? fg : cell.underline_color;
            if ((cell.flags & 128u) != 0u && cell.underline_color != 0xffffffffu) underline_color = (underline_color & 0x00fefefeu) >> 1u;
            if (paint) color = unpack_color(underline_color);
        }
        if ((cell.flags & 32u) != 0u && local_y >= float(ch / 2u - thick / 2u) && local_y < float(ch / 2u - thick / 2u + thick)) color = unpack_color(fg);
    }
    paint = false;
    if (is_cursor && grid.cursor_style == 1u && local_y >= float(ch - 2u * thick)) paint = true;
    if (is_cursor && grid.cursor_style == 2u && local_x < float(2u * thick)) paint = true;
    if (is_cursor && grid.cursor_style == 3u && (local_x < float(thick) || local_y < float(thick) || local_x >= grid.cell_width - float(thick) || local_y >= grid.cell_height - float(thick))) paint = true;
    if (paint) color = unpack_color(cursor_color);
}
