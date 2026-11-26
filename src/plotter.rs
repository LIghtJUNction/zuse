//! Text-only plotter export and a restricted LinuxCNC-style lathe preview.
//! All bounds here are format limits, independent of machine travel or tooling.

use std::fmt::Write;

const MAX_MM: f64 = 10_000.0;
const MAX_FEED: f64 = 100_000.0;
const MAX_POINTS: usize = 65_536;
const MAX_STROKES: usize = 16_384;

fn coordinate(value: f64) -> bool {
    (-MAX_MM..=MAX_MM).contains(&value)
}

fn feed_rate(value: f64) -> bool {
    (0.001..=MAX_FEED).contains(&value)
}

/// Export absolute XY strokes in millimetres, rounded to 0.001 mm.
/// A singleton stroke makes a dot. Every rapid XY move takes place at pen_up.
/// Coordinates are limited to ±10,000 mm, feed to 0.001–100,000 mm/min.
pub fn gcode(
    strokes: &[Vec<(f64, f64)>],
    pen_up: f64,
    pen_down: f64,
    feed: f64,
) -> Result<String, String> {
    gcode_with_feeds(strokes, pen_up, pen_down, feed, None, None)
}

pub(crate) fn gcode_with_feeds(
    strokes: &[Vec<(f64, f64)>],
    pen_up: f64,
    pen_down: f64,
    feed: f64,
    travel_feed: Option<f64>,
    lift_feed: Option<f64>,
) -> Result<String, String> {
    if !coordinate(pen_up)
        || !coordinate(pen_down)
        || !feed_rate(feed)
        || travel_feed.is_some_and(|f| !feed_rate(f))
        || lift_feed.is_some_and(|f| !feed_rate(f))
    {
        return Err("pen heights or feed outside plotter limits".into());
    }
    let up = format!("{pen_up:.3}");
    let down = format!("{pen_down:.3}");
    if up.parse::<f64>() == down.parse::<f64>() {
        return Err("pen-up and pen-down must differ at 0.001 mm output precision".into());
    }
    if strokes.is_empty() || strokes.len() > MAX_STROKES {
        return Err("expected 1–16384 strokes".into());
    }
    let mut remaining = MAX_POINTS;
    for (i, stroke) in strokes.iter().enumerate() {
        if stroke.is_empty() || stroke.len() > remaining {
            return Err(format!(
                "empty stroke or point limit exceeded at stroke {}",
                i + 1
            ));
        }
        if stroke
            .iter()
            .any(|&(x, y)| !coordinate(x) || !coordinate(y))
        {
            return Err(format!("invalid XY coordinate in stroke {}", i + 1));
        }
        remaining -= stroke.len();
    }

    let raise = lift_feed.map_or_else(
        || format!("G0 Z{up}"),
        |speed| format!("G1 Z{up} F{speed:.3}"),
    );
    let lower = lift_feed.unwrap_or(feed);
    let mut output = format!("G21 G17 G90 G94\n{raise}\n");
    for stroke in strokes {
        let (x, y) = stroke[0];
        if let Some(speed) = travel_feed {
            writeln!(output, "G1 X{x:.3} Y{y:.3} F{speed:.3}").unwrap();
        } else {
            writeln!(output, "G0 X{x:.3} Y{y:.3}").unwrap();
        }
        writeln!(output, "G1 Z{down} F{lower:.3}").unwrap();
        for (i, &(x, y)) in stroke[1..].iter().enumerate() {
            if i == 0 && lift_feed.is_some() {
                writeln!(output, "G1 X{x:.3} Y{y:.3} F{feed:.3}").unwrap();
            } else {
                writeln!(output, "G1 X{x:.3} Y{y:.3}").unwrap();
            }
        }
        writeln!(output, "{raise}").unwrap();
    }
    output.push_str("M2\n");
    Ok(output)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XMode {
    Diameter,
    Radius,
}

#[derive(Default)]
struct Block {
    metric: bool,
    absolute: bool,
    plane: bool,
    per_minute: bool,
    mode: Option<XMode>,
    motion: Option<u8>,
    x: Option<f64>,
    z: Option<f64>,
    feed: Option<f64>,
    end: bool,
    words: usize,
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err("repeated word or conflicting modal group".into());
    }
    Ok(())
}

fn flag_once(flag: &mut bool) -> Result<(), String> {
    if *flag {
        return Err("duplicate modal declaration".into());
    }
    *flag = true;
    Ok(())
}

