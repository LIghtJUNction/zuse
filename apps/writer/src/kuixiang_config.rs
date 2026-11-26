//! Read saved settings from a selected KuiXiang installation without launching it.
use crate::{
    controller::Form,
    kuixiang_page::{self, Preset},
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};
use zuse::json;

const BOTTLE: &str = "bottles/bottles/sys-wine/drive_c/Program Files (x86)/kdraw(x64)";
const FILES: [&str; 5] = [
    "MachineControllerData.json",
    "settings.json",
    "page_settings.json",
    "AxesData.json",
    "FormatData.json",
];

pub(crate) fn read_file(path: &Path) -> Result<String, String> {
    const LIMIT: u64 = 2 * 1024 * 1024;
    let read = || -> io::Result<String> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.len() > LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "需要 2 MiB 以内的普通 JSON 文件",
            ));
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "需要普通 JSON 文件",
            ));
        }
        let mut input = String::new();
        file.take(LIMIT + 1).read_to_string(&mut input)?;
        if input.len() as u64 > LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON 超过 2 MiB",
            ));
        }
        Ok(input.trim_start_matches('\u{feff}').to_owned())
    };
    read().map_err(|e| format!("{}: {e}", path.display()))
}

fn files(root: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    // Use the actual file names. Different releases place them in different subdirectories.
    let mut found = BTreeMap::new();
    let mut pending = vec![(root.to_owned(), 0)];
    let mut entries = 0;
    while let Some((dir, depth)) = pending.pop() {
        for item in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let item = item.map_err(|e| format!("{}: {e}", dir.display()))?;
            entries += 1;
            if entries > 20_000 {
                return Err(format!(
                    "{}: 目录过大，请选择更具体的配置目录",
                    root.display()
                ));
            }
            let kind = item
                .file_type()
                .map_err(|e| format!("{}: {e}", item.path().display()))?;
            if kind.is_dir() && depth < 5 {
                pending.push((item.path(), depth + 1));
            } else if let Some(name) = item.file_name().to_str()
                && FILES.contains(&name)
            {
                if !kind.is_file() {
                    return Err(format!("{}: 需要普通配置文件", item.path().display()));
                }
                if let Some(previous) = found.insert(name.to_owned(), item.path()) {
                    return Err(format!(
                        "配置名重复，请选择具体配置目录：{}，{}",
                        previous.display(),
                        item.path().display()
                    ));
                }
            }
        }
    }
    Ok(found)
}

// Keep scalar types intact and reject duplicate or missing required keys.
fn raw_field(input: &str, key: &str) -> Result<Option<String>, String> {
    json::with_value(input, |root| {
        if !root.raw().starts_with('{') {
            return Err("需要 JSON 对象".into());
        }
        let mut values = root.members().filter(|(k, _)| k.matches(key));
        let raw = values.next().map(|(_, value)| value.raw().to_owned());
        if values.next().is_some() {
            return Err(format!("字段重复：{key}"));
        }
        Ok(raw)
    })
}

fn required(input: &str, key: &str) -> Result<String, String> {
    raw_field(input, key)?.ok_or_else(|| format!("缺少 {key}"))
}

fn positive(input: &str, key: &str) -> Result<f64, String> {
    required(input, key)?
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.)
        .ok_or_else(|| format!("{key} 需要有限正数"))
}

#[derive(Debug, PartialEq)]
struct Axes {
    position: String,
    swap: bool,
    reverse_x: bool,
    reverse_y: bool,
}

struct Profile {
    location: PathBuf,
    baud: u32,
    distance: f64,
    jog_feed: f64,
    write_feed: f64,
    page: Preset,
    axes: Axes,
    font: Option<String>,
}

