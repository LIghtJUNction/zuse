//! Prepared once so font changes cannot alter a validated physical action.
use super::{boolean, number, range, text};
use crate::{
    drawing::MAX_PROGRAM_POINTS,
    font_store,
    handwriting::{self, GlyphVariation, Layout},
    json::quote,
    language::Call,
    motion::MAX_COORDINATE,
    observation::Cancellation,
    pen_input::{self, Target},
    plotter,
};
use std::{
    fmt::Write,
    hash::{BuildHasher, RandomState},
    path::PathBuf,
};

type Strokes = Vec<Vec<(f64, f64)>>;

pub(super) struct Writing {
    action: Action,
    pub points: usize,
    seed: u64,
    pub wait_ms: u64,
}

enum Action {
    Preview(Strokes),
    Input {
        strokes: Strokes,
        target: Target,
        interval: u32,
    },
    Gcode {
        program: String,
        path: Option<PathBuf>,
    },
}

impl Writing {
    pub fn prepare(call: &Call<'_>) -> Result<Self, String> {
        let units = number(call, "reference_units", 1.0);
        let distance = number(call, "reference_distance", 1.0);
        if units <= 0.0 || distance <= 0.0 {
            return Err("reference_units and reference_distance must be positive".into());
        }
        let scale = distance / units;
        if !scale.is_finite() || scale == 0.0 {
            return Err("reference distance conversion is outside numeric range".into());
        }
        let size = number(call, "size", 10.0);
        let glyph_offset = |absolute: &str, relative: &str| {
            if call.get(relative).is_some() {
                if call.get(absolute).is_some() {
                    return Err(format!("use either {absolute} or {relative}"));
                }
                let ratio = number(call, relative, 0.0);
                if !(0.0..=1.0).contains(&ratio) {
                    return Err(format!("{relative} must be between 0 and 1"));
                }
                Ok(size * ratio)
            } else {
                Ok(number(call, absolute, 0.0))
            }
        };
        let offset_x = glyph_offset("offset_x", "offset_x_ratio")?;
        let offset_y = glyph_offset("offset_y", "offset_y_ratio")?;
        let variation = GlyphVariation {
            tilt_degrees: number(call, "tilt_degrees", 0.0),
            size_ratio: number(call, "size_ratio", 0.0),
        };
        let seed = if call.get("seed").is_some() {
            range(call, "seed", 0, 0, i64::MAX)? as u64
        } else if offset_x != 0.0
            || offset_y != 0.0
            || variation.tilt_degrees != 0.0
            || variation.size_ratio != 0.0
        {
            // Generated seeds also round-trip through JavaScript's JSON number type.
            RandomState::new().hash_one(()) & ((1_u64 << 53) - 1)
        } else {
            0
        };
        let layout = Layout {
            size,
            advance: number(call, "advance", size * 1.2),
            line_height: number(call, "line_height", size * 1.5),
            offset_x,
            offset_y,
            seed,
        };
        let font = font_store::load(&text(call, "font", "builtin")?)?;
        let mut strokes = handwriting::strokes_in_area(
            &font,
            &text(call, "text", "")?,
            &layout,
            number(call, "area_width", 0.0),
            number(call, "area_height", 0.0),
            variation,
        )?;
        let origin = (number(call, "x", 0.0), number(call, "y", 0.0));
        let machine = matches!(call.name, "write.gcode" | "write.export");
        // A character's downstroke is +Y on screen and -Y on a Cartesian plotter.
        let swap = boolean(call, "swap_xy", false);
        let sign_x = if boolean(call, "flip_x", false) {
            -1.0
        } else {
            1.0
        };
        let sign_y = if boolean(call, "flip_y", false) {
            -1.0
        } else {
            1.0
        };
        for (x, y) in strokes.iter_mut().flatten() {
            let dx = *x * scale;
            let dy = *y * if machine { -scale } else { scale };
            let (dx, dy) = if swap { (dy, dx) } else { (dx, dy) };
            *x = origin.0 + dx * sign_x;
            *y = origin.1 + dy * sign_y;
            if !x.is_finite()
                || !y.is_finite()
                || x.abs() > MAX_COORDINATE
                || y.abs() > MAX_COORDINATE
            {
                return Err(
                    "transformed handwriting coordinate is outside the supported range".into(),
                );
            }
        }
        let points = strokes.iter().map(Vec::len).sum::<usize>();
        if points > MAX_PROGRAM_POINTS {
            return Err("handwriting exceeds program point limit".into());
        }
        let mut wait_ms = 0;
        let action = match call.name {
            "write.preview" => Action::Preview(strokes),
            "write.gcode" | "write.export" => {
                let program = plotter::gcode(
                    &strokes,
                    number(call, "pen_up", 3.0),
                    number(call, "pen_down", 0.0),
                    number(call, "feed", 300.0),
                )?;
                let path = if call.name == "write.export" {
                    let name = text(call, "path", "")?;
                    if name.trim().is_empty() {
                        return Err("write.export requires a file path".into());
                    }
                    Some(PathBuf::from(name))
                } else {
                    None
                };
                if path.is_none() && quote(&program).len() > 960_000 {
                    return Err("G-code response is too large; use write.export".into());
                }
                Action::Gcode { program, path }
            }
            _ => {
                let target = if call.name == "write.mouse" {
                    Target::Mouse
                } else {
                    let width = range(call, "width", 0, 2, u16::MAX.into())? as u16;
                    let height = range(call, "height", 0, 2, u16::MAX.into())? as u16;
                    if call.name == "write.touchpad" {
                        Target::Touchpad { width, height }
                    } else {
                        Target::Touchscreen { width, height }
                    }
                };
                let interval = range(call, "interval", 8, 0, 1000)? as u32;
                wait_ms = pen_input::validate(target, &strokes, interval)?;
                Action::Input {
                    strokes,
                    target,
                    interval,
                }
            }
        };
        Ok(Self {
            action,
            seed,
            points,
            wait_ms,
        })
    }

    pub fn execute(self, cancellation: &Cancellation) -> Result<String, String> {
        cancellation.check()?;
        match self.action {
            Action::Preview(strokes) => {
                let mut out = String::from("{\"strokes\":[");
                for (i, stroke) in strokes.iter().enumerate() {
                    if i != 0 {
                        out.push(',');
                    }
                    out.push('[');
                    for (j, (x, y)) in stroke.iter().enumerate() {
                        if j != 0 {
                            out.push(',');
                        }
                        write!(out, "[{x},{y}]").unwrap();
                    }
                    out.push(']');
                    if out.len() > 960_000 {
                        return Err("handwriting preview exceeds output limit".into());
                    }
                }
                write!(out, "],\"points\":{},\"seed\":{}}}", self.points, self.seed).unwrap();
                Ok(out)
            }
            Action::Input {
                strokes,
                target,
                interval,
            } => {
                pen_input::write(target, &strokes, interval, cancellation)?;
                Ok(format!(
                    "{{\"delivery\":\"submitted\",\"strokes\":{},\"points\":{}}}",
                    strokes.len(),
                    self.points
                ))
            }
            Action::Gcode {
                program,
                path: None,
            } => Ok(format!(
                "{{\"units\":\"mm\",\"gcode\":{}}}",
                quote(&program)
            )),
            Action::Gcode {
                program,
                path: Some(path),
            } => {
                crate::output_file::publish(&path, program.as_bytes())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(format!(
                    "{{\"path\":{},\"bytes\":{},\"units\":\"mm\"}}",
                    quote(&path.to_string_lossy()),
                    program.len()
                ))
            }
        }
    }
}
