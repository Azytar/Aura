use std::io;
use std::os::fd::AsRawFd;

mod config;
use as_raw_xcb_connection::AsRawXcbConnection;
use aterm_core::{MouseTracking, Pty, PtySize, SelectionPoint, Terminal};
use aterm_font::{CellMetrics, Font};
use aterm_render::Renderer;
use base64::Engine;
use config::{AppConfig, FontGeometry, RuntimeFontSize, grid_dimensions};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureNotifyEvent, ConnectionExt as XprotoExt, CreateWindowAux, EventMask,
    KeyButMask, PropMode, SelectionNotifyEvent, WindowClass,
};
use x11rb::wrapper::ConnectionExt as WrapperExt;
use x11rb::xcb_ffi::XCBConnection;

fn main() {
    let opts = match arguments() {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("aterm: {error}");
            std::process::exit(1);
        }
    };
    if opts.is_none() {
        return;
    }
    if let Err(error) = run() {
        eprintln!("aterm: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::load();
    let mut font_sizes = RuntimeFontSize::new(config.font.size);
    let mut font = Font::load_with_family(&config.font.family, font_sizes.current())
        .map_err(io::Error::other)?;
    let geometry = FontGeometry::new(
        font.metrics().width,
        font.metrics().height,
        config.window.columns,
        config.window.lines,
        config.window.padding_x,
        config.window.padding_y,
    );
    let (pixel_width, pixel_height) = (geometry.pixel_width, geometry.pixel_height);
    let (connection, screen_index) = XCBConnection::connect(None)?;
    let screen = &connection.setup().roots[screen_index];
    let window = connection.generate_id()?;
    let protocols = connection
        .intern_atom(false, b"WM_PROTOCOLS")?
        .reply()?
        .atom;
    let delete_window = connection
        .intern_atom(false, b"WM_DELETE_WINDOW")?
        .reply()?
        .atom;
    connection.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        pixel_width as u16,
        pixel_height as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION,
            ),
    )?;
    set_title(&connection, window, "Terminal")?;
    let mut clipboard = Clipboard::new(&connection)?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        protocols,
        AtomEnum::ATOM,
        &[delete_window],
    )?;
    connection.map_window(window)?;
    connection.flush()?;

    let (columns, rows) = (config.window.columns, config.window.lines);
    let mut terminal = Terminal::new(columns, rows)
        .map_err(|error| io::Error::other(format!("terminal grid: {error:?}")))?;
    let mut pty = Pty::spawn_shell(PtySize::new(columns, rows)?)?;
    // XCB stays alive in this scope, so the raw pointer remains valid until Renderer is dropped.
    let raw_connection = connection.as_raw_xcb_connection().cast();
    let mut renderer = Renderer::new(raw_connection, window, pixel_width, pixel_height)
        .map_err(io::Error::other)?;
    renderer
        .render(&mut terminal, &mut font, true)
        .map_err(io::Error::other)?;

    let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let timer = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if timer == -1 {
        unsafe { libc::close(epoll) };
        return Err(io::Error::last_os_error().into());
    }
    let interval = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 500_000_000,
        },
        it_value: libc::timespec {
            tv_sec: 0,
            tv_nsec: 500_000_000,
        },
    };
    if unsafe { libc::timerfd_settime(timer, 0, &interval, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let x_fd = connection.as_raw_fd();
    let pty_fd = pty.master_fd()?;
    epoll_add(epoll, x_fd, 1, libc::EPOLLIN as u32)?;
    epoll_add(
        epoll,
        pty_fd,
        2,
        libc::EPOLLIN as u32 | libc::EPOLLHUP as u32,
    )?;
    epoll_add(epoll, timer, 3, libc::EPOLLIN as u32)?;
    let (mut keysyms, mut keysyms_per_keycode) = keyboard_mapping(&connection)?;
    let mut running = true;
    let mut invalidated = false;
    let mut cursor_blink_on = true;
    let mut selection_anchor = None;
    let mut last_click: Option<(u32, u8, SelectionPoint, u8)> = None;
    let mut previous_title = terminal.title().to_owned();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 16];
    let mut buffer = [0u8; 16 * 1024];
    let mut stats = RenderStats::new();

    while running {
        let event_count =
            unsafe { libc::epoll_wait(epoll, events.as_mut_ptr(), events.len() as i32, -1) };
        if event_count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        for ready in events.iter().take(event_count as usize) {
            match ready.u64 {
                1 => {
                    while let Some(event) = connection.poll_for_event()? {
                        match event {
                            Event::ClientMessage(message)
                                if message.data.as_data32()[0] == delete_window =>
                            {
                                running = false
                            }
                            Event::DestroyNotify(_) => running = false,
                            // The server sends this when the keymap changes;
                            // without a refetch every key would mistranslate.
                            Event::MappingNotify(_) => {
                                (keysyms, keysyms_per_keycode) = keyboard_mapping(&connection)?;
                            }
                            Event::ConfigureNotify(configure) => {
                                resize(
                                    &mut terminal,
                                    &mut pty,
                                    &mut renderer,
                                    (font.metrics().width, font.metrics().height),
                                    (config.window.padding_x, config.window.padding_y),
                                    configure,
                                )?;
                                invalidated = true;
                            }
                            Event::Expose(_) => invalidated = true,
                            Event::KeyPress(key) => {
                                // Runtime font zoom consumes Ctrl+Plus/Minus/0
                                // here so the bytes never reach the PTY child.
                                if let Some(action) = font_size_action(
                                    &key,
                                    &keysyms,
                                    keysyms_per_keycode,
                                    connection.setup().min_keycode,
                                ) {
                                    let changed = match action {
                                        FontSizeAction::Grow => font_sizes.grow(),
                                        FontSizeAction::Shrink => font_sizes.shrink(),
                                        FontSizeAction::Reset => font_sizes.reset(),
                                    };
                                    if changed {
                                        resize_font(
                                            &config,
                                            &mut font_sizes,
                                            &mut font,
                                            &mut terminal,
                                            &mut pty,
                                            &connection,
                                            window,
                                            &mut renderer,
                                        )?;
                                        cursor_blink_on = true;
                                        invalidated = true;
                                    }
                                    continue;
                                }
                                let sym = key_sym(
                                    &key,
                                    &keysyms,
                                    keysyms_per_keycode,
                                    connection.setup().min_keycode,
                                );
                                if key.state.contains(KeyButMask::CONTROL)
                                    && key.state.contains(KeyButMask::SHIFT)
                                    && matches!(sym, Some(0x63 | 0x43))
                                {
                                    if let Some(text) = terminal.selection_text() {
                                        clipboard.set_text(
                                            &connection,
                                            window,
                                            clipboard.clipboard_atom,
                                            text.as_bytes(),
                                        )?;
                                    }
                                    continue;
                                }
                                if key.state.contains(KeyButMask::CONTROL)
                                    && key.state.contains(KeyButMask::SHIFT)
                                    && matches!(sym, Some(0x76 | 0x56))
                                {
                                    clipboard.request_paste(&connection, window)?;
                                    continue;
                                }
                                if key.state.contains(KeyButMask::CONTROL)
                                    && key.state.contains(KeyButMask::SHIFT)
                                    && matches!(sym, Some(0xff50 | 0xff57))
                                {
                                    if sym == Some(0xff50) {
                                        terminal.scrollback_top();
                                    } else {
                                        terminal.jump_to_bottom();
                                    }
                                    invalidated = true;
                                    continue;
                                }
                                if key.state.contains(KeyButMask::SHIFT)
                                    && matches!(sym, Some(0xff55 | 0xff56))
                                {
                                    if sym == Some(0xff55) {
                                        terminal.page_up();
                                    } else {
                                        terminal.page_down();
                                    }
                                    invalidated = true;
                                    continue;
                                }
                                if let Some(bytes) = translate_key(
                                    &key,
                                    keysyms.as_slice(),
                                    keysyms_per_keycode,
                                    connection.setup().min_keycode,
                                    terminal.modes().application_cursor,
                                ) {
                                    write_all_pty(&mut pty, &bytes)?;
                                    cursor_blink_on = true;
                                    invalidated = true;
                                }
                            }
                            Event::ButtonPress(button) => {
                                if terminal.modes().mouse_tracking == MouseTracking::Off
                                    && matches!(button.detail, 4 | 5)
                                {
                                    if button.detail == 4 {
                                        terminal.scroll_view_up(3);
                                    } else {
                                        terminal.scroll_view_down(3);
                                    }
                                    invalidated = true;
                                } else if terminal.modes().mouse_tracking == MouseTracking::Off
                                    && button.detail == 1
                                {
                                    let point = selection_point(
                                        button.event_x,
                                        button.event_y,
                                        font.metrics(),
                                        &terminal,
                                    );
                                    let count = last_click
                                        .filter(|(time, detail, old, _)| {
                                            *detail == button.detail
                                                && button.time.wrapping_sub(*time) <= 400
                                                && old.row == point.row
                                                && old.column.abs_diff(point.column) <= 1
                                        })
                                        .map_or(1, |(_, _, _, count)| count % 3 + 1);
                                    last_click = Some((button.time, button.detail, point, count));
                                    match count {
                                        2 => terminal.select_word(point),
                                        3 => terminal.select_line(point.row),
                                        _ => {
                                            selection_anchor = Some(point);
                                            terminal.set_selection(point, point);
                                        }
                                    }
                                    invalidated = true;
                                } else if let Some(bytes) = mouse_report(
                                    terminal.modes(),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: false,
                                        motion: false,
                                    },
                                    font.metrics(),
                                ) {
                                    write_all_pty(&mut pty, &bytes)?;
                                }
                            }
                            Event::ButtonRelease(button) => {
                                if terminal.modes().mouse_tracking == MouseTracking::Off {
                                    selection_anchor = None;
                                } else if terminal.modes().mouse_tracking == MouseTracking::Press {
                                    continue;
                                } else if let Some(bytes) = mouse_report(
                                    terminal.modes(),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: true,
                                        motion: false,
                                    },
                                    font.metrics(),
                                ) {
                                    write_all_pty(&mut pty, &bytes)?;
                                }
                            }
                            Event::MotionNotify(motion) => {
                                if terminal.modes().mouse_tracking == MouseTracking::Drag {
                                    if let Some(button) = held_button(motion.state) {
                                        if let Some(bytes) = mouse_report(
                                            terminal.modes(),
                                            MouseEvent {
                                                button,
                                                x: motion.event_x,
                                                y: motion.event_y,
                                                state: motion.state,
                                                released: false,
                                                motion: true,
                                            },
                                            font.metrics(),
                                        ) {
                                            write_all_pty(&mut pty, &bytes)?;
                                        }
                                    }
                                } else if let Some(anchor) = selection_anchor {
                                    let point = selection_point(
                                        motion.event_x,
                                        motion.event_y,
                                        font.metrics(),
                                        &terminal,
                                    );
                                    terminal.set_selection(anchor, point);
                                    invalidated = true;
                                }
                            }
                            Event::SelectionRequest(request) => {
                                clipboard.respond(&connection, request)?;
                            }
                            Event::SelectionNotify(notify)
                                if notify.property != AtomEnum::NONE.into() =>
                            {
                                if let Some(bytes) =
                                    clipboard.receive_paste(&connection, window, notify.property)?
                                {
                                    let input = terminal.encode_paste(&bytes);
                                    write_all_pty(&mut pty, &input)?;
                                    invalidated = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                2 => loop {
                    match pty.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(length) => {
                            stats.add_pty_bytes(length as u64);
                            let replies = terminal.process(&buffer[..length]);
                            for request in terminal.take_clipboard_requests() {
                                let selection = match request.selection.as_slice() {
                                    b"c" => Some(clipboard.clipboard_atom),
                                    b"p" => Some(clipboard.primary_atom),
                                    _ => None,
                                };
                                if let Some(selection) = selection
                                    && let Some(text) = decode_clipboard_payload(&request.base64)
                                {
                                    clipboard.set_text(&connection, window, selection, &text)?;
                                }
                            }
                            if !replies.is_empty() {
                                write_all_pty(&mut pty, &replies)?;
                            }
                            invalidated = true;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(error) => return Err(error.into()),
                    }
                },
                3 => {
                    let mut expirations = 0u64;
                    let _ = unsafe {
                        libc::read(
                            timer,
                            (&mut expirations as *mut u64).cast(),
                            size_of::<u64>(),
                        )
                    };
                    cursor_blink_on = !cursor_blink_on;
                    // A hidden or steady cursor needs no blink repaints.
                    if terminal.modes().cursor_visible && terminal.modes().cursor_blink {
                        invalidated = true;
                    }
                }
                _ => {}
            }
        }
        if terminal.title() != previous_title {
            previous_title = terminal.title().to_owned();
            set_title(&connection, window, &previous_title)?;
            invalidated = true;
        }
        if invalidated {
            let frame_start = std::time::Instant::now();
            renderer
                .render(&mut terminal, &mut font, cursor_blink_on)
                .map_err(io::Error::other)?;
            stats.add_frame(frame_start.elapsed());
            invalidated = false;
        }
        stats.maybe_report();
        if pty.try_wait()?.is_some() {
            running = false;
        }
    }
    unsafe {
        libc::close(timer);
        libc::close(epoll);
    }
    Ok(())
}

/// Lightweight render counters reported to stderr every few seconds while
/// frames are produced. No per-event logging; idle terminals stay silent.
struct RenderStats {
    window_start: std::time::Instant,
    frames: u64,
    pty_bytes: u64,
    render_nanos: u128,
}

impl RenderStats {
    fn new() -> Self {
        Self {
            window_start: std::time::Instant::now(),
            frames: 0,
            pty_bytes: 0,
            render_nanos: 0,
        }
    }

    fn add_pty_bytes(&mut self, bytes: u64) {
        self.pty_bytes = self.pty_bytes.saturating_add(bytes);
    }

    fn add_frame(&mut self, elapsed: std::time::Duration) {
        self.frames += 1;
        self.render_nanos += elapsed.as_nanos();
    }

    fn maybe_report(&mut self) {
        if self.window_start.elapsed() < std::time::Duration::from_secs(5) {
            return;
        }
        if self.frames > 0 {
            let seconds = self.window_start.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "aterm perf: {:.0} frames/s {:.0} KiB/s PTY {:.2} ms/frame",
                self.frames as f64 / seconds,
                self.pty_bytes as f64 / 1024.0 / seconds,
                self.render_nanos as f64 / self.frames as f64 / 1_000_000.0,
            );
        }
        *self = Self::new();
    }
}

/// Fetches the full server keymap: symbols per keycode plus the stride.
/// Called once at startup and again on every mapping notification so layout
/// switches never leave key translation behind.
fn keyboard_mapping(
    connection: &XCBConnection,
) -> Result<(Vec<u32>, usize), Box<dyn std::error::Error>> {
    let mapping = connection
        .get_keyboard_mapping(
            connection.setup().min_keycode,
            connection.setup().max_keycode - connection.setup().min_keycode + 1,
        )?
        .reply()?;
    Ok((mapping.keysyms, usize::from(mapping.keysyms_per_keycode)))
}

/// Parsed launch options; `None` means a flag like `--help` already ran.
type LaunchOptions = Option<()>;

#[allow(clippy::never_loop)]
fn arguments() -> Result<LaunchOptions, Box<dyn std::error::Error>> {
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "aterm {} - GPU terminal emulator (Vulkan/X11)\n\n\
                     Usage: aterm [OPTIONS]\n\n\
                     Options:\n  \
                     -h, --help       print this help and exit\n  \
                     -V, --version    print the version and exit\n\n\
                     Font and terminal geometry are configured in:\n  \
                     ~/.config/aterm/aterm.toml",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(None);
            }
            Some("--version" | "-V") => {
                println!("aterm {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Some(other) => return Err(format!("unknown argument: {other}").into()),
            None => return Err("arguments must be valid UTF-8".into()),
        }
    }
    Ok(Some(()))
}

fn epoll_add(epoll: i32, fd: i32, token: u64, flags: u32) -> io::Result<()> {
    let mut event = libc::epoll_event {
        events: flags,
        u64: token,
    };
    if unsafe { libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, fd, &mut event) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn set_title(
    connection: &XCBConnection,
    window: u32,
    title: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let atom = connection
        .intern_atom(false, b"_NET_WM_NAME")?
        .reply()?
        .atom;
    let utf8 = connection.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
    connection.change_property8(PropMode::REPLACE, window, atom, utf8, title.as_bytes())?;
    connection.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        title.as_bytes(),
    )?;
    connection.flush()?;
    Ok(())
}

const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

fn decode_clipboard_payload(encoded: &[u8]) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
    let decoded = STANDARD
        .decode(encoded)
        .or_else(|_| STANDARD_NO_PAD.decode(encoded))
        .ok()?;
    (decoded.len() <= MAX_CLIPBOARD_BYTES).then_some(decoded)
}

