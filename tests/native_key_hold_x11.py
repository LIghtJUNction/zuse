#!/usr/bin/env python3
"""Validate direction-key holds in an owned, authenticated Xvfb server."""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import tempfile
import time

from private_xvfb import private_xvfb_command, release_private_xvfb, verify_private_xvfb

FIXTURE = r"""
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <X11/XKBlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc,char**argv) {
 Display*d=XOpenDisplay(NULL); if(!d)return 1;
 if(argc==2 && strcmp(argv[1],"--probe")==0) {
  Window focus;int revert;char keys[32];XkbStateRec state;
  XGetInputFocus(d,&focus,&revert);XQueryKeymap(d,keys);
  int status=XkbGetState(d,XkbUseCoreKbd,&state);
  printf("{\"focus_decimal\":%lu,\"focus_hex\":\"0x%lx\",\"revert_to\":%d,\"xkb_status\":%d,\"held_keycodes\":[",focus,focus,revert,status);
  int separator=0;
  for(int code=0;code<256;code++) if(((unsigned char)keys[code/8])&(1u<<(code%8))) {
   printf("%s%d",separator?",":"",code);separator=1;
  }
  printf("]");
  if(status==Success)printf(",\"base_mods\":%u,\"latched_mods\":%u,\"locked_mods\":%u,\"group\":%u",state.base_mods,state.latched_mods,state.locked_mods,state.group);
  printf("}\n");XCloseDisplay(d);return 0;
 }
 Window windows[2];
 unsigned long pid=(unsigned long)getpid();
 for(int i=0;i<2;i++) {
  Window w=XCreateSimpleWindow(d,DefaultRootWindow(d),20+300*i,30,260,220,0,0,0);
  windows[i]=w;
  XStoreName(d,w,i ? "zuse hold second" : "zuse hold first");
  XChangeProperty(d,w,XInternAtom(d,"_NET_WM_PID",False),XA_CARDINAL,32,PropModeReplace,(unsigned char*)&pid,1);
  XSelectInput(d,w,KeyPressMask|KeyReleaseMask|StructureNotifyMask|FocusChangeMask);
  XMapWindow(d,w);
 }
 XSync(d,False); XSetInputFocus(d,windows[0],RevertToParent,CurrentTime); XSync(d,False);
 printf("READY %lu %lu\n",windows[0],windows[1]);fflush(stdout);
 for(;;) {
  XEvent e;XNextEvent(d,&e);
  if(e.type==MappingNotify)XRefreshKeyboardMapping(&e.xmapping);
  if(e.type==FocusIn || e.type==FocusOut) {
   printf("FOCUS %s %lu mode=%d detail=%d\n",e.type==FocusIn?"IN":"OUT",e.xfocus.window,e.xfocus.mode,e.xfocus.detail);fflush(stdout);
  }
  if(e.type==KeyPress || e.type==KeyRelease) {
   KeySym sym;char text[32];XLookupString(&e.xkey,text,sizeof(text),&sym,NULL);
   printf("EVENT %s %lu %lu %lu\n",e.type==KeyPress ? "DOWN":"UP",e.xkey.window,sym,e.xkey.time);fflush(stdout);
  }
 }
}
"""


