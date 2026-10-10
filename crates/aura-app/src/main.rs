mod input;
mod keyboard;
mod signals;

use keyboard::{FontSizeAction, Keyboard, font_size_action};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use as_raw_xcb_connection::AsRawXcbConnection;
use ash::vk;
use aura_config::{AppConfig, FontGeometry, RuntimeFontSize, grid_dimensions};
use aura_core::{MouseTracking, SelectionPoint, Terminal};
use aura_pty::{Pty, PtySize};
use aura_render::Renderer;
use aura_text::{CellMetrics, Font};
use base64::Engine;
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
            eprintln!("aura: {error}");
            std::process::exit(1);
        }
    };
    if opts.is_none() {
        return;
    }
    if let Err(error) = run() {
        eprintln!("aura: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let termination = signals::Termination::new()?;
    let config = AppConfig::load();
    let mut font_sizes = RuntimeFontSize::new(config.font.size);
    let mut font = Font::load_with_family(&config.font.family, font_sizes.current())
        .map_err(io::Error::other)?;
    font.set_fallback_families(&config.font.fallback_families);
    let geometry = FontGeometry::new(
        font.metrics().width,
        font.metrics().height,
        config.window.columns,
        config.window.lines,
        config.window.padding_x,
        config.window.padding_y,
        config.font.glyph_offsets(),
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
    set_window_properties(&connection, window, &geometry)?;
    let mut clipboard = Clipboard::new(&connection)?;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        protocols,
        AtomEnum::ATOM,
        &[delete_window],
    )?;

    // The window is mapped only once everything behind it exists. Mapping
    // earlier makes the window visible while the PTY and renderer are still
    // being built, and any keystroke that arrives in that gap is dropped
    // because there is no child to send it to.
    let (columns, rows) = (config.window.columns, config.window.lines);
    let mut terminal = Terminal::new(columns, rows)
        .map_err(|error| io::Error::other(format!("terminal grid: {error:?}")))?;
    apply_configured_modes(&mut terminal, &config);
    terminal.set_scrollback_limit(config.scrollback.lines);
    let mut pty = Pty::spawn_shell(PtySize::new(columns, rows)?)?;
    // XCB stays alive in this scope, so the raw pointer remains valid until Renderer is dropped.
    let raw_connection = connection.as_raw_xcb_connection().cast();
    let mut renderer = Renderer::new(raw_connection, window, pixel_width, pixel_height)
        .map_err(io::Error::other)?;
    renderer.set_glyph_offset(config.font.offset_x, config.font.offset_y);
    renderer.set_palette(config.colors.palette());
    renderer
        .render(&mut terminal, &mut font, true)
        .map_err(io::Error::other)?;
    connection.map_window(window)?;
    connection.flush()?;

    let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: epoll_create1 returned a new descriptor owned by this scope.
    let epoll_owner = unsafe { OwnedFd::from_raw_fd(epoll) };
    let timer = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if timer == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: timerfd_create returned a new descriptor owned by this scope.
    let _timer_owner = unsafe { OwnedFd::from_raw_fd(timer) };
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
    epoll_add(epoll, termination.fd(), 4, libc::EPOLLIN as u32)?;
    let mut input = input::Input::default();
    let mut watching_write = false;
    let mut keyboard = Keyboard::new(&connection)?;
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
        // XKB/property replies can read X events into XCB while emptying the
        // socket. Drain that queue before sleeping on its file descriptor.
        let mut queued_event = connection.poll_for_event()?;
        let event_count = if queued_event.is_some() {
            events[0] = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: 1,
            };
            1
        } else {
            // SAFETY: events holds exactly the advertised writable entries.
            unsafe {
                libc::epoll_wait(
                    epoll_owner.as_raw_fd(),
                    events.as_mut_ptr(),
                    events.len() as i32,
                    -1,
                )
            }
        };
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
                    while let Some(event) = match queued_event.take() {
                        Some(event) => Some(event),
                        None => connection.poll_for_event()?,
                    } {
                        match event {
                            Event::ClientMessage(message)
                                if message.data.as_data32()[0] == delete_window =>
                            {
                                running = false
                            }
                            Event::DestroyNotify(_) => running = false,
                            // The server sends this when the keymap changes;
                            // without a refetch every key would mistranslate.
                            Event::MappingNotify(_)
                            | Event::XkbMapNotify(_)
                            | Event::XkbNewKeyboardNotify(_) => {
                                keyboard.reload(&connection)?;
                            }
                            Event::ConfigureNotify(configure) => {
                                resize(
                                    &mut terminal,
                                    &mut pty,
                                    &mut renderer,
                                    (
                                        font.metrics().width,
                                        font.metrics().height,
                                    ),
                                    (config.window.padding_x, config.window.padding_y),
                                    configure,
                                )?;
                                invalidated = true;
                            }
                            Event::Expose(_) => invalidated = true,
                            Event::KeyPress(key) => {
                                let (sym, modifiers) = keyboard.key(&key);
                                if let Some(action) = font_size_action(sym, modifiers) {
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
                                if modifiers.contains(KeyButMask::CONTROL)
                                    && modifiers.contains(KeyButMask::SHIFT)
                                    && matches!(sym, 0x63 | 0x43)
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
                                if modifiers.contains(KeyButMask::CONTROL)
                                    && modifiers.contains(KeyButMask::SHIFT)
                                    && matches!(sym, 0x76 | 0x56)
                                {
                                    clipboard.request_paste(&connection, window)?;
                                    continue;
                                }
                                if modifiers.contains(KeyButMask::CONTROL)
                                    && modifiers.contains(KeyButMask::SHIFT)
                                    && matches!(sym, 0xff50 | 0xff57)
                                {
                                    if sym == 0xff50 {
                                        terminal.scrollback_top();
                                    } else {
                                        terminal.jump_to_bottom();
                                    }
                                    invalidated = true;
                                    continue;
                                }
                                if modifiers.contains(KeyButMask::SHIFT)
                                    && matches!(sym, 0xff55 | 0xff56)
                                {
                                    if sym == 0xff55 {
                                        terminal.page_up();
                                    } else {
                                        terminal.page_down();
                                    }
                                    invalidated = true;
                                    continue;
                                }
                                if let Some(bytes) =
                                    keyboard.bytes(sym, modifiers, terminal.modes())
                                {
                                    input.push(&bytes);
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
                                        [config.window.padding_x, config.window.padding_y],
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
                                    [config.window.padding_x, config.window.padding_y],
                                ) {
                                    input.push(&bytes);
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
                                    [config.window.padding_x, config.window.padding_y],
                                ) {
                                    input.push(&bytes);
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
                                            [config.window.padding_x, config.window.padding_y],
                                        ) {
                                            input.push(&bytes);
                                        }
                                    }
                                } else if let Some(anchor) = selection_anchor {
                                    let point = selection_point(
                                        motion.event_x,
                                        motion.event_y,
                                        font.metrics(),
                                        &terminal,
                                        [config.window.padding_x, config.window.padding_y],
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
                                    let paste = terminal.encode_paste(&bytes);
                                    input.push(&paste);
                                    invalidated = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                2 => {
                    for _ in 0..16 {
                        match pty.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(length) => {
                                stats.add_pty_bytes(length as u64);
                                let replies = terminal.process(&buffer[..length]);
                                for request in terminal.take_clipboard_requests() {
                                    // A read request (`?`) is never honoured: the
                                    // terminal already refuses to emit one, so a
                                    // non-empty payload here is always a write.
                                    if !config.security.osc52.allows_write() {
                                        continue;
                                    }
                                    let selection = match request.selection.as_slice() {
                                        b"c" => Some(clipboard.clipboard_atom),
                                        b"p" => Some(clipboard.primary_atom),
                                        _ => None,
                                    };
                                    if let Some(selection) = selection
                                        && let Some(text) =
                                            decode_clipboard_payload(&request.base64)
                                    {
                                        clipboard.set_text(
                                            &connection,
                                            window,
                                            selection,
                                            &text,
                                        )?;
                                    }
                                }
                                if !replies.is_empty() {
                                    input.push(&replies);
                                }
                                invalidated = true;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
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
                4 => running = false,
                _ => {}
            }
        }
        if running {
            input.flush(|bytes| pty.write(bytes))?;
            if watching_write != input.pending() {
                watching_write = input.pending();
                let mut interest = libc::epoll_event {
                    events: (libc::EPOLLIN
                        | libc::EPOLLHUP
                        | if watching_write { libc::EPOLLOUT } else { 0 })
                        as u32,
                    u64: 2,
                };
                // SAFETY: epoll and pty_fd remain owned and interest is initialized.
                if unsafe { libc::epoll_ctl(epoll, libc::EPOLL_CTL_MOD, pty_fd, &mut interest) }
                    == -1
                {
                    return Err(io::Error::last_os_error().into());
                }
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
    pty.close()?;
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
                "aura perf: {:.0} frames/s {:.0} KiB/s PTY {:.2} ms/frame",
                self.frames as f64 / seconds,
                self.pty_bytes as f64 / 1024.0 / seconds,
                self.render_nanos as f64 / self.frames as f64 / 1_000_000.0,
            );
        }
        *self = Self::new();
    }
}

/// Parsed launch options; `None` means a flag like `--help` already ran.
type LaunchOptions = Option<()>;

#[allow(clippy::never_loop)]
fn arguments() -> Result<LaunchOptions, Box<dyn std::error::Error>> {
    let mut launch = Some(());
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--help" | "-h") => {
                launch = None;
                println!(
                    "aura {} - GPU-accelerated terminal emulator for Linux\n\n\
                     Usage: aura [OPTIONS]\n\n\
                     Options:\n  \
                     -h, --help       print this help and exit\n  \
                     -V, --version    print the version and exit\n  \
                     --doctor         report display, font, config, and Vulkan\n  \
                     diagnostics, then exit\n\n\
                     Font and terminal geometry are configured in:\n  \
                     ~/.config/aura/config.toml\n\n\
                     With no argument aura starts an interactive shell. When the\n  \
                     configuration file is absent the built-in defaults are used.",
                    env!("CARGO_PKG_VERSION")
                );
            }
            Some("--version" | "-V") => {
                launch = None;
                println!("aura {}", env!("CARGO_PKG_VERSION"));
            }
            Some("--doctor") => {
                launch = None;
                doctor();
            }
            Some(other) => return Err(format!("unknown argument: {other}").into()),
            None => return Err("arguments must be valid UTF-8".into()),
        }
    }
    Ok(launch)
}

/// Seeds the terminal with the modes the configuration chooses, before any
/// application has a chance to set its own. Applications that emit DECSCUSR
/// or DECPrivateModeSet always win, because they arrive later.
fn apply_configured_modes(terminal: &mut Terminal, config: &AppConfig) {
    let modes = terminal.modes();
    let style = match config.cursor.style {
        aura_config::CursorShape::Block => aura_core::CursorStyle::Block,
        aura_config::CursorShape::Underline => aura_core::CursorStyle::Underline,
        aura_config::CursorShape::Bar => aura_core::CursorStyle::Bar,
    };
    let next = aura_core::Modes {
        cursor_style: style,
        cursor_blink: config.cursor.blink,
        ..modes
    };
    terminal.set_modes(next);
}

/// Reports what AURA can see on this machine, without starting a window or a
/// shell. Every line is actionable: the point is to answer "is my Vulkan /
/// display / font setup usable?" without reading a crash backtrace.
///
/// Nothing here opens the PTY, spawns a child, or writes to the user's
/// clipboard, and no file contents are echoed beyond the configuration path.
fn doctor() {
    println!("AURA Terminal {}", env!("CARGO_PKG_VERSION"));
    println!();

    println!("Display");
    println!(
        "  DISPLAY              {}",
        std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".into())
    );
    println!(
        "  WAYLAND_DISPLAY      {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "<unset>".into())
    );
    println!(
        "  XDG_SESSION_TYPE     {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "<unset>".into())
    );
    println!(
        "  XDG_RUNTIME_DIR      {}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "<unset>".into())
    );
    println!();

    println!("Configuration");
    match AppConfig::config_path() {
        Some(path) => {
            let present = path.is_file();
            println!("  path                 {}", path.display());
            println!(
                "  status               {}",
                if present {
                    "loaded"
                } else {
                    "absent; defaults are in effect"
                }
            );
            if present {
                match AppConfig::load_from_path(Some(&path)) {
                    Ok(config) => {
                        println!("  parsed               yes");
                        println!(
                            "  font                 {} @ {}px",
                            config.font.family, config.font.size
                        );
                        println!(
                            "  window               {}x{} cells, padding {}x{}",
                            config.window.columns,
                            config.window.lines,
                            config.window.padding_x,
                            config.window.padding_y
                        );
                    }
                    Err(error) => println!("  parsed               NO: {error}"),
                }
            }
        }
        None => println!("  path                 <no config directory available>"),
    }
    println!();

    println!("Fonts");
    for family in [
        "monospace",
        "DejaVu Sans Mono",
        "Liberation Mono",
        "Noto Sans Mono",
    ] {
        match Font::load_with_family(family, 12.0) {
            Ok(font) => {
                println!(
                    "  {family:<21} OK  cell {}x{}, ascent {} descent {}",
                    font.metrics().width,
                    font.metrics().height,
                    font.metrics().ascent,
                    font.metrics().descent
                );
            }
            Err(error) => println!("  {family:<21} --  {error}"),
        }
    }
    println!();

    println!("Vulkan");
    // ash can load the loader and enumerate devices without a surface, so a
    // missing display is not a blocker here.
    match probe_vulkan() {
        Ok(devices) if devices.is_empty() => {
            println!("  loader               OK");
            println!("  devices              none found");
        }
        Ok(devices) => {
            println!("  loader               OK");
            for device in devices {
                println!(
                    "  device               {} (Vulkan {}.{}.{}, {})",
                    device.name,
                    device.api_version.0,
                    device.api_version.1,
                    device.api_version.2,
                    device.driver
                );
                println!(
                    "  dynamic rendering    {}",
                    yes_no(device.dynamic_rendering)
                );
                println!(
                    "  synchronization2      {}",
                    yes_no(device.synchronization2)
                );
            }
        }
        Err(error) => println!("  unavailable          {error}"),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

struct VulkanDevice {
    name: String,
    api_version: (u32, u32, u32),
    driver: String,
    dynamic_rendering: bool,
    synchronization2: bool,
}

/// Enumerates physical devices through ash. Returns an error only when the
/// Vulkan loader itself cannot be loaded or no instance can be created.
fn probe_vulkan() -> Result<Vec<VulkanDevice>, String> {
    let entry =
        unsafe { ash::Entry::load() }.map_err(|error| format!("cannot load loader: {error}"))?;
    let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
    let instance = match unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::default().application_info(&app),
            None,
        )
    } {
        Ok(instance) => instance,
        Err(vk::Result::ERROR_INCOMPATIBLE_DRIVER) => {
            return Err("no compatible Vulkan driver".into());
        }
        Err(error) => return Err(format!("instance creation failed: {error}")),
    };
    let mut devices = Vec::new();
    for physical in unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| format!("device enumeration failed: {error}"))?
    {
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        let name = std::ffi::CStr::from_bytes_until_nul(unsafe {
            std::slice::from_raw_parts(
                properties.device_name.as_ptr().cast(),
                properties.device_name.len(),
            )
        })
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "<unnamed>".to_string());
        let mut device_12_properties = vk::PhysicalDeviceVulkan12Properties::default();
        let mut properties12 =
            vk::PhysicalDeviceProperties2::default().push_next(&mut device_12_properties);
        unsafe { instance.get_physical_device_properties2(physical, &mut properties12) };
        let mut dynamic_rendering = vk::PhysicalDeviceDynamicRenderingFeatures::default();
        let mut synchronization2 = vk::PhysicalDeviceSynchronization2Features::default();
        let mut features = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut dynamic_rendering)
            .push_next(&mut synchronization2);
        unsafe { instance.get_physical_device_features2(physical, &mut features) };
        let api_version = (
            vk::api_version_major(properties.api_version),
            vk::api_version_minor(properties.api_version),
            vk::api_version_patch(properties.api_version),
        );
        devices.push(VulkanDevice {
            name,
            api_version,
            // The concrete driver name (radv, anv, nvidia, ...) lives in the
            // Vulkan 1.2 properties, which is what users recognise.
            driver: match device_12_properties.driver_id {
                vk::DriverId::MESA_RADV => "Mesa RADV".to_owned(),
                vk::DriverId::MESA_NVK => "Mesa NVK".to_owned(),
                vk::DriverId::INTEL_OPEN_SOURCE_MESA => "Mesa ANV".to_owned(),
                vk::DriverId::MESA_LLVMPIPE => "Mesa llvmpipe (software)".to_owned(),
                vk::DriverId::NVIDIA_PROPRIETARY => "NVIDIA proprietary".to_owned(),
                other => format!("{other:?}"),
            },
            dynamic_rendering: dynamic_rendering.dynamic_rendering == vk::TRUE,
            synchronization2: synchronization2.synchronization2 == vk::TRUE,
        });
    }
    Ok(devices)
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

