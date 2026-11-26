use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    path::Path,
    process::{Command, Output},
};

fn cli(args: &[&str], fonts: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(args)
        .env("ZUSE_FONT_DIR", fonts)
        .output()
        .unwrap()
}
fn success(args: &[&str], fonts: &Path) -> String {
    let out = cli(args, fonts);
    assert!(
        out.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zuse-write-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn installed_unicode_font_reaches_calibrated_screen_and_plotter() {
    let temp = Temp::new();
    let root = &temp.0;
    let file = root.join("source.zfont");
    std::fs::write(&file, "U+4E8C 0,0 1,0 | 0,1 1,1\n").unwrap();
    success(&["font", "add", "test", file.to_str().unwrap()], root);
    assert!(success(&["font", "list"], root).contains("test"));
    let args = r#"text="二二",font="test",size=2,advance=3,x=10,y=12,reference_units=2,reference_distance=4"#;
    let preview = success(
        &["--read-only", "-c", &format!("write.preview({args})")],
        root,
    );
    assert!(preview.contains("[[10,12],[14,12]]"), "{preview}");
    assert!(preview.contains("[[16,16],[20,16]]"), "{preview}");
    let file = root.join("sample.nc");
    let export = format!(
        "write.export({args},path={})",
        zuse::json::quote(file.to_str().unwrap())
    );
    success(&["--dry-run", "-c", &export], root);
    assert!(!file.exists());
    assert!(!cli(&["--read-only", "-c", &export], root).status.success());
    success(&["-c", &export], root);
    let nc = std::fs::read_to_string(&file).unwrap();
    assert!(nc.contains("G21"));
    success(
        &[
            "--dry-run",
            "machine",
            "grbl-send",
            file.to_str().unwrap(),
            if cfg!(windows) {
                "COM1"
            } else {
                "/dev/ttyUSB0"
            },
            "115200",
        ],
        root,
    );
    assert!(!cli(&["-c", &export], root).status.success());
    success(&["font", "remove", "test"], root);
    assert!(
        !cli(&["-c", &format!("write.preview({args})")], root)
            .status
            .success()
    );
}

#[test]
fn later_invalid_input_is_rejected_before_exporting() {
    let temp = Temp::new();
    let out = temp.0.join("must-not-exist.nc");
    let script = format!(
        "write.export(text=\"Z\",path={}); write.touchscreen(text=\"Z\",width=10,height=10,x=5000)",
        zuse::json::quote(out.to_str().unwrap())
    );
    assert!(!cli(&["-c", &script], &temp.0).status.success());
    assert!(!out.exists());
    let dry = cli(
        &[
            "--dry-run",
            "--max-wait-ms",
            "2",
            "-c",
            "write.mouse(text=\"Z\",interval=10)",
        ],
        &temp.0,
    );
    assert!(!dry.status.success());
}
#[test]
fn random_layout_can_be_replayed_on_another_backend() {
    let root = Temp::new();
    let args = r#"text="你好",size=8,offset_x=2,offset_y=1,x=20,y=30"#;
    let preview = success(&["-c", &format!("write.preview({args})")], &root.0);
    let seed = preview
        .split_once("\"seed\":")
        .unwrap()
        .1
        .split('}')
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let again = success(
        &["-c", &format!("write.preview({args},seed={seed})")],
        &root.0,
    );
    // The outer receipt also records the time spent on each execution.
    assert_eq!(
        preview.split_once("\"value\":").unwrap().1,
        again.split_once("\"value\":").unwrap().1
    );
    let calibrated = success(
        &[
            "-c",
            r#"write.gcode(text="一",x=12,y=30,size=4,pen_up=0,pen_down=2,swap_xy=true,flip_y=true)"#,
        ],
        &root.0,
    );
    assert!(calibrated.contains("G0 Z0.000") && calibrated.contains("G1 Z2.000"));
}
