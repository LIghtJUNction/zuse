#![cfg(all(feature = "std", target_os = "linux"))]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant, SystemTime},
};

use zuse::json::{quote, with_value};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x04\x00\x00\x00\xb5\x1c\x0c\x02\x00\x00\x00\x0bIDAT\x78\xda\x63\xfc\xff\x1f\x00\x03\x03\x02\x00\xef\xbf\x68\x99\x00\x00\x00\x00IEND\xae\x42\x60\x82";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-wayland-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn cli(&self, socket: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zuse"));
        command
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .env_remove("SWAYSOCK")
            .env_remove("NIRI_SOCKET")
            .env("ZUSE_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", "zuse-isolated-fixture")
            .env("PATH", format!("{}:/usr/bin:/bin", self.0.display()))
            .env(socket, self.0.join(socket));
        command
    }

    fn server(&self, name: &str, replies: Vec<(u32, String, String)>) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(self.0.join(name)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let sway = name == "SWAYSOCK";
        thread::spawn(move || {
            for (kind, request, reply) in replies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing IPC request: {request}");
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                if sway {
                    let mut header = [0; 14];
                    stream.read_exact(&mut header).unwrap();
                    assert_eq!(&header[..6], b"i3-ipc");
                    assert_eq!(header[10..], kind.to_ne_bytes());
                    let length = u32::from_ne_bytes(header[6..10].try_into().unwrap());
                    assert_eq!(length as usize, request.len());
                    let mut payload = vec![0; length as usize];
                    stream.read_exact(&mut payload).unwrap();
                    assert_eq!(payload, request.as_bytes());
                    header[6..10].copy_from_slice(&(reply.len() as u32).to_ne_bytes());
                    stream.write_all(&header).unwrap();
                    stream.write_all(reply.as_bytes()).unwrap();
                } else {
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    assert_eq!(line.trim_end(), request);
                    writeln!(stream, "{reply}").unwrap();
                }
            }
        })
    }

    fn grim(&self, contents: &str) {
        let path = self.0.join("grim");
        fs::write(&path, format!("#!/bin/sh\n{contents}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn error_contains(output: Output, message: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && (stdout.contains(message) || stderr.contains(message)),
        "expected failure containing {message}: {output:?}"
    );
}

fn sway_tree(x: i32, focused: bool, visible: bool) -> String {
    format!(
        r#"{{"type":"root","nodes":[{{"type":"output","nodes":[{{"type":"workspace",
        "nodes":[{{"id":42,"name":"Editor","app_id":"editor","pid":123,"focused":{focused},
          "visible":{visible},"rect":{{"x":{x},"y":8,"width":60,"height":40}}}}],
        "floating_nodes":[{{"id":57,"name":"Game","window":999,"window_properties":{{"class":"Game.exe"}}}}]
    }}]}}]}}"#
    )
}

fn capture(path: &Path) -> String {
    format!(
        "window.capture(id=\"sway:42\", path={})",
        quote(path.to_str().unwrap())
    )
}

