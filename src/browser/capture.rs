//! Native CDP screenshot options and encoded-image dimensions.
//! JPEG validation checks marker framing, not Huffman decoding or pixel data.
use super::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Png,
    Jpeg(u8),
}

impl Format {
    pub(crate) fn parse(format: &str, quality: Option<i64>) -> Result<Self> {
        match format {
            "png" if quality.is_none() => Ok(Self::Png),
            "png" => Err("browser.capture quality is only available for format=jpeg".into()),
            "jpeg" => {
                let quality = quality.unwrap_or(80);
                if !(0..=100).contains(&quality) {
                    return Err("browser.capture quality must be 0..100".into());
                }
                Ok(Self::Jpeg(quality as u8))
            }
            _ => Err("browser.capture format must be png or jpeg".into()),
        }
    }

    pub(super) fn parameters(self) -> String {
        match self {
            Self::Png => {
                r#"{"format":"png","fromSurface":true,"captureBeyondViewport":false}"#.into()
            }
            Self::Jpeg(quality) => format!(
                r#"{{"format":"jpeg","quality":{quality},"fromSurface":true,"captureBeyondViewport":false}}"#
            ),
        }
    }

    pub(super) fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg(_) => "image/jpeg",
        }
    }

    pub(super) fn dimensions(self, bytes: &[u8]) -> Result<(u32, u32)> {
        match self {
            Self::Png => crate::screenshot::png_dimensions(bytes, 65_535),
            Self::Jpeg(_) => jpeg_dimensions(bytes),
        }
        .map_err(|error| format!("browser {error}"))
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("screenshot is not a JPEG".into());
    }
    let mut offset = 2usize;
    let mut dimensions = None;
    let mut components = Vec::new();
    let mut scan = false;
    let mut scan_seen = false;
    let mut scan_data = false;
    while offset < bytes.len() {
        if scan && bytes[offset] != 0xff {
            scan_data = true;
            offset += 1;
            continue;
        }
        if bytes[offset] != 0xff {
            return Err("JPEG marker is invalid".into());
        }
        let marker_start = offset;
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset).ok_or("JPEG marker is truncated")?;
        offset += 1;
        if scan && marker == 0x00 && offset == marker_start + 2 {
            // A literal FF byte in entropy-coded data is byte-stuffed.
            scan_data = true;
            continue;
        }
        if scan && (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if scan && !scan_data {
            return Err("JPEG scan has no image data".into());
        }
        scan = false;
        if marker == 0xd9 {
            if offset != bytes.len() || !scan_seen {
                return Err("JPEG terminator is invalid".into());
            }
            return dimensions.ok_or_else(|| "JPEG has no frame dimensions".into());
        }
        if matches!(marker, 0x00 | 0x01 | 0xd0..=0xd8) {
            return Err("JPEG contains an unexpected standalone marker".into());
        }
        let length_bytes = bytes
            .get(offset..offset + 2)
            .ok_or("JPEG segment length is truncated")?;
        let length = usize::from(u16::from_be_bytes(length_bytes.try_into().unwrap()));
        if length < 2 {
            return Err("JPEG segment length is invalid".into());
        }
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= bytes.len())
            .ok_or("JPEG segment is truncated")?;
        let payload = &bytes[offset + 2..end];
        match marker {
            0xc0..=0xc2 => {
                // CDP screenshots use an 8-bit DCT frame. Accept sequential
                // and progressive scans, but never an embedded APP thumbnail.
                if dimensions.is_some() || scan_seen || payload.len() < 6 {
                    return Err("JPEG frame header is invalid or duplicated".into());
                }
                let count = usize::from(payload[5]);
                if payload[0] != 8 || !(1..=4).contains(&count) || payload.len() != 6 + 3 * count {
                    return Err("JPEG frame encoding or component count is unsupported".into());
                }
                let height = u32::from(u16::from_be_bytes([payload[1], payload[2]]));
                let width = u32::from(u16::from_be_bytes([payload[3], payload[4]]));
                if width == 0 || height == 0 {
                    return Err("JPEG dimensions must be nonzero".into());
                }
                for component in payload[6..].as_chunks::<3>().0 {
                    if components.contains(&component[0]) {
                        return Err("JPEG frame contains a duplicate component".into());
                    }
                    components.push(component[0]);
                }
                dimensions = Some((width, height));
            }
            0xda => {
                if dimensions.is_none() || payload.is_empty() {
                    return Err("JPEG scan precedes its frame dimensions".into());
                }
                let count = usize::from(payload[0]);
                if count == 0 || count > components.len() || payload.len() != 4 + 2 * count {
                    return Err("JPEG scan header is invalid".into());
                }
                let mut selected = Vec::new();
                for component in payload[1..1 + 2 * count].as_chunks::<2>().0 {
                    if !components.contains(&component[0]) || selected.contains(&component[0]) {
                        return Err("JPEG scan component is invalid".into());
                    }
                    selected.push(component[0]);
                }
                scan = true;
                scan_seen = true;
                scan_data = false;
            }
            0xc4 | 0xdb | 0xdd | 0xe0..=0xef | 0xfe => (),
            _ => return Err("JPEG marker encoding is unsupported".into()),
        }
        offset = end;
    }
    Err("JPEG has no complete image terminator".into())
}