/// ICCCM `WM_SIZE_HINTS` flag bits (section 4.1.2.3).
const P_SIZE: u32 = 1 << 1;
const P_MIN_SIZE: u32 = 1 << 4;
const P_RESIZE_INC: u32 = 1 << 6;
const P_BASE_SIZE: u32 = 1 << 8;

/// Publishes the size hints that make window resizing land on whole cells.
///
/// The `WM_NORMAL_HINTS` payload is the 18-word ICCCM structure. Setting
/// `base_width`/`base_height` to the padding and `width_inc`/`height_inc` to
/// one cell means a window manager can only ever present a size that holds a
/// whole number of cells, so a resize never clips a half row or column. The
/// minimum size is one cell plus padding, which keeps a squeezed window
/// usable instead of degenerate.
fn set_size_hints(
    connection: &XCBConnection,
    window: u32,
    geometry: &FontGeometry,
) -> Result<(), Box<dyn std::error::Error>> {
    let hints_atom = connection
        .intern_atom(false, b"WM_NORMAL_HINTS")?
        .reply()?
        .atom;
    let size_hints_type = connection
        .intern_atom(false, b"WM_SIZE_HINTS")?
        .reply()?
        .atom;
    let padding_x = geometry
        .pixel_width
        .saturating_sub(geometry.columns as u32 * geometry.cell_width)
        / 2;
    let padding_y = geometry
        .pixel_height
        .saturating_sub(geometry.rows as u32 * geometry.cell_height)
        / 2;
    let hints: [u32; 18] = [
        P_SIZE | P_MIN_SIZE | P_RESIZE_INC | P_BASE_SIZE,
        0,
        0, // x, y placeholders
        geometry.pixel_width,
        geometry.pixel_height,
        padding_x + geometry.cell_width,
        padding_y + geometry.cell_height, // min_width, min_height
        0,
        0, // max_width, max_height
        geometry.cell_width,
        geometry.cell_height, // width_inc, height_inc
        0,
        0,
        0,
        0, // min/max aspect ratios
        padding_x,
        padding_y, // base_width, base_height
        1,         // win_gravity: NorthWest
    ];
    connection.change_property32(
        PropMode::REPLACE,
        window,
        hints_atom,
        size_hints_type,
        &hints,
    )?;
    Ok(())
}

