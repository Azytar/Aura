#!/usr/bin/env python3
"""Verify GTK/XSettings pointer sources and live updates on a dedicated X11 server."""
import ctypes as C
import ctypes.util
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
from unittest.mock import patch

from x11_daily_use import run, wait_for
from x11_pointer_selection import cursor_image, write_cursor
from x11_protocols import Fixture


class ClientData(C.Union):
    _fields_=[('bytes',C.c_char*20),('shorts',C.c_short*10),('longs',C.c_long*5)]


class ClientMessage(C.Structure):
    _fields_=[('type',C.c_int),('serial',C.c_ulong),('send_event',C.c_int),('display',C.c_void_p),
              ('window',C.c_ulong),('message_type',C.c_ulong),('format',C.c_int),('data',ClientData)]


class Event(C.Union):
    _fields_=[('client',ClientMessage),('padding',C.c_long*24)]


class Manager:
    def __init__(self):
        self.x=C.CDLL(ctypes.util.find_library('X11'))
        signatures={
            'XOpenDisplay':([C.c_char_p],C.c_void_p), 'XCloseDisplay':([C.c_void_p],C.c_int),
            'XDefaultRootWindow':([C.c_void_p],C.c_ulong), 'XDefaultScreen':([C.c_void_p],C.c_int),
            'XInternAtom':([C.c_void_p,C.c_char_p,C.c_int],C.c_ulong),
            'XCreateSimpleWindow':([C.c_void_p,C.c_ulong,C.c_int,C.c_int,C.c_uint,C.c_uint,C.c_uint,C.c_ulong,C.c_ulong],C.c_ulong),
            'XChangeProperty':([C.c_void_p,C.c_ulong,C.c_ulong,C.c_ulong,C.c_int,C.c_int,C.c_void_p,C.c_int],C.c_int),
            'XSetSelectionOwner':([C.c_void_p,C.c_ulong,C.c_ulong,C.c_ulong],C.c_int),
            'XSendEvent':([C.c_void_p,C.c_ulong,C.c_int,C.c_long,C.POINTER(Event)],C.c_int),
            'XDestroyWindow':([C.c_void_p,C.c_ulong],C.c_int), 'XSync':([C.c_void_p,C.c_int],C.c_int),
        }
        for name,(args,result) in signatures.items():
            function=getattr(self.x,name);function.argtypes=args;function.restype=result
        self.display=self.x.XOpenDisplay(None)
        assert self.display,'dedicated X11 unavailable'
        self.root=self.x.XDefaultRootWindow(self.display)
        self.selection=self.atom(f'_XSETTINGS_S{self.x.XDefaultScreen(self.display)}')
        self.property=self.atom('_XSETTINGS_SETTINGS')
        self.windows=set()

    def atom(self,name):
        return self.x.XInternAtom(self.display,name.encode(),False)

    def create(self,theme,size):
        window=self.x.XCreateSimpleWindow(self.display,self.root,0,0,1,1,0,0,0)
        self.windows.add(window)
        self.update(window,theme,size)
        self.x.XSetSelectionOwner(self.display,self.selection,window,0)
        event=Event()
        event.client.type=33;event.client.window=self.root;event.client.message_type=self.atom('MANAGER')
        event.client.format=32;event.client.data.longs[1]=self.selection;event.client.data.longs[2]=window
        self.x.XSendEvent(self.display,self.root,False,1<<17,C.byref(event))
        self.x.XSync(self.display,False)
        return window

    def update(self,window,theme,size,*,little=True):
        order='<' if little else '>'
        data=struct.pack(order+'B3xII',0 if little else 1,1,2)
        for kind,name,value in [(1,'Gtk/CursorThemeName',theme),(0,'Gtk/CursorThemeSize',size)]:
            name=name.encode();data+=struct.pack(order+'BBH',kind,0,len(name))+name
            data+=b'\0'*(-len(data)%4)+struct.pack(order+'I',1)
            if kind==1:
                value=value.encode();data+=struct.pack(order+'I',len(value))+value
                data+=b'\0'*(-len(data)%4)
            else:data+=struct.pack(order+'I',value)
        self.raw(window,data)

    def raw(self,window,data):
        self.x.XChangeProperty(self.display,window,self.property,self.property,8,0,data,len(data))
        self.x.XSync(self.display,False)

    def destroy(self,window):
        self.x.XDestroyWindow(self.display,window);self.x.XSync(self.display,False)
        self.windows.remove(window)

    def close(self):
        for window in self.windows:self.x.XDestroyWindow(self.display,window)
        self.x.XCloseDisplay(self.display)


