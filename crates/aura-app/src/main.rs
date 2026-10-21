mod bindings;
mod config_reload;
mod enhanced_keys;
mod input;
mod keyboard;
mod mouse;
mod perf;
mod pointer;
mod signals;
mod sync;

use aura_config::BindingAction;
use keyboard::Keyboard;
use mouse::{MouseEvent, mouse_modes, mouse_report};
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
    Gravity, KeyButMask, PropMode, SelectionNotifyEvent, WindowClass,
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
    let Some(opts) = opts else {
        return;
    };
    if let Err(error) = run(opts) {
        eprintln!("aura: {error}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    // Validate both flags before opening X11 or spawning a child.
    let debug_keyboard = perf::flag("AURA_DEBUG_KEYBOARD")?;
    let mut stats = perf::Perf::new(perf::flag("AURA_PERF")?);
    let termination = signals::Termination::new()?;
    let config_path = options.config_path.clone().or_else(AppConfig::config_path);
    let mut config = if let Some(path) = options.config_path.as_deref() {
        AppConfig::load_with_sources(path)?.0
    } else {
        AppConfig::load_from_path(None)?
    };
    let mut key_bindings = bindings::Bindings::new(&config.keyboard)?;
    let mut config_watcher = config_reload::Watcher::new(config_path);
    if options.check_config {
        load_font(&config, config.font.size)?;
        println!("configuration OK");
        return Ok(());
    }
    if options.doctor {
        doctor(options.config_path.as_deref());
        return Ok(());
    }

    let mut font_sizes = RuntimeFontSize::new(config.font.size);
    let mut font = load_font(&config, font_sizes.current())?;
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
    let window_width = u16::try_from(pixel_width)
        .map_err(|_| io::Error::other("configured window width exceeds X11's 65535-pixel limit"))?;
    let window_height = u16::try_from(pixel_height).map_err(|_| {
        io::Error::other("configured window height exceeds X11's 65535-pixel limit")
    })?;
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
    let background = config.colors.palette().background;
    let background_pixel = connection
        .alloc_color(
            screen.default_colormap,
            ((background >> 16) & 0xff) as u16 * 257,
            ((background >> 8) & 0xff) as u16 * 257,
            (background & 0xff) as u16 * 257,
        )?
        .reply()?
        .pixel;
    connection.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        window_width,
        window_height,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            // Preserve the overlap until Vulkan presents the resized frame.
            // New strips must use the palette background, not flash black.
            .bit_gravity(Gravity::NORTH_WEST)
            .background_pixel(background_pixel)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::FOCUS_CHANGE
                    | EventMask::ENTER_WINDOW,
            ),
    )?;
    set_title(&connection, window, &config.window.title)?;
    set_window_properties(&connection, window, &geometry, &config.window.class)?;
    let mut clipboard = Clipboard::new(&connection, config.security.max_paste_bytes)?;
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
    // The child sees the window pixels from the start; later resizes keep
    // these lanes current so SIGWINCH observers never see them drop to zero.
    let mut last_winsize = PtySize::new(columns, rows)?;
    (last_winsize.pixel_width, last_winsize.pixel_height) = geometry.pty_pixels();
    let program = config
        .shell
        .program
        .as_deref()
        .map(aura_config::expand_home)
        .transpose()?;
    let directory = config
        .shell
        .working_directory
        .as_deref()
        .map(aura_config::expand_home)
        .transpose()?;
    let args: Vec<_> = config.shell.args.iter().map(String::as_str).collect();
    let environment: Vec<_> = config
        .env
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let mut pty = Pty::spawn_configured(
        program.as_deref(),
        &args,
        directory.as_deref(),
        &environment,
        last_winsize,
    )?;
    // XCB stays alive in this scope, so the raw pointer remains valid until Renderer is dropped.
    let raw_connection = connection.as_raw_xcb_connection().cast();
    let mut renderer = Renderer::new(raw_connection, window, pixel_width, pixel_height)
        .map_err(io::Error::other)?;
    renderer.set_glyph_offset(config.font.offset_x, config.font.offset_y);
    renderer.set_padding(config.window.padding_x, config.window.padding_y);
    renderer.set_cell_spacing(config.font.letter_spacing, config.font.line_spacing);
    renderer.set_palette(config.colors.palette());
    renderer.set_cursor_color(config.colors.cursor);
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
    set_periodic_timer(timer, config.cursor.blink_interval_ms)?;
    // Configuration polling is independent from the cursor's typing reset.
    // SAFETY: timerfd_create has no borrowed memory and returns a new descriptor.
    let reload_timer = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if reload_timer == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: the successful timerfd_create returned an owned descriptor.
    let _reload_timer_owner = unsafe { OwnedFd::from_raw_fd(reload_timer) };
    set_periodic_timer(reload_timer, 1000)?;
    let autoscroll_timer = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if autoscroll_timer == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: timerfd_create returned a new descriptor owned by this scope.
    let _autoscroll_owner = unsafe { OwnedFd::from_raw_fd(autoscroll_timer) };
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
    epoll_add(epoll, autoscroll_timer, 5, libc::EPOLLIN as u32)?;
    epoll_add(epoll, termination.fd(), 4, libc::EPOLLIN as u32)?;
    epoll_add(epoll, reload_timer, 6, libc::EPOLLIN as u32)?;
    let mut sync_gate = sync::SyncGate::new(std::time::Duration::from_millis(
        config.general.sync_timeout_ms,
    ));
    let mut reload_pending = false;
    let mut last_typing = std::time::Instant::now();
    let mut pointer_cursor = pointer::Pointer::new(&connection, window)?;
    let mut pointer_hidden = false;
    let mut input = input::Input::default();
    let mut watching_write = false;
    let mut keyboard = Keyboard::new(&connection, debug_keyboard)?;
    let mut running = true;
    let mut invalidated = false;
    let mut font_resize_pending = false;
    let mut cursor_blink_on = true;
    let mut selection_anchor = None;
    let mut selection_kind = SelectionKind::Cell;
    let mut autoscroll_dir: Option<i8> = None;
    let mut pointer = (0i16, 0i16);
    let mut last_click: Option<(u32, u8, SelectionPoint, u8)> = None;
    let mut pressed_keys = std::collections::HashMap::<u8, (u32, KeyButMask, bool)>::new();
    let mut last_mouse_report: Option<Vec<u8>> = None;
    let mut previous_title = terminal.title().to_owned();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 16];
    let mut buffer = [0u8; 16 * 1024];

    while running {
        let mut pending_resize = None;
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
                    sync_gate.timeout_ms(),
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
                            Event::ConfigureNotify(configure) if configure.window == window => {
                                // Reconcile only the latest geometry in this batch;
                                // intermediate drag events need no reflow or swapchain.
                                pending_resize = Some(configure);
                                invalidated = true;
                            }
                            Event::Expose(_) => invalidated = true,
                            Event::EnterNotify(_) => {
                                pointer_hidden = false;
                            }
                            Event::FocusIn(event)
                                if event.mode == x11rb::protocol::xproto::NotifyMode::NORMAL =>
                            {
                                if let Some(bytes) = terminal.focus_report(true) {
                                    input.push(bytes);
                                }
                                renderer.set_focused(true);
                                last_typing = std::time::Instant::now();
                                set_periodic_timer(timer, config.cursor.blink_interval_ms)?;
                                cursor_blink_on = true;
                                invalidated = true;
                            }
                            Event::FocusOut(event)
                                if event.mode == x11rb::protocol::xproto::NotifyMode::NORMAL =>
                            {
                                renderer.set_focused(false);
                                invalidated = true;
                                pressed_keys.clear();
                                if let Some(bytes) = terminal.focus_report(false) {
                                    input.push(bytes);
                                }
                            }
                            Event::KeyRelease(key) => {
                                let (sym, mut modifiers) = keyboard.key(&key);
                                let forwarded = pressed_keys
                                    .remove(&key.detail)
                                    .is_some_and(|(_, _, forwarded)| forwarded);
                                if let Some(mask) = enhanced_keys::modifier_mask(sym)
                                    && !pressed_keys.values().any(|(sym, _, _)| {
                                        enhanced_keys::modifier_mask(*sym) == Some(mask)
                                    })
                                {
                                    modifiers =
                                        KeyButMask::from(u16::from(modifiers) & !u16::from(mask));
                                }
                                if !forwarded {
                                    continue;
                                }
                                if let Some(bytes) = keyboard.bytes_event(
                                    sym,
                                    modifiers,
                                    terminal.modes(),
                                    3,
                                    key.detail,
                                ) {
                                    input.push(&bytes);
                                }
                            }
                            Event::KeyPress(key) => {
                                pointer_hidden = true;
                                last_typing = std::time::Instant::now();
                                set_periodic_timer(timer, config.cursor.blink_interval_ms)?;
                                let (sym, mut modifiers) = keyboard.key(&key);
                                if let Some(mask) = enhanced_keys::modifier_mask(sym) {
                                    modifiers |= mask;
                                }
                                let key_kind = if pressed_keys
                                    .insert(key.detail, (sym, modifiers, false))
                                    .is_some()
                                {
                                    2
                                } else {
                                    1
                                };
                                if let Some(action) = key_bindings.action(
                                    sym,
                                    modifiers,
                                    terminal.is_alternate_screen(),
                                ) {
                                    match action {
                                        BindingAction::IncreaseFontSize
                                        | BindingAction::DecreaseFontSize
                                        | BindingAction::ResetFontSize => {
                                            let changed = match action {
                                                BindingAction::IncreaseFontSize => {
                                                    font_sizes.grow()
                                                }
                                                BindingAction::DecreaseFontSize => {
                                                    font_sizes.shrink()
                                                }
                                                _ => font_sizes.reset(),
                                            };
                                            if changed {
                                                font_resize_pending = true;
                                                cursor_blink_on = true;
                                                invalidated = true;
                                            }
                                        }
                                        BindingAction::Copy => {
                                            if let Some(text) = terminal.selection_text() {
                                                clipboard.set_text(
                                                    &connection,
                                                    window,
                                                    clipboard.clipboard_atom,
                                                    text.as_bytes(),
                                                )?;
                                            }
                                        }
                                        BindingAction::Paste => clipboard.request_paste(
                                            &connection,
                                            window,
                                            clipboard.clipboard_atom,
                                        )?,
                                        BindingAction::PasteSelection => clipboard.request_paste(
                                            &connection,
                                            window,
                                            clipboard.primary_atom,
                                        )?,
                                        BindingAction::ScrollPageUp => {
                                            terminal.page_up();
                                            invalidated = true;
                                        }
                                        BindingAction::ScrollPageDown => {
                                            terminal.page_down();
                                            invalidated = true;
                                        }
                                        BindingAction::ScrollTop => {
                                            terminal.scrollback_top();
                                            invalidated = true;
                                        }
                                        BindingAction::ScrollBottom => {
                                            terminal.jump_to_bottom();
                                            invalidated = true;
                                        }
                                        BindingAction::ReloadConfig => reload_pending = true,
                                        BindingAction::Send(bytes) => input.push(bytes.as_bytes()),
                                        BindingAction::None => {}
                                    }
                                    continue;
                                }
                                if let Some(bytes) = keyboard.bytes_event(
                                    sym,
                                    modifiers,
                                    terminal.modes(),
                                    key_kind,
                                    key.detail,
                                ) {
                                    // Typing belongs to the application now; a
                                    // lingering highlight would confuse it with
                                    // fresh output, and the echo must be visible
                                    // instead of landing under a scrolled view.
                                    terminal.clear_selection();
                                    terminal.jump_to_bottom();
                                    input.push(&bytes);
                                    if let Some(entry) = pressed_keys.get_mut(&key.detail) {
                                        entry.2 = true;
                                    }
                                    cursor_blink_on = true;
                                    invalidated = true;
                                }
                            }
                            Event::ButtonPress(button) => {
                                pointer_hidden = false;
                                last_mouse_report = None;
                                if mouse_modes(&terminal, button.state).mouse_tracking
                                    == MouseTracking::Off
                                    && matches!(button.detail, 4 | 5)
                                {
                                    if button.detail == 4 {
                                        terminal.scroll_view_up(config.mouse.scroll_lines);
                                    } else {
                                        terminal.scroll_view_down(config.mouse.scroll_lines);
                                    }
                                    invalidated = true;
                                } else if mouse_modes(&terminal, button.state).mouse_tracking
                                    == MouseTracking::Off
                                    && button.detail == 2
                                {
                                    // X11 convention: middle click pastes PRIMARY.
                                    clipboard.request_paste(
                                        &connection,
                                        window,
                                        clipboard.primary_atom,
                                    )?;
                                } else if mouse_modes(&terminal, button.state).mouse_tracking
                                    == MouseTracking::Off
                                    && button.detail == 1
                                {
                                    let point = selection_point(
                                        button.event_x,
                                        button.event_y,
                                        configured_metrics(&font, &config),
                                        &terminal,
                                        [config.window.padding_x, config.window.padding_y],
                                    );
                                    let count = last_click
                                        .filter(|(time, detail, old, _)| {
                                            *detail == button.detail
                                                && button.time.wrapping_sub(*time) <= 400
                                                && old.line == point.line
                                                && old.column.abs_diff(point.column) <= 1
                                        })
                                        .map_or(1, |(_, _, _, count)| count % 3 + 1);
                                    last_click = Some((button.time, button.detail, point, count));
                                    selection_anchor = Some(point);
                                    selection_kind = match count {
                                        2 => SelectionKind::Word,
                                        3 => SelectionKind::Line,
                                        _ => SelectionKind::Cell,
                                    };
                                    match selection_kind {
                                        SelectionKind::Cell => terminal.set_selection(point, point),
                                        SelectionKind::Word => {
                                            terminal.set_selection_words(point, point)
                                        }
                                        SelectionKind::Line => {
                                            terminal.set_selection_lines(point, point)
                                        }
                                    }
                                    invalidated = true;
                                } else if let Some(bytes) = mouse_report(
                                    mouse_modes(&terminal, button.state),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: false,
                                        motion: false,
                                    },
                                    configured_metrics(&font, &config),
                                    [config.window.padding_x, config.window.padding_y],
                                ) {
                                    input.push(&bytes);
                                }
                            }
                            Event::ButtonRelease(button) => {
                                last_mouse_report = None;
                                // Release always ends the local drag, even if the
                                // application enabled mouse tracking mid-gesture.
                                selection_anchor = None;
                                if autoscroll_dir.take().is_some() {
                                    set_autoscroll_timer(autoscroll_timer, false)?;
                                }
                                if mouse_modes(&terminal, button.state).mouse_tracking
                                    == MouseTracking::Off
                                {
                                    // Own the PRIMARY selection like any X11
                                    // client, so other applications can paste
                                    // the highlighted text with a middle click.
                                    if let Some(text) = terminal.selection_text() {
                                        clipboard.set_text(
                                            &connection,
                                            window,
                                            clipboard.primary_atom,
                                            text.as_bytes(),
                                        )?;
                                    }
                                } else if let Some(bytes) = mouse_report(
                                    mouse_modes(&terminal, button.state),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: true,
                                        motion: false,
                                    },
                                    configured_metrics(&font, &config),
                                    [config.window.padding_x, config.window.padding_y],
                                ) {
                                    input.push(&bytes);
                                }
                            }
                            Event::MotionNotify(motion) => {
                                pointer_hidden = false;
                                if matches!(
                                    mouse_modes(&terminal, motion.state).mouse_tracking,
                                    MouseTracking::Drag | MouseTracking::Motion
                                ) && let button = held_button(motion.state).unwrap_or(0)
                                    && let Some(bytes) = mouse_report(
                                        mouse_modes(&terminal, motion.state),
                                        MouseEvent {
                                            button,
                                            x: motion.event_x,
                                            y: motion.event_y,
                                            state: motion.state,
                                            released: false,
                                            motion: true,
                                        },
                                        configured_metrics(&font, &config),
                                        [config.window.padding_x, config.window.padding_y],
                                    )
                                {
                                    if last_mouse_report.as_ref() != Some(&bytes) {
                                        input.push(&bytes);
                                        last_mouse_report = Some(bytes);
                                    }
                                } else if mouse_modes(&terminal, motion.state).mouse_tracking
                                    == MouseTracking::Off
                                    && held_button(motion.state) == Some(1)
                                    && terminal.selection().is_some()
                                    && let Some(anchor) = selection_anchor
                                {
                                    // Dragging past the top or bottom edge scrolls
                                    // the view. The repeating tick keeps scrolling
                                    // while the pointer rests outside, so a still
                                    // hand never stalls a long selection.
                                    let padding_y = config.window.padding_y as i32;
                                    let bottom = i32::from(last_winsize.pixel_height) - padding_y;
                                    let y = i32::from(motion.event_y);
                                    let direction = if y < padding_y {
                                        Some(-1)
                                    } else if y >= bottom {
                                        Some(1)
                                    } else {
                                        None
                                    };
                                    if direction != autoscroll_dir {
                                        set_autoscroll_timer(
                                            autoscroll_timer,
                                            direction.is_some(),
                                        )?;
                                        autoscroll_dir = direction;
                                    }
                                    if direction == Some(-1) {
                                        terminal.scroll_view_up(1);
                                    } else if direction == Some(1) {
                                        terminal.scroll_view_down(1);
                                    }
                                    pointer = (motion.event_x, motion.event_y);
                                    let point = selection_point(
                                        pointer.0,
                                        pointer.1,
                                        configured_metrics(&font, &config),
                                        &terminal,
                                        [config.window.padding_x, config.window.padding_y],
                                    );
                                    match selection_kind {
                                        SelectionKind::Cell => {
                                            terminal.set_selection(anchor, point)
                                        }
                                        SelectionKind::Word => {
                                            terminal.set_selection_words(anchor, point)
                                        }
                                        SelectionKind::Line => {
                                            terminal.set_selection_lines(anchor, point)
                                        }
                                    }
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
                                    // A paste is input: show where it lands.
                                    terminal.jump_to_bottom();
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
                                last_mouse_report = None;
                                let started = stats.start();
                                update_host_properties(&mut terminal, &config, &font, last_winsize);
                                let replies = terminal.process(&buffer[..length]);
                                stats.finish("parse", started);
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
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
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
                    let old = cursor_blink_on;
                    cursor_blink_on = if config.cursor.blink_timeout_ms != 0
                        && last_typing.elapsed().as_millis()
                            >= u128::from(config.cursor.blink_timeout_ms)
                    {
                        true
                    } else {
                        !cursor_blink_on
                    };
                    // A hidden or steady cursor needs no blink repaints.
                    if old != cursor_blink_on
                        && terminal.modes().cursor_visible
                        && terminal.modes().cursor_blink
                    {
                        invalidated = true;
                    }
                }
                6 => {
                    let mut count = 0u64;
                    // SAFETY: count is writable and reload_timer remains owned.
                    let _ = unsafe {
                        libc::read(
                            reload_timer,
                            (&mut count as *mut u64).cast(),
                            size_of::<u64>(),
                        )
                    };
                    if config.general.live_config_reload && config_watcher.changed() {
                        reload_pending = true;
                    }
                }
                4 => running = false,
                5 => {
                    let mut expirations = 0u64;
                    let _ = unsafe {
                        libc::read(
                            autoscroll_timer,
                            (&mut expirations as *mut u64).cast(),
                            size_of::<u64>(),
                        )
                    };
                    if let (Some(direction), Some(anchor)) = (autoscroll_dir, selection_anchor)
                        && terminal.modes().mouse_tracking == MouseTracking::Off
                        && terminal.selection().is_some()
                    {
                        let metrics = configured_metrics(&font, &config);
                        let padding_y = config.window.padding_y as i32;
                        let bottom = i32::from(last_winsize.pixel_height) - padding_y;
                        // The further the pointer sits outside, the faster the
                        // tick drags the view along, like Alacritty's velocity.
                        let distance = if direction < 0 {
                            padding_y - i32::from(pointer.1)
                        } else {
                            i32::from(pointer.1) - bottom
                        };
                        let cell = i32::try_from(metrics.height.max(1)).unwrap_or(i32::MAX);
                        let extra = (distance / cell).clamp(0, 3);
                        let lines = 1 + extra as usize;
                        if direction < 0 {
                            terminal.scroll_view_up(lines);
                        } else {
                            terminal.scroll_view_down(lines);
                        }
                        let point = selection_point(
                            pointer.0,
                            pointer.1,
                            metrics,
                            &terminal,
                            [config.window.padding_x, config.window.padding_y],
                        );
                        match selection_kind {
                            SelectionKind::Cell => terminal.set_selection(anchor, point),
                            SelectionKind::Word => terminal.set_selection_words(anchor, point),
                            SelectionKind::Line => terminal.set_selection_lines(anchor, point),
                        }
                        invalidated = true;
                    } else {
                        // A screen switch, reset, resize or application-owned
                        // mouse mode invalidates the old buffer coordinates.
                        set_autoscroll_timer(autoscroll_timer, false)?;
                        autoscroll_dir = None;
                        selection_anchor = None;
                    }
                }
                _ => {}
            }
        }
        if reload_pending {
            reload_pending = false;
            let candidate = (|| -> Result<_, Box<dyn std::error::Error>> {
                let path = config_watcher
                    .path
                    .as_deref()
                    .ok_or_else(|| io::Error::other("no configuration path"))?;
                let (next, sources) = AppConfig::load_with_sources(path)?;
                let bindings = bindings::Bindings::new(&next.keyboard)?;
                let size = if next.font.size != config.font.size {
                    next.font.size
                } else {
                    font_sizes.current()
                };
                let next_font = if next.font != config.font {
                    Some(load_font(&next, size)?)
                } else {
                    None
                };
                Ok((next, sources, bindings, next_font))
            })();
            match candidate {
                Ok((next, sources, bindings, next_font)) => {
                    if next.font.size != config.font.size {
                        font_sizes = RuntimeFontSize::new(next.font.size);
                    }
                    if let Some(next_font) = next_font {
                        font = next_font;
                        renderer.invalidate_font();
                    }
                    if next.cursor.style != config.cursor.style
                        || next.cursor.blink != config.cursor.blink
                    {
                        apply_configured_modes(&mut terminal, &next);
                    }
                    terminal.set_scrollback_limit(next.scrollback.lines);
                    renderer.set_palette(next.colors.palette());
                    renderer.set_cursor_color(next.colors.cursor);
                    renderer.set_glyph_offset(next.font.offset_x, next.font.offset_y);
                    renderer.set_padding(next.window.padding_x, next.window.padding_y);
                    renderer.set_cell_spacing(next.font.letter_spacing, next.font.line_spacing);
                    let background = next.colors.palette().background;
                    let pixel = connection
                        .alloc_color(
                            screen.default_colormap,
                            ((background >> 16) & 255) as u16 * 257,
                            ((background >> 8) & 255) as u16 * 257,
                            (background & 255) as u16 * 257,
                        )?
                        .reply()?
                        .pixel;
                    connection.change_window_attributes(
                        window,
                        &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                            .background_pixel(pixel),
                    )?;
                    clipboard.max_paste_bytes = next.security.max_paste_bytes;
                    set_periodic_timer(timer, next.cursor.blink_interval_ms)?;
                    sync_gate = sync::SyncGate::new(std::time::Duration::from_millis(
                        next.general.sync_timeout_ms,
                    ));
                    key_bindings = bindings;
                    config = next;
                    config_watcher.loaded(sources);
                    font_resize_pending = true;
                    invalidated = true;
                    if !config.window.dynamic_title || terminal.title().is_empty() {
                        set_title(&connection, window, &config.window.title)?;
                    }
                    set_class(&connection, window, &config.window.class)?;
                }
                Err(error) => eprintln!("aura: configuration reload rejected: {error}"),
            }
        }
        if running && font_resize_pending {
            let started = stats.start();
            resize_font(
                &config,
                &font_sizes,
                &mut font,
                &mut terminal,
                &mut pty,
                &mut last_winsize,
                &connection,
                window,
                &mut renderer,
                &mut stats,
                pending_resize
                    .take()
                    .map(|event| (event.width, event.height)),
            )?;
            stats.finish("font_resize", started);
            font_resize_pending = false;
        }
        if running && let Some(event) = pending_resize {
            let started = stats.start();
            let metrics = configured_metrics(&font, &config);
            resize(
                &mut terminal,
                &mut pty,
                &mut last_winsize,
                &mut renderer,
                (metrics.width, metrics.height),
                (config.window.padding_x, config.window.padding_y),
                event,
                &mut stats,
            )?;
            stats.finish("window_resize", started);
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
        if config.window.dynamic_title && terminal.title() != previous_title {
            previous_title = terminal.title().to_owned();
            set_title(&connection, window, &previous_title)?;
            invalidated = true;
        }
        pointer_cursor.update(
            &connection,
            window,
            pointer_hidden && config.mouse.hide_when_typing,
            terminal.modes().mouse_tracking != MouseTracking::Off,
        )?;
        let synchronized = if terminal.modes().synchronized_output || sync_gate.active() {
            sync_gate.update(
                terminal.modes().synchronized_output,
                std::time::Instant::now(),
            )
        } else {
            false
        };
        if !synchronized && terminal.modes().synchronized_output {
            terminal.end_synchronized_output();
        }
        if invalidated && !synchronized {
            let started = stats.start();
            let timings = renderer
                .render_with_timings(&mut terminal, &mut font, cursor_blink_on, stats.enabled())
                .map_err(io::Error::other)?;
            stats.finish("frame_total", started);
            stats.renderer(timings);
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

#[derive(Default)]
struct Options {
    config_path: Option<std::path::PathBuf>,
    check_config: bool,
    doctor: bool,
}
type LaunchOptions = Option<Options>;

fn arguments() -> Result<LaunchOptions, Box<dyn std::error::Error>> {
    let mut options = Options::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config-file") => {
                options.config_path =
                    Some(args.next().ok_or("--config-file requires a path")?.into())
            }
            Some("--check-config") => options.check_config = true,
            Some("--doctor") => options.doctor = true,
            Some("--print-config") => {
                print!("{}", include_str!("../../../example/config.toml"));
                return Ok(None);
            }
            Some("--version" | "-V") => {
                println!("aura {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Some("--help" | "-h") => {
                println!(
                    "aura {}\nUsage: aura [--config-file PATH] [--check-config | --doctor]\n  --print-config  print the default TOML configuration\n  --version       print the version\n  --help          print this help",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(None);
            }
            _ => return Err(format!("unknown argument: {}", arg.to_string_lossy()).into()),
        }
    }
    Ok(Some(options))
}

fn load_font(config: &AppConfig, size: f32) -> Result<Font, Box<dyn std::error::Error>> {
    let mut font = Font::load_with_family(&config.font.family, size).map_err(io::Error::other)?;
    font.set_fallback_families(&config.font.fallback_families);
    for (bold, italic, face) in [
        (true, false, &config.font.bold),
        (false, true, &config.font.italic),
        (true, true, &config.font.bold_italic),
    ] {
        if let Some(face) = face {
            if face.family.is_none() && face.style.is_none() {
                continue;
            }
            font.set_style_face(bold, italic, face.family.as_deref(), face.style.as_deref())
                .map_err(io::Error::other)?;
        }
    }
    Ok(font)
}

fn set_periodic_timer(timer: i32, millis: u64) -> io::Result<()> {
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: (millis / 1000) as _,
            tv_nsec: ((millis % 1000) * 1_000_000) as _,
        },
        it_value: libc::timespec {
            tv_sec: (millis / 1000) as _,
            tv_nsec: ((millis % 1000) * 1_000_000) as _,
        },
    };
    // SAFETY: spec is initialized and timer remains owned by the caller.
    if unsafe { libc::timerfd_settime(timer, 0, &spec, std::ptr::null_mut()) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
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
fn doctor(config_path: Option<&std::path::Path>) {
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
    match config_path
        .map(std::path::Path::to_path_buf)
        .or_else(AppConfig::config_path)
    {
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

/// Starts or stops the repeating tick that keeps an edge drag scrolling while
/// the pointer rests outside the viewport. Motion events alone stop arriving
/// as soon as the hand stands still or the pointer reaches a screen edge.
fn set_autoscroll_timer(fd: i32, enabled: bool) -> io::Result<()> {
    let nanos = if enabled { 16_000_000 } else { 0 };
    let spec = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: nanos,
        },
        it_value: libc::timespec {
            tv_sec: 0,
            tv_nsec: nanos,
        },
    };
    if unsafe { libc::timerfd_settime(fd, 0, &spec, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
        .saturating_sub(geometry.columns as u32 * geometry.cell_width);
    let padding_y = geometry
        .pixel_height
        .saturating_sub(geometry.rows as u32 * geometry.cell_height);
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
    class: &str,
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
        format!("aura\0{class}\0").as_bytes(),
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
    max_paste_bytes: usize,
    clipboard_atom: u32,
    primary_atom: u32,
    utf8_atom: u32,
    targets_atom: u32,
    property_atom: u32,
    clipboard_contents: Vec<u8>,
    primary_contents: Vec<u8>,
}

impl Clipboard {
    fn new(
        connection: &XCBConnection,
        max_paste_bytes: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let atom = |name: &[u8]| -> Result<u32, Box<dyn std::error::Error>> {
            Ok(connection.intern_atom(false, name)?.reply()?.atom)
        };
        Ok(Self {
            max_paste_bytes,
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
        selection: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        connection
            .convert_selection(
                window,
                selection,
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
                self.max_paste_bytes.div_ceil(4) as u32,
            )?
            .reply()?;
        // X11 leaves a property intact after a partial read. Discard it even
        // when the paste exceeds the configured limit, never forward a prefix.
        connection.delete_property(window, property)?;
        if reply.format != 8
            || reply.value.len() > self.max_paste_bytes
            || reply.bytes_after != 0
            || (reply.type_ != self.utf8_atom && reply.type_ != AtomEnum::STRING.into())
        {
            return Ok(None);
        }
        Ok(Some(reply.value))
    }
}

#[allow(clippy::too_many_arguments)]
fn resize(
    terminal: &mut Terminal,
    pty: &mut Pty,
    last_winsize: &mut PtySize,
    renderer: &mut Renderer,
    metrics: (u32, u32),
    padding: (u32, u32),
    event: ConfigureNotifyEvent,
    stats: &mut perf::Perf,
) -> Result<(), Box<dyn std::error::Error>> {
    let (cell_width, cell_height) = metrics;
    let (padding_x, padding_y) = padding;
    let inner_width = u32::from(event.width).saturating_sub(2 * padding_x).max(1);
    let inner_height = u32::from(event.height).saturating_sub(2 * padding_y).max(1);
    let (width, height) = grid_dimensions(inner_width, inner_height, cell_width, cell_height);
    // The window pixels are the PTY pixel lanes; they change on every
    // ConfigureNotify, even when the cell count does not.
    let mut winsize = PtySize::new(width, height)?;
    winsize.pixel_width = event.width;
    winsize.pixel_height = event.height;
    if terminal.active_grid().width() != width || terminal.active_grid().height() != height {
        terminal
            .resize(width, height)
            .map_err(|error| io::Error::other(format!("terminal resize: {error:?}")))?;
    }
    // One ioctl per real change: repeated ConfigureNotify events for the
    // same geometry must not resend SIGWINCH to the child.
    if *last_winsize != winsize {
        pty.resize(winsize)?;
        *last_winsize = winsize;
    }
    let surface_rebuild = renderer
        .resize_with_timings(
            u32::from(event.width).max(1),
            u32::from(event.height).max(1),
            stats.enabled(),
        )
        .map_err(io::Error::other)?;
    stats.surface_rebuild(surface_rebuild);
    Ok(())
}

/// Re-rasterizes the font while preserving the actual outer window extent.
/// Grid, PTY and size hints then use the new cell metrics; the window manager
/// remains in charge of the window's size and placement.
#[allow(clippy::too_many_arguments)]
fn resize_font(
    config: &AppConfig,
    font_sizes: &RuntimeFontSize,
    font: &mut Font,
    terminal: &mut Terminal,
    pty: &mut Pty,
    last_winsize: &mut PtySize,
    connection: &XCBConnection,
    window: u32,
    renderer: &mut Renderer,
    stats: &mut perf::Perf,
    extent: Option<(u16, u16)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (pixel_width, pixel_height) =
        extent.unwrap_or((last_winsize.pixel_width, last_winsize.pixel_height));
    // New rasterization + new metrics + cleared glyph cache/atlas shelves.
    let font_changed = font
        .set_size(font_sizes.current())
        .map_err(io::Error::other)?;
    let metrics = configured_metrics(font, config);
    let (columns, rows) = grid_dimensions(
        u32::from(pixel_width).saturating_sub(2 * config.window.padding_x),
        u32::from(pixel_height).saturating_sub(2 * config.window.padding_y),
        metrics.width,
        metrics.height,
    );
    let geometry = FontGeometry::new(
        font.metrics().width,
        font.metrics().height,
        columns,
        rows,
        config.window.padding_x,
        config.window.padding_y,
        config.font.glyph_offsets(),
    );
    let started = stats.start();
    terminal
        .resize(columns, rows)
        .map_err(|error| io::Error::other(format!("terminal resize: {error:?}")))?;
    stats.finish("zoom_reflow", started);
    let mut winsize = PtySize::new(columns, rows)?;
    winsize.pixel_width = pixel_width;
    winsize.pixel_height = pixel_height;
    if *last_winsize != winsize {
        pty.resize(winsize)?;
        *last_winsize = winsize;
    }
    // A new size re-rasterizes every glyph, so the renderer's glyph table, cell
    // cache, and both atlases are stale and must be dropped. This is the only
    // path that does so: `Renderer::resize` reconciles the swapchain alone and
    // must not invalidate font state for a plain window resize.
    if font_changed {
        renderer.invalidate_font();
    }
    // Cell metrics changed, so the resize increments and minimum size move too.
    set_size_hints(connection, window, &geometry)?;
    let surface_rebuild = renderer
        .resize_with_timings(
            u32::from(last_winsize.pixel_width),
            u32::from(last_winsize.pixel_height),
            stats.enabled(),
        )
        .map_err(io::Error::other)?;
    stats.surface_rebuild(surface_rebuild);
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

fn update_host_properties(terminal: &mut Terminal, config: &AppConfig, font: &Font, size: PtySize) {
    let palette = config.colors.palette();
    let metrics = configured_metrics(font, config);
    terminal.set_host_properties(aura_core::HostProperties {
        foreground: palette.foreground,
        background: palette.background,
        cursor: config.colors.cursor_color(),
        ansi: palette.ansi,
        cell_pixels: [
            metrics.width.min(u16::MAX as u32) as u16,
            metrics.height.min(u16::MAX as u32) as u16,
        ],
        window_pixels: [size.pixel_width, size.pixel_height],
    });
}

fn set_class(
    connection: &XCBConnection,
    window: u32,
    class: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    connection.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        format!("aura\0{class}\0").as_bytes(),
    )?;
    Ok(())
}

fn configured_metrics(font: &Font, config: &AppConfig) -> CellMetrics {
    let mut metrics = font.metrics();
    metrics.width = metrics
        .width
        .saturating_add_signed(config.font.letter_spacing)
        .max(1);
    metrics.height = metrics
        .height
        .saturating_add_signed(config.font.line_spacing)
        .max(1);
    metrics
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

/// What a click-drag is selecting. A double click widens both endpoints to
/// whole words, a triple click to whole lines.
#[derive(Clone, Copy)]
enum SelectionKind {
    Cell,
    Word,
    Line,
}

fn selection_point(
    x: i16,
    y: i16,
    metrics: CellMetrics,
    terminal: &Terminal,
    padding: [u32; 2],
) -> SelectionPoint {
    let (column, row) = cell_position(x, y, metrics, padding);
    let row = row.min(terminal.active_grid().height() - 1);
    let column = column.min(terminal.active_grid().width() - 1);
    // Anchors are absolute buffer lines, not viewport rows: they must keep
    // naming the same content while the viewport scrolls under the drag.
    let line = terminal
        .viewport_line(row)
        .unwrap_or_else(|| terminal.active_grid().first_line());
    SelectionPoint { line, column }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_pastes_obey_the_configured_limit_without_partial_utf8() {
        let Ok((connection, screen_index)) = XCBConnection::connect(None) else {
            eprintln!("SKIP: configured paste integration needs X11");
            return;
        };
        let screen = &connection.setup().roots[screen_index];
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::default(),
            )
            .unwrap()
            .check()
            .unwrap();
        for limit in [4, 80_000] {
            let table = toml::from_str(&format!("[security]\nmax_paste_bytes = {limit}")).unwrap();
            let config = AppConfig::load_from_text(&table).unwrap();
            let clipboard = Clipboard::new(&connection, config.security.max_paste_bytes).unwrap();
            for length in [0, limit, limit + 1, limit + 100] {
                let bytes = vec![b'a'; length];
                connection
                    .change_property8(
                        PropMode::REPLACE,
                        window,
                        clipboard.property_atom,
                        clipboard.utf8_atom,
                        &bytes,
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                let result = clipboard
                    .receive_paste(&connection, window, clipboard.property_atom)
                    .unwrap();
                assert_eq!(result, (length <= limit).then_some(bytes));
                assert_eq!(
                    connection
                        .get_property(false, window, clipboard.property_atom, AtomEnum::ANY, 0, 1)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .type_,
                    u32::from(AtomEnum::NONE)
                );
            }
            let bytes = "é中".as_bytes();
            connection
                .change_property8(
                    PropMode::REPLACE,
                    window,
                    clipboard.property_atom,
                    clipboard.utf8_atom,
                    bytes,
                )
                .unwrap()
                .check()
                .unwrap();
            assert_eq!(
                clipboard
                    .receive_paste(&connection, window, clipboard.property_atom)
                    .unwrap(),
                (bytes.len() <= limit).then(|| bytes.to_vec())
            );
        }
        connection.destroy_window(window).unwrap().check().unwrap();
    }

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