#[test]
fn sway_list_focus_and_atomic_capture_through_cli() {
    let fixture = Fixture::new();
    let before = sway_tree(-25, false, true);
    let focused = sway_tree(-25, true, true);
    let server = fixture.server(
        "SWAYSOCK",
        vec![
            (4, String::new(), before.clone()), // doctor
            (4, String::new(), before.clone()), // list
            (4, String::new(), before),         // focus preflight
            (
                0,
                "[con_id=42] focus".into(),
                r#"[{"success":true}]"#.into(),
            ),
            (4, String::new(), focused.clone()), // focus readback
            (4, String::new(), focused.clone()), // initial capture
            (4, String::new(), focused.clone()), // stable capture readback
            (4, String::new(), focused.clone()), // next capture
            (4, String::new(), sway_tree(-30, true, true)), // moved during capture
            (4, String::new(), sway_tree(-25, false, false)), // hidden
            (4, String::new(), focused),         // truncated PNG
        ],
    );

    let doctor = success(fixture.cli("SWAYSOCK").arg("doctor").output().unwrap());
    with_value(&doctor, |root| {
        let desktop = root.get("desktop").unwrap();
        assert!(
            desktop
                .get("compositor")
                .unwrap()
                .string()
                .unwrap()
                .matches("sway")
        );
        assert_eq!(desktop.get("windows").unwrap().boolean(), Some(true));
        Ok(())
    })
    .unwrap();

    let listed = success(
        fixture
            .cli("SWAYSOCK")
            .args(["-c", "window.list()"])
            .output()
            .unwrap(),
    );
    assert!(listed.contains(r#""id":"sway:42""#), "{listed}");
    assert!(listed.contains(r#""class":"Game.exe""#), "{listed}");
    assert!(listed.contains(r#""wine":true"#), "{listed}");
    success(
        fixture
            .cli("SWAYSOCK")
            .args(["-c", r#"window.focus(id="sway:42")"#])
            .output()
            .unwrap(),
    );

    let image_path = fixture.0.join("capture.png");
    let fixture_image = fixture.0.join("fixture.png");
    fs::write(&fixture_image, PNG).unwrap();
    fixture.grim(
        r#"
        [ "$#" -eq 3 ] && [ "$1" = "-g" ] && [ "$2" = "-25,8 60x40" ] && [ "$3" = "-" ] || exit 3
        head -c 100000 /dev/zero >&2
        cat "$ZUSE_TEST_PNG"
    "#,
    );
    let run_capture = || {
        fixture
            .cli("SWAYSOCK")
            .env("ZUSE_TEST_PNG", &fixture_image)
            .args(["-c", &capture(&image_path)])
            .output()
            .unwrap()
    };
    let result = success(run_capture());
    assert!(
        result.contains(r#""logicalRegion":{"x":-25,"y":8,"width":60,"height":40}"#),
        "{result}"
    );
    assert_eq!(fs::read(&image_path).unwrap(), PNG);

    fs::write(&image_path, b"previous").unwrap();
    error_contains(run_capture(), "geometry changed during capture");
    error_contains(run_capture(), "target is not visible");
    fixture.grim(r#"head -c 32 "$ZUSE_TEST_PNG""#);
    error_contains(run_capture(), "complete PNG");
    assert_eq!(fs::read(&image_path).unwrap(), b"previous");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 4);
    server.join().unwrap();
}

#[test]
fn niri_uses_exact_ids_and_requires_focus_readback() {
    let fixture = Fixture::new();
    let windows = r#"{"Ok":{"Windows":[{"id":9223372036854775810,"title":"Terminal","app_id":"foot","is_focused":false}]}}"#;
    let focus = r#"{"Action":{"FocusWindow":{"id":9223372036854775810}}}"#;
    let server = fixture.server(
        "NIRI_SOCKET",
        vec![
            (0, r#""Windows""#.into(), windows.into()),
            (0, r#""Windows""#.into(), windows.into()),
            (0, focus.into(), r#"{"Ok":"Handled"}"#.into()),
            (0, r#""Windows""#.into(), windows.replace("false", "true")),
            (0, r#""Windows""#.into(), windows.into()),
            (0, focus.into(), r#"{"Ok":"Handled"}"#.into()),
            (0, r#""Windows""#.into(), windows.into()),
        ],
    );
    let run = |source| {
        fixture
            .cli("NIRI_SOCKET")
            .args(["-c", source])
            .output()
            .unwrap()
    };

    let listed = success(run("window.list()"));
    assert!(
        listed.contains(r#""id":"niri:9223372036854775810""#),
        "{listed}"
    );
    assert!(
        listed.contains(r#""x":null,"y":null,"width":null,"height":null"#),
        "{listed}"
    );
    let exact_focus = r#"window.focus(id="niri:9223372036854775810")"#;
    success(run(exact_focus));
    error_contains(run(exact_focus), "did not focus the selected window");
    error_contains(
        run(r#"window.focus(id="niri:12;focus")"#),
        "invalid compositor window ID",
    );
    fixture.grim("exit 99");
    error_contains(
        run(r#"window.capture(id="active",path="unused.png")"#),
        "niri window screen geometry",
    );
    server.join().unwrap();
}
