//! Bounded cursor images and a small glowing cursor renderer.
//!
//! Pixels use premultiplied ARGB, matching the native Xcursor representation.
//! Rendering and loading an image never opens a desktop or changes a cursor.

use std::{fs::File, io::Read, path::Path};

pub const MAX_CURSOR_DIMENSION: u32 = 256;
pub const MAX_PAM_HEADER_BYTES: usize = 4096;
pub const MAX_PAM_BYTES: usize =
    (MAX_CURSOR_DIMENSION * MAX_CURSOR_DIMENSION * 4) as usize + MAX_PAM_HEADER_BYTES;

/// One cursor image. The hotspot is the pixel anchored to the pointer position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    /// Premultiplied `0xAARRGGBB` pixels, in row-major order.
    pub pixels: Vec<u32>,
}

impl CursorImage {
    pub fn validate(&self) -> Result<(), String> {
        let count = image_geometry(self.width, self.height, self.hotspot_x, self.hotspot_y)?;
        if self.pixels.len() != count {
            return Err("cursor pixel count does not match its dimensions".into());
        }
        for pixel in &self.pixels {
            let alpha = pixel >> 24;
            if ((pixel >> 16) & 255) > alpha
                || ((pixel >> 8) & 255) > alpha
                || (pixel & 255) > alpha
            {
                return Err("cursor pixels must contain premultiplied ARGB channels".into());
            }
        }
        Ok(())
    }

    /// Convert straight-alpha RGBA bytes to the native premultiplied format.
    pub fn from_rgba(
        width: u32,
        height: u32,
        hotspot_x: u32,
        hotspot_y: u32,
        rgba: &[u8],
    ) -> Result<Self, String> {
        let count = image_geometry(width, height, hotspot_x, hotspot_y)?;
        if rgba.len() != count * 4 {
            return Err("cursor RGBA byte count does not match its dimensions".into());
        }
        let pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| {
                let alpha = u32::from(pixel[3]);
                let channel = |n: u8| (u32::from(n) * alpha + 127) / 255;
                (alpha << 24)
                    | (channel(pixel[0]) << 16)
                    | (channel(pixel[1]) << 8)
                    | channel(pixel[2])
            })
            .collect();
        Ok(Self {
            width,
            height,
            hotspot_x,
            hotspot_y,
            pixels,
        })
    }

    /// Read a P7 PAM image with `MAXVAL 255` and `RGB` or `RGB_ALPHA` tuples.
    /// The header and payload are strictly bounded; trailing data is rejected.
    pub fn from_pam(bytes: &[u8], hotspot_x: u32, hotspot_y: u32) -> Result<Self, String> {
        if bytes.len() > MAX_PAM_BYTES {
            return Err("cursor PAM exceeds the bounded image size".into());
        }
        let mut offset = 0;
        if header_line(bytes, &mut offset)? != "P7" {
            return Err("cursor image must use the P7 PAM format".into());
        }
        let (mut width, mut height, mut depth, mut maxval) = (None, None, None, None);
        let mut tuple = None;
        loop {
            let line = header_line(bytes, &mut offset)?;
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line == "ENDHDR" {
                break;
            }
            let mut words = line.split_ascii_whitespace();
            let key = words.next().ok_or("cursor PAM header field is missing")?;
            let value = words.next().ok_or("cursor PAM header value is missing")?;
            if words.next().is_some() {
                return Err("cursor PAM header fields require one value".into());
            }
            match key {
                "WIDTH" => pam_number(&mut width, value)?,
                "HEIGHT" => pam_number(&mut height, value)?,
                "DEPTH" => pam_number(&mut depth, value)?,
                "MAXVAL" => pam_number(&mut maxval, value)?,
                "TUPLTYPE" => {
                    if tuple.replace(value).is_some() {
                        return Err("cursor PAM contains a duplicate TUPLTYPE field".into());
                    }
                }
                _ => return Err(format!("unsupported cursor PAM field: {key}")),
            }
        }
        let width = width.ok_or("cursor PAM is missing WIDTH")?;
        let height = height.ok_or("cursor PAM is missing HEIGHT")?;
        let depth = depth.ok_or("cursor PAM is missing DEPTH")?;
        if maxval != Some(255)
            || !matches!((depth, tuple), (3, Some("RGB")) | (4, Some("RGB_ALPHA")))
        {
            return Err("cursor PAM requires MAXVAL 255 and RGB or RGB_ALPHA tuples".into());
        }
        let count = image_geometry(width, height, hotspot_x, hotspot_y)?;
        let payload = &bytes[offset..];
        if payload.len() != count * depth as usize {
            return Err("cursor PAM payload length does not match its dimensions".into());
        }
        if depth == 4 {
            Self::from_rgba(width, height, hotspot_x, hotspot_y, payload)
        } else {
            let mut rgba = Vec::with_capacity(count * 4);
            for pixel in payload.as_chunks::<3>().0 {
                rgba.extend_from_slice(pixel);
                rgba.push(255);
            }
            Self::from_rgba(width, height, hotspot_x, hotspot_y, &rgba)
        }
    }

    /// Load at most the permitted image size, including when the file grows
    /// after it is opened. The caller can validate appearance before input.
    pub fn load_pam(
        path: impl AsRef<Path>,
        hotspot_x: u32,
        hotspot_y: u32,
    ) -> Result<Self, String> {
        let file =
            File::open(path.as_ref()).map_err(|error| format!("open cursor PAM: {error}"))?;
        let mut bytes = Vec::new();
        file.take((MAX_PAM_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read cursor PAM: {error}"))?;
        Self::from_pam(&bytes, hotspot_x, hotspot_y)
    }
}

fn image_geometry(width: u32, height: u32, x: u32, y: u32) -> Result<usize, String> {
    if !(1..=MAX_CURSOR_DIMENSION).contains(&width) || !(1..=MAX_CURSOR_DIMENSION).contains(&height)
    {
        return Err("cursor image dimensions must be 1..256 pixels".into());
    }
    if x >= width || y >= height {
        return Err("cursor hotspot must lie inside its image".into());
    }
    Ok((width * height) as usize)
}

fn header_line<'a>(bytes: &'a [u8], offset: &mut usize) -> Result<&'a str, String> {
    let end = bytes.len().min(MAX_PAM_HEADER_BYTES);
    if *offset >= end {
        return Err("cursor PAM header is incomplete or exceeds 4096 bytes".into());
    }
    let length = bytes[*offset..end]
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("cursor PAM header is incomplete or exceeds 4096 bytes")?;
    let line = std::str::from_utf8(&bytes[*offset..*offset + length])
        .map_err(|_| "cursor PAM header is not valid UTF-8")?;
    *offset += length + 1;
    Ok(line.strip_suffix('\r').unwrap_or(line))
}