fn load(root: &Path) -> Result<Profile, String> {
    let found = files(root)?;
    let read = |name: &str| {
        let path = found
            .get(name)
            .ok_or_else(|| format!("{}: 缺少 {name}", root.display()))?;
        read_file(path)
    };
    let machine = read(FILES[0])?;
    let settings = read(FILES[1])?;
    let page = kuixiang_page::default_preset(&read(FILES[2])?)?;
    let axes = read(FILES[3])?;
    let parse_bool = |key| {
        required(&axes, key)?
            .parse::<bool>()
            .map_err(|_| format!("{key} 需要布尔值"))
    };
    let baud = json::with_value(&required(&machine, "baud")?, json::text)?
        .parse::<u32>()
        .map_err(|_| "baud 需要十进制波特率")?;
    zuse::serial::validate_baud(baud)?;
    let font = match found.get(FILES[4]) {
        Some(path) => raw_field(&read_file(path)?, "fontsName")?
            .map(|s| json::with_value(&s, json::text))
            .transpose()?,
        None => None,
    };
    let distance = crate::jog_settings::distance_mm(positive(&machine, "jogDist")?)
        .map_err(|e| format!("jogDist: {e}"))?;
    let jog_feed = positive(&settings, "jogSpeed")?;
    if jog_feed < 0.001 || !(jog_feed as f32).is_finite() {
        return Err(format!("jogSpeed {jog_feed} 无法表示为 Writer 点动请求"));
    }
    let write_feed = positive(&settings, "feedRate")?;
    if !(0.001..=100_000.).contains(&write_feed) {
        return Err(format!(
            "feedRate {write_feed} 超出 Writer G-code 格式的进给范围（0.001–100000 mm/min）"
        ));
    }
    Ok(Profile {
        location: root.to_owned(),
        baud,
        distance,
        jog_feed,
        write_feed,
        page,
        axes: Axes {
            position: json::with_value(&required(&axes, "pos")?, json::text)?,
            swap: parse_bool("revxy")?,
            reverse_x: parse_bool("xAxeRev")?,
            reverse_y: parse_bool("yAxeRev")?,
        },
        font,
    })
}

pub(crate) struct Defaults {
    pub form: Form,
    pub summary: String,
    pub source: Option<String>,
    pub report: String,
}

pub(crate) fn selected_root(
    value: &std::ffi::OsStr,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    let base = match value.to_str() {
        Some("native") => ".local/share",
        Some("flatpak") => ".var/app/com.usebottles.bottles/data",
        _ => return Ok(PathBuf::from(value)),
    };
    Ok(home
        .ok_or("缺少 HOME，请提供配置目录的路径")?
        .join(base)
        .join(BOTTLE))
}

pub(crate) fn paper_size(root: &Path) -> Option<[f64; 2]> {
    if !root.is_dir() {
        return None;
    }
    load(root).ok().map(|p| p.page.size_mm)
}

