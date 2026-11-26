//! Operator-measured limits and pen installation heights. Loading a profile only reads data.
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T> = std::result::Result<T, String>;
const VERSION: &str = "zuse-writer-workspace-v1";
const MAX_FILE: u64 = 8192;

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    device: String,
    controller: String,
    firmware: String,
    reference: String,
    offset: [f64; 3],
}

impl Binding {
    /// Use an observed stable hardware identity, not the current kernel TTY number.
    /// An observed work offset alone does not establish homing after a restart.
    pub fn new(
        device_id: &str,
        controller: &str,
        firmware: &str,
        work_offset: [f64; 3],
    ) -> Result<Self> {
        for value in [device_id, controller, firmware] {
            valid_label(value)?;
        }
        if device_id.starts_with("/dev/ttyUSB")
            || device_id.starts_with("/dev/ttyACM")
            || device_id.starts_with("/dev/pts/")
        {
            return Err("需要稳定的设备标识，临时串口名称不能关联标定档案".into());
        }
        if !work_offset.iter().all(|n| n.is_finite()) {
            return Err("需要有效的毫米工作偏移 WCO".into());
        }
        Self {
            device: device_id.into(),
            controller: controller.into(),
            firmware: firmware.into(),
            reference: String::new(),
            offset: work_offset.map(|v| if v == 0.0 { 0.0 } else { v }),
        }
        .with_reference("observed-work-offset")
    }

    /// Supply a trusted reference identifier when the controller or operator provides one.
    pub fn with_reference(mut self, reference: &str) -> Result<Self> {
        valid_label(reference)?;
        self.reference = reference.into();
        Ok(self)
    }

    pub fn work_offset(&self) -> [f64; 3] {
        self.offset
    }

    fn header(&self) -> String {
        let [x, y, z] = self.work_offset();
        format!(
            "{VERSION}\ndevice={}\ncontroller={}\nfirmware={}\nreference={}\nwco={x} {y} {z}\n",
            self.device, self.controller, self.firmware, self.reference
        )
    }

    fn file(&self, root: &Path) -> PathBuf {
        // Reversible lowercase encoding keeps punctuation and field boundaries exact on
        // case-insensitive filesystems. Split at a bounded component length for long IDs.
        let header = self.header();
        let mut path = root.to_path_buf();
        for piece in header.as_bytes()[VERSION.len() + 1..].chunks(80) {
            use std::fmt::Write as _;
            let mut component = String::with_capacity(piece.len() * 2);
            for byte in piece {
                write!(&mut component, "{byte:02x}").expect("format into String");
            }
            path.push(component);
        }
        path.join("workspace.profile")
    }
}

