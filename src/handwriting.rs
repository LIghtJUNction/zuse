//! Single-line glyphs in a unit square, with the origin at the top left.
use std::collections::BTreeMap;

use crate::{
    drawing::{MAX_POINTS, MAX_PROGRAM_POINTS},
    motion::{MAX_COORDINATE, Rng},
};

type Stroke = Vec<(f64, f64)>;

#[derive(Debug)]
pub struct Font {
    glyphs: BTreeMap<char, Vec<Stroke>>,
}

/// All distances are in output coordinates. Advances measure origin to origin.
/// Offsets are the maximum absolute translations applied to an entire glyph.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub size: f64,
    pub advance: f64,
    pub line_height: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub seed: u64,
}

impl Font {
    pub fn builtin() -> Self {
        let mut font = Self::parse(BUILTIN).expect("valid built-in handwriting font");
        for upper in 'A'..='Z' {
            let glyph = font.glyphs[&upper].clone();
            font.glyphs.insert(upper.to_ascii_lowercase(), glyph);
        }
        font
    }

    /// One glyph per line: a literal scalar or U+hex, then whitespace-separated
    /// x,y pairs in [0,1]. A vertical bar lifts the pen. Blank lines and lines
    /// beginning with # are ignored. Use U+0023 for a literal number sign.
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.len() > 1024 * 1024 {
            return Err("handwriting font exceeds 1 MiB".into());
        }
        let mut glyphs = BTreeMap::new();
        let mut total = 0;
        for (index, line) in source.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let error = |reason: &str| format!("font line {}: {reason}", index + 1);
            let (name, paths) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| error("expected a glyph and its strokes"))?;
            let glyph = if let Some(hex) = name.strip_prefix("U+") {
                if hex.is_empty() || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(error("expected 1..6 hexadecimal digits after U+"));
                }
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else {
                let mut chars = name.chars();
                chars.next().filter(|_| chars.next().is_none())
            }
            .filter(|ch| !ch.is_control() && !ch.is_whitespace())
            .ok_or_else(|| error("expected one visible Unicode scalar"))?;
            if glyphs.contains_key(&glyph) {
                return Err(error("duplicate glyph"));
            }
            let mut strokes = Vec::new();
            for path in paths.split('|') {
                let mut stroke = Vec::new();
                for pair in path.split_whitespace() {
                    if stroke.len() == MAX_POINTS || total == MAX_PROGRAM_POINTS {
                        return Err(error("font exceeds the point limit"));
                    }
                    let (x, y) = pair.split_once(',').ok_or_else(|| error("expected x,y"))?;
                    let coordinate = |s: &str| {
                        s.parse::<f64>()
                            .ok()
                            .filter(|n| (0.0..=1.0).contains(n))
                            .ok_or_else(|| error("expected a finite coordinate in [0,1]"))
                    };
                    stroke.push((coordinate(x)?, coordinate(y)?));
                    total += 1;
                }
                if stroke.len() < 2 {
                    return Err(error("each stroke needs at least two points"));
                }
                strokes.push(stroke);
            }
            glyphs.insert(glyph, strokes);
        }
        if glyphs.is_empty() {
            return Err("handwriting font contains no glyphs".into());
        }
        Ok(Self { glyphs })
    }
}

fn translation(rng: &mut Rng, bound: f64) -> f64 {
    let fraction = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    (2.0 * fraction - 1.0) * bound
}

