"""Small Xlib/XTest boundary; timestamps measure host observation, not scanout."""

import ctypes as C
import ctypes.util
from contextlib import contextmanager
import time


class Image(C.Structure):
    _fields_ = [
        ("width", C.c_int),
        ("height", C.c_int),
        ("xoffset", C.c_int),
        ("format", C.c_int),
        ("data", C.c_void_p),
        ("byte_order", C.c_int),
        ("bitmap_unit", C.c_int),
        ("bitmap_bit_order", C.c_int),
        ("bitmap_pad", C.c_int),
        ("depth", C.c_int),
        ("bytes_per_line", C.c_int),
        ("bits_per_pixel", C.c_int),
        ("red_mask", C.c_ulong),
        ("green_mask", C.c_ulong),
        ("blue_mask", C.c_ulong),
    ]


class FocusLostError(RuntimeError):
    pass


class X11:
    def __init__(self, display):
        self.x = C.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
        self.xt = C.CDLL(ctypes.util.find_library("Xtst") or "libXtst.so.6")
        signatures = {
            "XOpenDisplay": ([C.c_char_p], C.c_void_p),
            "XCloseDisplay": ([C.c_void_p], C.c_int),
            "XSync": ([C.c_void_p, C.c_int], C.c_int),
            "XFlush": ([C.c_void_p], C.c_int),
            "XSetInputFocus": ([C.c_void_p, C.c_ulong, C.c_int, C.c_ulong], C.c_int),
            "XGetInputFocus": ([C.c_void_p, C.POINTER(C.c_ulong), C.POINTER(C.c_int)], C.c_int),
            "XGrabServer": ([C.c_void_p], C.c_int),
            "XUngrabServer": ([C.c_void_p], C.c_int),
            "XStringToKeysym": ([C.c_char_p], C.c_ulong),
            "XKeysymToKeycode": ([C.c_void_p, C.c_ulong], C.c_ubyte),
            "XResizeWindow": ([C.c_void_p, C.c_ulong, C.c_uint, C.c_uint], C.c_int),
            "XGetGeometry": (
                [
                    C.c_void_p,
                    C.c_ulong,
                    C.POINTER(C.c_ulong),
                    C.POINTER(C.c_int),
                    C.POINTER(C.c_int),
                    C.POINTER(C.c_uint),
                    C.POINTER(C.c_uint),
                    C.POINTER(C.c_uint),
                    C.POINTER(C.c_uint),
                ],
                C.c_int,
            ),
            "XGetImage": (
                [
                    C.c_void_p,
                    C.c_ulong,
                    C.c_int,
                    C.c_int,
                    C.c_uint,
                    C.c_uint,
                    C.c_ulong,
                    C.c_int,
                ],
                C.POINTER(Image),
            ),
            "XGetPixel": ([C.POINTER(Image), C.c_int, C.c_int], C.c_ulong),
            "XDestroyImage": ([C.POINTER(Image)], C.c_int),
        }
        for name, (args, result) in signatures.items():
            function = getattr(self.x, name)
            function.argtypes, function.restype = args, result
        self.xt.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.xt.XTestFakeKeyEvent.restype = C.c_int
        self.display = self.x.XOpenDisplay(display.encode())
        if not self.display:
            raise RuntimeError(f"cannot open X11 display {display}")
        # The callback stays alive until the display closes; Xlib errors become
        # Python failures instead of aborting the entire multi-emulator campaign.
        self.error = False
        self.callback = C.CFUNCTYPE(C.c_int, C.c_void_p, C.c_void_p)(self._error)
        self.x.XSetErrorHandler.argtypes = [C.c_void_p]
        self.x.XSetErrorHandler.restype = C.c_void_p
        self.previous_handler = self.x.XSetErrorHandler(self.callback)
        self.target = None
        self.previous_focus = None
        self.keycode = self.x.XKeysymToKeycode(
            self.display, self.x.XStringToKeysym(b"a")
        )
        if not self.keycode:
            self.close()
            raise RuntimeError("X11 keymap has no a key")

    def _error(self, _display, _event):
        self.error = True
        return 0

    def sync(self):
        self.x.XSync(self.display, False)
        if self.error:
            self.error = False
            raise RuntimeError(
                "X11 request failed (window closed or unsupported operation)"
            )

    def focus(self, window):
        self.x.XSetInputFocus(self.display, window, 2, 0)
        self.sync()
        self.target = window

    def focused(self):
        window, revert = C.c_ulong(), C.c_int()
        self.x.XGetInputFocus(self.display, C.byref(window), C.byref(revert))
        self.sync()
        return window.value

    def begin_trial(self):
        # Capture before launch: a window manager may focus newly mapped clients.
        self.previous_focus, self.target = self.focused(), None

    @contextmanager
    def input_guard(self):
        # Keep focus verification and delivery atomic with respect to other X
        # clients. Never hold the server across sleeps, polling or subprocesses.
        self.x.XGrabServer(self.display)
        try:
            if self.target is None or self.focused() != self.target:
                raise FocusLostError('benchmark window lost focus; campaign input cancelled')
            yield
            self.sync()
        finally:
            self.x.XUngrabServer(self.display)
            self.x.XFlush(self.display)

    def restore_focus(self):
        try:
            if self.previous_focus is not None and self.target is not None:
                self.x.XGrabServer(self.display)
                try:
                    if self.focused() == self.target:
                        self.x.XSetInputFocus(self.display, self.previous_focus, 2, 0)
                        self.sync()
                finally:
                    self.x.XUngrabServer(self.display)
                    self.x.XFlush(self.display)
        except RuntimeError:
            # The previous window may have closed. Do not steal another focus.
            pass
        finally:
            self.target, self.previous_focus = None, None

    def key(self):
        with self.input_guard():
            # Guard round-trip is outside the input clock, on every emulator.
            started = time.monotonic_ns()
            try:
                self._event(self.keycode, True)
            finally:
                self._event(self.keycode, False)
        return started

    def _event(self, code, pressed):
        if not self.xt.XTestFakeKeyEvent(self.display, code, pressed, 0):
            raise RuntimeError('XTest key injection unavailable')

    def keys(self, *chords):
        for chord in chords:
            names = chord.replace('ctrl+', 'Control_L+').replace('shift+', 'Shift_L+').split('+')
            codes = [self.x.XKeysymToKeycode(self.display, self.x.XStringToKeysym(name.encode())) for name in names]
            if not all(codes):
                raise ValueError(f'X11 keymap cannot encode {chord}')
            with self.input_guard():
                pressed = []
                try:
                    for code in codes:
                        self._event(code, True)
                        pressed.append(code)
                finally:
                    for code in reversed(pressed):
                        self._event(code, False)

    def geometry(self, window):
        root, x, y = C.c_ulong(), C.c_int(), C.c_int()
        width, height, border, depth = (C.c_uint() for _ in range(4))
        result = self.x.XGetGeometry(
            self.display,
            window,
            C.byref(root),
            C.byref(x),
            C.byref(y),
            C.byref(width),
            C.byref(height),
            C.byref(border),
            C.byref(depth),
        )
        self.sync()
        if not result:
            raise RuntimeError("cannot read window geometry")
        return [width.value, height.value]

    def resize(self, window, width, height):
        started = time.monotonic_ns()
        self.x.XResizeWindow(self.display, window, width, height)
        self.x.XFlush(self.display)
        return started

    def pixel(self, window, x, y):
        image = self.x.XGetImage(self.display, window, x, y, 1, 1, C.c_ulong(-1), 2)
        try:
            self.sync()
            if not image:
                raise RuntimeError("X11 image unavailable")
            value = self.x.XGetPixel(image, 0, 0)
            channels = []
            for mask in (
                image.contents.red_mask,
                image.contents.green_mask,
                image.contents.blue_mask,
            ):
                if not mask:
                    raise RuntimeError("TrueColor X11 visual required")
                shift = (mask & -mask).bit_length() - 1
                channels.append(
                    round(((value & mask) >> shift) * 255 / (mask >> shift))
                )
            return channels
        finally:
            if image:
                self.x.XDestroyImage(image)

    def close(self):
        if self.display:
            self.x.XSync(self.display, False)
            self.x.XSetErrorHandler(self.previous_handler)
            self.x.XCloseDisplay(self.display)
            self.display = None
