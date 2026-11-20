#![cfg(feature = "std")]

use zuse::cursor::{
    CursorColor, CursorImage, CursorShape, CursorStyle, MAX_CURSOR_DIMENSION, MAX_PAM_BYTES,
    MAX_PAM_HEADER_BYTES,
};

fn pixel(image: &CursorImage, dx: i32, dy: i32) -> u32 {
    let x = (image.hotspot_x as i32 + dx) as usize;
    let y = (image.hotspot_y as i32 + dy) as usize;
    image.pixels[y * image.width as usize + x]
}

#[test]
fn rgba_conversion_preserves_opacity_and_premultiplies_color() {
    let image = CursorImage::from_rgba(
        4,
        1,
        2,
        0,
        &[
            255, 127, 63, 255, 255, 127, 63, 128, 255, 255, 255, 0, 2, 3, 4, 1,
        ],
    )
    .unwrap();
    assert_eq!(image.pixels, [0xffff7f3f, 0x80804020, 0, 0x01000000]);
    image.validate().unwrap();
    assert_eq!((image.hotspot_x, image.hotspot_y), (2, 0));
}

#[test]
fn images_reject_invalid_geometry_lengths_and_nonpremultiplied_pixels() {
    for (width, height, x, y) in [
        (0, 1, 0, 0),
        (1, 0, 0, 0),
        (257, 1, 0, 0),
        (1, 257, 0, 0),
        (1, 1, 1, 0),
        (1, 1, 0, 1),
        (u32::MAX, u32::MAX, 0, 0),
    ] {
        assert!(CursorImage::from_rgba(width, height, x, y, &[]).is_err());
    }
    assert!(CursorImage::from_rgba(1, 1, 0, 0, &[0; 3]).is_err());
    assert!(CursorImage::from_rgba(1, 1, 0, 0, &[0; 5]).is_err());
    let mut image = CursorImage::from_rgba(1, 1, 0, 0, &[255; 4]).unwrap();
    for invalid in [0x00ff0000, 0x80008100, 0x800000ff] {
        image.pixels[0] = invalid;
        assert!(image.validate().is_err());
    }
    image.pixels.clear();
    assert!(image.validate().is_err());
}

#[test]
fn color_and_shape_parsers_are_strict_and_handle_unicode_without_panicking() {
    assert_eq!(
        CursorColor::parse("#12AbEF80").unwrap(),
        CursorColor {
            r: 0x12,
            g: 0xab,
            b: 0xef,
            a: 0x80
        }
    );
    assert_eq!(CursorColor::parse("#123456").unwrap().a, 255);
    for invalid in [
        "red",
        "123456",
        "#123",
        "#1234567",
        "#123456789",
        "#gg0000",
        "#💙00",
        "#ééé",
    ] {
        assert!(CursorColor::parse(invalid).is_err(), "{invalid}");
    }
    for (name, shape) in [
        ("arrow", CursorShape::Arrow),
        ("ring", CursorShape::Ring),
        ("crosshair", CursorShape::Crosshair),
        ("dot", CursorShape::Dot),
    ] {
        assert_eq!(name.parse::<CursorShape>().unwrap(), shape);
    }
    assert!(CursorShape::parse("unknown").is_err());
}

#[test]
fn rendered_shapes_have_exact_visible_hotspots_and_clear_outer_borders() {
    for shape in [
        CursorShape::Arrow,
        CursorShape::Ring,
        CursorShape::Crosshair,
        CursorShape::Dot,
    ] {
        for size in [16, 32, 96] {
            let image = CursorStyle {
                shape,
                size,
                glow: 1.0,
                ..CursorStyle::default()
            }
            .render()
            .unwrap();
            image.validate().unwrap();
            assert!(image.width <= MAX_CURSOR_DIMENSION && image.height <= MAX_CURSOR_DIMENSION);
            assert!(pixel(&image, 0, 0) >> 24 > 128);
            for x in 0..image.width as usize {
                assert_eq!(image.pixels[x], 0, "top {shape:?} {size}");
                assert_eq!(
                    image.pixels[(image.height as usize - 1) * image.width as usize + x],
                    0,
                    "bottom {shape:?} {size}"
                );
            }
            for y in 0..image.height as usize {
                assert_eq!(
                    image.pixels[y * image.width as usize],
                    0,
                    "left {shape:?} {size}"
                );
                assert_eq!(
                    image.pixels[(y + 1) * image.width as usize - 1],
                    0,
                    "right {shape:?} {size}"
                );
            }
            assert!(image.pixels.iter().any(|p| (1..255).contains(&(p >> 24))));
        }
    }
}

#[test]
fn shape_choices_keep_their_distinct_geometry() {
    let render = |shape| {
        CursorStyle {
            shape,
            size: 48,
            glow: 0.0,
            ..CursorStyle::default()
        }
        .render()
        .unwrap()
    };
    let ring = render(CursorShape::Ring);
    assert_eq!(pixel(&ring, 7, 0), 0);
    assert!(pixel(&ring, 15, 0) >> 24 > 128);
    let crosshair = render(CursorShape::Crosshair);
    assert!(pixel(&crosshair, 12, 0) >> 24 > 128);
    assert!(pixel(&crosshair, 0, -12) >> 24 > 128);
    assert_eq!(pixel(&crosshair, 9, 9), 0);
    let dot = render(CursorShape::Dot);
    assert!(pixel(&dot, 3, 0) >> 24 > 128);
    assert_eq!(pixel(&dot, 12, 0), 0);
    let arrow = render(CursorShape::Arrow);
    assert!(pixel(&arrow, 6, 16) >> 24 > 128);
    assert_eq!(pixel(&arrow, -3, 16), 0);
}

