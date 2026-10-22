use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, CreateGCAux, FontWrapper};
use x11rb::xcb_ffi::XCBConnection;

/// Session-themed cursors, with the X11 cursor font as a fallback.
pub struct Pointer {
    text: u32,
    arrow: u32,
    invisible: u32,
    current: u32,
}

impl Pointer {
    pub fn new(
        connection: &XCBConnection,
        screen: usize,
        window: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let resources = x11rb::resource_manager::new_from_default(connection)?;
        let cursors = x11rb::cursor::Handle::new(connection, screen, &resources)?.reply()?;
        let text = load_cursor(connection, &cursors, "xterm", 152)?;
        let arrow = load_cursor(connection, &cursors, "left_ptr", 68)?;
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

fn load_cursor(
    connection: &XCBConnection,
    cursors: &x11rb::cursor::Handle,
    name: &str,
    shape: u16,
) -> Result<u32, Box<dyn std::error::Error>> {
    if let Ok(cursor) = cursors.load_cursor(connection, name)
        && cursor != x11rb::NONE
    {
        return Ok(cursor);
    }
    // Missing or malformed themes and servers without RENDER still need a
    // visible pointer. Match the traditional black foreground/white outline.
    let font = FontWrapper::open_font(connection, b"cursor")?;
    let cursor = connection.generate_id()?;
    connection
        .create_glyph_cursor(
            cursor,
            font.font(),
            font.font(),
            shape,
            shape + 1,
            0,
            0,
            0,
            u16::MAX,
            u16::MAX,
            u16::MAX,
        )?
        .check()?;
    Ok(cursor)
}