pub(crate) fn discover(selected: Option<&Path>, home: Option<&Path>) -> Result<Defaults, String> {
    let mut profiles = Vec::new();
    let mut unreadable = Vec::new();
    if let Some(root) = selected {
        profiles.push(load(root).map_err(|e| format!("{}: {e}", root.display()))?);
    } else if let Some(home) = home {
        for base in [".local/share", ".var/app/com.usebottles.bottles/data"] {
            let root = home.join(base).join(BOTTLE);
            match root.try_exists() {
                Ok(false) => continue,
                Ok(true) => match load(&root) {
                    Ok(profile) => profiles.push(profile),
                    Err(e) => unreadable.push(format!("{}: {e}", root.display())),
                },
                Err(e) => unreadable.push(format!("{}: {e}", root.display())),
            }
        }
    }
    let mut form = Form::default();
    let Some(first) = profiles.first() else {
        if !unreadable.is_empty() {
            return Err(unreadable.join("\n"));
        }
        return Ok(Defaults {
            form,
            summary: String::new(),
            source: None,
            report: "当前使用 Writer 默认值。".into(),
        });
    };
    let mut differences = Vec::new();
    if profiles.iter().all(|p| p.baud == first.baud) {
        form.baud = first.baud;
    } else {
        differences.push("波特率");
    }
    if profiles.iter().all(|p| p.distance == first.distance) {
        form.jog_step = first.distance;
    } else {
        differences.push("单击距离");
    }
    let common_speed = profiles.iter().all(|p| p.jog_feed == first.jog_feed);
    if common_speed {
        form.jog_feed = first.jog_feed.to_string();
    } else {
        differences.push("点动速度");
    }
    if profiles.iter().all(|p| p.write_feed == first.write_feed) {
        // Preserve Writer's existing Z writing feed independently of the imported XY feed.
        form.lift_feed = Some(form.feed);
        form.feed = first.write_feed;
    } else {
        differences.push("书写进给");
    }
    let same_size = profiles
        .iter()
        .all(|p| p.page.size_mm == first.page.size_mm);
    let same_margins = profiles
        .iter()
        .all(|p| p.page.margins_mm == first.page.margins_mm);
    first
        .page
        .apply_fields(&mut form, same_size, same_margins)?;
    if !same_size {
        differences.push("纸张");
    }
    if !same_margins {
        differences.push("页边距");
    }
    if !profiles.iter().all(|p| p.font == first.font) {
        differences.push("字体");
    }
    let source = if profiles.len() > 1 {
        "奎享两套安装的共同值"
    } else {
        "奎享配置"
    };
    let mut summary = if differences.is_empty() {
        format!("已读取{source}。")
    } else {
        format!(
            "已读取奎享共同值。{}有差异，保留 Writer 设置，可用 --kuixiang-config 选择安装。",
            differences.join("、")
        )
    };
    if !unreadable.is_empty() {
        summary.push_str("另有安装配置未能读取。");
    }
    let mut report = summary.clone();
    for error in unreadable {
        report.push_str(&format!("\n{error}"));
    }
    for p in &profiles {
        let [width, height] = p.page.size_mm;
        let a = &p.axes;
        report.push_str(&format!("\n{}\n  baud={}  jogDist={} mm  jogSpeed={} mm/min  feedRate={} mm/min\n  纸张 {:?} {} × {} mm；左、上、右、下边距 {:?} mm\n  源轴设置 pos={} revxy={} xAxeRev={} yAxeRev={}\n  源字体={}（Writer 字体需另行安装并选择）", p.location.display(), p.baud, p.distance, p.jog_feed, p.write_feed, p.page.name, width, height, p.page.margins_mm, a.position, a.swap, a.reverse_x, a.reverse_y, p.font.as_deref().unwrap_or("未指定")));
    }
    Ok(Defaults {
        form,
        source: common_speed.then(|| format!("{source} · {} mm/min", first.jog_feed)),
        summary,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "zuse-kx-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn install(&self, name: &str) -> PathBuf {
            let root = selected_root(name.as_ref(), Some(&self.0)).unwrap();
            let data = root.join("test-fixture/settings");
            fs::create_dir_all(&data).unwrap();
            let flat = name == "flatpak";
            let port = if flat { "COM33" } else { "COM1" };
            let (width, height, size) = if flat {
                (150, 200, 120)
            } else {
                (200, 290, 95)
            };
            let machine = format!(
                r#"{{"port":"{port}","baud":"115200","jogDist":1.0,"autoShutdown":false,"serverPort":0,"penDrawLength":0.0,"autoPauseWithPen":false}}"#
            );
            let page = format!(
                r#"[{{"name":"草稿","pageWidth":90,"pageHeight":90,"leftGap":0,"topGap":0,"rightGap":0,"bottomGap":0,"isDefault":false}},{{"name":"默认","pageWidth":{width},"pageHeight":{height},"leftGap":0,"rightGap":0,"topGap":0,"bottomGap":0,"lineNum":30,"fontSize":{size},"space":1.0,"contentAlign":"Center","isDefault":true}}]"#
            );
            let settings = r#"{"dpi":96.0,"feedRate":8000,"toolOnDelay":0.0,"toolOffDelay":0.0,"jogSpeed":8000,"drawQuality":"VeryLow","maxImageSize":4000,"autoSetZero":true,"autoConnect":true,"autoReconnect":false,"homeDelay":1}"#;
            let axes = r#"{"revxy":false,"xAxeRev":false,"yAxeRev":true,"pos":"LeftUp","manget":false,"lock":false}"#;
            let format = if flat {
                r#"{"fontsName":"陈继世6_kvenjoy_1.gfont","chance":1,"fontSize":120.0,"charGap":1.0,"lineGap":0.0}"#
            } else {
                r#"{"fontSize":100.0,"charGap":0.0,"lineGap":0.0}"#
            };
            for (name, content) in
                FILES
                    .into_iter()
                    .zip([machine.as_str(), settings, &page, axes, format])
            {
                fs::write(data.join(name), content).unwrap();
            }
            root
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn selected_installation_reads_actual_values_without_connecting_or_setting_origin() {
        let h = Home::new();
        for (source, size) in [("native", [200., 290.]), ("flatpak", [150., 200.])] {
            let path = h.install(source);
            let d = discover(Some(&path), None).unwrap();
            assert_eq!([d.form.area_width, d.form.area_height], size);
            assert_eq!(d.form.margins, [0.; 4]);
            assert_eq!(
                (
                    d.form.baud,
                    d.form.jog_step,
                    d.form.feed,
                    d.form.xy_jog_feed().unwrap()
                ),
                (115200, 1., 8000., 8000.)
            );
            assert_eq!(d.form.lift_feed, Some(300.));
            assert!(d.form.port.is_empty());
            assert_eq!(d.form.font, "builtin");
            assert_eq!(
                (d.form.x, d.form.y, d.form.pen_up, d.form.pen_down),
                (0., 0., 3., 0.)
            );
            assert!(
                d.report
                    .contains("pos=LeftUp revxy=false xAxeRev=false yAxeRev=true")
            );
            assert!(d.report.contains(if source == "flatpak" {
                "陈继世6_kvenjoy_1.gfont"
            } else {
                "未指定"
            }));
        }
    }
    #[test]
    fn discovery_applies_only_common_fields_and_explicit_source_observes_changes() {
        let h = Home::new();
        assert!(discover(None, Some(&h.0)).unwrap().source.is_none());
        let path = h.install("native");
        assert_eq!(discover(None, Some(&h.0)).unwrap().form.area_width, 200.);
        h.install("flatpak");
        let common = discover(None, Some(&h.0)).unwrap();
        assert_eq!(common.form.feed, 8000.);
        assert!(common.summary.contains("纸张、字体"));
        assert_eq!(common.form.margins, [0.; 4]);
        let machine = path.join("test-fixture/settings/MachineControllerData.json");
        fs::write(machine, r#"{"baud":"57600","jogDist":17.25}"#).unwrap();
        let explicit = discover(Some(&path), None).unwrap();
        assert_eq!((explicit.form.baud, explicit.form.jog_step), (57600, 17.25));
        let split = discover(None, Some(&h.0)).unwrap();
        assert!(split.summary.contains("波特率、单击距离"));
    }
    #[test]
    fn desktop_source_preference_tracks_available_installations() {
        use crate::kuixiang_selector::{Choice, Sources};
        let home = Home::new();
        let native = home.install("native");
        let flat = selected_root("flatpak".as_ref(), Some(&home.0)).unwrap();
        fs::create_dir_all(&flat).unwrap(); // Directory exists but its settings are unavailable.
        let mut sources = Sources::from_home(Some(&home.0));
        assert!(sources.choices == vec![Choice::Common, Choice::Native]);
        assert!(
            discover(None, Some(&home.0))
                .unwrap()
                .report
                .contains("未能读取")
        );
        assert!(
            sources
                .paper_label(Choice::Native)
                .starts_with("200 × 290 mm")
        );
        home.install("flatpak");
        let both = Sources::from_home(Some(&home.0));
        assert!(
            both.paper_label(Choice::Flatpak)
                .starts_with("150 × 200 mm")
        );
        let preference = home.0.join("kuixiang-source.json");
        assert_eq!(sources.saved_root(&preference).unwrap(), None);
        fs::write(&preference, r#""native""#).unwrap();
        assert_eq!(
            sources.saved_root(&preference).unwrap(),
            Some(native.clone())
        );
        sources.mark_explicit(&home.0);
        assert!(sources.active.is_none());
        fs::write(&preference, r#""flatpak""#).unwrap();
        assert!(sources.saved_root(&preference).is_err());
        fs::write(&preference, r#""common""#).unwrap();
        assert_eq!(sources.saved_root(&preference).unwrap(), None);
        assert!(sources.active == Some(Choice::Common));
    }
    #[test]
    fn rejects_partial_ambiguous_malformed_and_nonregular_sources() {
        let h = Home::new();
        let root = h.install("native");
        let data = root.join("test-fixture/settings");
        fs::write(root.join("settings.json"), "{}").unwrap();
        assert!(discover(Some(&root), None).err().unwrap().contains("重复"));
        fs::remove_file(root.join("settings.json")).unwrap();
        for bad in [
            r#"{"jogSpeed":0,"feedRate":8000}"#,
            r#"{"jogSpeed":8000,"feedRate":"8000"}"#,
            r#"{"jogSpeed":100,"jogSpeed":8000,"feedRate":8000}"#,
        ] {
            fs::write(data.join("settings.json"), bad).unwrap();
            assert!(discover(Some(&root), None).is_err());
        }
        fs::remove_file(data.join("settings.json")).unwrap();
        assert!(
            discover(Some(&root), None)
                .err()
                .unwrap()
                .contains("缺少 settings.json")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(data.join("AxesData.json"), data.join("settings.json"))
                .unwrap();
            assert!(
                discover(Some(&root), None)
                    .err()
                    .unwrap()
                    .contains("普通配置文件")
            );
        }
    }
}
