#!/usr/bin/env python3
"""Run fixture-only input in an owned nested Hyprland and optional private IME.

The host sees one temporary compositor window. A private D-Bus server has no
service activation directories; configuration, data, cache and Wayland sockets
are temporary. Existing applications and the user's IME are never test targets.
"""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import tempfile
import time

ROOT=Path(__file__).resolve().parents[1]
CONFIG="""
hl.monitor({output="", mode="800x600@60", position="0x0", scale=1})
hl.config({
  input={kb_layout="us", kb_variant="", kb_options="", follow_mouse=0},
  general={gaps_in=0, gaps_out=0, border_size=0},
  decoration={rounding=0, blur={enabled=false}, shadow={enabled=false}},
  animations={enabled=false},
  misc={disable_hyprland_logo=true, disable_splash_rendering=true},
  xwayland={enabled=false},
  cursor={no_warps=true},
  debug={enable_stdout_logs=true}
})
"""
BUS_CONFIG="""<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy>
</busconfig>
"""
def run(args,**kw):
    return subprocess.run(args,capture_output=True,text=True,timeout=20,**kw)
def terminate(process):
    if process is not None and process.poll() is None:
        os.killpg(process.pid,signal.SIGTERM)
        try:process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=3)
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=ROOT/"target/native-validation/debug/zuse")
    parser.add_argument("--report",type=Path,default=ROOT/"target/native-wayland-private.json")
    parser.add_argument("--with-ime",action="store_true")
    parser.add_argument("--state-tests",action="store_true")
    parser.add_argument("--ascii-background",action="store_true")
    args=parser.parse_args()
    host=os.environ.copy()
    original=json.loads(run(["hyprctl","-j","activewindow"],env=host).stdout).get("address")
    cursor=json.loads(run(["hyprctl","-j","cursorpos"],env=host).stdout)
    parent_display=Path(host["WAYLAND_DISPLAY"])
    if not parent_display.is_absolute():
        parent_display=Path(host["XDG_RUNTIME_DIR"])/parent_display
    def restore():
        if original:
            run(["hyprctl","eval","hl.dispatch(hl.dsp.focus({window="+json.dumps("address:"+original)+"}))"],env=host)
        run(["hyprctl","eval",f'hl.dispatch(hl.dsp.cursor.move({{x={cursor["x"]},y={cursor["y"]}}}))'],env=host)
    with tempfile.TemporaryDirectory(prefix="zw-") as directory:
        directory=Path(directory)
        runtime=directory/"r";runtime.mkdir(mode=0o700)
        config=directory/"hyprland.lua";config.write_text(CONFIG)
        bus_config=directory/"dbus.conf";bus_config.write_text(BUS_CONFIG)
        for name in ("config","cache","data"):(directory/name).mkdir(mode=0o700)
        log=(directory/"private.log").open("w")
        env=host.copy()
        for name in ("HYPRLAND_INSTANCE_SIGNATURE","DISPLAY","NOTIFY_SOCKET","DBUS_SESSION_BUS_ADDRESS","GTK_IM_MODULE"):
            env.pop(name,None)
        env.update(XDG_RUNTIME_DIR=str(runtime),WAYLAND_DISPLAY=str(parent_display),
                   HYPRLAND_NO_SD_VARS="1",HYPRLAND_NO_SD_NOTIFY="1",HYPRLAND_NO_RT="1",
                   HYPRLAND_NO_CRASHREPORTER="1",AQ_DRM_DEVICES="/dev/null",NO_AT_BRIDGE="1",
                   XDG_CONFIG_HOME=str(directory/"config"),XDG_CACHE_HOME=str(directory/"cache"),
                   XDG_DATA_HOME=str(directory/"data"),GIO_USE_VFS="local",GSETTINGS_BACKEND="memory",
                   GTK_A11Y="none",GTK_USE_PORTAL="0")
        bus=child=ime=None
        try:
            bus=subprocess.Popen(["dbus-daemon","--config-file="+str(bus_config),"--nofork","--print-address=1"],
                                 env=env,stdout=subprocess.PIPE,stderr=log,text=True,start_new_session=True)
            if not select.select([bus.stdout],[],[],3)[0]:
                raise RuntimeError("private D-Bus startup timed out")
            address=bus.stdout.readline().strip()
            if not address.startswith("unix:"):
                raise RuntimeError("private D-Bus did not report a Unix address")
            env["DBUS_SESSION_BUS_ADDRESS"]=address
            child=subprocess.Popen(["Hyprland","--config",str(config)],env=env,
                                   stdout=log,stderr=log,start_new_session=True)
            deadline=time.monotonic()+12
            while time.monotonic()<deadline:
                sockets=list((runtime/"hypr").glob("*/.socket.sock")) if (runtime/"hypr").is_dir() else []
                displays=[p for p in runtime.glob("wayland-*") if p.is_socket()]
                if sockets and displays:break
                if child.poll() is not None:raise RuntimeError("private compositor exited")
                time.sleep(.05)
            else:raise RuntimeError("private compositor startup timed out")
            assert len(sockets)==1 and len(displays)==1
            env["HYPRLAND_INSTANCE_SIGNATURE"]=sockets[0].parent.name
            env["WAYLAND_DISPLAY"]=displays[0].name
            env["ZUSE_BACKEND"]="wayland"
            env["ZUSE_PRIVATE_WAYLAND_FIXTURE"]="1"
            restore()
            if args.with_ime:
                ime=subprocess.Popen(["fcitx5","--disable","all","--enable","wayland,waylandim,keyboard,dbus,dbusfrontend"],
                                     env=env,stdout=log,stderr=log,start_new_session=True)
                time.sleep(.3)
                if ime.poll() is not None:raise RuntimeError("private IME exited")
                env["ZUSE_TEST_EXPECT_FCITX"]="1"
            command=["python3",str(ROOT/"tests/native_wayland.py"),"--allow-desktop",
                     "--binary",str(args.binary.resolve()),"--report",str(args.report.resolve())]
            if args.ascii_background:command.append("--ascii-background")
            if args.state_tests:command.append("--state-tests")
            result=subprocess.run(command,env=env,timeout=60)
            return result.returncode
        except Exception as error:
            log.flush()
            raise RuntimeError(str(error)+"; private log: "+(directory/"private.log").read_text()[-6000:]) from error
        finally:
            terminate(ime);terminate(child);terminate(bus);log.close()
            if child is not None:
                current=json.loads(run(["hyprctl","-j","activewindow"],env=host).stdout)
                if current.get("pid")==child.pid or not current.get("address"):restore()
if __name__=="__main__":
    raise SystemExit(main())