struct Clipboard {
    clipboard_atom: u32,
    primary_atom: u32,
    utf8_atom: u32,
    targets_atom: u32,
    property_atom: u32,
    clipboard_contents: Vec<u8>,
    primary_contents: Vec<u8>,
}

impl Clipboard {
    fn new(connection: &XCBConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let atom = |name: &[u8]| -> Result<u32, Box<dyn std::error::Error>> {
            Ok(connection.intern_atom(false, name)?.reply()?.atom)
        };
        Ok(Self {
            clipboard_atom: atom(b"CLIPBOARD")?,
            primary_atom: AtomEnum::PRIMARY.into(),
            utf8_atom: atom(b"UTF8_STRING")?,
            targets_atom: atom(b"TARGETS")?,
            property_atom: atom(b"ATERM_CLIPBOARD")?,
            clipboard_contents: Vec::new(),
            primary_contents: Vec::new(),
        })
    }

    fn set_text(
        &mut self,
        connection: &XCBConnection,
        window: u32,
        selection: u32,
        text: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Ok(());
        }
        let contents = if selection == self.clipboard_atom {
            &mut self.clipboard_contents
        } else {
            &mut self.primary_contents
        };
        contents.clear();
        contents.extend_from_slice(text);
        connection
            .set_selection_owner(window, selection, x11rb::CURRENT_TIME)?
            .check()?;
        Ok(())
    }

    fn request_paste(
        &self,
        connection: &XCBConnection,
        window: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        connection
            .convert_selection(
                window,
                self.clipboard_atom,
                self.utf8_atom,
                self.property_atom,
                x11rb::CURRENT_TIME,
            )?
            .check()?;
        Ok(())
    }

    fn respond(
        &self,
        connection: &XCBConnection,
        request: x11rb::protocol::xproto::SelectionRequestEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let property = if request.property == AtomEnum::NONE.into() {
            request.target
        } else {
            request.property
        };
        let supported_selection =
            request.selection == self.clipboard_atom || request.selection == self.primary_atom;
        let contents = if request.selection == self.clipboard_atom {
            &self.clipboard_contents
        } else {
            &self.primary_contents
        };
        let accepted = if !supported_selection {
            false
        } else if request.target == self.targets_atom {
            connection.change_property32(
                PropMode::REPLACE,
                request.requestor,
                property,
                AtomEnum::ATOM,
                &[self.targets_atom, self.utf8_atom, AtomEnum::STRING.into()],
            )?;
            true
        } else if request.target == self.utf8_atom || request.target == AtomEnum::STRING.into() {
            connection.change_property8(
                PropMode::REPLACE,
                request.requestor,
                property,
                request.target,
                contents,
            )?;
            true
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if accepted {
                property
            } else {
                AtomEnum::NONE.into()
            },
        };
        connection
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?
            .check()?;
        Ok(())
    }

    fn receive_paste(
        &self,
        connection: &XCBConnection,
        window: u32,
        property: u32,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
        let reply = connection
            .get_property(
                true,
                window,
                property,
                AtomEnum::ANY,
                0,
                MAX_CLIPBOARD_BYTES.div_ceil(4) as u32,
            )?
            .reply()?;
        if reply.format != 8
            || reply.value.len() > MAX_CLIPBOARD_BYTES
            || reply.bytes_after != 0
            || (reply.type_ != self.utf8_atom && reply.type_ != AtomEnum::STRING.into())
        {
            return Ok(None);
        }
        Ok(Some(reply.value))
    }
}