def pointer_settings(root,binary):
    themes=root/'themes'
    for name,text,arrow in [('gtk-default',0xff123456,0xff654321),('desktop',0xffabcdef,0xfffedcba),
                            ('explicit',0xff012345,0xff543210)]:
        directory=themes/name/'cursors';directory.mkdir(parents=True)
        write_cursor(directory/'xterm',text);write_cursor(directory/'left_ptr',arrow)
    saved=run('xrdb','-query')
    manager=Manager()
    fixture=None
    try:
        run('xrdb','-load',input=b'')
        environment={k:v for k,v in os.environ.items() if k not in ['XCURSOR_THEME','XCURSOR_SIZE']}
        environment.update(XCURSOR_PATH=str(themes),XDG_CONFIG_DIRS=str(root/'empty'))
        def gtk(folder,size,version='3.0'):
            path=folder/f'gtk-{version}'/'settings.ini';path.parent.mkdir(parents=True,exist_ok=True)
            temporary=path.with_suffix('.tmp')
            temporary.write_text(f'[Settings]\ngtk-cursor-theme-name=gtk-default\ngtk-cursor-theme-size={size}\n')
            temporary.replace(path)
        def launch(name,version='3.0',**variables):
            folder=root/name;gtk(folder,48,version)
            with patch.dict(os.environ,dict(environment,**variables),clear=True):
                return Fixture(folder,binary,b'',config='[window]\ncolumns=20\nlines=8\n[cursor]\nblink=false\n',complete_config=True)
        def expect(size,color,description):
            run('xdotool','mousemove','--window',fixture.window,'40','40')
            expected=(size,size,1,2,(color,)*(size*size))
            wait_for(lambda: cursor_image()==expected,description)
            assert fixture.process.poll() is None,'settings update closed Aura'
        fixture=launch('gtk3')
        expect(48,0xff123456,'GTK3 theme/size ignored with no Xcursor overrides')
        gtk(root/'gtk3',24)
        expect(24,0xff123456,'live GTK size change ignored')
        owner=manager.create('desktop',48)
        expect(48,0xffabcdef,'new XSettings manager did not replace GTK defaults')
        manager.update(owner,'desktop',24,little=False)
        expect(24,0xffabcdef,'big-endian XSettings update ignored')
        fixture.emit(b'\x1b[?1003h','MOUSE')
        expect(24,0xfffedcba,'themed application arrow differs')
        fixture.emit(b'\x1b[?1003l','TEXT')
        expect(24,0xffabcdef,'themed text pointer not restored')
        replacement=manager.create('desktop',48)
        expect(48,0xffabcdef,'replacement settings manager ignored')
        manager.destroy(owner)
        expect(48,0xffabcdef,'old settings manager destruction closed Aura')
        manager.raw(replacement,b'\0')
        expect(24,0xff123456,'malformed XSettings did not retain GTK fallback')
        manager.update(replacement,'desktop',48)
        expect(48,0xffabcdef,'valid XSettings did not recover')
        run('xrdb','-merge',input=b'Xcursor.theme: explicit\nXcursor.size: 24\n')
        expect(24,0xff012345,'explicit Xresources did not override defaults')
        run('xrdb','-load',input=b'')
        expect(48,0xffabcdef,'removed Xresources did not restore desktop defaults')
        manager.destroy(replacement)
        expect(24,0xff123456,'settings manager exit closed Aura or lost fallback')
        fixture.close();fixture=None
        fixture=launch('gtk4',version='4.0')
        expect(48,0xff123456,'GTK4-only theme/size ignored')
        fixture.close();fixture=None
        owner=manager.create('desktop',48)
        fixture=launch('environment',XCURSOR_THEME='explicit',XCURSOR_SIZE='24')
        expect(24,0xff012345,'environment did not override XSettings/GTK defaults')
        print('PASS: GTK3/GTK4, live size, XSettings start/update/replacement/exit, malformed fallback, Xresources and environment; exact ARGB/hotspots')
    finally:
        if fixture:fixture.close()
        manager.close()
        run('xrdb','-load',input=saved)


def main():
    missing=[name for name in ['xdotool','xdpyinfo','xrdb'] if not shutil.which(name)]
    if missing or subprocess.run(['xdpyinfo'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode:
        if os.environ.get('AURA_REQUIRE_GPU')=='1':raise RuntimeError(f'required X11 prerequisites unavailable: {missing}')
        print(f'SKIP: dedicated X11 prerequisites unavailable: {missing}');return 77
    binary=str(Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/aura').resolve())
    with tempfile.TemporaryDirectory(prefix='aura-pointer-settings-') as folder:
        pointer_settings(Path(folder),binary)
    return 0


if __name__=='__main__':raise SystemExit(main())
