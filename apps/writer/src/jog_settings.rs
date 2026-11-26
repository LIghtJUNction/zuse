//! A Writer preference in millimetres, independent of machine travel bounds.
use crate::workspace_profile::{config_root, write_atomic};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
};

pub(crate) fn distance_mm(value: f64) -> Result<f64, String> {
    if (value as f32).is_finite() && value >= 0.001 {
        Ok(value)
    } else {
        Err("单次 XY 距离须为至少 0.001 mm 的有限正数。".into())
    }
}

pub(crate) fn parse_distance(s: &str) -> Result<f64, String> {
    distance_mm(s.trim().parse().unwrap_or(f64::NAN))
}

fn read_from(path: &Path) -> Result<Option<f64>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut value = String::new();
    file.take(65)
        .read_to_string(&mut value)
        .map_err(|e| e.to_string())?;
    if value.len() > 64 {
        return Err("单次 XY 距离设置文件过长。".into());
    }
    parse_distance(&value).map(Some)
}

pub(crate) fn load() -> Result<Option<f64>, String> {
    read_from(&config_root()?.with_file_name("jog-distance-mm"))
}
pub(crate) fn save(mm: f64) -> Result<(), String> {
    let mm = distance_mm(mm)?;
    write_atomic(
        &config_root()?.with_file_name("jog-distance-mm"),
        &format!("{mm:e}\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_decimal_distances_and_rejects_values_grbl_cannot_represent() {
        for mm in [0.001, 1., 35.5, 50., 100.] {
            assert_eq!(parse_distance(&mm.to_string()), Ok(mm));
        }
        for invalid in ["", "-", "0", "-1", ".0001", "NaN", "inf", "1e99"] {
            assert!(parse_distance(invalid).is_err());
        }
    }
}
