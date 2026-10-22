#!/usr/bin/env python3
"""Check native cursor pixels and focus reports across keyboard grabs on dedicated X11."""
import ctypes as C
import ctypes.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from x11_daily_use import capture, pixel, run, wait_for
from x11_protocols import Fixture


def focus_recovery(root, binary):
    fixture = Fixture(root, binary, b'\x1b[?1004h', config='''[window]
columns=20
lines=8
padding_x=4
padding_y=6
[cursor]
blink=false
[colors]
background="#102030"
foreground="#ffffff"
cursor="#ffffff"
''', complete_config=True)
    x = C.CDLL(ctypes.util.find_library('X11'))
    signatures = {
        'XOpenDisplay': ([C.c_char_p], C.c_void_p),
        'XCloseDisplay': ([C.c_void_p], C.c_int),
        'XDefaultRootWindow': ([C.c_void_p], C.c_ulong),
        'XCreateSimpleWindow': ([C.c_void_p,C.c_ulong,C.c_int,C.c_int,C.c_uint,C.c_uint,C.c_uint,C.c_ulong,C.c_ulong], C.c_ulong),
        'XMapWindow': ([C.c_void_p,C.c_ulong], C.c_int),
        'XDestroyWindow': ([C.c_void_p,C.c_ulong], C.c_int),
        'XSetInputFocus': ([C.c_void_p,C.c_ulong,C.c_int,C.c_ulong], C.c_int),
        'XGrabKeyboard': ([C.c_void_p,C.c_ulong,C.c_int,C.c_int,C.c_int,C.c_ulong], C.c_int),
        'XUngrabKeyboard': ([C.c_void_p,C.c_ulong], C.c_int),
        'XSync': ([C.c_void_p,C.c_int], C.c_int),
    }
    for name,(args,result) in signatures.items():
        getattr(x,name).argtypes = args
        getattr(x,name).restype = result
    display=x.XOpenDisplay(None)
    if not display:
        fixture.close()
        raise RuntimeError('dedicated X11 unavailable')
    window=int(fixture.window)
    root_window=x.XDefaultRootWindow(display)
    try:
        def cursor(filled):
            image=capture(fixture.window,root/'cursor.xwd')
            cw,ch=(image[0]-8)//20,(image[1]-12)//8
            # The border remains white in both styles; the interior distinguishes
            # the solid focused block from the unfocused hollow block.
            return pixel(image,4+cw//2,6+ch//2)==((255,255,255) if filled else (16,32,48)) and pixel(image,4,6)==(255,255,255)
        def focus(target):
            x.XSetInputFocus(display,target,2,0)
            x.XSync(display,False)
        def grab():
            if x.XGrabKeyboard(display,root_window,False,1,1,0)!=0:
                raise RuntimeError('dedicated keyboard grab unavailable')
        def ungrab():
            x.XUngrabKeyboard(display,0)
            x.XSync(display,False)
        focus(window)
        wait_for(lambda: cursor(True),'initial block cursor is not filled')
        child=x.XCreateSimpleWindow(display,window,40,40,1,1,0,0,0)
        x.XMapWindow(display,child)
        x.XSync(display,False)
        before=len(fixture.data())
        try:
            focus(child)
            wait_for(lambda: cursor(True),'child focus was mistaken for window focus loss')
            focus(window)
            wait_for(lambda: cursor(True),'returning from child focus changed the block')
            assert fixture.data()[before:]==b'', 'child focus emitted application focus reports'
        finally:
            x.XDestroyWindow(display,child)
            x.XSync(display,False)
        before=len(fixture.data())
        grab()
        wait_for(lambda: cursor(True),'temporary keyboard grab changed logical focus')
        ungrab()
        wait_for(lambda: cursor(True),'temporary ungrab changed logical focus')
        assert fixture.data()[before:]==b'', 'temporary grab emitted application focus reports'
        focus(root_window)
        wait_for(lambda: cursor(False),'normal focus loss did not show hollow cursor')
        wait_for(lambda: b'\x1b[O' in fixture.data()[before:],'normal focus-out report missing')
        before=len(fixture.data())
        grab()
        focus(window)
        wait_for(lambda: cursor(True),'focus regained while grabbed left a hollow cursor')
        wait_for(lambda: b'\x1b[I' in fixture.data()[before:],'focus-in while grabbed missing')
        ungrab()
        before=len(fixture.data())
        run('xdotool','key','a')
        wait_for(lambda: b'a' in fixture.data()[before:],'raw input after focus recovery missing')
        wait_for(lambda: cursor(True),'typing after focus recovery left a hollow cursor')
        before=len(fixture.data())
        grab()
        focus(root_window)
        wait_for(lambda: cursor(False),'focus lost while grabbed kept a filled cursor')
        wait_for(lambda: b'\x1b[O' in fixture.data()[before:],'focus-out while grabbed missing')
        ungrab()
        focus(window)
        wait_for(lambda: cursor(True),'normal focus recovery did not fill cursor')
        print('PASS: solid/hollow cursor, real focus during grabs, temporary grabs and raw typing')
    finally:
        x.XUngrabKeyboard(display,0)
        x.XCloseDisplay(display)
        fixture.close()


def main():
    missing=[name for name in ['xdotool','xdpyinfo','xwd','magick'] if not shutil.which(name)]
    if missing or subprocess.run(['xdpyinfo'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode:
        if os.environ.get('AURA_REQUIRE_GPU')=='1':
            raise RuntimeError(f'required dedicated X11 prerequisites unavailable: {missing}')
        print(f'SKIP: dedicated X11 prerequisites unavailable: {missing}')
        return 77
    binary=str(Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/aura').resolve())
    with tempfile.TemporaryDirectory(prefix='aura-focus-') as folder:
        focus_recovery(Path(folder),binary)
    return 0


if __name__=='__main__':
    raise SystemExit(main())