def process_receipt(stdout, stderr, returncode):
    def text(value):
        return value.decode("utf-8", errors="replace") if isinstance(value, bytes) else value or ""
    stdout, stderr = text(stdout), text(stderr)
    result = {"exit_code": returncode, "stdout": stdout[-65536:], "stderr": stderr[-65536:]}
    try:
        result["receipt"] = json.loads(stdout)
    except ValueError as error:
        result["receipt_error"] = str(error)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="zuse-key-hold-x11-") as tmp:
        root = Path(tmp)
        (root / "fixture.c").write_text(FIXTURE)
        subprocess.run(["cc", str(root / "fixture.c"), "-o", str(root / "fixture"), "-lX11"], check=True)
        lease = xvfb = fixture = None
        auth = root / "Xauthority"
        report = {"native_x11_key_holds": "fail", "checks": {}}
        try:
            command, number, lease = private_xvfb_command("800x600x24", auth)
            xvfb = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            if not select.select([xvfb.stdout], [], [], 5)[0]:
                raise RuntimeError("private Xvfb did not become ready")
            display = verify_private_xvfb(xvfb, number, xvfb.stdout.readline(), auth)
            env = dict(os.environ, DISPLAY=display, XAUTHORITY=str(auth), ZUSE_BACKEND="x11")
            env.pop("WAYLAND_DISPLAY", None)
            env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
            report["private_display"] = display
            log_path = root / "events.log"
            with log_path.open("w") as log:
                fixture = subprocess.Popen([str(root / "fixture")], env=env, stdout=log, stderr=log)
                report["receiver_pid"] = fixture.pid
                def wait_for(predicate, process=None, label="fixture event"):
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline:
                        value = predicate()
                        if value:
                            return value
                        if process is not None and process.poll() not in (None, 0):
                            raise AssertionError(f"{label} absent; process exited with {process.returncode}")
                        time.sleep(.01)
                    raise AssertionError(f"private fixture event timeout: {label}")
                line = wait_for(lambda: next((line for line in log_path.read_text().splitlines() if line.startswith("READY ")), None), fixture, "receiver READY")
                first, second = line.split()[1:]
                report["receiver_windows"] = {
                    "first_decimal": first, "first_hex": hex(int(first)),
                    "second_decimal": second, "second_hex": hex(int(second)),
                }
                def events():
                    result = []
                    for line in log_path.read_text().splitlines():
                        if line.startswith("EVENT "):
                            _, action, window, key, timestamp = line.split()
                            result.append({"action": action, "window": window, "key": int(key), "ms": int(timestamp)})
                    return result
                def invoke(source):
                    output = subprocess.run([str(binary), "eval", source], env=env, capture_output=True, text=True, timeout=10)
                    evidence = process_receipt(output.stdout, output.stderr, output.returncode)
                    assert output.returncode == 0, evidence
                    assert "receipt" in evidence, evidence
                    receipt = evidence["receipt"]
                    return receipt["results"][-1]["value"]
                def observation():
                    # Every probe uses only this authenticated private display.
                    result = {"receiver_log": log_path.read_text()[-65536:],
                              "receiver_exit_code": fixture.poll()}
                    for name, command in [
                        ("native_x11", [str(root / "fixture"), "--probe"]),
                        ("zuse", [str(binary), "eval", "window.list(); keyboard.state()", "--read-only"]),
                    ]:
                        try:
                            output = subprocess.run(command, env=env, capture_output=True, text=True, timeout=3)
                            result[name] = process_receipt(output.stdout, output.stderr, output.returncode)
                        except subprocess.TimeoutExpired as error:
                            result[name] = dict(process_receipt(error.stdout, error.stderr, None), timed_out=True)
                        except OSError as error:
                            result[name] = {"probe_error": str(error)}
                    return result
                def hold(mode, stop=None):
                    name = stop or f"num_lock_{mode}"
                    check = {"status": "running", "mode": mode, "stop": stop}
                    report["checks"][name] = check
                    child = None
                    try:
                        invoke(f'window.focus(id="{first}")')
                        invoke(f'keyboard.num_lock(state="{mode}")')
                        initial = invoke('keyboard.state()')["current"]["numLock"]
                        before = len(events())
                        check["before_hold"] = observation()
                        source = f'keyboard.hold(key="Up",duration={5000 if stop else 180},window="{first}")'
                        check["source"] = source
                        child = subprocess.Popen([str(binary), "eval", source],
                                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                        wait_for(lambda: any(e["action"] == "DOWN" and e["key"] == 0xff52 for e in events()[before:]), child, "ArrowUp DOWN")
                        if stop:
                            time.sleep(.06)
                            if stop == "cancel":
                                child.send_signal(signal.SIGINT)
                            else:
                                invoke(f'window.focus(id="{second}")')
                        stdout, stderr = child.communicate(timeout=5)
                        check["cli"] = process_receipt(stdout, stderr, child.returncode)
                        receipt = json.loads(stdout)
                        wait_for(lambda: any(e["action"] == "UP" and e["key"] == 0xff52 for e in events()[before:]))
                        received = events()[before:]
                        down = next(e for e in received if e["action"] == "DOWN" and e["key"] == 0xff52)
                        up = next(e for e in received if e["action"] == "UP" and e["key"] == 0xff52)
                        elapsed = (up["ms"] - down["ms"]) & 0xffffffff
                        if stop:
                            assert child.returncode and receipt["error_code"] == ("cancelled" if stop == "cancel" else "stale_target"), receipt
                        else:
                            assert child.returncode == 0 and receipt["status"] == "completed", (receipt, stderr)
                            assert 140 <= elapsed <= 800, received
                        state = invoke('keyboard.state()')
                        assert state["keys_held"] is False and state["current"]["numLock"] == initial, state
                        check.update(status="pass", receiver_events=received, receiver_hold_ms=elapsed,
                                     keyboard_state=state, receipt=receipt)
                        return check
                    except BaseException as error:
                        check.update(status="fail", error_type=type(error).__name__, error=str(error))
                        raise
                    finally:
                        if child is not None and "cli" not in check:
                            check["exit_code_before_cleanup"] = child.poll()
                            if child.poll() is None:
                                check["cleanup_signal"] = "SIGINT"
                                child.send_signal(signal.SIGINT)
                            try:
                                stdout, stderr = child.communicate(timeout=3)
                            except subprocess.TimeoutExpired:
                                check["cleanup_signal"] = "SIGKILL"
                                child.kill()
                                stdout, stderr = child.communicate(timeout=3)
                            check["cli"] = process_receipt(stdout, stderr, child.returncode)
                        check["after_cleanup"] = observation()
                report["checks"]["num_lock_off"] = hold("off")
                report["checks"]["num_lock_on"] = hold("on")
                report["checks"]["cancelled"] = hold("off", "cancel")
                report["checks"]["focus_change"] = hold("off", "focus")
                report["native_x11_key_holds"] = "pass"
        except BaseException as error:
            report.update(error_type=type(error).__name__, error=str(error))
            raise
        finally:
            if 'log_path' in locals() and log_path.exists():
                report["receiver_log"] = log_path.read_text()[-65536:]
            for process in (fixture, xvfb):
                if process is not None and process.poll() is None:
                    process.terminate()
                    try: process.wait(timeout=3)
                    except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=3)
            if xvfb is not None:
                stdout, stderr = xvfb.communicate(timeout=3)
                report["xvfb"] = {"exit_code": xvfb.returncode, "stderr": stderr[-65536:]}
            release_private_xvfb(lease, auth)
            if args.report:
                args.report.parent.mkdir(parents=True, exist_ok=True)
                args.report.write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