fn resize(
    terminal: &mut Terminal,
    pty: &mut Pty,
    renderer: &mut Renderer,
    metrics: (u32, u32),
    padding: (u32, u32),
    event: ConfigureNotifyEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let (cell_width, cell_height) = metrics;
    let (padding_x, padding_y) = padding;
    let inner_width = u32::from(event.width).saturating_sub(2 * padding_x).max(1);
    let inner_height = u32::from(event.height).saturating_sub(2 * padding_y).max(1);
    let (width, height) = grid_dimensions(inner_width, inner_height, cell_width, cell_height);
    if terminal.active_grid().width() != width || terminal.active_grid().height() != height {
        terminal
            .resize(width, height)
            .map_err(|error| io::Error::other(format!("terminal resize: {error:?}")))?;
        pty.resize(PtySize::new(width, height)?)?;
    }
    renderer
        .resize(
            u32::from(event.width).max(1),
            u32::from(event.height).max(1),
        )
        .map_err(io::Error::other)?;
    Ok(())
}

/// Runtime font-size change: re-rasterizes at the new size, rebuilds the
/// atlas state, recomputes the single-source geometry, resizes the window
/// pixels (keeping the configured columns×rows), propagates `TIOCSWINSZ`,
/// and invalidates the frame. Screen content, modes, cursor, alternate
/// screen, and scrollback all survive via the existing `Terminal::resize`
/// path — which is a no-op here because columns×rows do not change; pixel
/// geometry and the renderer still refresh.
#[allow(clippy::too_many_arguments)]
fn resize_font(
    config: &AppConfig,
    font_sizes: &mut RuntimeFontSize,
    font: &mut Font,
    terminal: &mut Terminal,
    pty: &mut Pty,
    connection: &XCBConnection,
    window: u32,
    renderer: &mut Renderer,
) -> Result<(), Box<dyn std::error::Error>> {
    // New rasterization + new metrics + cleared glyph cache/atlas shelves.
    font.set_size(font_sizes.current())
        .map_err(io::Error::other)?;
    // Single source of truth: one geometry feeds window, grid, PTY, renderer.
    let geometry = FontGeometry::new(
        font.metrics().width,
        font.metrics().height,
        terminal.active_grid().width(),
        terminal.active_grid().height(),
        config.window.padding_x,
        config.window.padding_y,
    );
    // Keep the current grid size; only pixel geometry changed. Reflow policy
    // stays identical to a normal resize (no second reflow implementation).
    terminal
        .resize(geometry.columns, geometry.rows)
        .map_err(|error| io::Error::other(format!("terminal resize: {error:?}")))?;
    let mut winsize = PtySize::new(geometry.columns, geometry.rows)?;
    let (px, py) = geometry.pty_pixels();
    winsize.pixel_width = px;
    winsize.pixel_height = py;
    pty.resize(winsize)?;
    // Ask the window manager for the new pixel size; a tiling WM may ignore
    // it, in which case the next ConfigureNotify reconciles via `resize()`.
    connection.configure_window(
        window,
        &x11rb::protocol::xproto::ConfigureWindowAux::new()
            .width(geometry.pixel_width)
            .height(geometry.pixel_height),
    )?;
    connection.flush()?;
    renderer
        .resize(geometry.pixel_width, geometry.pixel_height)
        .map_err(io::Error::other)?;
    Ok(())
}

