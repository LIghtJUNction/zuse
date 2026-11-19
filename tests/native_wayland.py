#!/usr/bin/env python3
"""Exercise zuse on two private native Wayland windows, then restore focus.

Requires GTK4 development files, cc, Hyprland and a built zuse executable.
This opt-in live test briefly changes desktop focus and pointer position.
It never opens a browser, captures another window, changes settings or kills
unrelated processes. Run with --allow-desktop; pass --report for JSON evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import struct
import subprocess
import tempfile
import time
import zlib

ROOT = Path(__file__).resolve().parents[1]

class UnexpectedDesktopState(RuntimeError):
    pass

def command(args, **kwargs):
    return subprocess.run(args, capture_output=True, text=True, timeout=30, **kwargs)

def compositor(name):
    result = command(["hyprctl", "-j", name])
    result.check_returncode()
    return json.loads(result.stdout)

def png_center(path):
    raw = path.read_bytes()
    assert raw[:8] == b"\x89PNG\r\n\x1a\n"
    pos, compressed, dimensions = 8, bytearray(), None
    while pos < len(raw):
        n = struct.unpack(">I", raw[pos:pos+4])[0]
        kind, data = raw[pos+4:pos+8], raw[pos+8:pos+8+n]
        assert zlib.crc32(kind + data) & 0xffffffff == struct.unpack(">I",raw[pos+8+n:pos+12+n])[0]
        if kind == b"IHDR":
            width, height, depth, color, compression, filtering, interlace = struct.unpack(">IIBBBBB", data)
            assert depth == 8 and color in (2, 6) and not interlace
            dimensions = width, height
            bpp = 3 if color == 2 else 4
        if kind == b"IDAT":
            compressed.extend(data)
        pos += 12+n
    pixels = zlib.decompress(compressed)
    width, height = dimensions
    stride = width*bpp
    assert len(pixels) == height*(stride+1)
    previous = bytearray(stride)
    def paeth(a,b,c):
        p=a+b-c
        da,db,dc=abs(p-a),abs(p-b),abs(p-c)
        return a if da <= db and da <= dc else b if db <= dc else c
    for y in range(height//2+1):
        begin=y*(stride+1)
        mode=pixels[begin]
        row=bytearray(pixels[begin+1:begin+1+stride])
        for x in range(stride):
            a=row[x-bpp] if x >= bpp else 0
            b=previous[x]
            c=previous[x-bpp] if x >= bpp else 0
            predictor=(0,a,b,(a+b)//2,paeth(a,b,c))[mode]
            row[x]=(row[x]+predictor)&255
        previous=row
    x=(width//2)*bpp
    rgb=list(row[x:x+3])
    assert all(abs(a-b) <= 2 for a,b in zip(rgb,(32,160,96))), rgb
    return {"width":width,"height":height,"center_rgb":rgb,"bytes":len(raw)}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-desktop",action="store_true")
    parser.add_argument("--binary",type=Path,default=ROOT/"target/native-validation/debug/zuse")
    parser.add_argument("--report",type=Path)
    parser.add_argument("--ascii-background",action="store_true",help=argparse.SUPPRESS)
    parser.add_argument("--state-tests",action="store_true")
    args=parser.parse_args()
    if not args.allow_desktop:
        parser.error("--allow-desktop is required for live fixture-only desktop input")
    if args.state_tests and os.environ.get("ZUSE_PRIVATE_WAYLAND_FIXTURE")!="1":
        parser.error("--state-tests is restricted to native_wayland_private.py")
    binary=args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"Build zuse first; missing {binary}")
    with binary.open("rb") as binary_file:
        binary_digest=hashlib.file_digest(binary_file,"sha256").hexdigest()
    if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"):
        raise SystemExit("This test requires an existing Hyprland session")
    env=os.environ.copy()
    env.update(ZUSE_BACKEND="wayland",GDK_BACKEND="wayland",GSK_RENDERER="cairo")
    original_focus=compositor("activewindow").get("address")
    original_pointer=compositor("cursorpos")
    report={"native_wayland":"fail","compositor":compositor("version").get("tag"),"binary_sha256":binary_digest,"checks":{}}
    def zuse(code):
        result=command([str(binary),"eval",code],env=env)
        if result.returncode:
            detail=result.stderr.strip() or result.stdout.strip()
            if code.startswith("keyboard."):
                raise UnexpectedDesktopState("keyboard operation stopped; no further input: "+detail)
            raise RuntimeError(detail)
        data=json.loads(result.stdout)
        return data
    def value(data):
        return data["results"][-1]["value"]
    def rejected(code, expected=None):
        result=command([str(binary),"eval",code],env=env)
        data=json.loads(result.stdout)
        assert result.returncode and data["status"]=="failed", data
        assert data["completed"]==0 and data["results"]==[], data
        if expected:
            assert data["error_code"]==expected, data
            assert data["error"].startswith(expected+":"), data
        return data
    def evaluate(name, action):
        try:
            outcome=action()
            report["checks"][name]={"ok":True,"result":outcome}
        except UnexpectedDesktopState as error:
            report["checks"][name]={"ok":False,"error":str(error)}
            raise
        except Exception as error:
            report["checks"][name]={"ok":False,"error":str(error)}
    processes=[]
    logs=[]
    try:
        with tempfile.TemporaryDirectory(prefix="zuse-wayland-") as directory:
            directory=Path(directory)
            fixture=directory/"fixture"
            flags=shlex.split(command(["pkg-config","--cflags","--libs","gtk4"]).stdout)
            result=command(["cc",str(ROOT/"tests/native_wayland_fixture.c"),"-o",str(fixture),*flags])
            result.check_returncode()
            stamp=str(time.monotonic_ns())
            titles=[f"zuse isolated Wayland A {stamp}",f"zuse isolated Wayland B {stamp}"]
            for i,title in enumerate(titles):
                log=(directory/f"{i}.log").open("w")
                logs.append(log)
                processes.append(subprocess.Popen([str(fixture),title],env=env,stdout=log,stderr=log,start_new_session=True))
                deadline=time.monotonic()+8
                while time.monotonic()<deadline:
                    match=[w for w in compositor("clients") if w.get("pid")==processes[-1].pid and w.get("title")==title]
                    if match:
                        break
                    if processes[-1].poll() is not None:
                        raise RuntimeError("private fixture exited before mapping")
                    time.sleep(.05)
                else:
                    raise RuntimeError("private fixture window did not map")
            if os.environ.get("ZUSE_TEST_EXPECT_FCITX"):
                deadline=time.monotonic()+3
                while time.monotonic()<deadline:
                    if any("fcitx" in k.get("name","") for k in compositor("devices").get("keyboards",[])):
                        report["private_ime"]=True
                        break
                    time.sleep(.05)
                else:
                    raise RuntimeError("private Fcitx did not expose a Wayland virtual keyboard")
            def private_held():
                lua='return (function() local codes={} for c=8,775 do if hl.is_key_down(c) then codes[#codes+1]=c end end return "["..table.concat(codes,",").."]" end)()'
                return json.loads(command(["hyprctl","repl",lua]).stdout)
            if os.environ.get("ZUSE_PRIVATE_WAYLAND_FIXTURE")=="1":
                report["private_initial_held_key_count"]=len(private_held())
                if report["private_initial_held_key_count"]:
                    raise UnexpectedDesktopState("private compositor received held keys before test input; stopping")
            own={w["pid"]:w for w in compositor("clients") if w.get("pid") in {p.pid for p in processes}}
            assert len(own)==2 and all(not w.get("xwayland") for w in own.values())
            first,second=[own[p.pid]["address"] for p in processes]
            def current_own(window):
                match=[w for w in compositor("clients") if w["address"]==window and w.get("pid") in {p.pid for p in processes}]
                if len(match)!=1:
                    raise RuntimeError("owned target disappeared; refusing input")
                return match[0]
            def active(window):
                current_own(window)
                if compositor("activewindow").get("address")!=window:
                    raise UnexpectedDesktopState("focus moved away from owned target; stopping desktop input")
            def focus(window):
                current_own(window)
                zuse("window.focus(id="+json.dumps(window)+")")
                time.sleep(.15)
                active(window)
            def text(i):
                lines=(directory/f"{i}.log").read_text().splitlines()
                values=[bytes.fromhex(s[5:]).decode() for s in lines if s.startswith("TEXT ")]
                return values[-1] if values else ""
            def wait_text(i,expected):
                deadline=time.monotonic()+2
                while time.monotonic()<deadline:
                    observed=text(i)
                    if observed==expected:return observed
                    if not expected.startswith(observed):
                        raise UnexpectedDesktopState(f"unexpected fixture input {observed!r}; stopping desktop input")
                    time.sleep(.02)
                raise AssertionError(f"expected {expected!r}, observed {observed!r}")
            def list_windows():
                rows=value(zuse("window.list()"))
                selected=[w for w in rows if w["id"] in (first,second)]
                assert len(selected)==2
                for row in selected:
                    raw=current_own(row["id"])
                    assert (row["x"],row["y"],row["width"],row["height"])==(*raw["at"],*raw["size"])
                return {"owned_windows":len(selected),"native_wayland":True}
            evaluate("window_metadata",list_windows)
            def pointer():
                focus(first)
                w=current_own(first)
                point=(w["at"][0]+w["size"][0]//2,w["at"][1]+w["size"][1]//2)
                active(first)
                zuse(f"mouse.move(x={point[0]},y={point[1]},duration=0)")
                position=value(zuse("mouse.position()"))
                assert (position["x"],position["y"])==point
                return {"coordinate_matches":True}
            evaluate("pointer_motion",pointer)
            def foreground():
                focus(first);active(first)
                zuse('keyboard.key(key="Shift+a")')
                wait_text(0,"A");active(first)
                zuse('keyboard.type(text="Zuse 42! 中文✓🙂")')
                return {"text":wait_text(0,"AZuse 42! 中文✓🙂")}
            evaluate("foreground_input",foreground)
            def background():
                focus(second);active(second)
                start=text(0)
                payload=" background!?" + "".join(chr(i) for i in range(32,127))
                try:
                    zuse("keyboard.type(window="+json.dumps(first)+",text="+json.dumps(payload)+")")
                except UnexpectedDesktopState:
                    raise
                except RuntimeError as error:
                    raise RuntimeError(str(error)+"; private fixture observed="+repr(text(0))) from error
                observed=wait_text(0,start+payload)
                active(second)
                # Foreground Unicode restores the original map. Background
                # text uses that real map and leaves B's text/focus unchanged.
                zuse("keyboard.key(window="+json.dumps(first)+',key="Shift+z")')
                observed=wait_text(0,start+payload+"Z")
                active(second)
                assert text(1)=="",text(1)
                return {"text":observed,"focus_unchanged":True,"foreground_text_unchanged":True}
            evaluate("background_input",background)
            def rejected_input():
                focus(second);active(second)
                before=(text(0),text(1))
                for payload in ("prefix\u0001","prefix\u007f","prefix 中文🙂"):
                    rejected("keyboard.type(window="+json.dumps(first)+",text="+json.dumps(payload)+")",
                             "unsupported_character" if payload.endswith("中文🙂") else None)
                    time.sleep(.05)
                    assert (text(0),text(1))==before, "rejected input changed a fixture"
                    active(second)
                return {"unsupported_text_rejected_before_input":True}
            evaluate("rejected_input",rejected_input)
            def capture():
                focus(first);active(first)
                time.sleep(.15)
                screenshot=directory/"owned-window.png"
                metadata=value(zuse("window.capture(id="+json.dumps(first)+",path="+json.dumps(str(screenshot))+")"))
                image=png_center(screenshot)
                assert (metadata["width"],metadata["height"])==(image["width"],image["height"])
                return image
            evaluate("window_capture",capture)
            if args.state_tests:
                def unsupported_states():
                    helper_path=directory/"keyboard-fixture"
                    flags=shlex.split(command(["pkg-config","--cflags","--libs","wayland-client","xkbcommon"]).stdout)
                    compiled=command(["cc",str(ROOT/"tests/native_wayland_keyboard_fixture.c"),"-o",str(helper_path),*flags])
                    compiled.check_returncode()
                    helper=subprocess.Popen([str(helper_path)],env=env,stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                    def receive():
                        import select
                        if not select.select([helper.stdout],[],[],3)[0]:
                            raise RuntimeError("private keyboard fixture did not respond")
                        response=helper.stdout.readline().strip()
                        if response not in ("READY","OK"):
                            raise RuntimeError("private keyboard fixture: "+response)
                        return response
                    def send(line):
                        helper.stdin.write(line+"\n");helper.stdin.flush()
                        assert receive()=="OK"
                    def state():
                        rows=compositor("devices")["keyboards"]
                        main=[k for k in rows if k.get("main")]
                        assert len(main)==1
                        return (main[0]["capsLock"],main[0]["numLock"],main[0]["active_layout_index"])
                    checked=[]
                    try:
                        assert receive()=="READY"
                        focus(first);active(first)
                        for mode,expected,code,character in [
                            ("caps",(True,False,0),38,"A"),
                            ("num",(False,True,0),38,"a"),
                            ("group1",(False,False,1),29,"z"),
                        ]:
                            send(mode)
                            assert state()==expected,(mode,state())
                            before=text(0);send(f"press {code}");wait_text(0,before+character)
                            for operation in [
                                'keyboard.type(text="SHOULD_NOT_APPEAR")',
                                "keyboard.type(window="+json.dumps(first)+',text="SHOULD_NOT_APPEAR")',
                                'keyboard.key(key="a")',
                            ]:
                                active(first);rejected(operation,"unsupported_state")
                                assert text(0)==before+character
                                assert state()==expected,(mode,state())
                            # The original keyboard sends an ordinary letter:
                            # no Shift/focus event masks a lost Caps/group.
                            send(f"press {code}");wait_text(0,before+character*2)
                            assert state()==expected
                            checked.append(mode)
                            send("neutral")
                        send("down 50")
                        before=text(0)
                        active(first);rejected('keyboard.type(text="SHOULD_NOT_APPEAR")',"unsupported_state")
                        assert text(0)==before
                        send("up 50");send("neutral")
                        send("press 38");wait_text(0,before+"a")
                        checked.append("held_key")
                        return {"rejected_before_effect":checked,"original_plain_letters_after_rejection":True}
                    except Exception as error:
                        raise RuntimeError(str(error)+"; own fixture events="+
                                           repr((directory/"0.log").read_text()[-1500:])+
                                           "; private held count="+str(len(private_held()))) from error
                    finally:
                        if helper.poll() is None:
                            try:send("neutral");send("quit")
                            except Exception:pass
                            try:helper.wait(timeout=3)
                            except subprocess.TimeoutExpired:helper.kill();helper.wait(timeout=3)
                evaluate("unsupported_keyboard_states",unsupported_states)
            report["native_wayland"]="pass" if all(v["ok"] for v in report["checks"].values()) else "fail"
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill();process.wait(timeout=3)
        for log in logs:log.close()
        try:
            x,y=original_pointer["x"],original_pointer["y"]
            if original_focus and any(w.get("address")==original_focus for w in compositor("clients")):
                zuse("window.focus(id="+json.dumps(original_focus)+")")
            time.sleep(.15)
            after_focus=compositor("cursorpos")
            zuse(f"mouse.move(x={x},y={y},duration=0)")
            time.sleep(.05)
            remaining=compositor("clients")
            report["restored"]={
                "own_windows_closed":not any(w.get("pid") in {p.pid for p in processes} for w in remaining),
                "pointer":compositor("cursorpos")==original_pointer,
                "focus":compositor("activewindow").get("address")==original_focus,
            }
            if not all(report["restored"].values()):
                report["native_wayland"]="fail"
            report["restored"]["pointer_after_focus"]=after_focus
            report["restored"]["pointer_expected"]=original_pointer
            report["restored"]["pointer_observed"]=compositor("cursorpos")
        except Exception as error:
            report["restored"]={"error":str(error)}
            report["native_wayland"]="fail"
        if args.report:
            args.report.parent.mkdir(parents=True,exist_ok=True)
            args.report.write_text(json.dumps(report,indent=2)+"\n")
        print(json.dumps(report,indent=2))
    return 0 if report["native_wayland"]=="pass" else 1

if __name__=="__main__":
    raise SystemExit(main())
