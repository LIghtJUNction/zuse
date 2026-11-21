#![cfg(feature = "vision")]

use zuse::vision::{
    Bounds, GrayImage, MAX_IMAGE_SIDE, MAX_TEMPLATE_SIDE, MatchMode, MatchOptions, MatchStatus,
    match_template,
};

// Rasterize the same geometry independently at each scale. Subpixel coverage
// differs from bilinear template resizing, like a real DPI-rendered icon.
fn icon(size: u32) -> GrayImage {
    let mut pixels = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let mut sum = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = (x as f64 + (sx as f64 + 0.5) / 4.0) * 20.0 / size as f64;
                    let py = (y as f64 + (sy as f64 + 0.5) / 4.0) * 20.0 / size as f64;
                    let inside = |x0, x1, y0, y1| px >= x0 && px < x1 && py >= y0 && py < y1;
                    let stroke = inside(4.0, 6.0, 3.0, 17.0)
                        || inside(4.0, 16.0, 3.0, 5.0)
                        || inside(4.0, 16.0, 15.0, 17.0)
                        || inside(14.0, 16.0, 8.0, 17.0)
                        || inside(9.0, 16.0, 8.0, 10.0);
                    sum += if stroke {
                        30
                    } else if inside(9.0, 11.0, 11.0, 13.0) {
                        100
                    } else {
                        230
                    };
                }
            }
            pixels.push(((sum + 8) / 16) as u8);
        }
    }
    GrayImage::new(size, size, pixels).unwrap()
}

fn paste(pixels: &mut [u8], stride: usize, image: &GrayImage, x: usize, y: usize) {
    for row in 0..image.height() as usize {
        let source = row * image.width() as usize;
        let start = (y + row) * stride + x;
        pixels[start..start + image.width() as usize]
            .copy_from_slice(&image.pixels()[source..source + image.width() as usize]);
    }
}

fn one_scale(mode: MatchMode) -> MatchOptions {
    MatchOptions {
        scales: vec![1.0],
        mode,
        ..MatchOptions::default()
    }
}

#[test]
fn unique_icon_returns_exact_image_bounds_after_a_complete_search() {
    let template = icon(20);
    let mut pixels = vec![230; 120 * 85];
    paste(&mut pixels, 120, &template, 47, 31);
    let image = GrayImage::new(120, 85, pixels).unwrap();
    let result = match_template(&image, &template, &one_scale(MatchMode::Edges)).unwrap();
    assert_eq!(result.status, MatchStatus::Unique);
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(
        result.candidates[0].bounds,
        Bounds {
            x: 47,
            y: 31,
            width: 20,
            height: 20,
        }
    );
    assert!(result.candidates[0].score > 0.999_999);
    assert_eq!(result.evaluated_positions, 101 * 66);
}

#[test]
fn independently_rendered_125_percent_icon_matches_both_themes() {
    let template = icon(20);
    for inverted in [false, true] {
        let rendered = icon(25);
        let rendered = if inverted {
            GrayImage::new(
                25,
                25,
                rendered.pixels().iter().map(|&pixel| 255 - pixel).collect(),
            )
            .unwrap()
        } else {
            rendered
        };
        let mut pixels = vec![if inverted { 25 } else { 230 }; 120 * 85];
        paste(&mut pixels, 120, &rendered, 47, 31);
        let image = GrayImage::new(120, 85, pixels).unwrap();
        let result = match_template(&image, &template, &MatchOptions::default()).unwrap();
        assert_eq!(result.status, MatchStatus::Unique, "inverted={inverted}");
        let best = &result.candidates[0];
        assert_eq!(best.scale, 1.25);
        assert_eq!(
            best.bounds,
            Bounds {
                x: 47,
                y: 31,
                width: 25,
                height: 25,
            }
        );
        assert!(best.score > 0.98);
    }
}

#[test]
fn repeated_icons_return_ambiguous_candidates_instead_of_picking_first() {
    let template = icon(20);
    let mut pixels = vec![230; 140 * 70];
    paste(&mut pixels, 140, &template, 10, 20);
    paste(&mut pixels, 140, &template, 80, 20);
    let image = GrayImage::new(140, 70, pixels).unwrap();
    let mut options = one_scale(MatchMode::Edges);
    options.min_margin = 0.0; // An exact tie stays ambiguous even with zero margin.
    let result = match_template(&image, &template, &options).unwrap();
    assert_eq!(result.status, MatchStatus::Ambiguous);
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(result.candidates[0].bounds.x, 10);
    assert_eq!(result.candidates[1].bounds.x, 80);
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| candidate.score > 0.999_999)
    );
}

#[test]
fn runner_up_below_threshold_still_makes_a_close_match_ambiguous() {
    let template = icon(20);
    let mut altered = template.pixels().to_vec();
    altered[10 * 20 + 10] = 0;
    let altered = GrayImage::new(20, 20, altered).unwrap();
    let mut pixels = vec![230; 140 * 70];
    paste(&mut pixels, 140, &template, 10, 20);
    paste(&mut pixels, 140, &altered, 80, 20);
    let image = GrayImage::new(140, 70, pixels).unwrap();
    let options = MatchOptions {
        min_score: 0.9999,
        min_margin: 0.02,
        ..one_scale(MatchMode::Gray)
    };
    let result = match_template(&image, &template, &options).unwrap();
    assert_eq!(result.status, MatchStatus::Ambiguous);
    assert_eq!(result.candidates.len(), 2);
    assert!(result.candidates[1].score < options.min_score);
    assert!(result.candidates[0].score - result.candidates[1].score < options.min_margin);
}

