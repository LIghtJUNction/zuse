//! Explicit image operations. The ordinary observation path never enters here.
use crate::{
    json::quote,
    observation::Cancellation,
    platform::Desktop,
    target::TargetWindow,
    vision::{self, GrayImage, MatchOptions, MatchStatus},
    visual_timeline::{EventKind, Frame, Region, SvgOptions, TimelineOptions, VisualTimeline},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);

struct CapturePath(PathBuf);
impl CapturePath {
    fn new() -> Result<Self> {
        for _ in 0..128 {
            let path = std::env::temp_dir().join(format!(
                "zuse-vision-{}-{}",
                std::process::id(),
                NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot create private image staging directory: {error}"
                    ));
                }
            }
        }
        Err("cannot reserve a private image staging directory".into())
    }
    fn image(&self) -> PathBuf {
        self.0.join("window.png")
    }
}
impl Drop for CapturePath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.image());
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn current_window(desktop: &Desktop, expected: &TargetWindow) -> Result<()> {
    let current = TargetWindow::from_windows(&desktop.windows()?, &expected.id)?;
    if &current != expected || !current.focused {
        return Err("stale_target: the selected image window moved, resized, changed scale, identity or focus".into());
    }
    Ok(())
}

fn capture(desktop: &Desktop, window: &TargetWindow) -> Result<(GrayImage, Vec<u8>)> {
    current_window(desktop, window)?;
    let path = CapturePath::new()?;
    desktop.screenshot(&path.image(), Some(&window.id))?;
    let bytes = std::fs::read(path.image()).map_err(|error| error.to_string())?;
    let image = GrayImage::from_png(&bytes)?;
    current_window(desktop, window)?;
    Ok((image, bytes))
}

fn patch(image: &GrayImage, bounds: vision::Bounds) -> Result<Vec<u8>> {
    if bounds.width == 0
        || bounds.height == 0
        || bounds
            .x
            .checked_add(bounds.width)
            .is_none_or(|x| x > image.width())
        || bounds
            .y
            .checked_add(bounds.height)
            .is_none_or(|y| y > image.height())
    {
        return Err("image target is outside the captured window".into());
    }
    let mut pixels = Vec::with_capacity(bounds.width as usize * bounds.height as usize);
    for y in bounds.y..bounds.y + bounds.height {
        let offset = y as usize * image.width() as usize + bounds.x as usize;
        pixels.extend_from_slice(&image.pixels()[offset..offset + bounds.width as usize]);
    }
    Ok(pixels)
}

#[derive(Clone)]
pub(crate) struct ImageTarget {
    pub window: TargetWindow,
    pub x: f64,
    pub y: f64,
    image_width: u32,
    image_height: u32,
    bounds: vision::Bounds,
    pixels: Vec<u8>,
}
impl ImageTarget {
    /// A bounded pixel comparison at the retained location, never a second match.
    pub fn verify(&self, desktop: &Desktop) -> Result<()> {
        let (image, _) = capture(desktop, &self.window)?;
        if image.width() != self.image_width || image.height() != self.image_height {
            return Err("stale_target: captured image dimensions changed".into());
        }
        let current = patch(&image, self.bounds)?;
        let difference: u64 = current
            .iter()
            .zip(&self.pixels)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        if difference > 8 * current.len() as u64 {
            return Err(
                "stale_target: pixels at the retained image target changed; locate again".into(),
            );
        }
        if !desktop.window_hit_test(
            &self.window.id,
            self.x.round() as i32,
            self.y.round() as i32,
        )? {
            return Err("stale_target: another input surface covers the image target".into());
        }
        Ok(())
    }
}

struct Timeline {
    window: TargetWindow,
    started: Instant,
    detector: VisualTimeline,
    baseline_png: Vec<u8>,
    dimensions: (u32, u32),
    captures: u64,
}

#[derive(Default)]
pub(crate) struct VisionRuntime {
    pub targets: BTreeMap<String, ImageTarget>,
    timelines: BTreeMap<String, Timeline>,
}
impl VisionRuntime {
    pub fn locate(
        &mut self,
        desktop: &Desktop,
        template: &Path,
        window: &str,
        name: &str,
        options: &MatchOptions,
    ) -> Result<String> {
        self.targets.remove(name);
        if self.targets.len() >= 64 {
            return Err("at most 64 image targets may be retained".into());
        }
        let template = GrayImage::load_png(template)?;
        let window = TargetWindow::from_windows(&desktop.windows()?, window)?;
        let (image, _) = capture(desktop, &window)?;
        let matched = vision::match_template(&image, &template, options)?;
        let candidates = matched.candidates.iter().map(|candidate| {
            let b = candidate.bounds;
            let x = window.bounds.x + (f64::from(b.x) + f64::from(b.width) / 2.0) * window.bounds.width / f64::from(image.width());
            let y = window.bounds.y + (f64::from(b.y) + f64::from(b.height) / 2.0) * window.bounds.height / f64::from(image.height());
            format!("{{\"x\":{x},\"y\":{y},\"score\":{},\"template_scale\":{},\"image_bounds\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}}}", candidate.score, candidate.scale, b.x, b.y, b.width, b.height)
        }).collect::<Vec<_>>().join(",");
        if matched.status == MatchStatus::Unique {
            let bounds = matched.candidates[0].bounds;
            self.targets.insert(
                name.into(),
                ImageTarget {
                    x: window.bounds.x
                        + (f64::from(bounds.x) + f64::from(bounds.width) / 2.0)
                            * window.bounds.width
                            / f64::from(image.width()),
                    y: window.bounds.y
                        + (f64::from(bounds.y) + f64::from(bounds.height) / 2.0)
                            * window.bounds.height
                            / f64::from(image.height()),
                    window: window.clone(),
                    image_width: image.width(),
                    image_height: image.height(),
                    pixels: patch(&image, bounds)?,
                    bounds,
                },
            );
        }
        let status = match matched.status {
            MatchStatus::Unique => "unique",
            MatchStatus::Ambiguous => "ambiguous",
            MatchStatus::NotFound => "not_found",
        };
        Ok(format!(
            "{{\"status\":{},\"name\":{},\"window\":{},\"basis\":\"template_ncc\",\"coordinate_space\":\"native_desktop\",\"recognition_passes\":1,\"images_returned\":0,\"evaluated_positions\":{},\"candidates\":[{candidates}]}}",
            quote(status),
            quote(name),
            window.to_json(),
            matched.evaluated_positions
        ))
    }

