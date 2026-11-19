#!/usr/bin/env python3
"""Run native desktop integration checks in a new Xvfb server.
Requires cargo, gcc, Xvfb, libX11 and libXtst. No host desktop input is generated.
"""
import json
import os
import pathlib
import shutil
import struct
import subprocess
import tempfile
import time
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURE = r"""
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
int main(void) {
 Display*d=XOpenDisplay(NULL); if(!d)return 1;
 int s=DefaultScreen(d);
 Window w=XCreateSimpleWindow(d,RootWindow(d,s),20,30,400,240,0,0,0x202020);
 XStoreName(d,w,"zuse isolated fixture");
 XClassHint hint={"zuse-fixture","ZuseFixture"}; XSetClassHint(d,w,&hint);
 XSelectInput(d,w,KeyPressMask|KeyReleaseMask|ButtonPressMask|ButtonReleaseMask|PointerMotionMask|StructureNotifyMask);
 XMapWindow(d,w);XFlush(d);printf("WINDOW=%lu\n",w);fflush(stdout);
 while(1) {
  XEvent e;XNextEvent(d,&e);
  if(e.type==MappingNotify)XRefreshKeyboardMapping(&e.xmapping);
  if(e.type==KeyPress) {
   char b[64];KeySym sym;int n=XLookupString(&e.xkey,b,sizeof b,&sym,NULL);
   printf("KEY synthetic=%d sym=%lu text=",e.xkey.send_event,sym);
   for(int i=0;i<n;i++)printf("%02x",(unsigned char)b[i]);
   puts("");fflush(stdout);
  }
  if(e.type==ButtonPress){printf("BUTTON=%d\n",e.xbutton.button);fflush(stdout);}
 }
}
"""
HARNESS = r"""
use zuse::platform::Desktop;
fn main() {
 let args=std::env::args().collect::<Vec<_>>();
 let id=&args[1]; let dir=std::path::Path::new(&args[2]);
 let mut d=Desktop::connect().unwrap();
 assert_eq!(d.backend(),"x11");
 d.focus(id).unwrap();
 d.move_to(123,234).unwrap();
 assert_eq!(d.position().unwrap(),(123,234));
 d.button("left","click").unwrap();
 d.key("Shift+a").unwrap();
 d.type_text("Hello Zuse 123!").unwrap();
 d.scroll(0,1).unwrap();
 d.type_into(id,"background").unwrap();
 d.key_into(id,"Return").unwrap();
 let windows=d.windows().unwrap();
 assert!(windows.contains("zuse isolated fixture"));
 assert!(windows.contains("ZuseFixture"));
 d.screenshot(&dir.join("screen.png"),None).unwrap();
 d.screenshot(&dir.join("window.png"),Some(id)).unwrap();
 assert!(d.key("NotAKey").is_err());
 assert!(d.button("invalid","click").is_err());
 assert!(d.focus("0xffffffffff").is_err());
}
"""

def check_png(path, expected):
    raw = path.read_bytes()
    assert raw[:8] == b"\x89PNG\r\n\x1a\n"
    pos, idat, dims = 8, bytearray(), None
    while pos < len(raw):
        n = struct.unpack(">I", raw[pos:pos + 4])[0]
        kind = raw[pos + 4:pos + 8]
        data = raw[pos + 8:pos + 8 + n]
        assert zlib.crc32(kind + data) & 0xffffffff == struct.unpack(
            ">I", raw[pos + 8 + n:pos + 12 + n])[0]
        if kind == b"IHDR":
            dims = struct.unpack(">II", data[:8])
        if kind == b"IDAT":
            idat.extend(data)
        pos += n + 12
    pixels = zlib.decompress(idat)
    assert dims == expected
    assert len(pixels) == (dims[0] * 3 + 1) * dims[1]
    if path.name == "window.png":
        assert pixels[1:4] == b"\x20\x20\x20"
    return {"width": dims[0], "height": dims[1], "bytes": len(raw)}

def main():
    for tool in ("cargo", "gcc", "Xvfb"):
        if not shutil.which(tool):
            raise SystemExit(f"required integration-test tool is missing: {tool}")
    with tempfile.TemporaryDirectory(prefix="zuse-native-x11-") as directory:
        p = pathlib.Path(directory)
        (p / "Cargo.toml").write_text(
            '[package]\nname="zuse-native-check"\nversion="0.0.0"\nedition="2024"\n'
            '[dependencies]\nzuse={package="zuse-computer",path=' + json.dumps(str(ROOT)) + '}\n'
            '[[bin]]\nname="native-check"\npath="main.rs"\n')
        (p / "main.rs").write_text(HARNESS)
        (p / "fixture.c").write_text(FIXTURE)
        subprocess.run(["gcc", str(p / "fixture.c"), "-o", str(p / "fixture"), "-lX11"], check=True)
        subprocess.run(["cargo", "build", "--manifest-path", str(p / "Cargo.toml"), "--offline"], check=True)
        fixture = None
        xvfb = subprocess.Popen(
            ["Xvfb", "-displayfd", "1", "-screen", "0", "640x480x24", "-nolisten", "tcp"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            display = ":" + xvfb.stdout.readline().strip()
            if display == ":":
                raise RuntimeError("Xvfb failed: " + xvfb.stderr.read())
            env = os.environ.copy()
            env.update(DISPLAY=display, ZUSE_BACKEND="x11")
            with (p / "events.log").open("w") as log:
                fixture = subprocess.Popen([str(p / "fixture")], env=env, stdout=log)
                for _ in range(200):
                    lines = (p / "events.log").read_text().splitlines()
                    if lines:
                        break
                    time.sleep(0.01)
                else:
                    raise RuntimeError("fixture did not create its window")
                window_id = lines[0].split("=")[1]
                time.sleep(0.05)
                subprocess.run(
                    [str(p / "target/debug/native-check"), window_id, str(p)],
                    env=env, check=True, timeout=30)
                time.sleep(0.1)
                events = (p / "events.log").read_text()
            observed = "".join(
                bytes.fromhex(line.split("text=")[1]).decode("latin1")
                for line in events.splitlines() if line.startswith("KEY "))
            assert observed == "AHello Zuse 123!background\r", repr(observed)
            assert "BUTTON=1" in events and "BUTTON=5" in events
            assert "synthetic=1" in events
            report = {
                "native_x11": "pass",
                "pointer": [123, 234],
                "foreground_and_background_text": "pass",
                "screen_png": check_png(p / "screen.png", (640, 480)),
                "window_png": check_png(p / "window.png", (400, 240)),
            }
            print(json.dumps(report, indent=2))
        finally:
            if fixture is not None:
                fixture.terminate()
                fixture.wait(timeout=5)
            xvfb.terminate()
            xvfb.wait(timeout=5)

if __name__ == "__main__":
    main()
