#!/usr/bin/env python3
"""Exercise zuse on two private native Wayland windows, then restore focus.

Requires GTK4 development files, cc, Hyprland and a built zuse executable.
This opt-in live test briefly changes desktop focus and pointer position.
Default capture checks use only owned window surfaces. Optional marker checks
explicitly capture the single output of the private headless fixture, because
compositor overlays are outside a window surface. It never opens a browser,
changes settings or kills unrelated processes. Run with --allow-desktop;
pass --report for JSON evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
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

def png_pixels(path):
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
    rows=[]
    for y in range(height):
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
        rows.append(row)
    return {"width":width,"height":height,"bpp":bpp,"rows":rows,"bytes":len(raw)}

def png_center(path):
    image=png_pixels(path)
    width,height,bpp=image["width"],image["height"],image["bpp"]
    row=image["rows"][height//2]
    x=(width//2)*bpp
    rgb=list(row[x:x+3])
    assert all(abs(a-b) <= 2 for a,b in zip(rgb,(32,160,96))), rgb
    return {"width":width,"height":height,"center_rgb":rgb,"bytes":image["bytes"]}

class OwnedKeyboard:
    """A persistent protocol keyboard, confined to the private compositor."""
    def __init__(self, binary, env):
        if env.get("ZUSE_PRIVATE_WAYLAND_FIXTURE")!="1":
            raise RuntimeError("keyboard fixtures require the owned compositor")
        self.process=subprocess.Popen([str(binary)],env=env,stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            self.receive("READY")
        except Exception:
            self.close()
            raise
    def receive(self, expected):
        if not select.select([self.process.stdout],[],[],3)[0]:
            raise RuntimeError("private keyboard fixture response timed out")
        response=self.process.stdout.readline().strip()
        if response!=expected:
            raise RuntimeError(f"private keyboard fixture expected {expected!r}, got {response!r}")
    def send(self, line):
        self.process.stdin.write(line+"\n");self.process.stdin.flush()
        self.receive("OK")
    def close(self):
        if self.process.poll() is None:
            try:self.send("neutral");self.send("quit")
            except Exception:pass
            try:self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait(timeout=3)
        for stream in (self.process.stdin,self.process.stdout,self.process.stderr):
            if stream:stream.close()

def device_states(rows):
    return sorted((k["address"],k["main"],k["capsLock"],
                   k["numLock"],k["active_layout_index"]) for k in rows)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-desktop",action="store_true")
    parser.add_argument("--binary",type=Path,default=ROOT/"target/native-validation/debug/zuse")
    parser.add_argument("--report",type=Path)
    parser.add_argument("--ascii-background",action="store_true",help=argparse.SUPPRESS)
    parser.add_argument("--state-tests",action="store_true")
    parser.add_argument("--marker-tests",action="store_true")
    parser.add_argument("--hold-tests",action="store_true",help="run only bounded key-hold checks in the owned compositor")
    args=parser.parse_args()
    if not args.allow_desktop:
        parser.error("--allow-desktop is required for live fixture-only desktop input")
    if (args.state_tests or args.marker_tests or args.hold_tests) and os.environ.get("ZUSE_PRIVATE_WAYLAND_FIXTURE")!="1":
        parser.error("state/marker tests are restricted to native_wayland_private.py")
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
        if args.hold_tests and name not in ("window_metadata", "bounded_key_holds"):
            return
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
                monitors={m["id"]:m for m in compositor("monitors")}
                focused=compositor("activewindow").get("address")
                for row in selected:
                    raw=current_own(row["id"])
                    assert (row["x"],row["y"],row["width"],row["height"])==(*raw["at"],*raw["size"])
                    assert row["scale"]==monitors[raw["monitor"]]["scale"],row
                    assert row["focused"]==(row["id"]==focused),row
                    assert row["mapped"] is True and raw["mapped"] is True,row
                return {"owned_windows":len(selected),"native_wayland":True,
                        "scale_focus_mapped_verified":True}
            evaluate("window_metadata",list_windows)
            if args.hold_tests:
                from native_key_hold import run_private_holds
                evaluate("bounded_key_holds", lambda: run_private_holds(
                    binary, env, directory, first, second, focus, compositor, private_held))
            def pointer():
                focus(first)
                w=current_own(first)
                point=(w["at"][0]+w["size"][0]//2,w["at"][1]+w["size"][1]//2)
                active(first)
                zuse(f'mouse.use(kind="user",appearance="system");mouse.move(x={point[0]},y={point[1]},duration=0)')
                position=value(zuse('mouse.use(kind="user",appearance="system");mouse.position()'))
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
            def focused_target():
                focus(first);active(first)
                start=text(0)
                before=value(zuse('keyboard.state()'))["current"]
                payload=" explicit Zuse 中文✓🙂"
                zuse("keyboard.type(window="+json.dumps(first)+",text="+json.dumps(payload)+")")
                observed=wait_text(0,start+payload)
                active(first)
                after=value(zuse('keyboard.state()'))["current"]
                for field in ("address","name","rules","model","layout","variant","options","active_keymap","capsLock","numLock","active_layout_index"):
                    assert before[field]==after[field],(before,after)
                assert text(1)=="",text(1)
                return {"text":observed,"explicit_window":first,
                        "focus_unchanged":True,"keyboard_restored":True}
            evaluate("focused_target_unicode",focused_target)
            def text_keymap_batches():
                focus(first);active(first)
                start=text(0)
                before=value(zuse('keyboard.state()'))["current"]
                assert before["active_keymap"]!="zuse",before
                # Cross the sparse, Chromium-compatible keycode capacity. An
                # IME must observe a fresh identity for the second map and for
                # the final original-map restoration, not retain batch one.
                payload=" zuse zaz "+"".join(chr(0x4e00+i) for i in range(200))+" zuse"
                zuse("keyboard.type(window="+json.dumps(first)+",text="+json.dumps(payload)+")")
                observed=wait_text(0,start+payload)
                after=value(zuse('keyboard.state()'))["current"]
                for field in ("address","name","rules","model","layout","variant","options","active_keymap","capsLock","numLock","active_layout_index"):
                    assert before[field]==after[field],(before,after)
                return {"text":observed,"multiple_maps":True,"original_keymap_restored":True}
            evaluate("text_keymap_batches",text_keymap_batches)
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
                w=current_own(first)
                region=metadata["logicalRegion"]
                assert metadata["window"]==first,metadata
                assert metadata["captureMode"]=="hyprland-toplevel-export",metadata
                assert metadata["coordinateSpace"]=="window-local",metadata
                assert region=={"x":0,"y":0,
                                "width":w["size"][0],"height":w["size"][1]},metadata
                ratio=metadata["pixelsPerLogicalUnit"]
                assert abs(ratio["x"]-image["width"]/region["width"])<1e-12,metadata
                assert abs(ratio["y"]-image["height"]/region["height"])<1e-12,metadata
                # The image only maps to window-local logical coordinates.
                # This owned-fixture pointer check separately observes the
                # current window position; it never reads a desktop origin
                # from the native surface capture receipt.
                point=(round(w["at"][0]+image["width"]/2/ratio["x"]),
                       round(w["at"][1]+image["height"]/2/ratio["y"]))
                assert abs(point[0]-(w["at"][0]+w["size"][0]/2))<=.5
                assert abs(point[1]-(w["at"][1]+w["size"][1]/2))<=.5
                zuse(f'mouse.use(kind="user",appearance="system");mouse.move(x={point[0]},y={point[1]},duration=0)')
                observed=value(zuse('mouse.use(kind="user",appearance="system");mouse.position()'))
                assert (observed["x"],observed["y"])==point,observed
                image.update(logical_region=region,pixels_per_logical_unit=ratio,
                             receipt_point_verified=True)
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
                        # Num Lock is supported for foreground chords using
                        # an unchanged XKB map. Keep this original keyboard
                        # alive, and send a keypad key without replaying its
                        # modifiers after each zuse chord. A lost client lock
                        # mask turns that exact key into KP_End instead of 1.
                        send("num")
                        expected=(False,True,0)
                        assert state()==expected
                        before=text(0)
                        other_before=text(1)
                        before_devices=compositor("devices")["keyboards"]
                        original_devices=device_states(before_devices)
                        def shortcut_events():
                            return [tuple(map(int,line.split()[1:]))
                                    for line in (directory/"0.log").read_text().splitlines()
                                    if line.startswith("KEY ")]
                        for operation in [
                            'keyboard.key(key="Ctrl+T")',
                            "keyboard.key(window="+json.dumps(first)+',key="Ctrl+T")',
                        ]:
                            active(first)
                            events_before=len(shortcut_events())
                            zuse(operation)
                            deadline=time.monotonic()+2
                            while time.monotonic()<deadline:
                                new_events=shortcut_events()[events_before:]
                                if any(keyval==ord("t") and flags&4
                                       for keyval,code,flags in new_events):
                                    break
                                time.sleep(.02)
                            else:
                                raise AssertionError("foreground Ctrl+T was not received")
                            assert text(0)==before
                            assert state()==expected
                            assert device_states(compositor("devices")["keyboards"])==original_devices
                            active(first)
                            send("press 87");before+="1";wait_text(0,before)
                            send("press 38");before+="a";wait_text(0,before)
                            assert state()==expected
                        zuse('keyboard.key(key="code:87")')
                        before+="1";wait_text(0,before)
                        send("press 87");before+="1";wait_text(0,before)
                        assert device_states(compositor("devices")["keyboards"])==original_devices
                        for operation in [
                            'keyboard.type(text="SHOULD_NOT_APPEAR")',
                            "keyboard.type(window="+json.dumps(first)+',text="SHOULD_NOT_APPEAR")',
                            "keyboard.key(window="+json.dumps(second)+',key="Ctrl+T")',
                        ]:
                            active(first);rejected(operation,"unsupported_state")
                            assert (text(0),text(1))==(before,other_before)
                            assert state()==expected
                        send("press 87");before+="1";wait_text(0,before)
                        assert device_states(compositor("devices")["keyboards"])==original_devices
                        checked.append("num_lock_text_and_background")
                        send("neutral")
                        send("down 50")
                        before=text(0)
                        active(first);rejected('keyboard.type(text="SHOULD_NOT_APPEAR")',"unsupported_state")
                        assert text(0)==before
                        send("up 50");send("neutral")
                        send("press 38");wait_text(0,before+"a")
                        checked.append("held_key")
                        return {"rejected_before_effect":checked,"original_plain_letters_after_rejection":True,
                                "num_lock_foreground_shortcuts":2,"num_lock_keypad_after_shortcut":True,
                                "num_lock_original_keyboard_states_unchanged":True}
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
                def independent_keyboard_locks():
                    helper_path=directory/"keyboard-fixture"
                    helpers=[]
                    try:
                        focus(first)
                        helpers.append(OwnedKeyboard(helper_path,env))
                        for _ in range(8):
                            helper=OwnedKeyboard(helper_path,env)
                            helpers.append(helper)
                            helper.send("num")
                        # Make the earlier neutral keyboard current by using
                        # that real device. Its ordinary letter does not replay
                        # a modifier mask that could conceal lost lock state.
                        before=text(0)
                        helpers[0].send("press 38")
                        before+="a";wait_text(0,before)
                        rows=compositor("devices")["keyboards"]
                        current=[k for k in rows if k.get("main") is True]
                        assert len(current)==1,current
                        main=current[0]
                        assert main["numLock"] is False and main["capsLock"] is False,main
                        assert main["active_layout_index"]==0,main
                        assert rows[-1]["address"]!=main["address"],"regression must exercise a non-last main keyboard"
                        assert sum(k.get("numLock") is True for k in rows if not k.get("main"))==8,rows
                        expected=device_states(rows)
                        status=value(zuse("keyboard.state()"))
                        assert status["current"]["address"]==main["address"],status
                        assert status["current"]["numLock"] is False,status
                        assert sum(k.get("numLock") is True for k in status["other_devices"])==8,status
                        active(first)
                        payload=" mixed devices 42!"
                        zuse("keyboard.type(text="+json.dumps(payload)+")")
                        before+=payload;wait_text(0,before)
                        assert device_states(compositor("devices")["keyboards"])==expected
                        # Follow-up input from the same original keyboard
                        # detects unintended modifier/keymap restoration.
                        helpers[0].send("press 38")
                        before+="a";wait_text(0,before)
                        active(first)
                        assert device_states(compositor("devices")["keyboards"])==expected
                        assert not private_held()
                        return {"other_num_lock_on":8,"current_num_lock_on":False,
                                "current_is_last":False,"current_address":main["address"],
                                "text_received":payload,"all_device_states_unchanged":True,
                                "original_keyboard_plain_letter_after_input":True}
                    finally:
                        for helper in reversed(helpers):helper.close()
                evaluate("independent_keyboard_locks",independent_keyboard_locks)
            if args.marker_tests:
                def operation_marker():
                    assert env.get("ZUSE_PRIVATE_WAYLAND_HEADLESS")=="1"
                    assert env.get("XDG_CURRENT_DESKTOP")=="zuse-private-headless"
                    outputs=compositor("monitors")
                    assert len(outputs)==1,"marker screen capture requires exactly one owned output"
                    output=outputs[0]
                    assert output.get("transform",0)==0,output
                    output_x,output_y=output["x"],output["y"]
                    output_width=output["width"]/output["scale"]
                    output_height=output["height"]/output["scale"]
                    focus(first)
                    window=current_own(first)
                    geometry=tuple(window["at"]+window["size"])
                    x,y,width,height=geometry
                    points=[(x+width//4,y+height*2//3),(x+width*3//4,y+height*2//3)]
                    assert width>=240 and height>=200,geometry
                    marker_log_path=directory/"marker-protocol.log"
                    marker_log=marker_log_path.open("w")
                    shell_env=env.copy();shell_env["WAYLAND_DEBUG"]="client"
                    shell=subprocess.Popen([str(binary),"shell"],env=shell_env,
                                           stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                                           stderr=marker_log,text=True)
                    pointer=None
                    screenshots={}
                    def session(code):
                        shell.stdin.write(code+"\n");shell.stdin.flush()
                        if not select.select([shell.stdout],[],[],5)[0]:
                            raise RuntimeError("persistent marker runtime timed out: "+marker_log_path.read_text()[-1600:])
                        response=shell.stdout.readline()
                        if not response:
                            raise RuntimeError("persistent marker runtime exited: "+marker_log_path.read_text()[-1600:])
                        data=json.loads(response)
                        assert data.get("status")=="completed",data
                        return data
                    def capture_stage(name):
                        active(first)
                        assert tuple(current_own(first)["at"]+current_own(first)["size"])==geometry
                        path=directory/("marker-"+name+".png")
                        # Layer-shell markers are compositor content, not part
                        # of either owned GTK toplevel's exported surface.
                        receipt=value(session("wait(ms=120);screen.capture(path="+json.dumps(str(path))+")"))
                        assert receipt["captureMode"]=="composited-screen",receipt
                        assert "window" not in receipt and "logicalRegion" not in receipt,receipt
                        active(first)
                        screenshots[name]=path
                        return png_pixels(path)
                    def marked_pixels(image, point):
                        # This branch explicitly captures the sole private
                        # output, whose origin/scale were observed above.
                        assert (image["width"],image["height"])==(output["width"],output["height"])
                        sx,sy=image["width"]/output_width,image["height"]/output_height
                        cx,cy=(point[0]-output_x)*sx,(point[1]-output_y)*sy
                        found=[]
                        for py in range(max(0,int(cy-54*sy)),min(image["height"],int(cy+54*sy)+1)):
                            row=image["rows"][py]
                            for px in range(max(0,int(cx-54*sx)),min(image["width"],int(cx+54*sx)+1)):
                                start=px*image["bpp"];r,g,b=row[start:start+3]
                                if r>120 and b>150 and r>g*1.3 and b>g*1.3:found.append((px,py))
                        if found:
                            bounds=[min(p[0] for p in found),min(p[1] for p in found),
                                    max(p[0] for p in found),max(p[1] for p in found)]
                            assert abs((bounds[0]+bounds[2])/2-cx)<=3*sx,(bounds,(cx,cy))
                            assert abs((bounds[1]+bounds[3])/2-cy)<=3*sy,(bounds,(cx,cy))
                        return len(found)
                    click_result={"status":"unsupported","reason":"private compositor lacks virtual pointer protocol; host input is disabled"}
                    try:
                        independent=rejected('mouse.use(kind="independent")')
                        assert "independent" in independent["error"].lower(),independent
                        # This protocol helper is restricted to the owned
                        # socket. It verifies surface hit testing; it does not
                        # pretend to exercise the production ydotool backend.
                        pointer_binary=directory/"pointer-fixture"
                        flags=shlex.split(command(["pkg-config","--cflags","--libs","wayland-client"]).stdout)
                        command(["cc","-Wall","-Wextra","-Werror",str(ROOT/"tests/native_wayland_pointer_fixture.c"),
                                 "-o",str(pointer_binary),*flags]).check_returncode()
                        pointer=subprocess.Popen([str(pointer_binary)],env=env,stdin=subprocess.PIPE,
                                                 stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                        if not select.select([pointer.stdout],[],[],3)[0]:
                            raise RuntimeError("private pointer fixture startup timed out")
                        pointer_status=pointer.stdout.readline().strip()
                        assert pointer_status in ("READY","UNSUPPORTED"),pointer_status
                        def click_through():
                            if pointer_status!="READY":return
                            def clicks():
                                return [line for line in (directory/"0.log").read_text().splitlines() if line.startswith("BUTTON ")]
                            before=len(clicks())
                            pointer.stdin.write("click\n");pointer.stdin.flush()
                            if not select.select([pointer.stdout],[],[],3)[0]:
                                raise RuntimeError("private pointer click timed out")
                            assert pointer.stdout.readline().strip()=="OK"
                            deadline=time.monotonic()+2
                            while time.monotonic()<deadline:
                                if len(clicks())>before:break
                                time.sleep(.01)
                            assert len(clicks())==before+1,"marker intercepted the owned fixture click"
                            active(first)
                        session('mouse.use(kind="user",appearance="system")')
                        session(f'mouse.move(x={points[0][0]},y={points[0][1]},duration=0)')
                        baseline=capture_stage("before")
                        assert all(marked_pixels(baseline,p)==0 for p in points)
                        created=value(session('mouse.use(kind="user",appearance="glow",color="#ED2FFF",shape="ring",size=40,glow=0.8)'))
                        assert created["kind"]=="user" and created["device_id"] is None,created
                        session(f'mouse.move(x={points[0][0]},y={points[0][1]},duration=0)')
                        first_image=capture_stage("first")
                        first_count=marked_pixels(first_image,points[0])
                        assert first_count>=20,first_count
                        assert marked_pixels(first_image,points[1])==0
                        click_through()
                        session(f'mouse.move(x={points[1][0]},y={points[1][1]},duration=160)')
                        second_image=capture_stage("moved")
                        second_count=marked_pixels(second_image,points[1])
                        assert second_count>=20,second_count
                        assert marked_pixels(second_image,points[0])==0
                        observed=value(session("mouse.position()"))
                        assert (observed["x"],observed["y"])==points[1],observed
                        click_through()
                        if pointer_status=="READY":
                            click_result={"status":"passed","source":"owned-wayland-virtual-pointer","received_clicks":2}
                        session("mouse.release()")
                        released=capture_stage("released")
                        assert all(marked_pixels(released,p)==0 for p in points)
                        # EOF exercises Drop independently of explicit release.
                        session('mouse.use(kind="user",appearance="glow",color="#ED2FFF",shape="ring",size=40,glow=0.8)')
                        assert marked_pixels(capture_stage("before-drop"),points[1])>=20
                        shell.stdin.close();shell.wait(timeout=5)
                        assert shell.returncode==0,shell.returncode
                        time.sleep(.12)
                        dropped_path=directory/"marker-dropped.png"
                        zuse("screen.capture(path="+json.dumps(str(dropped_path))+")")
                        screenshots["dropped"]=dropped_path
                        dropped=png_pixels(dropped_path)
                        assert all(marked_pixels(dropped,p)==0 for p in points)
                        active(first)
                        marker_log.flush()
                        protocol=marker_log_path.read_text()
                        regions=re.findall(r"set_input_region\(wl_region[#@](\d+)\)",protocol)
                        assert regions,"no non-null input region was submitted"
                        assert all(not re.search(r"wl_region[#@]"+region+r"\.add\(",protocol) for region in regions)
                        assert "set_keyboard_interactivity(0)" in protocol
                        assert "set_exclusive_zone(-1)" in protocol
                        artifacts={}
                        if args.report:
                            artifact_dir=args.report.resolve().parent/(args.report.stem+"-artifacts")
                            artifact_dir.mkdir(parents=True,exist_ok=True)
                            for name,path in {**screenshots,"protocol":marker_log_path}.items():
                                target=artifact_dir/path.name;shutil.copyfile(path,target)
                                artifacts[name]={"path":str(target),"sha256":hashlib.sha256(target.read_bytes()).hexdigest()}
                        return {"mode":"user-pointer-with-last-operation-marker","persistent_mouse_id":created["mouse_id"],
                                "points":points,"visible_pixels":[first_count,second_count],
                                "focus_unchanged":True,"old_position_cleared":True,
                                "explicit_release_removed_marker":True,"drop_removed_marker":True,
                                "independent_mode_rejected":True,
                                "empty_input_region":True,"keyboard_interactivity":"none",
                                "click_through":click_result,"production_button_backend":"not exercised; host ydotool disabled",
                                "artifacts":artifacts}
                    finally:
                        if shell.poll() is None:
                            if not shell.stdin.closed:shell.stdin.close()
                            try:shell.wait(timeout=3)
                            except subprocess.TimeoutExpired:shell.kill();shell.wait(timeout=3)
                        shell.stdout.close();marker_log.close()
                        if args.report:
                            artifact_dir=args.report.resolve().parent/(args.report.stem+"-artifacts")
                            artifact_dir.mkdir(parents=True,exist_ok=True)
                            saved={}
                            for name,path in {**screenshots,"protocol":marker_log_path}.items():
                                if path.is_file():
                                    target=artifact_dir/path.name;shutil.copyfile(path,target)
                                    saved[name]={"path":str(target),"sha256":hashlib.sha256(target.read_bytes()).hexdigest()}
                            report["marker_artifacts"]=saved
                        if pointer:
                            if pointer.poll() is None:
                                pointer.stdin.close()
                                try:pointer.wait(timeout=3)
                                except subprocess.TimeoutExpired:pointer.kill();pointer.wait(timeout=3)
                            for stream in (pointer.stdin,pointer.stdout,pointer.stderr):
                                if stream and not stream.closed:stream.close()
                evaluate("operation_marker",operation_marker)
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
            zuse(f'mouse.use(kind="user",appearance="system");mouse.move(x={x},y={y},duration=0)')
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
