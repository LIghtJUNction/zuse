use super::*;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJZkAAAAASUVORK5CYII=";
const VIEWPORT: &str = r#"{"url":"https://fixture.invalid/","timeOrigin":42,"width":980,"height":1133,"devicePixelRatio":1.25,"scrollX":0,"scrollY":100,"visualViewport":{"width":980,"height":1133,"scale":1,"offsetX":0,"offsetY":0,"pageX":0,"pageY":100}}"#;

fn request(peer: &mut TcpStream) -> String {
    let mut header = [0; 2];
    peer.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        126 => {
            let mut bytes = [0; 2];
            peer.read_exact(&mut bytes).unwrap();
            usize::from(u16::from_be_bytes(bytes))
        }
        127 => panic!("unexpected oversized fixture request"),
        length => usize::from(length),
    };
    let mut mask = [0; 4];
    peer.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes).unwrap();
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    String::from_utf8(bytes).unwrap()
}

fn reply(peer: &mut TcpStream, request: &str, body: &str) {
    let id = with_json(request, |root| {
        Ok(root.get("id").unwrap().integer().unwrap())
    })
    .unwrap();
    let text = format!(r#"{{"id":{id},"result":{body}}}"#);
    let mut frame = vec![0x81];
    if text.len() < 126 {
        frame.push(text.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(text.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(text.as_bytes());
    peer.write_all(&frame).unwrap();
}

fn capture(before: String, after: String) -> (Browser, std::thread::JoinHandle<()>) {
    capture_image(
        before,
        Some(after),
        "png",
        None,
        decode_base64(PNG).unwrap(),
    )
}

fn capture_image(
    before: String,
    after: Option<String>,
    format: &'static str,
    quality: Option<i64>,
    bytes: Vec<u8>,
) -> (Browser, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut steps = vec![
            ("Runtime.evaluate", Some(before)),
            ("Page.captureScreenshot", None),
        ];
        if let Some(after) = after {
            steps.push(("Runtime.evaluate", Some(after)));
        }
        for (method, state) in steps {
            let message = request(&mut peer);
            assert_eq!(field_string(&message, &["method"]).unwrap(), method);
            if let Some(state) = state {
                let expression = field_string(&message, &["params", "expression"]).unwrap();
                assert!(
                    !expression.contains("focus"),
                    "capture must not depend on focus"
                );
                assert!(!expression.contains("visibilityState"));
                with_json(&message, |root| {
                    assert_eq!(
                        root.get("params")
                            .unwrap()
                            .get("awaitPromise")
                            .unwrap()
                            .boolean(),
                        Some(false)
                    );
                    assert!(root.get("params").unwrap().get("userGesture").is_none());
                    Ok(())
                })
                .unwrap();
                reply(
                    &mut peer,
                    &message,
                    &format!(r#"{{"result":{{"value":{state}}}}}"#),
                );
            } else {
                assert_eq!(
                    field_string(&message, &["params", "format"]).unwrap(),
                    format
                );
                with_json(&message, |root| {
                    let params = root.get("params").unwrap();
                    assert_eq!(
                        params.get("quality").map(|value| value.integer().unwrap()),
                        quality
                    );
                    assert_eq!(params.get("fromSurface").unwrap().boolean(), Some(true));
                    assert_eq!(
                        params.get("captureBeyondViewport").unwrap().boolean(),
                        Some(false)
                    );
                    Ok(())
                })
                .unwrap();
                reply(
                    &mut peer,
                    &message,
                    &format!(r#"{{"data":{}}}"#, quote(&encode_base64(&bytes))),
                );
            }
        }
    });
    (
        Browser::unattached_socket(WebSocket::new(stream), false),
        worker,
    )
}

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "zuse-browser-capture-{}-{}",
        std::process::id(),
        PROFILE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn fractional_dpr_mapping_uses_actual_png_dimensions_for_each_axis() {
    let metadata = screenshot_metadata(
        "capture.png",
        100,
        1225,
        1417,
        "selected-page",
        VIEWPORT,
        capture::Format::Png,
    )
    .unwrap();
    with_json(&metadata, |root| {
        assert_eq!(root.get("width").unwrap().integer().unwrap(), 1225);
        assert_eq!(root.get("height").unwrap().integer().unwrap(), 1417);
        let viewport = root.get("cssViewport").unwrap();
        assert_eq!(number(viewport.get("width"))?, 980.0);
        assert_eq!(number(viewport.get("height"))?, 1133.0);
        let ratio = root.get("pixelsPerCssUnit").unwrap();
        let x = number(ratio.get("x"))?;
        let y = number(ratio.get("y"))?;
        assert_eq!(x, 1.25);
        assert_ne!(x, y, "rounding is independent on the two axes");
        assert!((1417.0 / y - 1133.0).abs() < 1e-9);
        assert!((1225.0 / x - 980.0).abs() < 1e-9);
        Ok(())
    })
    .unwrap();
}

#[test]
fn capture_remains_read_only_without_focus_and_reports_real_png_size() {
    let directory = directory();
    let path = directory.join("capture.png");
    let state = VIEWPORT.replace(
        "\"timeOrigin\":42",
        "\"timeOrigin\":42,\"focused\":false,\"visible\":false",
    );
    let (mut browser, worker) = capture(state.clone(), state);
    let result = browser.screenshot(path.to_str().unwrap()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), decode_base64(PNG).unwrap());
    with_json(&result, |root| {
        assert_eq!(root.get("width").unwrap().integer().unwrap(), 1);
        assert_eq!(root.get("height").unwrap().integer().unwrap(), 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    browser.close().unwrap();
    worker.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn changed_capture_mapping_preserves_the_previous_image() {
    for after in [
        VIEWPORT.replace("fixture.invalid", "another.invalid"),
        VIEWPORT.replace("\"timeOrigin\":42", "\"timeOrigin\":43"),
        VIEWPORT.replace("\"width\":980", "\"width\":981"),
        VIEWPORT.replace("\"height\":1133", "\"height\":1134"),
        VIEWPORT.replace("\"devicePixelRatio\":1.25", "\"devicePixelRatio\":1.5"),
        VIEWPORT.replace("\"scrollX\":0", "\"scrollX\":1"),
        VIEWPORT.replace("\"scrollY\":100", "\"scrollY\":101"),
        VIEWPORT.replace("\"scale\":1", "\"scale\":1.5"),
        VIEWPORT.replace("\"offsetX\":0", "\"offsetX\":1"),
    ] {
        let directory = directory();
        let path = directory.join("capture.png");
        fs::write(&path, b"previous image").unwrap();
        let (mut browser, worker) = capture(VIEWPORT.into(), after);
        let error = browser.screenshot(path.to_str().unwrap()).unwrap_err();
        assert!(error.starts_with("stale_target:"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"previous image");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        browser.close().unwrap();
        worker.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn pinch_zoom_keeps_capture_but_does_not_advertise_an_unverified_mapping() {
    let state = VIEWPORT.replace("\"scale\":1", "\"scale\":2");
    let metadata = screenshot_metadata(
        "capture.png",
        100,
        1225,
        1417,
        "selected-page",
        &state,
        capture::Format::Png,
    )
    .unwrap();
    with_json(&metadata, |root| {
        assert_eq!(root.get("pixelsPerCssUnit").unwrap().raw(), "null");
        assert!(root.get("mappingError").unwrap().string().is_some());
        Ok(())
    })
    .unwrap();
}

#[test]
fn mismatched_surface_scales_require_a_fresh_explicit_viewport() {
    let state = VIEWPORT
        .replace("\"width\":980", "\"width\":1000")
        .replace("\"height\":1133", "\"height\":900")
        .replace("\"devicePixelRatio\":1.25", "\"devicePixelRatio\":1");
    let metadata = screenshot_metadata(
        "capture.png",
        100,
        1225,
        1417,
        "selected-page",
        &state,
        capture::Format::Png,
    )
    .unwrap();
    with_json(&metadata, |root| {
        assert_eq!(root.get("pixelsPerCssUnit").unwrap().raw(), "null");
        let error = crate::json::text(root.get("mappingError").unwrap())?;
        assert!(error.contains("inconsistent axis scales"), "{error}");
        assert!(error.contains("browser.viewport"), "{error}");
        Ok(())
    })
    .unwrap();
    let metadata = screenshot_metadata(
        "capture.png",
        100,
        1000,
        900,
        "selected-page",
        &state,
        capture::Format::Png,
    )
    .unwrap();
    with_json(&metadata, |root| {
        let mapping = root.get("pixelsPerCssUnit").unwrap();
        assert_eq!(number(mapping.get("x"))?, 1.0);
        assert_eq!(number(mapping.get("y"))?, 1.0);
        assert!(root.get("mappingError").is_none());
        Ok(())
    })
    .unwrap();
}

// A source-only grayscale JPEG: constant DC=0/AC=EOB blocks, one-bit
// Huffman codes, and all-one padding at each scan's byte boundary.
fn jpeg_fixture(width: u16, height: u16, progressive: bool) -> Vec<u8> {
    fn segment(bytes: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
    }
    let mut bytes = vec![0xff, 0xd8];
    let mut quantization = vec![1; 65];
    quantization[0] = 0;
    segment(&mut bytes, 0xdb, &quantization);
    let [h0, h1] = height.to_be_bytes();
    let [w0, w1] = width.to_be_bytes();
    segment(
        &mut bytes,
        if progressive { 0xc2 } else { 0xc0 },
        &[8, h0, h1, w0, w1, 1, 1, 0x11, 0],
    );
    let mut huffman = Vec::new();
    for table in [0, 0x10] {
        huffman.extend_from_slice(&[table, 1]);
        huffman.extend_from_slice(&[0; 15]);
        huffman.push(0);
    }
    segment(&mut bytes, 0xc4, &huffman);
    let blocks = usize::from(width).div_ceil(8) * usize::from(height).div_ceil(8);
    let scans: &[(u8, u8, usize)] = if progressive {
        &[(0, 0, 1), (1, 63, 1)]
    } else {
        &[(0, 63, 2)]
    };
    for &(start, end, bits_per_block) in scans {
        segment(&mut bytes, 0xda, &[1, 1, 0, start, end, 0]);
        let bits = blocks * bits_per_block;
        bytes.resize(bytes.len() + bits / 8, 0);
        if bits % 8 != 0 {
            bytes.push((1u16 << (8 - bits % 8)) as u8 - 1);
        }
    }
    bytes.extend_from_slice(&[0xff, 0xd9]);
    bytes
}

#[test]
fn jpeg_dimensions_skip_embedded_thumbnails_and_validate_complete_scans() {
    let format = capture::Format::Jpeg(80);
    for progressive in [false, true] {
        let image = jpeg_fixture(17, 9, progressive);
        assert_eq!(format.dimensions(&image).unwrap(), (17, 9));
        for length in 0..image.len() {
            assert!(
                format.dimensions(&image[..length]).is_err(),
                "accepted prefix {length}"
            );
        }
        let thumbnail = jpeg_fixture(1, 1, false);
        let mut embedded = vec![0xff, 0xd8, 0xff, 0xe1];
        embedded.extend_from_slice(&((thumbnail.len() + 2) as u16).to_be_bytes());
        embedded.extend_from_slice(&thumbnail);
        embedded.extend_from_slice(&image[2..]);
        assert_eq!(format.dimensions(&embedded).unwrap(), (17, 9));
    }
    for image in [
        jpeg_fixture(0, 9, false),
        jpeg_fixture(17, 0, false),
        vec![0xff, 0xd8, 0xff, 0xd9],
        vec![0xff, 0xd8, 0xff, 0xe0, 0, 1, 0xff, 0xd9],
        vec![0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff, 0xff, 0xd9],
        [jpeg_fixture(1, 1, false), vec![0]].concat(),
        decode_base64(PNG).unwrap(),
    ] {
        assert!(format.dimensions(&image).is_err());
    }
    let image = jpeg_fixture(1, 1, false);
    let mut empty_scan = image.clone();
    empty_scan.remove(empty_scan.len() - 3);
    assert!(
        format
            .dimensions(&empty_scan)
            .unwrap_err()
            .contains("no image data")
    );
    let mut stuffed = image[..image.len() - 2].to_vec();
    stuffed.extend_from_slice(&[0xff, 0x00, 0xff, 0xd0, 0xff, 0xd9]);
    assert_eq!(format.dimensions(&stuffed).unwrap(), (1, 1));
}

#[test]
fn jpeg_capture_uses_native_quality_and_encoded_dimensions() {
    for quality in [None, Some(0), Some(100)] {
        let directory = directory();
        // The explicit format, not the filename extension, controls encoding.
        let path = directory.join("capture.png");
        let bytes = jpeg_fixture(1225, 1417, false);
        let (mut browser, worker) = capture_image(
            VIEWPORT.into(),
            Some(VIEWPORT.into()),
            "jpeg",
            Some(quality.unwrap_or(80)),
            bytes.clone(),
        );
        let result = browser
            .screenshot_with_format(path.to_str().unwrap(), "jpeg", quality)
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        with_json(&result, |root| {
            assert_eq!(field_string(&result, &["mimeType"])?, "image/jpeg");
            assert_eq!(
                root.get("bytes").unwrap().integer().unwrap(),
                bytes.len() as i64
            );
            assert_eq!(root.get("width").unwrap().integer().unwrap(), 1225);
            assert_eq!(root.get("height").unwrap().integer().unwrap(), 1417);
            let ratio = root.get("pixelsPerCssUnit").unwrap();
            assert_eq!(number(ratio.get("x"))?, 1.25);
            assert!((number(ratio.get("y"))? - 1417.0 / 1133.0).abs() < 1e-12);
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        browser.close().unwrap();
        worker.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn invalid_jpeg_or_changed_geometry_preserves_the_destination() {
    let image = jpeg_fixture(1, 1, false);
    for (bytes, after) in [
        (image[..image.len() - 2].to_vec(), None),
        (decode_base64(PNG).unwrap(), None),
        (
            image,
            Some(VIEWPORT.replace("\"width\":980", "\"width\":981")),
        ),
    ] {
        let directory = directory();
        let path = directory.join("capture.jpg");
        fs::write(&path, b"previous image").unwrap();
        let geometry_changed = after.is_some();
        let (mut browser, worker) = capture_image(VIEWPORT.into(), after, "jpeg", Some(80), bytes);
        let error = browser
            .screenshot_with_format(path.to_str().unwrap(), "jpeg", None)
            .unwrap_err();
        if geometry_changed {
            assert!(error.starts_with("stale_target:"), "{error}");
        }
        assert_eq!(fs::read(&path).unwrap(), b"previous image");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        browser.close().unwrap();
        worker.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
