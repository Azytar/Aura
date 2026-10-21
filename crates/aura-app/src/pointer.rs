use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, CreateGCAux};
use x11rb::xcb_ffi::XCBConnection;

/// Core X11 cursors need neither XFixes nor an additional native dependency.
pub struct Pointer {
    text: u32,
    arrow: u32,
    invisible: u32,
    current: u32,
}

impl Pointer {
    pub fn new(
        connection: &XCBConnection,
        window: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let font = connection.generate_id()?;
        connection.open_font(font, b"cursor")?.check()?;
        let text = connection.generate_id()?;
        let arrow = connection.generate_id()?;
        for (cursor, character) in [(text, 152), (arrow, 68)] {
            connection
                .create_glyph_cursor(
                    cursor,
                    font,
                    font,
                    character,
                    character + 1,
                    65535,
                    65535,
                    65535,
                    0,
                    0,
                    0,
                )?
                .check()?;
        }
        connection.close_font(font)?;
        let pixmap = connection.generate_id()?;
        let gc = connection.generate_id()?;
        connection.create_pixmap(1, pixmap, window, 1, 1)?.check()?;
        connection.create_gc(gc, pixmap, &CreateGCAux::new().foreground(0))?;
        connection.poly_fill_rectangle(
            pixmap,
            gc,
            &[x11rb::protocol::xproto::Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
        )?;
        let invisible = connection.generate_id()?;
        connection
            .create_cursor(invisible, pixmap, pixmap, 0, 0, 0, 0, 0, 0, 0, 0)?
            .check()?;
        connection.free_gc(gc)?;
        connection.free_pixmap(pixmap)?;
        Ok(Self {
            text,
            arrow,
            invisible,
            current: 0,
        })
    }

    pub fn update(
        &mut self,
        connection: &XCBConnection,
        window: u32,
        hidden: bool,
        application_mouse: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cursor = if hidden {
            self.invisible
        } else if application_mouse {
            self.arrow
        } else {
            self.text
        };
        if self.current != cursor {
            connection.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().cursor(cursor),
            )?;
            connection.flush()?;
            self.current = cursor;
        }
        Ok(())
    }
}
