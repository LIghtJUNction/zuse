//! Streaming PNG encoder with stored DEFLATE blocks and a 64 KiB buffer.
use super::Result;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

fn crc_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    crc
}
fn chunk<W: Write>(w: &mut W, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(kind)?;
    w.write_all(data)?;
    let crc = crc_update(crc_update(u32::MAX, kind), data) ^ u32::MAX;
    w.write_all(&crc.to_be_bytes())
}
/// Native pixel storage remains owned by the OS adapter. Stored DEFLATE
/// prioritizes capture latency over compression ratio.
pub(super) fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    pixel: impl FnMut(u32, u32) -> [u8; 3],
) -> Result<()> {
    write_png_checked(path, width, height, pixel, || Ok(()))
}

/// The checked variant bounds cooperative encoding work for a native capture.
/// Its caller owns the temporary output and commits it only after validation.
pub(super) fn write_png_checked(
    path: &Path,
    width: u32,
    height: u32,
    mut pixel: impl FnMut(u32, u32) -> [u8; 3],
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    if width == 0
        || height == 0
        || width > 16_384
        || height > 16_384
        || u64::from(width) * u64::from(height) > 67_108_864
    {
        return Err("capture dimensions exceed 64 megapixels or 16384 pixels per axis".into());
    }
    check()?;
    let mut encode = || -> std::io::Result<()> {
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(b"\x89PNG\r\n\x1a\n")?;
        let mut ihdr = [0u8; 13];
        ihdr[..4].copy_from_slice(&width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&height.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = 2;
        chunk(&mut writer, b"IHDR", &ihdr)?;
        let raw_len = (u64::from(width) * 3 + 1) * u64::from(height);
        let blocks = raw_len.div_ceil(65_535);
        let idat_len = raw_len + blocks * 5 + 6;
        writer.write_all(&(idat_len as u32).to_be_bytes())?;
        writer.write_all(b"IDAT")?;
        let mut crc = crc_update(u32::MAX, b"IDAT");
        let header = [0x78, 0x01];
        writer.write_all(&header)?;
        crc = crc_update(crc, &header);
        let mut buffer = Vec::with_capacity(65_535);
        let mut done = 0u64;
        let mut a = 1u32;
        let mut b = 0u32;
        let flush = |writer: &mut BufWriter<File>,
                     bytes: &mut Vec<u8>,
                     done: &mut u64,
                     crc: &mut u32,
                     a: &mut u32,
                     b: &mut u32|
         -> std::io::Result<()> {
            *done += bytes.len() as u64;
            let len = bytes.len() as u16;
            let inv = !len;
            let block = [
                u8::from(*done == raw_len),
                len as u8,
                (len >> 8) as u8,
                inv as u8,
                (inv >> 8) as u8,
            ];
            writer.write_all(&block)?;
            *crc = crc_update(*crc, &block);
            writer.write_all(bytes)?;
            *crc = crc_update(*crc, bytes);
            for &v in bytes.iter() {
                *a = (*a + u32::from(v)) % 65_521;
                *b = (*b + *a) % 65_521;
            }
            bytes.clear();
            Ok(())
        };
        for y in 0..height {
            check().map_err(std::io::Error::other)?;
            buffer.push(0);
            if buffer.len() == 65_535 {
                flush(
                    &mut writer,
                    &mut buffer,
                    &mut done,
                    &mut crc,
                    &mut a,
                    &mut b,
                )?;
            }
            for x in 0..width {
                for v in pixel(x, y) {
                    buffer.push(v);
                    if buffer.len() == 65_535 {
                        flush(
                            &mut writer,
                            &mut buffer,
                            &mut done,
                            &mut crc,
                            &mut a,
                            &mut b,
                        )?;
                    }
                }
            }
        }
        if !buffer.is_empty() {
            flush(
                &mut writer,
                &mut buffer,
                &mut done,
                &mut crc,
                &mut a,
                &mut b,
            )?;
        }
        let adler = ((b << 16) | a).to_be_bytes();
        writer.write_all(&adler)?;
        crc = crc_update(crc, &adler);
        writer.write_all(&(crc ^ u32::MAX).to_be_bytes())?;
        chunk(&mut writer, b"IEND", &[])?;
        check().map_err(std::io::Error::other)?;
        writer.flush()
    };
    encode().map_err(|e| format!("write PNG {}: {e}", path.display()))
}
#[cfg(test)]
mod tests {
    #[test]
    fn known_crc() {
        assert_eq!(
            super::crc_update(u32::MAX, b"123456789") ^ u32::MAX,
            0xcbf4_3926
        );
    }
}
