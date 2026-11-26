//! Import the six millimetre fields in a KuiXiang page_settings.json preset.
use crate::controller::Form;
use std::path::Path;
use zuse::{json, motion::MAX_COORDINATE};

#[derive(Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub(crate) size_mm: [f64; 2],
    pub(crate) margins_mm: [f64; 4],
}

impl Preset {
    pub fn apply(&self, form: &mut Form, calibrated: bool) -> Result<(), String> {
        self.apply_fields(form, !calibrated, true)
    }

    pub(crate) fn apply_fields(
        &self,
        form: &mut Form,
        use_size: bool,
        use_margins: bool,
    ) -> Result<(), String> {
        let scale = form.reference_distance / form.reference_units;
        if form.reference_distance <= 0.
            || form.reference_units <= 0.
            || !scale.is_finite()
            || scale <= 0.
        {
            return Err("纸张参考比例无效".into());
        }
        let size = if use_size {
            self.size_mm.map(|v| v / scale)
        } else {
            [form.area_width, form.area_height]
        };
        let margins = if use_margins {
            self.margins_mm.map(|v| v / scale)
        } else {
            form.margins
        };
        if !fits(size, margins) {
            return Err("页边距超出当前书写范围，或换算后的页面尺寸无效".into());
        }
        [form.area_width, form.area_height] = size;
        form.margins = margins;
        Ok(())
    }
}

fn fits([width, height]: [f64; 2], margins: [f64; 4]) -> bool {
    let [left, top, right, bottom] = margins;
    [width, height, left, top, right, bottom]
        .iter()
        .all(|n| (0.0..=MAX_COORDINATE).contains(n))
        && left + right < width
        && top + bottom < height
}