impl Block {
    fn parse(mut line: &str) -> Result<Self, String> {
        let mut block = Self::default();
        loop {
            line = line.trim_start_matches([' ', '\t']);
            let Some(&first) = line.as_bytes().first() else {
                break;
            };
            if !first.is_ascii_alphabetic() {
                return Err("expected a G, M, X, Z or F word".into());
            }
            line = &line[1..];
            let n = line
                .bytes()
                .take_while(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+'))
                .count();
            let (number, tail) = line.split_at(n);
            line = tail;
            block.words += 1;
            match first.to_ascii_uppercase() {
                code @ (b'G' | b'M') => {
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return Err("expected an unsigned integer command code".into());
                    }
                    let value = number.parse::<u16>().map_err(|_| "invalid command code")?;
                    match (code, value) {
                        (b'G', 0 | 1) => set_once(&mut block.motion, value as u8)?,
                        (b'G', 7) => set_once(&mut block.mode, XMode::Diameter)?,
                        (b'G', 8) => set_once(&mut block.mode, XMode::Radius)?,
                        (b'G', 18) => flag_once(&mut block.plane)?,
                        (b'G', 21) => flag_once(&mut block.metric)?,
                        (b'G', 90) => flag_once(&mut block.absolute)?,
                        (b'G', 94) => flag_once(&mut block.per_minute)?,
                        (b'M', 2 | 30) => flag_once(&mut block.end)?,
                        _ => {
                            return Err(format!(
                                "unsupported command {}{number}",
                                char::from(code)
                            ));
                        }
                    }
                }
                axis @ (b'X' | b'Z' | b'F') => {
                    let value = number
                        .parse::<f64>()
                        .map_err(|_| "invalid decimal number")?;
                    match axis {
                        b'X' if (0.0..=MAX_MM).contains(&value) => set_once(&mut block.x, value)?,
                        b'Z' if coordinate(value) => set_once(&mut block.z, value)?,
                        b'F' if feed_rate(value) => set_once(&mut block.feed, value)?,
                        _ => return Err(format!("{} outside preview limits", char::from(axis))),
                    }
                }
                _ => return Err(format!("unsupported address {}", char::from(first))),
            }
        }
        Ok(block)
    }
}

