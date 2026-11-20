#![cfg(feature = "std")]

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use zuse::android::Android;

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 16, 84, 50, 6, 0, 0, 174,
    0, 103, 21, 30, 161, 95, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-android-capture-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn service(stream: &mut TcpStream) -> String {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16).unwrap();
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}
fn packet(stream: &mut TcpStream, kind: u8, bytes: &[u8]) {
    stream.write_all(&[kind]).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    stream.write_all(bytes).unwrap();
}

fn capture_server(images: Vec<Vec<u8>>) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let mut replies = vec![
            (
                "host:devices-l".to_owned(),
                b"fixture\tdevice model:Fixture\n".to_vec(),
            ),
            (
                "cmd display get-displays --ids-only".to_owned(),
                b"0\n".to_vec(),
            ),
        ];
        for image in images {
            replies.extend([
                ("cmd display get-displays --ids-only".to_owned(), b"0\n".to_vec()),
                (
                    "cmd display get-displays".to_owned(),
                    b"Display id 0: DisplayInfo{\"built-in\", type INTERNAL, uniqueId \"local:123\"}\n".to_vec(),
                ),
                (
                    "dumpsys SurfaceFlinger --display-id".to_owned(),
                    b"Display 123 (HWC display 0): displayName=\"\"\n".to_vec(),
                ),
                ("screencap -p -d 123".to_owned(), image),
            ]);
        }
        for (command, response) in replies {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing ADB request: {command}");
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
            if command == "host:devices-l" {
                assert_eq!(request, command);
                stream
                    .write_all(format!("{:04x}", response.len()).as_bytes())
                    .unwrap();
                stream.write_all(&response).unwrap();
            } else {
                assert_eq!(request, "host:transport:fixture");
                assert_eq!(service(&mut stream), format!("shell,v2,raw:{command}"));
                stream.write_all(b"OKAY").unwrap();
                let mut closed_stdin = [0; 5];
                stream.read_exact(&mut closed_stdin).unwrap();
                assert_eq!(closed_stdin, [4, 0, 0, 0, 0]);
                packet(&mut stream, 1, &response);
                packet(&mut stream, 3, &[0]);
            }
        }
    });
    (address, worker)
}

#[test]
fn truncated_png_is_rejected_without_replacing_previous_capture() {
    let directory = Directory::new();
    let path = directory.0.join("screen.png");
    fs::write(&path, b"previous screenshot").unwrap();
    let (address, worker) = capture_server(vec![PNG[..24].to_vec()]);
    let device = Android::connect_at(address, Some("fixture"), "foreground", 0).unwrap();
    let result = device.screenshot(&path);
    worker.join().unwrap();
    assert!(result.is_err(), "truncated screenshot accepted: {result:?}");
    assert_eq!(fs::read(&path).unwrap(), b"previous screenshot");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn incomplete_png_chunks_preserve_the_previous_capture() {
    let mut no_data = PNG[..33].to_vec();
    no_data.extend_from_slice(&PNG[PNG.len() - 12..]);
    let mut trailing = PNG.to_vec();
    trailing.push(0);
    let mut oversized_chunk = PNG.to_vec();
    oversized_chunk[33..37].copy_from_slice(&u32::MAX.to_be_bytes());
    let images = vec![
        PNG[..33].to_vec(),
        PNG[..PNG.len() - 1].to_vec(),
        no_data,
        trailing,
        oversized_chunk,
    ];
    let (address, worker) = capture_server(images.clone());
    let device = Android::connect_at(address, Some("fixture"), "foreground", 0).unwrap();
    let directory = Directory::new();
    let path = directory.0.join("screen.png");
    fs::write(&path, b"previous screenshot").unwrap();
    for image in images {
        assert!(
            device.screenshot(&path).is_err(),
            "accepted {} bytes",
            image.len()
        );
        assert_eq!(fs::read(&path).unwrap(), b"previous screenshot");
    }
    worker.join().unwrap();
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn complete_png_replaces_destination_and_reports_dimensions() {
    let (address, worker) = capture_server(vec![PNG.to_vec()]);
    let device = Android::connect_at(address, Some("fixture"), "foreground", 0).unwrap();
    let directory = Directory::new();
    let path = directory.0.join("screen.png");
    fs::write(&path, b"previous screenshot").unwrap();
    let result = device.screenshot(&path).unwrap();
    worker.join().unwrap();
    zuse::json::with_value(&result, |value| {
        assert_eq!(value.get("width").unwrap().integer().unwrap(), 1);
        assert_eq!(value.get("height").unwrap().integer().unwrap(), 1);
        assert_eq!(value.get("display").unwrap().integer().unwrap(), 0);
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), PNG);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn failed_destination_replace_cleans_the_staged_image() {
    let (address, worker) = capture_server(vec![PNG.to_vec()]);
    let device = Android::connect_at(address, Some("fixture"), "foreground", 0).unwrap();
    let directory = Directory::new();
    let path = directory.0.join("screen.png");
    fs::create_dir(&path).unwrap();
    assert!(device.screenshot(&path).is_err());
    worker.join().unwrap();
    assert!(path.is_dir());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn invalid_capture_path_fails_preflight_and_does_not_query_the_device() {
    let source = r#"wait(ms=0);android.capture(path="")"#;
    assert!(zuse::runtime::check(source, zuse::runtime::Options::default()).is_err());
    assert!(
        zuse::runtime::Runtime::new()
            .execute(source, zuse::runtime::Options::default())
            .is_err()
    );
    let (address, worker) = capture_server(Vec::new());
    let device = Android::connect_at(address, Some("fixture"), "foreground", 0).unwrap();
    assert!(device.screenshot(std::path::Path::new("")).is_err());
    worker.join().unwrap();
}