#[test]
fn glow_expands_soft_visibility_without_changing_hotspot_or_opacity_limit() {
    let base = CursorStyle {
        shape: CursorShape::Dot,
        glow: 0.0,
        ..CursorStyle::default()
    };
    let plain = base.render().unwrap();
    let glowing = CursorStyle { glow: 1.0, ..base }.render().unwrap();
    assert_eq!(
        (plain.width, plain.height, plain.hotspot_x, plain.hotspot_y),
        (
            glowing.width,
            glowing.height,
            glowing.hotspot_x,
            glowing.hotspot_y
        )
    );
    assert!(
        plain
            .pixels
            .iter()
            .zip(&glowing.pixels)
            .all(|(a, b)| a >> 24 <= b >> 24)
    );
    assert!(
        plain
            .pixels
            .iter()
            .zip(&glowing.pixels)
            .any(|(a, b)| *a == 0 && b >> 24 > 0)
    );
    let translucent = CursorStyle {
        color: CursorColor::parse("#ff000080").unwrap(),
        glow: 1.0,
        ..base
    }
    .render()
    .unwrap();
    assert!(translucent.pixels.iter().all(|p| p >> 24 <= 128));
    let body = pixel(&translucent, 3, 0);
    assert!(((body >> 16) & 255) > (body & 255));
    let invisible = CursorStyle {
        color: CursorColor::parse("#ffffff00").unwrap(),
        ..base
    }
    .render()
    .unwrap();
    assert!(invisible.pixels.iter().all(|p| *p == 0));
}

#[test]
fn invalid_style_options_fail_before_rendering() {
    for size in [0, 15, 97, u32::MAX] {
        assert!(
            CursorStyle {
                size,
                ..CursorStyle::default()
            }
            .render()
            .is_err()
        );
    }
    for glow in [-0.1, 1.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            CursorStyle {
                glow,
                ..CursorStyle::default()
            }
            .render()
            .is_err()
        );
    }
}

#[test]
fn pam_import_preserves_custom_rgb_rgba_and_hotspot() {
    let mut rgba = b"P7\n# custom cursor\nHEIGHT 1\nWIDTH 2\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
    rgba.extend_from_slice(&[255, 0, 128, 128, 2, 4, 8, 255]);
    let image = CursorImage::from_pam(&rgba, 1, 0).unwrap();
    assert_eq!(image.pixels, [0x80800040, 0xff020408]);
    assert_eq!((image.hotspot_x, image.hotspot_y), (1, 0));
    let mut rgb =
        b"P7\r\nWIDTH 1\r\nHEIGHT 1\r\nDEPTH 3\r\nMAXVAL 255\r\nTUPLTYPE RGB\r\nENDHDR\r\n"
            .to_vec();
    rgb.extend_from_slice(&[12, 34, 56]);
    assert_eq!(
        CursorImage::from_pam(&rgb, 0, 0).unwrap().pixels,
        [0xff0c2238]
    );
}

#[test]
fn pam_rejects_ambiguous_headers_unsupported_formats_and_wrong_payloads() {
    let valid = "P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
    for header in [
        valid.replace("P7", "P6"),
        valid.replace("WIDTH 1", "WIDTH 1\nWIDTH 1"),
        valid.replace("WIDTH 1", "WIDTH 0"),
        valid.replace("WIDTH 1", "WIDTH 257"),
        valid.replace("WIDTH 1", "WIDTH +1"),
        valid.replace("WIDTH 1", "WIDTH 999999999999999999999"),
        valid.replace("WIDTH 1", "WIDTH 1 junk"),
        valid.replace("DEPTH 4", "DEPTH 3"),
        valid.replace("MAXVAL 255", "MAXVAL 65535"),
        valid.replace(
            "TUPLTYPE RGB_ALPHA",
            "TUPLTYPE RGB_ALPHA\nTUPLTYPE RGB_ALPHA",
        ),
        valid.replace("TUPLTYPE RGB_ALPHA\n", ""),
        valid.replace("WIDTH 1\n", ""),
        valid.replace("ENDHDR", "UNKNOWN 2\nENDHDR"),
        valid.replace("ENDHDR\n", "ENDHDR"),
    ] {
        let mut bytes = header.into_bytes();
        bytes.extend_from_slice(&[255; 4]);
        assert!(CursorImage::from_pam(&bytes, 0, 0).is_err());
    }
    for len in [0, 3, 5] {
        let mut bytes = valid.as_bytes().to_vec();
        bytes.extend_from_slice(&vec![255; len]);
        assert!(CursorImage::from_pam(&bytes, 0, 0).is_err());
    }
    let too_long = [
        b"P7\n#".as_slice(),
        &vec![b'x'; MAX_PAM_HEADER_BYTES],
        b"\nENDHDR\n",
    ]
    .concat();
    assert!(CursorImage::from_pam(&too_long, 0, 0).is_err());
    assert!(CursorImage::from_pam(&vec![0; MAX_PAM_BYTES + 1], 0, 0).is_err());
}