/// Preview absolute, metric X/Z endpoints in the XZ plane, F in mm/min.
/// Require G21, G90 and an explicit G7 (diameter) or G8 (radius) before
/// motion. G18/G94 are the only supported plane/feed conventions. Require
/// G0/G1 on every move, both axes on the first move and F by the first G1.
/// X is nonnegative. Omitted axes retain their last value. Changing the X
/// convention after motion is unsupported. M2/M30 are optional terminators.
/// Comments and macros are excluded, including controller-active comments.
/// This endpoint list contains no machine, tool, spindle or collision model.
pub fn lathe_preview(source: &str) -> Result<String, String> {
    if source.len() > 1_048_576 {
        return Err("lathe source exceeds 1 MiB".into());
    }
    let mut output = String::from("Lathe XZ preview | mm | absolute | feed mm/min\n");
    let (mut metric, mut absolute, mut end) = (false, false, false);
    let (mut mode, mut feed) = (None, None);
    let mut position: Option<(f64, f64)> = None;
    let mut moves = 0;
    for (index, line) in source.lines().enumerate() {
        let fail = |why: &str| format!("line {}: {why}", index + 1);
        if line.len() > 4096 {
            return Err(fail("block exceeds 4096 bytes"));
        }
        let b = Block::parse(line).map_err(|why| fail(&why))?;
        if b.words == 0 {
            continue;
        }
        if end {
            return Err(fail("instruction after program end"));
        }
        if b.end {
            if b.words != 1 {
                return Err(fail("M2/M30 must occupy a separate block"));
            }
            end = true;
            continue;
        }
        if let Some(new_mode) = b.mode {
            if position.is_some() && mode != Some(new_mode) {
                return Err(fail(
                    "X diameter/radius convention changed after first move",
                ));
            }
            mode = Some(new_mode);
        }
        metric |= b.metric;
        absolute |= b.absolute;
        if let Some(f) = b.feed {
            if !metric {
                return Err(fail("declare G21 before feed"));
            }
            feed = Some(f);
        }
        let Some(motion) = b.motion else {
            if b.x.is_some() || b.z.is_some() {
                return Err(fail("axis word requires explicit G0 or G1"));
            }
            continue;
        };
        if !metric || !absolute {
            return Err(fail("motion requires G21 and G90"));
        }
        let x_mode = mode.ok_or_else(|| fail("declare G7 diameter or G8 radius before motion"))?;
        if b.x.is_none() && b.z.is_none() {
            return Err(fail("move has no endpoint"));
        }
        let initial = || fail("first move must set both X and Z");
        let x = b.x.or(position.map(|p| p.0)).ok_or_else(initial)?;
        let z = b.z.or(position.map(|p| p.1)).ok_or_else(initial)?;
        moves += 1;
        if moves > MAX_STROKES {
            return Err(fail("move limit exceeded"));
        }
        let (basis, radius, diameter) = match x_mode {
            XMode::Diameter => ("diameter", x / 2.0, x),
            XMode::Radius => ("radius", x, x * 2.0),
        };
        write!(
            output,
            "line {} G{motion} X={x} ({basis}) Z={z} radius={radius} diameter={diameter}",
            index + 1
        )
        .unwrap();
        if motion == 1 {
            let f = feed.ok_or_else(|| fail("G1 requires a positive F in mm/min"))?;
            write!(output, " F={f}").unwrap();
        }
        output.push('\n');
        position = Some((x, z));
    }
    if position.is_none() {
        return Err("no X/Z moves in program".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifts_before_each_xy_transfer_including_a_dot() {
        let program = gcode(
            &[vec![(1.0, 2.0), (3.0, 4.0)], vec![(-1.0, 0.0)]],
            4.0,
            -0.5,
            120.0,
        )
        .unwrap();
        assert_eq!(
            program,
            concat!(
                "G21 G17 G90 G94\nG0 Z4.000\nG0 X1.000 Y2.000\nG1 Z-0.500 F120.000\n",
                "G1 X3.000 Y4.000\nG0 Z4.000\nG0 X-1.000 Y0.000\nG1 Z-0.500 F120.000\nG0 Z4.000\nM2\n"
            )
        );
    }

    #[test]
    fn rejects_invalid_heights_feed_and_late_points() {
        let good = vec![vec![(0.0, 0.0)]];
        for (up, down, feed) in [
            (f64::NAN, 0., 1.),
            (1., f64::INFINITY, 1.),
            (1., 1., 1.),
            (0.0001, -0.0001, 1.),
            (2., 0., 0.0001),
            (2., 0., f64::NAN),
        ] {
            assert!(gcode(&good, up, down, feed).is_err());
        }
        assert!(gcode(&good, 0., 2., 1.).unwrap().contains("G1 Z2.000"));
        for x in [f64::NAN, f64::INFINITY, MAX_MM + 1.] {
            assert!(gcode(&[good[0].clone(), vec![(0., x)]], 2., 0., 1.).is_err());
        }
        for strokes in [
            vec![],
            vec![vec![]],
            vec![vec![(0., 0.); MAX_POINTS + 1]],
            vec![vec![]; MAX_STROKES + 1],
        ] {
            assert!(gcode(&strokes, 1., 0., 1.).is_err());
        }
    }

    #[test]
    fn diameter_and_radius_interpretations_differ() {
        let p = lathe_preview("g21g90g18g94g7\ng00x10z2\ng01z-1f120\nm30").unwrap();
        assert!(p.contains("G1 X=10 (diameter) Z=-1 radius=5 diameter=10 F=120"));
        let p = lathe_preview("G21 G90 G8 G0 X10 Z2\nF100\nG1 X12\nM2").unwrap();
        assert!(p.contains("G1 X=12 (radius) Z=2 radius=12 diameter=24 F=100"));
    }

    #[test]
    fn refuses_ambiguous_or_unsupported_blocks() {
        let header = "G21 G90 G7\nG0 X20 Z2\n";
        for line in [
            "X4",
            "G1 Z1",
            "G1 F2",
            "G8",
            "G0 G1 X1",
            "G7 G8",
            "G0 X2 X4",
            "G0 X-3",
            "G0 Z10001",
            "G0 X1e2",
            "G0 Z1.2.3",
            "G0 X+.",
            "G20",
            "G91",
            "G95",
            "G96",
            "G53 G0 Z0",
            "G2 X2",
            "G0 Y2",
            "G01.1 X2",
            "G0 Z#1",
            ";PROBEOPEN file",
            "(LOGCLOSE)",
            "T1",
            "M3",
            "S500",
            "G92 Z0",
            "G1 F0 Z1",
            "M2 G0 X1",
            "M30\nG0 Z1",
        ] {
            assert!(
                lathe_preview(&format!("{header}{line}")).is_err(),
                "accepted {line}"
            );
        }
        for program in [
            "G0 X0 Z0",
            "G21 G90 G0 X2 Z0",
            "G21 G7 G0 X0 Z0",
            "G21 G90 G8 G0 X1",
            "G21 G90 G8",
        ] {
            assert!(lathe_preview(program).is_err(), "accepted {program}");
        }
        assert!(lathe_preview(&(header.to_owned() + &" ".repeat(4097))).is_err());
        assert!(lathe_preview(&" ".repeat(1_048_577)).is_err());
    }
}