fn valid_label(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 240 || value.chars().any(char::is_control) {
        return Err("缺少有效的设备、固件或坐标参考标识".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceProfile {
    pub corner_a: [f64; 2],
    pub corner_b: [f64; 2],
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub upper_z: f64,
    pub lower_z: f64,
    pub mount_z: f64,
    pub clearance: f64,
    pub pen_up_z: f64,
    pub pen_down_z: f64,
}

impl WorkspaceProfile {
    /// Corners, physical upper/lower Z, and the resulting heights are in WPos millimetres.
    pub fn new(
        corner_a: [f64; 2],
        corner_b: [f64; 2],
        upper_z: f64,
        lower_z: f64,
        mount_z: Option<f64>,
        clearance: f64,
    ) -> Result<Self> {
        let mount_z = mount_z.unwrap_or_else(|| f64::midpoint(upper_z, lower_z));
        if !corner_a
            .into_iter()
            .chain(corner_b)
            .chain([upper_z, lower_z, mount_z, clearance])
            .all(f64::is_finite)
        {
            return Err("标定坐标和抬笔间隙必须是有限毫米数值".into());
        }
        let a = [corner_a[0], corner_a[1], upper_z];
        let b = [corner_b[0], corner_b[1], lower_z];
        let min: [f64; 3] = std::array::from_fn(|i| a[i].min(b[i]));
        let max: [f64; 3] = std::array::from_fn(|i| a[i].max(b[i]));
        if (0..3).any(|i| min[i] >= max[i] || !(max[i] - min[i]).is_finite()) {
            return Err("XY 两角必须形成有效的矩形，Z 上下限必须不同".into());
        }
        if !(min[2]..=max[2]).contains(&mount_z) {
            return Err("装笔高度超出实测 Z 范围".into());
        }
        let room = (upper_z - mount_z).abs();
        if clearance <= 0.0 || clearance > room {
            return Err(format!(
                "抬笔间隙必须大于 0，且不能超过装笔位至上限的 {room} mm"
            ));
        }
        // Taking the exact endpoint also avoids one-ULP overshoot when clearance == room.
        let pen_up_z = if clearance == room {
            upper_z
        } else {
            mount_z + (upper_z - lower_z).signum() * clearance
        };
        if pen_up_z == mount_z || !(min[2]..=max[2]).contains(&pen_up_z) {
            return Err("当前坐标精度下无法在 Z 范围内区分抬笔和落笔高度".into());
        }
        Ok(Self {
            corner_a,
            corner_b,
            min,
            max,
            upper_z,
            lower_z,
            mount_z,
            clearance,
            pen_up_z,
            pen_down_z: mount_z,
        })
    }

    pub fn up_sign(&self) -> f64 {
        (self.upper_z - self.lower_z).signum()
    }

    fn encode(&self, binding: &Binding) -> Result<String> {
        let validated = Self::new(
            self.corner_a,
            self.corner_b,
            self.upper_z,
            self.lower_z,
            Some(self.mount_z),
            self.clearance,
        )?;
        if &validated != self {
            return Err("标定档案包含不一致的派生范围或笔位".into());
        }
        let [ax, ay] = self.corner_a;
        let [bx, by] = self.corner_b;
        Ok(format!(
            "{}measurements={ax} {ay} {bx} {by} {} {} {} {}\n",
            binding.header(),
            self.upper_z,
            self.lower_z,
            self.mount_z,
            self.clearance
        ))
    }

    fn decode(binding: &Binding, body: &str) -> Result<Self> {
        let data = body
            .strip_prefix(&binding.header())
            .and_then(|s| s.strip_prefix("measurements="))
            .and_then(|s| s.strip_suffix('\n'))
            .ok_or("标定档案版本、设备或坐标参考不匹配")?;
        if data.contains('\n') {
            return Err("标定档案包含多余记录".into());
        }
        let numbers: Vec<f64> = data
            .split_ascii_whitespace()
            .map(|s| s.parse::<f64>().map_err(|_| "标定档案数值无效"))
            .collect::<std::result::Result<_, _>>()?;
        let [ax, ay, bx, by, upper, lower, mount, clearance] = numbers.as_slice() else {
            return Err("标定档案的测量字段不完整".into());
        };
        Self::new(
            [*ax, *ay],
            [*bx, *by],
            *upper,
            *lower,
            Some(*mount),
            *clearance,
        )
    }
}

fn config_root() -> Result<PathBuf> {
    let absolute_env = |name| {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    #[cfg(windows)]
    let base = absolute_env("APPDATA");
    #[cfg(not(windows))]
    let base = absolute_env("XDG_CONFIG_HOME").or_else(|| {
        absolute_env("HOME").map(|home| {
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support")
            } else {
                home.join(".config")
            }
        })
    });
    Ok(base
        .ok_or("找不到用户配置目录")?
        .join("zuse")
        .join("writer")
        .join("profiles"))
}

/// Save only this exact binding. Other devices, firmware, offsets, and references keep their records.
pub fn save_profile(binding: &Binding, profile: &WorkspaceProfile) -> Result<()> {
    save_to(&config_root()?, binding, profile)
}

/// Read an inactive candidate. No bounds, pen height, or motion are installed by this function.
pub fn load_profile(binding: &Binding) -> Result<Option<WorkspaceProfile>> {
    load_from(&config_root()?, binding)
}

fn save_to(root: &Path, binding: &Binding, profile: &WorkspaceProfile) -> Result<()> {
    let body = profile.encode(binding)?;
    let path = binding.file(root);
    let parent = path.parent().expect("profile directory");
    let mut dirs = fs::DirBuilder::new();
    dirs.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dirs.mode(0o700);
    }
    dirs.create(parent).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let name = format!(
            ".workspace-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let temp = parent.join(name);
        match options.open(&temp) {
            Ok(f) => break (temp, f),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let outcome = (|| -> io::Result<()> {
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(temp);
    }
    outcome.map_err(|e| e.to_string())
}

fn load_from(root: &Path, binding: &Binding) -> Result<Option<WorkspaceProfile>> {
    let file = match File::open(binding.file(root)) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        res => res.map_err(|e| e.to_string())?,
    };
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FILE {
        return Err("标定档案超出尺寸限制".into());
    }
    WorkspaceProfile::decode(binding, &text).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding::new(
            "unit/étalon:17",
            "BtWriter",
            "1.3a.20221114",
            [2.5, 0.0, -1.0],
        )
        .unwrap()
    }

    fn profile(mount: Option<f64>) -> WorkspaceProfile {
        WorkspaceProfile::new([90.0, -20.0], [-10.0, 60.0], -4.0, 6.0, mount, 2.0).unwrap()
    }

    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            static INDEX: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = env::temp_dir().join(format!(
                    "zuse-profile-{}-{}",
                    std::process::id(),
                    INDEX.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("temporary directory: {e}"),
                }
            }
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn captured_corners_and_physical_z_direction_survive_either_axis_convention() {
        let p = profile(None);
        assert_eq!((p.corner_a, p.corner_b), ([90., -20.], [-10., 60.]));
        assert_eq!((p.min, p.max), ([-10., -20., -4.], [90., 60., 6.]));
        assert_eq!(
            (p.up_sign(), p.mount_z, p.pen_down_z, p.pen_up_z),
            (-1., 1., 1., -1.)
        );
        let p = WorkspaceProfile::new(p.corner_b, p.corner_a, 6., -4., Some(5.), 1.).unwrap();
        assert_eq!((p.up_sign(), p.pen_down_z, p.pen_up_z), (1., 5., 6.));
    }

    #[test]
    fn invalid_or_unrepresentable_geometry_does_not_produce_heights() {
        let build = |a, b, u, l, m, c| WorkspaceProfile::new(a, b, u, l, m, c);
        for (a, b, u, l, m, c) in [
            ([0., 0.], [0., 1.], 3., 0., None, 1.),
            ([0., 0.], [1., 0.], 3., 0., None, 1.),
            ([0., 0.], [1., 1.], 3., 3., None, 1.),
            ([0., 0.], [1., 1.], 3., 0., Some(-1.), 1.),
            ([0., 0.], [1., 1.], 3., 0., Some(3.), 1.),
            ([0., 0.], [1., 1.], 3., 0., None, 0.),
            ([0., 0.], [1., 1.], 3., 0., None, 2.),
            ([0., 0.], [1., 1.], f64::INFINITY, 0., None, 1.),
            ([0., 0.], [1., 1.], 3., 0., Some(f64::NAN), 1.),
            ([0., 0.], [f64::MAX, 1.], 1e100, 1e99, None, 0.1),
            ([-f64::MAX, 0.], [f64::MAX, 1.], 3., 0., None, 1.),
        ] {
            assert!(build(a, b, u, l, m, c).is_err());
        }
    }

    #[test]
    fn file_round_trip_keeps_identity_reference_and_wco_separate() {
        let root = Folder::new();
        let d = binding();
        assert_eq!(d.work_offset(), [2.5, 0., -1.]);
        assert_eq!(load_from(&root.0, &d).unwrap(), None);
        save_to(&root.0, &d, &profile(None)).unwrap();
        let mut altered = profile(None);
        altered.pen_up_z = 123.;
        assert!(save_to(&root.0, &d, &altered).is_err());
        assert_eq!(load_from(&root.0, &d).unwrap(), Some(profile(None)));
        let others = [
            Binding::new(
                "unit/étalon:18",
                "BtWriter",
                "1.3a.20221114",
                d.work_offset(),
            )
            .unwrap(),
            Binding::new("unit/étalon:17", "BtWriter", "other-fw", d.work_offset()).unwrap(),
            Binding::new("unit/étalon:17", "GRBL", "1.3a.20221114", d.work_offset()).unwrap(),
            Binding::new(
                "unit/étalon:17",
                "BtWriter",
                "1.3a.20221114",
                [2.501, 0., -1.],
            )
            .unwrap(),
            d.clone().with_reference("homed-fixture-02").unwrap(),
        ];
        for other in &others {
            assert_eq!(load_from(&root.0, other).unwrap(), None);
            save_to(&root.0, other, &profile(Some(0.5))).unwrap();
        }
        save_to(&root.0, &d, &profile(Some(2.))).unwrap();
        assert_eq!(load_from(&root.0, &d).unwrap(), Some(profile(Some(2.))));
        for other in &others {
            assert_eq!(load_from(&root.0, other).unwrap(), Some(profile(Some(0.5))));
        }
    }

    #[test]
    fn rejects_invalid_identity_and_mismatched_or_damaged_records() {
        let root = Folder::new();
        let d = binding();
        for id in ["", "x\ny", "/dev/ttyUSB0", "/dev/ttyACM1", "/dev/pts/2"] {
            assert!(Binding::new(id, "grbl", "1.1", [0.; 3]).is_err());
        }
        assert!(Binding::new("17", "grbl", "1.1", [f64::NAN, 0., 0.]).is_err());
        let negative_zero = Binding::new(
            "unit/étalon:17",
            "BtWriter",
            "1.3a.20221114",
            [2.5, -0., -1.],
        )
        .unwrap();
        assert_eq!(d.file(&root.0), negative_zero.file(&root.0));
        save_to(&root.0, &d, &profile(None)).unwrap();
        let path = d.file(&root.0);
        let original = fs::read_to_string(&path).unwrap();
        for bad in [
            original.replace("firmware=1.3a.20221114", "firmware=other"),
            original.replace("measurements=", "measurements=NaN "),
            format!("{original}extra\n"),
            "x".repeat(MAX_FILE as usize + 1),
        ] {
            fs::write(&path, bad).unwrap();
            assert!(load_from(&root.0, &d).is_err());
        }
    }

    #[test]
    fn concurrent_replacement_always_leaves_a_whole_valid_profile() {
        let root = Folder::new();
        let d = binding();
        save_to(&root.0, &d, &profile(None)).unwrap();
        std::thread::scope(|scope| {
            for choice in [0.0, 1.0, 2.0] {
                let (root, d) = (&root.0, &d);
                scope.spawn(move || {
                    for _ in 0..3 {
                        save_to(root, d, &profile(Some(choice))).unwrap();
                        assert!(load_from(root, d).unwrap().is_some());
                    }
                });
            }
        });
        let final_p = load_from(&root.0, &d).unwrap().unwrap();
        assert!([0., 1., 2.].contains(&final_p.mount_z));
        let path = d.file(&root.0);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