/// Publishes the window properties AURA needs for correct desktop
/// integration: `_NET_WM_PID` so launchers, taskbars, and tooling can map the
/// window back to this process, `WM_CLASS` so the `.desktop` entry matches,
/// and the cell-aligned size hints.
fn set_window_properties(
    connection: &XCBConnection,
    window: u32,
    geometry: &FontGeometry,
) -> Result<(), Box<dyn std::error::Error>> {
    let net_wm_pid = connection.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_pid,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )?;
    // ICCCM WM_CLASS is two NUL-terminated strings: res_name then res_class.
    connection.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"aura\0AURA Terminal\0",
    )?;
    set_size_hints(connection, window, geometry)?;
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
            property_atom: atom(b"AURA_CLIPBOARD")?,
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
        config.font.glyph_offsets(),
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
    // A new size re-rasterizes every glyph, so the renderer's glyph table, cell
    // cache, and both atlases are stale and must be dropped. This is the only
    // path that does so: `Renderer::resize` reconciles the swapchain alone and
    // must not invalidate font state for a plain window resize.
    renderer.invalidate_font();
    // Cell metrics changed, so the resize increments and minimum size move too.
    set_size_hints(connection, window, &geometry)?;
    renderer
        .resize(geometry.pixel_width, geometry.pixel_height)
        .map_err(io::Error::other)?;
    Ok(())
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
    modes: aura_core::Modes,
    event: MouseEvent,
    metrics: CellMetrics,
    padding: [u32; 2],
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
    if released && !modes.mouse_sgr {
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
    let (column, row) = cell_position(pixel_x, pixel_y, metrics, padding);
    let x = (column + 1).min(9999);
    let y = (row + 1).min(9999);
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


fn cell_position(x: i16, y: i16, metrics: CellMetrics, padding: [u32; 2]) -> (usize, usize) {
    // X11 coordinates are window-relative; the grid starts inside the padding.
    let x = (i64::from(x) - i64::from(padding[0])).max(0) as u64;
    let y = (i64::from(y) - i64::from(padding[1])).max(0) as u64;
    (
        (x / u64::from(metrics.width.max(1))) as usize,
        (y / u64::from(metrics.height.max(1))) as usize,
    )
}

fn selection_point(
    x: i16,
    y: i16,
    metrics: CellMetrics,
    terminal: &Terminal,
    padding: [u32; 2],
) -> SelectionPoint {
    let (column, row) = cell_position(x, y, metrics, padding);
    SelectionPoint {
        row: row.min(terminal.active_grid().height() - 1),
        column: column.min(terminal.active_grid().width() - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_reports_follow_tracking_and_sgr_modes() {
        let mut modes = aura_core::Modes::default();
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
        assert!(mouse_report(modes, press, metrics, [0; 2]).is_none());
        modes.mouse_tracking = MouseTracking::Drag;
        modes.mouse_sgr = true;
        assert_eq!(
            mouse_report(modes, press, metrics, [0; 2]).as_deref(),
            Some(&b"\x1b[<0;3;3M"[..])
        );
        assert_eq!(
            mouse_report(
                modes,
                MouseEvent {
                    motion: true,
                    ..press
                },
                metrics,
                [0; 2],
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
                metrics,
                [0; 2],
            )
            .as_deref(),
            Some(&b"\x1b[<0;3;3m"[..])
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