    pub fn start(
        &mut self,
        desktop: &Desktop,
        name: &str,
        window: &str,
        grid: (u16, u16),
        thresholds: (u8, u8),
    ) -> Result<String> {
        let (columns, rows) = grid;
        let (enter, exit) = thresholds;
        self.timelines.remove(name);
        if self.timelines.len() >= 4 {
            return Err("at most four visual timelines may be retained".into());
        }
        let window = TargetWindow::from_windows(&desktop.windows()?, window)?;
        let (image, baseline_png) = capture(desktop, &window)?;
        let mut options = TimelineOptions::new(
            Region {
                x: 0,
                y: 0,
                width: image.width(),
                height: image.height(),
            },
            columns,
            rows,
        );
        options.enter_threshold = enter;
        options.exit_threshold = exit;
        let mut detector = VisualTimeline::new(options)?;
        detector.push_frame(
            0,
            Frame::gray(image.width(), image.height(), image.pixels())?,
        )?;
        self.timelines.insert(
            name.into(),
            Timeline {
                window,
                started: Instant::now(),
                detector,
                baseline_png,
                dimensions: (image.width(), image.height()),
                captures: 1,
            },
        );
        self.state(name)
    }
    pub fn state(&self, name: &str) -> Result<String> {
        let timeline = self
            .timelines
            .get(name)
            .ok_or("unknown visual timeline; use timeline.start first")?;
        Ok(format!(
            "{{\"name\":{},\"window\":{},\"captured_frames\":{},\"images_returned\":0,\"timeline\":{}}}",
            quote(name),
            timeline.window.to_json(),
            timeline.captures,
            timeline.detector.snapshot().to_json()
        ))
    }
    fn sample_events(
        &mut self,
        desktop: &Desktop,
        name: &str,
    ) -> Result<crate::visual_timeline::EventBatch> {
        let timeline = self
            .timelines
            .get_mut(name)
            .ok_or("unknown visual timeline; use timeline.start first")?;
        let (image, _) = capture(desktop, &timeline.window)?;
        if (image.width(), image.height()) != timeline.dimensions {
            return Err("stale_target: visual timeline frame dimensions changed".into());
        }
        timeline.detector.push_frame(
            timeline.started.elapsed().as_millis() as u64,
            Frame::gray(image.width(), image.height(), image.pixels())?,
        )?;
        timeline.captures += 1;
        Ok(timeline.detector.take_events())
    }
    pub fn sample(&mut self, desktop: &Desktop, name: &str) -> Result<String> {
        let events = self.sample_events(desktop, name)?;
        Ok(format!(
            "{{\"state\":{},\"events\":{}}}",
            self.state(name)?,
            events.to_json()
        ))
    }
    pub fn wait_until(
        &mut self,
        desktop: &Desktop,
        name: &str,
        timeout: u64,
        interval: u64,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let deadline = Instant::now() + Duration::from_millis(timeout);
        loop {
            cancellation.check()?;
            let events = self.sample_events(desktop, name)?;
            cancellation.check()?;
            if events
                .events
                .iter()
                .any(|event| event.kind == EventKind::Appeared)
            {
                return Ok(format!(
                    "{{\"status\":\"satisfied\",\"state\":{},\"events\":{}}}",
                    self.state(name)?,
                    events.to_json()
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timeout: no new visual episode before the deadline".into());
            }
            cancellation.sleep(remaining.min(Duration::from_millis(interval)))?;
        }
    }
    pub fn close(&mut self, name: &str) -> Result<String> {
        let state = self.state(name)?;
        self.timelines.remove(name);
        Ok(state)
    }
    pub fn render(&self, name: &str, path: &Path) -> Result<String> {
        let timeline = self
            .timelines
            .get(name)
            .ok_or("unknown visual timeline; use timeline.start first")?;
        if path.extension().and_then(|s| s.to_str()) != Some("svg") {
            return Err("timeline.render requires an explicit .svg destination".into());
        }
        let snapshot = timeline.detector.snapshot();
        let options = SvgOptions::default();
        let mut svg = snapshot.render_svg(options)?;
        let scale = (f64::from(options.width - 32) / f64::from(snapshot.region.width))
            .min(f64::from(options.max_grid_height) / f64::from(snapshot.region.height));
        let background = format!(
            "<image x=\"16\" y=\"40\" width=\"{}\" height=\"{}\" opacity=\"0.3\" href=\"data:image/png;base64,{}\"/>",
            f64::from(snapshot.region.width) * scale,
            f64::from(snapshot.region.height) * scale,
            crate::browser::encode_base64(&timeline.baseline_png)
        );
        svg = svg.replacen("<!-- baseline -->", &background, 1);
        std::fs::write(path, svg).map_err(|error| format!("cannot write timeline SVG: {error}"))?;
        Ok(format!(
            "{{\"path\":{},\"mimeType\":\"image/svg+xml\",\"episodes\":{},\"background_frames\":1}}",
            quote(&path.to_string_lossy()),
            snapshot.episodes.len()
        ))
    }
}
