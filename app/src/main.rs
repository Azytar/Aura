use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use as_raw_xcb_connection::AsRawXcbConnection;
use aterm_core::{MouseTracking, Pty, PtySize, Terminal};
use aterm_font::{CellMetrics, Font};
use aterm_render::Renderer;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureNotifyEvent, ConnectionExt as XprotoExt, CreateWindowAux, EventMask,
    KeyButMask, PropMode, WindowClass,
};
use x11rb::wrapper::ConnectionExt as WrapperExt;
use x11rb::xcb_ffi::XCBConnection;

fn main() {
    if let Err(error) = run() {
        eprintln!("aterm: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (font_path, font_size) = arguments()?;
    let mut font = Font::load(font_path.as_deref(), font_size).map_err(io::Error::other)?;
    let metrics = font.metrics();
    let (connection, screen_index) = XCBConnection::connect(None)?;
    let screen = &connection.setup().roots[screen_index];
    let window = connection.generate_id()?;
    let pixel_width = metrics.width * 100;
    let pixel_height = metrics.height * 32;
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
    connection.change_property32(
        PropMode::REPLACE,
        window,
        protocols,
        AtomEnum::ATOM,
        &[delete_window],
    )?;
    connection.map_window(window)?;
    connection.flush()?;

    let (columns, rows) = grid_dimensions(pixel_width, pixel_height, metrics.width, metrics.height);
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
    let mapping = connection
        .get_keyboard_mapping(
            connection.setup().min_keycode,
            connection.setup().max_keycode - connection.setup().min_keycode + 1,
        )?
        .reply()?;
    let keysyms_per_keycode = usize::from(mapping.keysyms_per_keycode);
    let mut running = true;
    let mut invalidated = false;
    let mut cursor_blink_on = true;
    let mut previous_title = terminal.title().to_owned();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 16];
    let mut buffer = [0u8; 16 * 1024];

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
                            Event::ConfigureNotify(configure) => {
                                resize(
                                    &mut terminal,
                                    &mut pty,
                                    &mut renderer,
                                    metrics.width,
                                    metrics.height,
                                    configure,
                                )?;
                                invalidated = true;
                            }
                            Event::Expose(_) => invalidated = true,
                            Event::KeyPress(key) => {
                                if let Some(bytes) = translate_key(
                                    &key,
                                    mapping.keysyms.as_slice(),
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
                                if let Some(bytes) = mouse_report(
                                    terminal.modes(),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: false,
                                        motion: false,
                                    },
                                    metrics,
                                ) {
                                    write_all_pty(&mut pty, &bytes)?;
                                }
                            }
                            Event::ButtonRelease(button) => {
                                if let Some(bytes) = mouse_report(
                                    terminal.modes(),
                                    MouseEvent {
                                        button: button.detail,
                                        x: button.event_x,
                                        y: button.event_y,
                                        state: button.state,
                                        released: true,
                                        motion: false,
                                    },
                                    metrics,
                                ) {
                                    write_all_pty(&mut pty, &bytes)?;
                                }
                            }
                            Event::MotionNotify(motion)
                                if terminal.modes().mouse_tracking == MouseTracking::Drag =>
                            {
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
                                        metrics,
                                    ) {
                                        write_all_pty(&mut pty, &bytes)?;
                                    }
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
                            let replies = terminal.process(&buffer[..length]);
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
                    invalidated = true;
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
            renderer
                .render(&mut terminal, &mut font, cursor_blink_on)
                .map_err(io::Error::other)?;
            invalidated = false;
        }
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

fn arguments() -> Result<(Option<PathBuf>, f32), Box<dyn std::error::Error>> {
    let mut path = None;
    let mut size = 14.0;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--font") => {
                path = Some(PathBuf::from(args.next().ok_or("--font requires a path")?))
            }
            Some("--font-size") => {
                size = args
                    .next()
                    .ok_or("--font-size requires a value")?
                    .to_string_lossy()
                    .parse()?
            }
            Some(other) => return Err(format!("unknown argument: {other}").into()),
            None => return Err("arguments must be valid UTF-8".into()),
        }
    }
    Ok((path, size))
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

fn resize(
    terminal: &mut Terminal,
    pty: &mut Pty,
    renderer: &mut Renderer,
    cell_width: u32,
    cell_height: u32,
    event: ConfigureNotifyEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = grid_dimensions(
        u32::from(event.width),
        u32::from(event.height),
        cell_width,
        cell_height,
    );
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
    let offset = usize::from(event.detail.checked_sub(min_keycode)?) * per_key;
    let keysyms = symbols.get(offset..offset + per_key)?;
    let shifted = event.state.contains(KeyButMask::SHIFT) && keysyms.len() > 1;
    let sym = keysyms[usize::from(shifted)];
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

fn grid_dimensions(width: u32, height: u32, cell_width: u32, cell_height: u32) -> (usize, usize) {
    (
        (width / cell_width.max(1)).max(1) as usize,
        (height / cell_height.max(1)).max(1) as usize,
    )
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
    fn grid_size_uses_complete_cells_and_never_zero() {
        assert_eq!(grid_dimensions(100, 64, 9, 16), (11, 4));
        assert_eq!(grid_dimensions(2, 3, 9, 16), (1, 1));
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
}
