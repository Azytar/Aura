#version 460
struct Cell { uint glyph; uint fg; uint bg; uint flags; uint underline_color; };
layout(set = 0, binding = 1, std430) readonly buffer Cells { Cell cells[]; };
struct Glyph { vec4 uv; vec4 size_bearing; vec4 bearing_advance; };
layout(set = 0, binding = 2, std430) readonly buffer Glyphs { Glyph glyphs[]; };
layout(push_constant) uniform Grid { uint cols; uint rows; float cell_width; float cell_height; float ascent; int offset_x; int offset_y; uint cursor; uint cursor_visible; uint selection_start; uint selection_end; uint cursor_style; uint selection_color; uint padding_x; uint padding_y; uint cursor_color; uint background_color; uint background_dense; } grid;
layout(location = 0) out vec2 uv;
layout(location = 1) flat out uint fg;
layout(location = 2) flat out uint visible;
layout(location = 3) flat out uint color_flag;
layout(location = 4) flat out vec4 cell_bounds;
layout(location = 5) flat out uint graphic;
layout(location = 6) flat out float dim_factor;
void main() {
    uint index = gl_InstanceIndex;
    uint col = index % grid.cols;
    uint row = index / grid.cols;
    Cell cell = cells[index];
    vec2 cell_origin = vec2(grid.padding_x, grid.padding_y) + vec2(col, row) * vec2(grid.cell_width, grid.cell_height);
    float span = (cell.flags & 1u) != 0u ? 2.0 : 1.0;
    cell_bounds = vec4(cell_origin, cell_origin + vec2(span * grid.cell_width, grid.cell_height));
    visible = cell.glyph == 0xffffffffu || (cell.flags & 256u) != 0u ? 0u : 1u;
    // Hidden and empty cells previously rasterized glyph zero before the
    // fragment shader discarded them. Degenerate quads produce no fragments.
    if (visible == 0u) {
        gl_Position = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    graphic = visible != 0u && (cell.glyph & 0x80000000u) != 0u ? cell.glyph & 0x7fffffffu : 0u;
    Glyph glyph = glyphs[visible == 0u || graphic != 0u ? 0u : cell.glyph];
    vec2 corners[4] = vec2[](vec2(0,0), vec2(1,0), vec2(0,1), vec2(1,1));
    vec2 corner = corners[gl_VertexIndex];
    vec2 origin = vec2(float(col) * grid.cell_width + glyph.size_bearing.z + float(grid.offset_x),
                       float(row) * grid.cell_height + grid.ascent - glyph.size_bearing.w + float(grid.offset_y));
    vec2 pixel = origin + corner * glyph.size_bearing.xy;
    if (graphic != 0u) pixel = vec2(col, row) * vec2(grid.cell_width, grid.cell_height) + corner * vec2(grid.cell_width, grid.cell_height);
    gl_Position = vec4(pixel.x * 2.0 / (float(grid.cols) * grid.cell_width) - 1.0,
                       pixel.y * 2.0 / (float(grid.rows) * grid.cell_height) - 1.0, 0.0, 1.0);
    uv = mix(glyph.uv.xy, glyph.uv.zw, corner);
    fg = (cell.flags & 64u) != 0u ? cell.bg : cell.fg;
    dim_factor = (cell.flags & 128u) != 0u ? 0.5 : 1.0;
    if (dim_factor < 1.0) fg = (fg & 0x00fefefeu) >> 1u;
    color_flag = uint(glyph.bearing_advance.w + 0.5);
    if (grid.cursor_visible != 0u && grid.cursor_style == 0u && index == grid.cursor) fg = (cell.flags & 64u) != 0u ? cell.fg : cell.bg;
}