pub fn strokes(font: &Font, text: &str, layout: &Layout) -> Result<Vec<Vec<(f64, f64)>>, String> {
    if [layout.size, layout.advance, layout.line_height]
        .into_iter()
        .any(|n| !(n > 0.0 && n <= MAX_COORDINATE))
    {
        return Err(
            "size, advance and line height must be positive and within coordinate bounds".into(),
        );
    }
    if [layout.offset_x, layout.offset_y]
        .into_iter()
        .any(|n| !(0.0..=MAX_COORDINATE).contains(&n))
    {
        return Err("glyph offsets must be nonnegative and within coordinate bounds".into());
    }
    if text.len() > 1024 * 1024 {
        return Err("handwriting text exceeds 1 MiB".into());
    }
    let mut rng = Rng::new(layout.seed);
    let mut output = Vec::new();
    let mut total = 0;
    let mut chars = text.chars().peekable();
    let (mut column, mut row) = (0u32, 0u32);
    while let Some(ch) = chars.next() {
        if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            column = 0;
            row += 1;
            continue;
        }
        let x = f64::from(column) * layout.advance;
        let y = f64::from(row) * layout.line_height;
        column += 1;
        if x > MAX_COORDINATE || y > MAX_COORDINATE {
            return Err("glyph origin exceeds coordinate bounds".into());
        }
        if ch == ' ' || ch == '\u{3000}' {
            continue;
        }
        let glyph = font
            .glyphs
            .get(&ch)
            .ok_or_else(|| format!("missing glyph U+{:04X}", ch as u32))?;
        let x = x + translation(&mut rng, layout.offset_x);
        let y = y + translation(&mut rng, layout.offset_y);
        for path in glyph {
            total += path.len();
            if total > MAX_PROGRAM_POINTS {
                return Err("laid-out handwriting exceeds the total point limit".into());
            }
            let mut stroke = Vec::with_capacity(path.len());
            for &(u, v) in path {
                let point = (x + u * layout.size, y + v * layout.size);
                if point.0.abs() > MAX_COORDINATE || point.1.abs() > MAX_COORDINATE {
                    return Err("handwriting point exceeds coordinate bounds".into());
                }
                stroke.push(point);
            }
            output.push(stroke);
        }
    }
    Ok(output)
}