fn write_all_pty(pty: &mut Pty, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match pty.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "PTY accepted no input",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn translate_key(
    event: &x11rb::protocol::xproto::KeyPressEvent,
    symbols: &[u32],
    per_key: usize,
    min_keycode: u8,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    let sym = key_sym(event, symbols, per_key, min_keycode)?;
    let ctrl = event.state.contains(KeyButMask::CONTROL);
    let alt = event.state.contains(KeyButMask::MOD1);
    let mut bytes = match sym {
        0xff08 => vec![0x7f],
        0xff09 | 0xfe20 => vec![b'\t'],
        0xff0d => vec![b'\r'],
        0xff1b => vec![0x1b],
        0xff50 => {
            if application_cursor {
                b"\x1bOH".to_vec()
            } else {
                b"\x1b[H".to_vec()
            }
        }
        0xff51 => cursor_key(b'D', application_cursor),
        0xff52 => cursor_key(b'A', application_cursor),
        0xff53 => cursor_key(b'C', application_cursor),
        0xff54 => cursor_key(b'B', application_cursor),
        0xff57 => {
            if application_cursor {
                b"\x1bOF".to_vec()
            } else {
                b"\x1b[F".to_vec()
            }
        }
        0xff63 => b"\x1b[2~".to_vec(),
        0xffff => b"\x1b[3~".to_vec(),
        0xff55 => b"\x1b[5~".to_vec(),
        0xff56 => b"\x1b[6~".to_vec(),
        0xffbe..=0xffc9 => function_key(sym)?,
        ascii if (0x20..=0x7e).contains(&ascii) => vec![ascii as u8],
        latin if (0xa0..=0xff).contains(&latin) => char::from_u32(latin)?.to_string().into_bytes(),
        unicode if (0x0100_0000..=0x0110_ffff).contains(&unicode) => {
            char::from_u32(unicode & 0x00ff_ffff)?
                .to_string()
                .into_bytes()
        }
        _ => return None,
    };
    if ctrl && bytes.len() == 1 {
        let byte = bytes[0].to_ascii_uppercase();
        if (b'@'..=b'_').contains(&byte) {
            bytes[0] = byte & 0x1f;
        }
        if bytes[0] == b' ' {
            bytes[0] = 0;
        }
    }
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn key_sym(
    event: &x11rb::protocol::xproto::KeyPressEvent,
    symbols: &[u32],
    per_key: usize,
    min_keycode: u8,
) -> Option<u32> {
    let offset = usize::from(event.detail.checked_sub(min_keycode)?) * per_key;
    let keysyms = symbols.get(offset..offset + per_key)?;
    let shifted = event.state.contains(KeyButMask::SHIFT) && keysyms.len() > 1;
    keysyms.get(usize::from(shifted)).copied()
}

/// Runtime font-size intent derived from a key press. Pure logic over the
/// translated keymap so tests never need X11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontSizeAction {
    Grow,
    Shrink,
    Reset,
}

/// Matches `Ctrl+Plus/Minus/0` from the translated keymap without hardcoding
/// physical keycodes, so Dvorak, QWERTY, and other layouts all work.
/// Consumes both shifted and unshifted levels (`+` is normally `Shift+=`)
/// plus keypad keys; requires Ctrl without Alt so `Alt` combos still reach
/// the application. Returns `None` for every other key.
fn font_size_action(
    event: &x11rb::protocol::xproto::KeyPressEvent,
    symbols: &[u32],
    per_key: usize,
    min_keycode: u8,
) -> Option<FontSizeAction> {
    if !event.state.contains(KeyButMask::CONTROL) || event.state.contains(KeyButMask::MOD1) {
        return None;
    }
    let offset = usize::from(event.detail.checked_sub(min_keycode)?) * per_key;
    let keysyms = symbols.get(offset..offset + per_key)?;
    // Level 0 is the unshifted keysym, level 1 the shifted one. Either may
    // carry the zoom key (`+` lives on level 1 of `=` on US layouts), and
    // later levels hold layout-specific variants we also accept.
    let mut grow = false;
    let mut shrink = false;
    let mut reset = false;
    for &sym in keysyms.iter().take(per_key.max(2).min(keysyms.len())) {
        match sym {
            // `+`, `=` (Shift+= produces `+`), keypad add / equal.
            0x2b | 0x3d | 0xffaa | 0xffbd => grow = true,
            // `-`, `_` (Shift+-), keypad subtract.
            0x2d | 0x5f | 0xffad => shrink = true,
            // `0`, keypad 0/insert.
            0x30 | 0xffb0 | 0xff9e => reset = true,
            _ => {}
        }
    }
    if grow {
        Some(FontSizeAction::Grow)
    } else if shrink {
        Some(FontSizeAction::Shrink)
    } else if reset {
        Some(FontSizeAction::Reset)
    } else {
        None
    }
}

fn cursor_key(final_byte: u8, application_cursor: bool) -> Vec<u8> {
    if application_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn function_key(keysym: u32) -> Option<Vec<u8>> {
    let number = keysym - 0xffbe + 1;
    Some(match number {
        1..=4 => vec![0x1b, b'O', b'P' + number as u8 - 1],
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => return None,
    })
}

fn held_button(state: KeyButMask) -> Option<u8> {
    if state.contains(KeyButMask::BUTTON1) {
        Some(1)
    } else if state.contains(KeyButMask::BUTTON2) {
        Some(2)
    } else if state.contains(KeyButMask::BUTTON3) {
        Some(3)
    } else {
        None
    }
}

#[derive(Clone, Copy)]
struct MouseEvent {
    button: u8,
    x: i16,
    y: i16,
    state: KeyButMask,
    released: bool,
    motion: bool,
}

fn mouse_report(
    modes: aterm_core::Modes,
    event: MouseEvent,
    metrics: CellMetrics,
) -> Option<Vec<u8>> {
    let MouseEvent {
        button,
        x: pixel_x,
        y: pixel_y,
        state,
        released,
        motion,
    } = event;
    if modes.mouse_tracking == MouseTracking::Off || (released && button >= 4) {
        return None;
    }
    let mut code = match button {
        1 => 0u32,
        2 => 1,
        3 => 2,
        4 => 64,
        5 => 65,
        _ => return None,
    };
    if released {
        code = 3;
    } else if motion {
        code += 32;
    }
    if state.contains(KeyButMask::SHIFT) {
        code += 4;
    }
    if state.contains(KeyButMask::MOD1) {
        code += 8;
    }
    if state.contains(KeyButMask::CONTROL) {
        code += 16;
    }
    let x = (u32::try_from(pixel_x.max(0)).ok()? / metrics.width.max(1) + 1).min(9999);
    let y = (u32::try_from(pixel_y.max(0)).ok()? / metrics.height.max(1) + 1).min(9999);
    if modes.mouse_sgr {
        Some(
            format!(
                "\x1b[<{code};{x};{}{suffix}",
                y,
                suffix = if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        )
    } else if code <= 223 && x <= 223 && y <= 223 {
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (code + 32) as u8,
            (x + 32) as u8,
            (y + 32) as u8,
        ])
    } else {
        None
    }
}

fn selection_point(x: i16, y: i16, metrics: CellMetrics, terminal: &Terminal) -> SelectionPoint {
    SelectionPoint {
        row: ((u32::try_from(y.max(0)).unwrap_or(0) / metrics.height.max(1)) as usize)
            .min(terminal.active_grid().height() - 1),
        column: ((u32::try_from(x.max(0)).unwrap_or(0) / metrics.width.max(1)) as usize)
            .min(terminal.active_grid().width() - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::KeyPressEvent;

    #[test]
    fn ordinary_and_control_keys_are_translated() {
        let mut symbols = vec![0; 16];
        symbols[0] = b'a' as u32;
        symbols[1] = b'A' as u32;
        let event = KeyPressEvent {
            detail: 8,
            state: KeyButMask::default(),
            ..KeyPressEvent::default()
        };
        assert_eq!(
            translate_key(&event, &symbols, 2, 8, false).as_deref(),
            Some(&b"a"[..])
        );
        let control = KeyPressEvent {
            state: KeyButMask::CONTROL,
            ..event
        };
        assert_eq!(
            translate_key(&control, &symbols, 2, 8, false).as_deref(),
            Some(&[1][..])
        );
    }

    #[test]
    fn font_zoom_shortcuts_match_keysyms_not_keycodes() {
        // US layout: `=`/`+` share keycode 35 (levels 0/1), `-`/`_` keycode 48.
        let key = |detail: u8, state: KeyButMask| KeyPressEvent {
            detail,
            state,
            ..KeyPressEvent::default()
        };
        // Symbols indexed by (keycode - min_keycode) * per_key.
        let mut symbols = vec![0u32; 96 * 2];
        let put = |symbols: &mut Vec<u32>, detail: u8, l0: u32, l1: u32| {
            let base = (u32::from(detail) - 8) as usize * 2;
            symbols[base] = l0;
            symbols[base + 1] = l1;
        };
        put(&mut symbols, 35, 0x3d, 0x2b); // equal / plus
        put(&mut symbols, 48, 0x2d, 0x5f); // minus / underscore
        put(&mut symbols, 19, 0x30, 0x29); // 0 / parenright
        put(&mut symbols, 38, b'a' as u32, b'A' as u32);

        // Ctrl+- / Ctrl++ (via Shift+=) / Ctrl+= / Ctrl+0 all match.
        assert_eq!(
            font_size_action(&key(48, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Shrink)
        );
        assert_eq!(
            font_size_action(
                &key(35, KeyButMask::CONTROL | KeyButMask::SHIFT),
                &symbols,
                2,
                8
            ),
            Some(FontSizeAction::Grow)
        );
        assert_eq!(
            font_size_action(&key(35, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Grow)
        );
        assert_eq!(
            font_size_action(&key(19, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Reset)
        );
        // Keypad variants.
        put(&mut symbols, 86, 0xffaa, 0);
        put(&mut symbols, 82, 0xffad, 0);
        put(&mut symbols, 90, 0xffb0, 0);
        assert_eq!(
            font_size_action(&key(86, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Grow)
        );
        assert_eq!(
            font_size_action(&key(82, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Shrink)
        );
        assert_eq!(
            font_size_action(&key(90, KeyButMask::CONTROL), &symbols, 2, 8),
            Some(FontSizeAction::Reset)
        );
        // Without Ctrl nothing matches; with Alt the app keeps the combo.
        assert_eq!(
            font_size_action(&key(48, KeyButMask::default()), &symbols, 2, 8),
            None
        );
        assert_eq!(
            font_size_action(
                &key(48, KeyButMask::CONTROL | KeyButMask::MOD1),
                &symbols,
                2,
                8
            ),
            None
        );
        // Ordinary Ctrl combos (Ctrl+A) are not zoom: they reach the PTY.
        assert_eq!(
            font_size_action(&key(38, KeyButMask::CONTROL), &symbols, 2, 8),
            None
        );
        assert_eq!(
            translate_key(&key(38, KeyButMask::CONTROL), &symbols, 2, 8, false).as_deref(),
            Some(&[1][..])
        );
    }

    #[test]
    fn cursor_and_alt_prefix_follow_modes() {
        let symbols = [0xff51, 0];
        let event = KeyPressEvent {
            detail: 8,
            state: KeyButMask::MOD1,
            ..KeyPressEvent::default()
        };
        assert_eq!(
            translate_key(&event, &symbols, 2, 8, false).as_deref(),
            Some(&b"\x1b\x1b[D"[..])
        );
        assert_eq!(
            translate_key(&event, &symbols, 2, 8, true).as_deref(),
            Some(&b"\x1b\x1bOD"[..])
        );
    }

    #[test]
    fn mouse_reports_follow_tracking_and_sgr_modes() {
        let mut modes = aterm_core::Modes::default();
        let metrics = CellMetrics {
            width: 9,
            height: 16,
            ascent: 12,
            descent: 3,
            line_gap: 1,
        };
        let press = MouseEvent {
            button: 1,
            x: 18,
            y: 32,
            state: KeyButMask::default(),
            released: false,
            motion: false,
        };
        assert!(mouse_report(modes, press, metrics).is_none());
        modes.mouse_tracking = MouseTracking::Drag;
        modes.mouse_sgr = true;
        assert_eq!(
            mouse_report(modes, press, metrics).as_deref(),
            Some(&b"\x1b[<0;3;3M"[..])
        );
        assert_eq!(
            mouse_report(
                modes,
                MouseEvent {
                    motion: true,
                    ..press
                },
                metrics
            )
            .as_deref(),
            Some(&b"\x1b[<32;3;3M"[..])
        );
        assert_eq!(
            mouse_report(
                modes,
                MouseEvent {
                    released: true,
                    ..press
                },
                metrics
            )
            .as_deref(),
            Some(&b"\x1b[<3;3;3m"[..])
        );
    }

    #[test]
    fn osc52_payload_decode_accepts_bounded_padded_and_unpadded_data() {
        assert_eq!(
            decode_clipboard_payload(b"aGVsbG8=").as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            decode_clipboard_payload(b"aGVsbG8").as_deref(),
            Some(&b"hello"[..])
        );
        assert!(decode_clipboard_payload(b"%%%=").is_none());
    }
}