#[test]
fn nearby_scale_responses_are_merged_but_separate_icons_are_preserved() {
    let template = icon(20);
    let options = MatchOptions {
        scales: vec![0.95, 1.0, 1.05],
        mode: MatchMode::Gray,
        min_margin: 0.25,
        ..MatchOptions::default()
    };
    let mut pixels = vec![230; 140 * 70];
    paste(&mut pixels, 140, &template, 10, 20);
    let image = GrayImage::new(140, 70, pixels.clone()).unwrap();
    let result = match_template(&image, &template, &options).unwrap();
    assert_eq!(result.status, MatchStatus::Unique);
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].scale, 1.0);

    paste(&mut pixels, 140, &template, 80, 20);
    let image = GrayImage::new(140, 70, pixels).unwrap();
    let result = match_template(&image, &template, &options).unwrap();
    assert_eq!(result.status, MatchStatus::Ambiguous);
    assert_eq!(result.candidates.len(), 2);
}

#[test]
fn untextured_templates_and_invalid_limits_fail_without_coordinates() {
    let image = GrayImage::new(70, 50, vec![127; 70 * 50]).unwrap();
    let constant = GrayImage::new(20, 20, vec![127; 400]).unwrap();
    assert!(
        match_template(&image, &constant, &MatchOptions::default())
            .unwrap_err()
            .contains("texture")
    );
    let template = icon(20);
    let result = match_template(&image, &template, &one_scale(MatchMode::Gray)).unwrap();
    assert_eq!(result.status, MatchStatus::NotFound);
    assert!(result.candidates.is_empty());
    assert!(GrayImage::new(10, 10, vec![0; 99]).is_err());
    assert!(GrayImage::new(0, 10, Vec::new()).is_err());
    assert!(GrayImage::new(MAX_IMAGE_SIDE + 1, 1, Vec::new()).is_err());
    let too_wide = GrayImage::new(
        MAX_TEMPLATE_SIDE + 1,
        5,
        vec![0; (MAX_TEMPLATE_SIDE as usize + 1) * 5],
    )
    .unwrap();
    assert!(
        match_template(&image, &too_wide, &one_scale(MatchMode::Gray))
            .unwrap_err()
            .contains("template dimensions")
    );
    for options in [
        MatchOptions {
            scales: vec![f64::NAN],
            ..MatchOptions::default()
        },
        MatchOptions {
            min_score: f64::INFINITY,
            ..MatchOptions::default()
        },
        MatchOptions {
            min_margin: -0.01,
            ..MatchOptions::default()
        },
        MatchOptions {
            max_candidates: 1,
            ..MatchOptions::default()
        },
    ] {
        assert!(match_template(&image, &template, &options).is_err());
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn chunk(png: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(name);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

// A tiny standards-conforming PNG encoder with uncompressed DEFLATE blocks.
// Fixtures stay readable source code; no image or compressed binary is stored.
fn png(width: u32, height: u32, color: u8, channels: usize, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), width as usize * height as usize * channels);
    let stride = width as usize * channels;
    let mut scanlines = Vec::new();
    for row in pixels.chunks_exact(stride) {
        scanlines.push(0); // PNG filter: None.
        scanlines.extend_from_slice(row);
    }
    let mut compressed = vec![0x78, 0x01]; // zlib, no compression.
    let blocks = scanlines.chunks(65_535);
    let count = blocks.len();
    for (index, block) in blocks.enumerate() {
        compressed.push(u8::from(index + 1 == count));
        let length = block.len() as u16;
        compressed.extend_from_slice(&length.to_le_bytes());
        compressed.extend_from_slice(&(!length).to_le_bytes());
        compressed.extend_from_slice(block);
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &scanlines {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    compressed.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, color, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &compressed);
    chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
#[cfg(target_os = "linux")]
fn generated_png_decodes_gray_rgba_and_rejects_incomplete_streams() {
    let bytes = png(2, 2, 0, 1, &[0, 64, 128, 255]);
    let image = GrayImage::from_png(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (2, 2));
    assert_eq!(image.pixels(), &[0, 64, 128, 255]);
    let bytes = png(
        2,
        2,
        6,
        4,
        &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0],
    );
    let image = GrayImage::from_png(&bytes).unwrap();
    assert_eq!(image.pixels(), &[77, 149, 29, 255]);
    assert!(GrayImage::from_png(&bytes[..bytes.len() / 2]).is_err());
}

#[test]
fn png_header_limits_apply_before_native_decoding() {
    assert!(GrayImage::from_png(b"not a PNG").is_err());
    let mut bytes = png(1, 1, 0, 1, &[127]);
    bytes[16..20].copy_from_slice(&(MAX_IMAGE_SIDE + 1).to_be_bytes());
    assert!(
        GrayImage::from_png(&bytes)
            .unwrap_err()
            .contains("image dimensions")
    );
}

#[test]
#[cfg(not(target_os = "linux"))]
fn png_decoding_is_explicitly_unsupported_on_other_platforms() {
    let bytes = png(1, 1, 0, 1, &[127]);
    assert!(
        GrayImage::from_png(&bytes)
            .unwrap_err()
            .contains("unsupported")
    );
}