pub fn load(path: &Path, name: Option<&str>) -> Result<Preset, String> {
    let input = crate::kuixiang_config::read_file(path)?;
    parse(&input, name).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn default_preset(input: &str) -> Result<Preset, String> {
    let name = json::with_value(input, |root| {
        let mut name = None;
        let mut has_flags = false;
        for row in root.elements() {
            let mut flags = row.members().filter(|(key, _)| key.matches("isDefault"));
            let flag = flags.next().map(|(_, value)| value.raw());
            if flags.next().is_some() || !matches!(flag, None | Some("true" | "false")) {
                return Err("isDefault 需要唯一的布尔值".into());
            }
            has_flags |= flag.is_some();
            if flag == Some("true") {
                if name.is_some() {
                    return Err("存在多份默认纸张".into());
                }
                name = Some(json::text(row.get("name").ok_or("默认纸张缺少名称")?)?);
            }
        }
        if has_flags && name.is_none() {
            return Err("配置未选中默认纸张".into());
        }
        Ok(name)
    })?;
    parse(input, name.as_deref())
}

fn parse(input: &str, wanted: Option<&str>) -> Result<Preset, String> {
    json::with_value(input.trim_start_matches('\u{feff}'), |root| {
        if !root.raw().starts_with('[') {
            return Err("需要 page_settings.json 中的纸张预设数组".into());
        }
        let mut selected = None;
        let mut names = Vec::new();
        for row in root.elements() {
            let mut names_in_row = row.members().filter(|(key, _)| key.matches("name"));
            let (_, value) = names_in_row.next().ok_or("纸张预设缺少 name")?;
            let name = json::text(value)?;
            if name.trim().is_empty() || names_in_row.next().is_some() {
                return Err("纸张预设需要唯一的非空 name 字段".into());
            }
            if wanted.is_none_or(|w| w == name) {
                if selected.is_some() && wanted.is_some() {
                    return Err(format!("预设名称重复：{name:?}"));
                }
                selected = Some((name.clone(), row));
            }
            names.push(name);
        }
        if names.is_empty() {
            return Err("纸张预设数组为空".into());
        }
        if wanted.is_none() && names.len() > 1 {
            return Err(format!("请指定要导入的预设名称，可选 {names:?}"));
        }
        let (name, row) = selected.ok_or_else(|| format!("找不到所选预设，可选 {names:?}"))?;
        let mm = |field: &str| -> Result<f64, String> {
            let mut matches = row.members().filter(|(key, _)| key.matches(field));
            let (_, value) = matches.next().ok_or_else(|| format!("缺少 {field}"))?;
            if matches.next().is_some() {
                return Err(format!("预设字段重复：{field}"));
            }
            value
                .raw()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("{field} 需要有限的毫米数值"))
        };
        let size_mm = [mm("pageWidth")?, mm("pageHeight")?];
        let margins_mm = [
            mm("leftGap")?,
            mm("topGap")?,
            mm("rightGap")?,
            mm("bottomGap")?,
        ];
        if !fits(size_mm, margins_mm) {
            return Err("纸张尺寸无效，或页边距没有留下可书写面积".into());
        }
        Ok(Preset {
            name,
            size_mm,
            margins_mm,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE: &str = r#"[{"name":"默认","pageWidth":200.0,"pageHeight":290.0,"leftGap":0.0,"rightGap":0.0,"topGap":0.0,"bottomGap":0.0,"lineNum":30,"fontSize":95.0,"space":1.0,"contentAlign":"Center"}]"#;
    const FLATPAK: &str = r#"[{"name":"默认","pageWidth":150.0,"pageHeight":200.0,"leftGap":0.0,"rightGap":0.0,"topGap":0.0,"bottomGap":0.0,"lineNum":30,"fontSize":120.0,"space":1.0,"contentAlign":"Center"}]"#;
    const ASYMMETRIC: &str = r#"[{"name":"信纸","pageWidth":120,"pageHeight":160,"leftGap":2,"rightGap":6,"topGap":4,"bottomGap":8,"port":"COM1","autoConnect":true,"speed":8000,"revxy":true,"xAxeRev":true,"yAxeRev":true,"pos":"LeftUp"}]"#;

    #[test]
    fn separate_installations_keep_their_own_page_and_writer_typography() {
        for (input, width, height) in [(NATIVE, 200., 290.), (FLATPAK, 150., 200.)] {
            let mut form = Form::default();
            let expect = Form {
                area_width: width,
                area_height: height,
                margins: [0.; 4],
                ..form.clone()
            };
            let preset = parse(input, None).unwrap();
            assert_eq!(preset.name, "默认");
            preset.apply(&mut form, false).unwrap();
            assert_eq!(form, expect);
        }
    }

    #[test]
    fn preserves_calibrated_area_axes_and_machine_settings() {
        let preset = parse(ASYMMETRIC, Some("信纸")).unwrap();
        let mut form = Form {
            area_width: 40.,
            area_height: 55.,
            x: -12.,
            flip_x: true,
            pen_down: -2.3,
            feed: 218.,
            reference_distance: 2.,
            ..Form::default()
        };
        let expect = Form {
            margins: [1., 2., 3., 4.],
            ..form.clone()
        };
        preset.apply(&mut form, true).unwrap();
        assert_eq!(form, expect);
        preset.apply(&mut form, false).unwrap();
        assert_eq!([form.area_width, form.area_height], [60., 80.]);
        form.area_height = 3.;
        let before = form.clone();
        assert!(preset.apply(&mut form, true).is_err());
        assert_eq!(form, before);
    }

    #[test]
    fn named_selection_and_invalid_geometry() {
        let two = format!(
            "[{},{}]",
            &NATIVE[1..NATIVE.len() - 1],
            &ASYMMETRIC[1..ASYMMETRIC.len() - 1]
        );
        assert!(parse(&two, None).is_err());
        assert!(parse(&two, Some("其他")).is_err());
        assert_eq!(parse(&two, Some("信纸")).unwrap().size_mm, [120., 160.]);
        assert!(parse(&two.replace("信纸", "默认"), Some("默认")).is_err());
        for bad in [
            ASYMMETRIC.replace("120", "8"),
            ASYMMETRIC.replace("120", "1e999"),
            ASYMMETRIC.replace("160", "-1"),
            ASYMMETRIC.replace("120", "\"120\""),
            ASYMMETRIC.replace("120", "120,\"pageWidth\":150"),
        ] {
            assert!(parse(&bad, None).is_err(), "{bad}");
        }
        assert!(parse("[]", None).is_err());
        assert!(parse("{}", None).is_err());
    }
}