// An original monoline alphabet. Lowercase ASCII uses the same block forms.
const BUILTIN: &str = "
A 0,1 .5,0 1,1 | .2,.6 .8,.6
B 0,1 0,0 .65,0 .9,.15 .9,.35 .65,.5 0,.5 | .65,.5 1,.65 1,.85 .65,1 0,1
C 1,.15 .75,0 .2,0 0,.2 0,.8 .2,1 .75,1 1,.85
D 0,1 0,0 .6,0 1,.25 1,.75 .6,1 0,1
E 1,0 0,0 0,1 1,1 | 0,.5 .75,.5
F 0,1 0,0 1,0 | 0,.5 .75,.5
G 1,.2 .75,0 .2,0 0,.2 0,.8 .2,1 1,1 1,.55 .55,.55
H 0,0 0,1 | 1,0 1,1 | 0,.5 1,.5
I .1,0 .9,0 | .5,0 .5,1 | .1,1 .9,1
J .3,0 1,0 | .8,0 .8,.8 .6,1 .2,1 0,.8
K 0,0 0,1 | 1,0 0,.55 1,1
L 0,0 0,1 1,1
M 0,1 0,0 .5,.55 1,0 1,1
N 0,1 0,0 1,1 1,0
O .2,0 .8,0 1,.2 1,.8 .8,1 .2,1 0,.8 0,.2 .2,0
P 0,1 0,0 .7,0 1,.2 1,.4 .7,.55 0,.55
Q .2,0 .8,0 1,.2 1,.75 .75,.95 .2,.95 0,.75 0,.2 .2,0 | .55,.6 1,1
R 0,1 0,0 .7,0 1,.2 1,.4 .7,.55 0,.55 | .5,.55 1,1
S 1,.15 .8,0 .2,0 0,.2 .1,.4 .85,.6 1,.8 .8,1 .2,1 0,.85
T 0,0 1,0 | .5,0 .5,1
U 0,0 0,.75 .25,1 .75,1 1,.75 1,0
V 0,0 .5,1 1,0
W 0,0 .2,1 .5,.5 .8,1 1,0
X 0,0 1,1 | 1,0 0,1
Y 0,0 .5,.5 1,0 | .5,.5 .5,1
Z 0,0 1,0 0,1 1,1
0 .2,0 .8,0 1,.2 1,.8 .8,1 .2,1 0,.8 0,.2 .2,0
1 .2,.25 .55,0 .55,1 | .2,1 .9,1
2 0,.2 .25,0 .8,0 1,.2 1,.4 0,1 1,1
3 0,.1 .25,0 .8,0 1,.2 .8,.5 .35,.5 | .8,.5 1,.75 .8,1 .2,1 0,.9
4 .8,1 .8,0 0,.7 1,.7
5 1,0 .1,0 0,.5 .75,.5 1,.7 .9,.9 .7,1 .15,1 0,.85
6 .9,0 .4,.05 0,.45 0,.8 .2,1 .75,1 1,.8 1,.55 .75,.4 .25,.4 0,.6
7 0,0 1,0 .35,1
8 .2,.5 0,.3 .1,.1 .3,0 .7,0 .9,.1 1,.3 .8,.5 .2,.5 0,.7 .1,.9 .3,1 .7,1 .9,.9 1,.7 .8,.5
9 1,.45 .75,.6 .25,.6 0,.45 0,.2 .2,0 .8,0 1,.2 1,.55 .6,.95 .1,1
- .1,.5 .9,.5
. .45,.95 .55,.95
! .5,0 .5,.7 | .48,.95 .52,.95
? 0,.25 .2,0 .75,0 1,.2 .95,.4 .5,.65 .5,.72 | .48,.95 .52,.95
一 .05,.5 .95,.5
二 .15,.25 .85,.25 | .05,.8 .95,.8
三 .15,.15 .85,.15 | .2,.48 .8,.48 | .05,.85 .95,.85
十 .05,.35 .95,.35 | .5,.05 .5,.95
人 .52,.05 .48,.4 .32,.7 .05,.95 | .5,.35 .66,.68 .95,.95
中 .1,.2 .1,.7 | .1,.2 .9,.2 .9,.7 | .1,.65 .9,.65 | .5,.02 .5,.98
文 .4,.02 .52,.16 | .05,.23 .95,.23 | .75,.25 .62,.55 .4,.75 .05,.95 | .23,.3 .4,.55 .62,.76 .95,.95
你 .28,.02 .17,.25 .04,.41 | .17,.25 .17,.96 | .52,.06 .45,.22 .34,.35 | .46,.2 .94,.2 .84,.36 | .63,.29 .63,.94 .53,.86 | .49,.45 .34,.75 | .76,.45 .92,.72
好 .24,.04 .09,.57 .38,.78 | .04,.32 .43,.32 .36,.62 .24,.83 .04,.96 | .48,.16 .93,.16 .69,.4 | .69,.4 .75,.58 .75,.91 .68,.97 .59,.9 | .44,.56 .98,.56
";

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> Layout {
        Layout {
            size: 8.0,
            advance: 10.0,
            line_height: 12.0,
            offset_x: 0.0,
            offset_y: 0.0,
            seed: 0,
        }
    }

    #[test]
    fn imported_unicode_keeps_stroke_order_spacing_and_newlines() {
        let font =
            Font::parse("# two strokes\nU+4E8C 0,.25 1,.25 | 0,.75 1,.75\n𠀀 0,0 1,1\n").unwrap();
        let paths = strokes(&font, "二 𠀀\r\n二", &geometry()).unwrap();
        assert_eq!(
            paths,
            [
                vec![(0.0, 2.0), (8.0, 2.0)],
                vec![(0.0, 6.0), (8.0, 6.0)],
                vec![(20.0, 0.0), (28.0, 8.0)],
                vec![(0.0, 14.0), (8.0, 14.0)],
                vec![(0.0, 18.0), (8.0, 18.0)],
            ]
        );
    }

    #[test]
    fn offsets_translate_whole_glyphs_and_keep_axes_independent() {
        let font = Font::parse("字 0,0 1,0 | 0,1 1,1").unwrap();
        let make = |x, y, seed| {
            strokes(
                &font,
                "字字",
                &Layout {
                    offset_x: x,
                    offset_y: y,
                    seed,
                    ..geometry()
                },
            )
            .unwrap()
        };
        let paths = make(2.0, 3.0, 19);
        assert_eq!(paths, make(2.0, 3.0, 19));
        assert_ne!(paths, make(2.0, 3.0, 20));
        let horizontal = make(2.0, 0.0, 19);
        let vertical = make(0.0, 3.0, 19);
        let plain = make(0.0, 0.0, 19);
        for glyph in 0..2 {
            let (x, y) = paths[2 * glyph][0];
            let dx = x - plain[2 * glyph][0].0;
            assert!(dx.abs() <= 2.0 && y.abs() <= 3.0);
            for s in 2 * glyph..2 * glyph + 2 {
                for i in 0..2 {
                    let (u, v) = plain[s][i];
                    assert!((paths[s][i].0 - u - dx).abs() < 1e-12);
                    assert!((paths[s][i].1 - v - y).abs() < 1e-12);
                    assert_eq!(paths[s][i], (horizontal[s][i].0, vertical[s][i].1));
                    assert_eq!((vertical[s][i].0, horizontal[s][i].1), (u, v));
                }
            }
        }
        assert_ne!(paths[0][0].1, paths[2][0].1);
    }

    #[test]
    fn invalid_fonts_report_line_numbers() {
        for bad in [
            "x 0,0",
            "x 0,0 1,1 |",
            "x 0,0 NaN,0",
            "x 0,0 1.1,0",
            "x 0,0 inf,1",
            "x 0,0 1,2,3",
            "U+D800 0,0 1,1",
            "U+110000 0,0 1,1",
            "xy 0,0 1,1",
            "U+0020 0,0 1,1",
            "U+ 0,0 1,1",
            "U+0041 0,0 1,1",
        ] {
            assert!(
                Font::parse(&format!("A 0,0 1,1\n{bad}"))
                    .unwrap_err()
                    .contains("line 2"),
                "{bad}"
            );
        }
        assert!(Font::parse("# empty").is_err());
    }

    #[test]
    fn caps_apply_to_source_strokes_layout_and_expanded_points() {
        let font = Font::parse("x 0,0 1,1").unwrap();
        assert!(
            strokes(
                &font,
                "x",
                &Layout {
                    offset_y: f64::NAN,
                    ..geometry()
                }
            )
            .is_err()
        );
        assert!(
            strokes(
                &font,
                "x",
                &Layout {
                    size: f64::INFINITY,
                    ..geometry()
                }
            )
            .is_err()
        );
        assert!(
            strokes(
                &font,
                "xx",
                &Layout {
                    advance: MAX_COORDINATE,
                    ..geometry()
                }
            )
            .is_err()
        );
        assert!(
            strokes(
                &font,
                "\nx",
                &Layout {
                    line_height: MAX_COORDINATE,
                    ..geometry()
                }
            )
            .is_err()
        );
        assert!(strokes(&font, &"x".repeat(MAX_PROGRAM_POINTS / 2 + 1), &geometry()).is_err());
        assert!(Font::parse(&format!("x {}", "0,0 ".repeat(MAX_POINTS + 1))).is_err());
        assert!(Font::parse(&"#".repeat(1024 * 1024 + 1)).is_err());
    }

    #[test]
    fn builtin_handles_letters_digits_and_chinese_and_names_missing_glyph() {
        let font = Font::builtin();
        let text = "Hello Zuse! 0123456789\n你好中文";
        assert!(!strokes(&font, text, &geometry()).unwrap().is_empty());
        assert!(strokes(&font, " \n　", &geometry()).unwrap().is_empty());
        assert!(
            strokes(&font, "🙂", &geometry())
                .unwrap_err()
                .contains("U+1F642")
        );
    }
}