fn pam_number(slot: &mut Option<u32>, value: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err("cursor PAM contains a duplicate numeric field".into());
    }
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("cursor PAM header numbers must be unsigned decimal integers".into());
    }
    *slot = Some(
        value
            .parse()
            .map_err(|_| "cursor PAM header number is too large")?,
    );
    Ok(())
}

/// Straight-alpha color. Alpha controls the opacity of the entire rendered
/// cursor, including its outline and glow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Default for CursorColor {
    fn default() -> Self {
        Self {
            r: 47,
            g: 226,
            b: 255,
            a: 255,
        }
    }
}

impl CursorColor {
    pub fn parse(value: &str) -> Result<Self, String> {
        let bytes = value.as_bytes();
        if !matches!(bytes.len(), 7 | 9) || bytes.first() != Some(&b'#') {
            return Err("cursor color must be #RRGGBB or #RRGGBBAA".into());
        }
        let pair = |i| -> Result<u8, String> {
            let hex = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            let high = hex(bytes[i]).ok_or("cursor color contains a non-hexadecimal digit")?;
            let low = hex(bytes[i + 1]).ok_or("cursor color contains a non-hexadecimal digit")?;
            Ok(high * 16 + low)
        };
        Ok(Self {
            r: pair(1)?,
            g: pair(3)?,
            b: pair(5)?,
            a: if bytes.len() == 9 { pair(7)? } else { 255 },
        })
    }
}

impl std::str::FromStr for CursorColor {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Arrow,
    Ring,
    Crosshair,
    Dot,
}

impl CursorShape {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "arrow" => Ok(Self::Arrow),
            "ring" => Ok(Self::Ring),
            "crosshair" => Ok(Self::Crosshair),
            "dot" => Ok(Self::Dot),
            _ => Err("cursor shape must be arrow, ring, crosshair or dot".into()),
        }
    }
}

impl std::str::FromStr for CursorShape {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorStyle {
    pub color: CursorColor,
    pub shape: CursorShape,
    /// Shape size in pixels, excluding transparent padding for the glow.
    pub size: u32,
    /// Glow strength in 0..=1. Zero retains the shape and contrast outline.
    pub glow: f64,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            color: CursorColor::default(),
            shape: CursorShape::Arrow,
            size: 32,
            glow: 0.7,
        }
    }
}

