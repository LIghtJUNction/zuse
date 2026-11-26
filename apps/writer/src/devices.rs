//! Discover candidate serial ports from device-node and USB metadata.
//! Enumeration never opens a serial port or identifies controller firmware.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceCandidate {
    pub path: String,
    pub label: String,
    pub detail: String,
    pub identifier: Option<String>,
}

impl DeviceCandidate {
    pub(crate) fn display_name(&self) -> &str {
        let name = self.detail.rsplit_once(" · USB ").map_or(self.detail.as_str(), |(name, _)| name);
        if name.is_empty() || name == "USB serial" || (name.starts_with("USB ") && name.contains(':')) {
            crate::locale::text("USB 设备")
        } else {
            name
        }
    }
    pub(crate) fn port_name(&self) -> &str {
        std::path::Path::new(&self.label).file_name().and_then(|s| s.to_str()).unwrap_or(&self.label)
    }
}

pub(crate) fn discover() -> Vec<DeviceCandidate> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::FileTypeExt;
        linux::devices("/dev".as_ref(), "/sys/class/tty".as_ref(), |m| {
            m.file_type().is_char_device()
        })
    }
    #[cfg(target_os = "macos")]
    {
        macos::devices("/dev".as_ref())
    }
    #[cfg(target_os = "windows")]
    {
        // QueryDosDevice enumerates COM names without opening or configuring them.
        zuse::serial::ports().and_then(|ports| zuse::json::with_value(&ports, |root| {
            root.elements().map(|item| {
                let path = zuse::json::text(item)?;
                Ok(DeviceCandidate { label: path.clone(), identifier: Some(path.clone()),
                    detail: crate::locale::text("串口设备").into(), path })
            }).collect()
        })).unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux {
    use super::DeviceCandidate;
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::{Path, PathBuf},
    };

    fn paths(dir: &Path) -> impl Iterator<Item = PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
    }

    fn kernel_usb_name(name: &str) -> bool {
        ["ttyUSB", "ttyACM"].iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
    }

    fn attribute(path: &Path, key: &str) -> Option<String> {
        let raw = fs::read_to_string(path.join(key)).ok()?;
        let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        (!text.is_empty() && !text.chars().any(char::is_control))
            .then(|| text.chars().take(160).collect())
    }

    fn usb_description(tty: &Path) -> Option<(String, Option<String>)> {
        // tty/device points to the serial interface. The USB device containing
        // the interface owns idVendor, idProduct, manufacturer and product.
        let device = tty.join("device").canonicalize().ok()?;
        for parent in device.ancestors() {
            let (Some(vid), Some(pid)) = (
                attribute(parent, "idVendor"),
                attribute(parent, "idProduct"),
            ) else {
                continue;
            };
            if ![&vid, &pid]
                .iter()
                .all(|id| id.len() == 4 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                continue;
            }
            let mut fields = Vec::new();
            for key in ["manufacturer", "product"] {
                if let Some(s) = attribute(parent, key)
                    && !fields.contains(&s)
                {
                    fields.push(s);
                }
            }
            fields.push(format!(
                "USB {}:{}",
                vid.to_ascii_lowercase(),
                pid.to_ascii_lowercase()
            ));
            let identifier = attribute(parent, "serial").map(|s| format!("{} {s}", crate::locale::text("序列号"))).or_else(|| {
                let name = parent.file_name()?.to_str()?;
                (name.contains('-') && name.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'-' | b'.')))
                    .then(|| format!("{} {name}", crate::locale::text("USB 插口")))
            });
            return Some((fields.join(" · "), identifier));
        }
        None
    }

    pub(super) fn devices(
        dev: &Path,
        ttys: &Path,
        is_node: impl Fn(&fs::Metadata) -> bool,
    ) -> Vec<DeviceCandidate> {
        let Ok(root) = dev.canonicalize() else {
            return Vec::new();
        };
        let mut aliases = BTreeMap::<PathBuf, PathBuf>::new();
        for alias in paths(&root.join("serial/by-id")) {
            let Ok(target) = alias.canonicalize() else {
                continue;
            };
            if target.parent() != Some(root.as_path()) {
                continue;
            }
            // Select the same stable alias regardless of readdir order.
            aliases
                .entry(target)
                .and_modify(|old| {
                    if alias < *old {
                        old.clone_from(&alias)
                    }
                })
                .or_insert(alias);
        }
        let mut nodes: BTreeSet<_> = aliases.keys().cloned().collect();
        for class in paths(ttys) {
            // Physical serial entries have a device link. Avoid following sysfs
            // ancestors for PTYs and virtual console endpoints.
            if class.join("device").exists()
                && let Some(name) = class.file_name()
            {
                nodes.insert(root.join(name));
            }
        }
        nodes.extend(paths(&root).filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(kernel_usb_name)
        }));
        let mut found = BTreeMap::new();
        for path in nodes {
            let Ok(node) = path.canonicalize() else {
                continue;
            };
            if node.parent() != Some(root.as_path()) || found.contains_key(&node) {
                continue;
            }
            let Some(name) = node.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("tty") || !fs::metadata(&node).is_ok_and(|m| is_node(&m)) {
                continue;
            }
            let usb = usb_description(&ttys.join(name));
            if usb.is_none() && !kernel_usb_name(name) {
                continue;
            }
            let (detail, identifier) = usb.unwrap_or_else(|| ("USB serial".into(), None));
            let candidate = DeviceCandidate {
                path: aliases
                    .get(&node)
                    .unwrap_or(&node)
                    .to_string_lossy()
                    .into_owned(),
                label: node.to_string_lossy().into_owned(),
                detail,
                identifier,
            };
            found.insert(node, candidate);
        }
        found.into_values().collect()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::DeviceCandidate;
    use std::{collections::BTreeMap, fs, os::unix::fs::FileTypeExt, path::Path};
    pub(super) fn devices(dev: &Path) -> Vec<DeviceCandidate> {
        let mut by_port = BTreeMap::new();
        for entry in fs::read_dir(dev).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(suffix) = name
                .strip_prefix("cu.")
                .or_else(|| name.strip_prefix("tty."))
            else {
                continue;
            };
            if !suffix.starts_with("usb") || !entry.file_type().is_ok_and(|t| t.is_char_device()) {
                continue;
            }
            let path = entry.path().to_string_lossy().into_owned();
            let c = DeviceCandidate {
                label: path.clone(),
                path,
                detail: "USB serial".into(),
                identifier: Some(suffix.to_owned()),
            };
            if name.starts_with("cu.") {
                by_port.insert(suffix.to_owned(), c);
            } else {
                by_port.entry(suffix.to_owned()).or_insert(c);
            }
        }
        by_port.into_values().collect()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::linux;
    use std::{
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU32, Ordering},
    };
    struct Fixture {
        root: PathBuf,
        dev: PathBuf,
        sys: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let root = loop {
                let p = std::env::temp_dir().join(format!(
                    "zuse-device-test-{}-{}",
                    std::process::id(),
                    SEQ.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&p) {
                    Ok(()) => break p,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("{e}"),
                }
            };
            let dev = root.join("dev");
            let sys = root.join("class/tty");
            fs::create_dir_all(dev.join("serial/by-id")).unwrap();
            fs::create_dir_all(&sys).unwrap();
            Self { root, dev, sys }
        }
        fn node(&self, name: &str) {
            fs::write(self.dev.join(name), []).unwrap();
        }
        fn alias(&self, name: &str, dest: &str) {
            symlink(
                Path::new("../..").join(dest),
                self.dev.join("serial/by-id").join(name),
            )
            .unwrap();
        }
        fn usb(&self, name: &str) {
            let usb = self.root.join("bus").join(name);
            fs::create_dir_all(usb.join("interface")).unwrap();
            for (key, value) in [
                ("idVendor", "1A86"),
                ("idProduct", "7523"),
                ("manufacturer", "Example"),
                ("product", "USB Serial"),
            ] {
                fs::write(usb.join(key), format!("{value}\n")).unwrap();
            }
            let tty = self.sys.join(name);
            fs::create_dir(&tty).unwrap();
            symlink(usb.join("interface"), tty.join("device")).unwrap();
        }
        fn discover(&self) -> Vec<super::DeviceCandidate> {
            linux::devices(&self.dev, &self.sys, |m| m.is_file())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn stable_aliases_merge_and_show_real_metadata() {
        let f = Fixture::new();
        f.node("ttyUSB0");
        f.usb("ttyUSB0");
        f.alias("z-alias", "ttyUSB0");
        f.alias("usb-bridge", "ttyUSB0");
        let ports = f.discover();
        assert_eq!(ports.len(), 1);
        assert!(ports[0].path.ends_with("/serial/by-id/usb-bridge"));
        assert!(ports[0].label.ends_with("/ttyUSB0"));
        assert_eq!(ports[0].detail, "Example · USB Serial · USB 1a86:7523");
        assert_eq!(ports[0].display_name(), "Example · USB Serial");
        assert_eq!(ports[0].port_name(), "ttyUSB0");
    }
    #[test]
    fn identical_usb_models_can_be_distinguished_by_their_reported_serials() {
        let f = Fixture::new();
        for (tty, serial) in [("ttyUSB0", "A120"), ("ttyUSB1", "B340")] {
            f.node(tty);
            f.usb(tty);
            fs::write(f.root.join("bus").join(tty).join("serial"), serial).unwrap();
        }
        let found = f.discover();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].display_name(), found[1].display_name());
        assert_eq!(found[0].identifier.as_deref(), Some("序列号 A120"));
        assert_eq!(found[1].identifier.as_deref(), Some("序列号 B340"));
    }

    #[test]
    fn sorts_usb_nodes_and_recognizes_nonstandard_sysfs_names() {
        let f = Fixture::new();
        f.node("ttyUSB2");
        f.node("ttyXR0");
        f.usb("ttyXR0");
        f.node("ttyACM0");
        let names: Vec<_> = f
            .discover()
            .iter()
            .map(|c| {
                Path::new(&c.label)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["ttyACM0", "ttyUSB2", "ttyXR0"]);
    }
    #[test]
    fn excludes_dangling_links_nondevices_and_unrelated_ttys() {
        let f = Fixture::new();
        f.node("ttyS0");
        f.node("ttyUSBbad");
        f.node("null");
        f.alias("unrelated", "ttyS0");
        f.alias("missing", "ttyUSB9");
        f.alias("not-tty", "null");
        fs::create_dir(f.dev.join("ttyACM1")).unwrap();
        let outside = f.root.join("ttyUSB4");
        fs::write(&outside, []).unwrap();
        symlink(outside, f.dev.join("serial/by-id/external")).unwrap();
        assert!(f.discover().is_empty());
    }
}
