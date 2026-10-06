use aterm_core::Terminal;
use aterm_font::Font;
use aterm_render::OffscreenRenderer;

#[test]
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn renders_terminal_cells_and_reads_back_expected_colors() {
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let mut terminal = Terminal::new(3, 2).expect("create terminal grid");
    terminal.process(b"\x1b[41mA\x1b[42mB\x1b[44mC\x1b[0m\r\n123");
    assert_eq!(
        terminal.viewport_cell(1, 0).unwrap().bg,
        aterm_core::DEFAULT_COLOR
    );
    let width = metrics.width * 3;
    let height = metrics.height * 2;
    let mut renderer = OffscreenRenderer::new(width, height).expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    assert_eq!(pixels.len(), (width * height * 4) as usize);

    let pixel = |x: u32, y: u32| {
        let index = ((y * width + x) * 4) as usize;
        &pixels[index..index + 4]
    };
    assert_eq!(pixel(0, 0), &[205, 0, 0, 255]);
    assert_eq!(pixel(metrics.width, 0), &[0, 205, 0, 255]);
    assert_eq!(pixel(metrics.width * 2, 0), &[0, 0, 238, 255]);
    assert_eq!(
        pixel(0, metrics.height + metrics.height / 2),
        &[24, 24, 24, 255]
    );

    let first_cell = (0..metrics.height)
        .flat_map(|y| (0..metrics.width).map(move |x| (x, y)))
        .map(|(x, y)| pixel(x, y))
        .any(|rgba| rgba[0] > 150 && rgba[1] > 150 && rgba[2] > 150);
    assert!(
        first_cell,
        "the atlas sampled at least one bright A glyph pixel"
    );
}