impl CursorStyle {
    pub fn validate(&self) -> Result<(), String> {
        if !(16..=96).contains(&self.size) {
            return Err("cursor size must be 16..96 pixels".into());
        }
        if !self.glow.is_finite() || !(0.0..=1.0).contains(&self.glow) {
            return Err("cursor glow must be finite and in 0..1".into());
        }
        Ok(())
    }

    /// Render a small antialiased cursor with a dark contrast outline and a
    /// bounded colored glow. The outer border remains completely transparent.
    pub fn render(&self) -> Result<CursorImage, String> {
        self.validate()?;
        let size = f64::from(self.size);
        let radius = size * 0.16 + 2.0;
        let padding = radius.ceil() as u32 + 2;
        let dimension = self.size + padding * 2;
        let hotspot = if self.shape == CursorShape::Arrow {
            padding
        } else {
            padding + self.size / 2
        };
        let rgb = [
            f64::from(self.color.r) / 255.0,
            f64::from(self.color.g) / 255.0,
            f64::from(self.color.b) / 255.0,
        ];
        let opacity = f64::from(self.color.a) / 255.0;
        let mut pixels = Vec::with_capacity((dimension * dimension) as usize);
        for y in 0..dimension {
            for x in 0..dimension {
                let px = f64::from(x) - f64::from(hotspot);
                let py = f64::from(y) - f64::from(hotspot);
                let distance = signed_distance(self.shape, px, py, size);
                let fade = (1.0 - distance.max(0.0) / radius).clamp(0.0, 1.0);
                let mut pixel = [0.0; 4];
                over(&mut pixel, rgb, self.glow * 0.42 * fade * fade);
                over(
                    &mut pixel,
                    [0.035, 0.045, 0.065],
                    coverage(distance - 1.15) * 0.94,
                );
                over(&mut pixel, rgb, coverage(distance));
                // A tiny light anchor makes the exact hotspot visible on both
                // light and dark backgrounds without changing the pointer tip.
                let anchor = coverage((px * px + py * py).sqrt() - 0.45) * 0.65;
                over(&mut pixel, [1.0; 3], anchor);
                let alpha = (pixel[3] * opacity * 255.0).round() as u32;
                let channel = |n: usize| ((pixel[n] * opacity * 255.0).round() as u32).min(alpha);
                pixels.push((alpha << 24) | (channel(0) << 16) | (channel(1) << 8) | channel(2));
            }
        }
        Ok(CursorImage {
            width: dimension,
            height: dimension,
            hotspot_x: hotspot,
            hotspot_y: hotspot,
            pixels,
        })
    }
}

fn coverage(distance: f64) -> f64 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn over(pixel: &mut [f64; 4], rgb: [f64; 3], alpha: f64) {
    for (channel, color) in pixel[..3].iter_mut().zip(rgb) {
        *channel = color * alpha + *channel * (1.0 - alpha);
    }
    pixel[3] = alpha + pixel[3] * (1.0 - alpha);
}

fn signed_distance(shape: CursorShape, x: f64, y: f64, size: f64) -> f64 {
    match shape {
        CursorShape::Arrow => {
            polygon_distance(
                x / size,
                y / size,
                &[
                    (0.0, 0.0),
                    (0.0, 0.82),
                    (0.22, 0.60),
                    (0.40, 0.96),
                    (0.57, 0.87),
                    (0.38, 0.51),
                    (0.74, 0.51),
                ],
            ) * size
        }
        CursorShape::Ring => {
            let length = (x * x + y * y).sqrt();
            let ring = (length - size * 0.32).abs() - (size * 0.045).max(1.2);
            ring.min(length - (size * 0.025).max(0.8))
        }
        CursorShape::Crosshair => {
            let radius = (size * 0.027).max(0.7);
            let gap = (size * 0.12).max(2.5);
            let length = size * 0.38;
            let horizontal = segment_distance(x.abs(), y, (gap, 0.0), (length, 0.0)) - radius;
            let vertical = segment_distance(x, y.abs(), (0.0, gap), (0.0, length)) - radius;
            horizontal.min(vertical).min((x * x + y * y).sqrt() - 0.8)
        }
        CursorShape::Dot => (x * x + y * y).sqrt() - size * 0.14,
    }
}

fn segment_distance(x: f64, y: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let px = x - a.0 - dx * t;
    let py = y - a.1 - dy * t;
    (px * px + py * py).sqrt()
}

fn polygon_distance(x: f64, y: f64, points: &[(f64, f64)]) -> f64 {
    let mut distance = f64::INFINITY;
    let mut inside = false;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        distance = distance.min(segment_distance(x, y, a, b));
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
    }
    if inside { -distance } else { distance }
}
