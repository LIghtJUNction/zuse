#![cfg(feature = "std")]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use zuse::runtime::{self, Options, Runtime};

#[test]
fn android_argument_errors_reject_the_complete_program() {
    for tail in [
        r#"android.type(text="中文")"#,
        r#"android.type(text="\u4f60")"#,
        r#"android.type(text="line\nbreak")"#,
        r#"android.type(text="%s")"#,
        r#"android.key(key="Ctrl+Enter")"#,
        r#"android.key(key="")"#,
        r#"android.start(component="not-a-component")"#,
        r#"android.start(component="/")"#,
        r#"android.start(component="com.example/")"#,
        r#"android.start(component="com.example/.Main/Other")"#,
    ] {
        // The first step cannot run: all argument errors must be rejected first.
        // With a fresh runtime this remains safe even if preflight regresses.
        let source = format!("android.tap(x=1,y=1);{tail}");
        assert!(
            runtime::check(&source, Options::default()).is_err(),
            "{tail}"
        );
        assert!(
            Runtime::new().execute(&source, Options::default()).is_err(),
            "{tail}"
        );
        assert!(
            Runtime::new()
                .execute(
                    &source,
                    Options {
                        dry_run: true,
                        ..Options::default()
                    }
                )
                .is_err(),
            "{tail}"
        );
    }
    let long_text = format!("android.type(text=\"{}\")", "a".repeat(4097));
    assert!(runtime::check(&long_text, Options::default()).is_err());
}

#[test]
fn printable_ascii_aliases_and_components_pass_preflight() {
    let source = r#"android.type(text="A b 'c' \"d\" %x");
        android.key(key="back"); android.key(key="KEYCODE_ENTER"); android.key(key="66");
        android.start(component="com.example/.MainActivity$Nested")"#;
    assert!(runtime::check(source, Options::default()).is_ok());
    let boundary = format!("android.type(text=\"{}\")", "a".repeat(4096));
    assert!(runtime::check(&boundary, Options::default()).is_ok());
}

enum Reply {
    Inventory,
    Ids(&'static str),
    Metadata(&'static str, bool),
}
fn service(stream: &mut TcpStream) -> String {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16).unwrap();
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}
fn packet(stream: &mut TcpStream, id: u8, bytes: &[u8]) {
    stream.write_all(&[id]).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    stream.write_all(bytes).unwrap();
}
fn server(replies: Vec<Reply>) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture connection timed out");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = service(&mut stream);
            stream.write_all(b"OKAY").unwrap();
            if matches!(reply, Reply::Inventory) {
                assert_eq!(request, "host:devices-l");
                let devices = b"fixture\tdevice model:Fixture\nnewtarget\tdevice model:Fixture\n";
                stream
                    .write_all(format!("{:04x}", devices.len()).as_bytes())
                    .unwrap();
                stream.write_all(devices).unwrap();
                continue;
            }
            let (serial, metadata, broken) = match reply {
                Reply::Ids(serial) => (serial, false, false),
                Reply::Metadata(serial, broken) => (serial, true, broken),
                Reply::Inventory => unreachable!(),
            };
            assert_eq!(request, format!("host:transport:{serial}"));
            let command = service(&mut stream);
            assert_eq!(
                command,
                if metadata {
                    "shell,v2,raw:cmd display get-displays"
                } else {
                    "shell,v2,raw:cmd display get-displays --ids-only"
                }
            );
            stream.write_all(b"OKAY").unwrap();
            let mut closed_stdin = [0; 5];
            stream.read_exact(&mut closed_stdin).unwrap();
            assert_eq!(closed_stdin, [4, 0, 0, 0, 0]);
            let text = if !metadata {
                "0\n"
            } else if broken {
                "invalid display metadata\n"
            } else {
                "Displays:\nDisplay id 0: DisplayInfo{\"Fixture\", displayId 0, uniqueId \"local:123\"}\n"
            };
            packet(&mut stream, 1, text.as_bytes());
            packet(&mut stream, 3, &[0]);
        }
    });
    (address, worker)
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
#[test]
fn failed_retarget_clears_the_previous_device() {
    for fail_metadata in [false, true] {
        let mut replies = vec![
            Reply::Inventory,
            Reply::Ids("fixture"),
            Reply::Ids("fixture"),
            Reply::Metadata("fixture", false),
            Reply::Inventory,
        ];
        let new_serial = if fail_metadata {
            replies.extend([
                Reply::Ids("newtarget"),
                Reply::Ids("newtarget"),
                Reply::Metadata("newtarget", true),
            ]);
            "newtarget"
        } else {
            "missing"
        };
        let (address, worker) = server(replies);
        let mut child = OwnedChild(Some(
            Command::new(env!("CARGO_BIN_EXE_zuse"))
                .arg("shell")
                .env("ZUSE_ADB_SERVER", address.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
        let source = format!(
            "android.connect(serial=\"fixture\")\nandroid.connect(serial=\"{new_serial}\")\nandroid.status()\n:quit\n"
        );
        child
            .0
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "fixture client timed out");
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.0.take().unwrap().wait_with_output().unwrap();
        worker.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{stdout}");
        assert!(!runtime::failed(lines[0]), "{stdout}");
        assert!(runtime::failed(lines[1]), "{stdout}");
        assert!(runtime::failed(lines[2]), "{stdout}");
        assert!(lines[2].contains("use android.connect first"), "{stdout}");
    }
}
