//! Bounded visual episodes from an explicitly scoped sequence of image frames.
//!
//! The first frame is the fixed baseline. Each grid cell compares subsequent
//! pixels with that baseline; entering and leaving use different thresholds.
//! One continuous interval is one episode, regardless of its frame count. A
//! new episode requires an observed return below the exit threshold first.
//!
//! This module captures no desktop and performs no OCR. Events are available
//! immediately after `push_frame`. Rendering consumes an owned snapshot, so a
//! caller can render on another thread while continuing to feed the detector.

use std::{collections::VecDeque, fmt::Write};

pub const MAX_BASELINE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_GRID_CELLS: usize = 4096;
pub const MAX_COMPLETED_EPISODES: usize = 16_384;
pub const MAX_PENDING_EVENTS: usize = 32_768;
pub const MAX_SVG_EPISODES: usize = 512;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Gray8,
    Rgba8,
}
impl PixelFormat {
    fn channels(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgba8 => 4,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gray8 => "gray8",
            Self::Rgba8 => "rgba8",
        }
    }
}

/// Borrowed pixels. RGBA uses straight alpha; transparent RGB does not count
/// as a visible change. Padding is permitted through an explicit row stride.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    pub pixels: &'a [u8],
}
impl<'a> Frame<'a> {
    pub fn gray(width: u32, height: u32, pixels: &'a [u8]) -> Result<Self> {
        Self::new(width, height, width as usize, PixelFormat::Gray8, pixels)
    }
    pub fn rgba(width: u32, height: u32, pixels: &'a [u8]) -> Result<Self> {
        let stride = (width as usize)
            .checked_mul(4)
            .ok_or("frame row size overflow")?;
        Self::new(width, height, stride, PixelFormat::Rgba8, pixels)
    }
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        pixels: &'a [u8],
    ) -> Result<Self> {
        let frame = Self {
            width,
            height,
            stride,
            format,
            pixels,
        };
        frame.validate()?;
        Ok(frame)
    }
    fn validate(self) -> Result<()> {
        let row = (self.width as usize)
            .checked_mul(self.format.channels())
            .ok_or("frame row size overflow")?;
        if self.width == 0 || self.height == 0 || self.stride < row {
            return Err(
                "frame dimensions must be nonzero and stride must cover every pixel".into(),
            );
        }
        let bytes = self
            .stride
            .checked_mul(self.height as usize)
            .ok_or("frame byte size overflow")?;
        if self.pixels.len() != bytes {
            return Err("frame byte count does not match its dimensions and stride".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineOptions {
    pub region: Region,
    pub columns: u16,
    pub rows: u16,
    /// Mean per-pixel difference, in 0..255. Enter when score >= this value.
    pub enter_threshold: u8,
    /// End an active episode when score <= this value.
    pub exit_threshold: u8,
    pub completed_capacity: usize,
    pub event_capacity: usize,
}
impl TimelineOptions {
    pub fn new(region: Region, columns: u16, rows: u16) -> Self {
        Self {
            region,
            columns,
            rows,
            enter_threshold: 24,
            exit_threshold: 8,
            completed_capacity: 4096,
            event_capacity: 8192,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let pixels = u64::from(self.region.width) * u64::from(self.region.height);
        let cells = usize::from(self.columns) * usize::from(self.rows);
        if pixels == 0 || pixels > MAX_BASELINE_BYTES as u64 {
            return Err("timeline region must contain 1..33554432 pixels".into());
        }
        if self.region.x.checked_add(self.region.width).is_none()
            || self.region.y.checked_add(self.region.height).is_none()
        {
            return Err("timeline region coordinates overflow".into());
        }
        if cells == 0
            || cells > MAX_GRID_CELLS
            || u32::from(self.columns) > self.region.width
            || u32::from(self.rows) > self.region.height
        {
            return Err("timeline grid must contain 1..4096 nonempty cells".into());
        }
        if self.exit_threshold >= self.enter_threshold {
            return Err("timeline exit threshold must be lower than its enter threshold".into());
        }
        if !(1..=MAX_COMPLETED_EPISODES).contains(&self.completed_capacity)
            || !(cells..=MAX_PENDING_EVENTS).contains(&self.event_capacity)
        {
            return Err("timeline history must hold 1..16384 episodes and event capacity must hold at least one event per cell, at most 32768".into());
        }
        Ok(())
    }
}

/// Times are caller-supplied monotonic milliseconds. The beginning is the
/// first changed frame; the end is the first frame below the exit threshold.
/// `last_seen_ms` preserves the sampling boundary preceding disappearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Episode {
    pub id: u64,
    pub cell: usize,
    pub column: u16,
    pub row: u16,
    pub bounds: Region,
    pub appeared_ms: u64,
    pub last_seen_ms: u64,
    pub disappeared_ms: Option<u64>,
    pub duration_ms: u64,
    pub visible_frames: u64,
    pub peak_score: u8,
}
impl Episode {
    pub fn is_active(self) -> bool {
        self.disappeared_ms.is_none()
    }
    pub fn to_json(self) -> String {
        format!(
            "{{\"id\":{},\"cell\":{},\"column\":{},\"row\":{},\"bounds\":{},\"appeared_ms\":{},\"last_seen_ms\":{},\"disappeared_ms\":{},\"duration_ms\":{},\"visible_frames\":{},\"peak_score\":{},\"active\":{}}}",
            self.id,
            self.cell,
            self.column,
            self.row,
            region_json(self.bounds),
            self.appeared_ms,
            self.last_seen_ms,
            optional_time(self.disappeared_ms),
            self.duration_ms,
            self.visible_frames,
            self.peak_score,
            self.is_active()
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Appeared,
    Disappeared,
}
impl EventKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Appeared => "appeared",
            Self::Disappeared => "disappeared",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineEvent {
    pub kind: EventKind,
    pub at_ms: u64,
    pub episode: Episode,
}
impl TimelineEvent {
    pub fn to_json(self) -> String {
        format!(
            "{{\"event\":\"{}\",\"at_ms\":{},\"episode\":{}}}",
            self.kind.name(),
            self.at_ms,
            self.episode.to_json()
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventBatch {
    pub events: Vec<TimelineEvent>,
    /// Events discarded since the preceding drain because the consumer lagged.
    pub discarded_events: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameUpdate {
    pub at_ms: u64,
    pub initialized_baseline: bool,
    pub started: usize,
    pub finished: usize,
    pub active: usize,
}

impl EventBatch {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"discarded_events\":{},\"events\":[{}]}}",
            self.discarded_events,
            self.events
                .iter()
                .map(|event| event.to_json())
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

#[derive(Clone, Debug)]
struct Cell {
    bounds: Region,
    active: Option<Episode>,
}

pub struct VisualTimeline {
    options: TimelineOptions,
    baseline: Vec<u8>,
    frame_layout: Option<(u32, u32, PixelFormat)>,
    baseline_at: Option<u64>,
    last_at: Option<u64>,
    cells: Vec<Cell>,
    scores: Vec<u8>,
    completed: VecDeque<Episode>,
    events: VecDeque<TimelineEvent>,
    next_id: u64,
    discarded_completed: u64,
    discarded_events: u64,
    discarded_since_drain: u64,
}
impl VisualTimeline {
    pub fn new(options: TimelineOptions) -> Result<Self> {
        options.validate()?;
        let cells = grid_cells(&options)
            .into_iter()
            .map(|bounds| Cell {
                bounds,
                active: None,
            })
            .collect::<Vec<_>>();
        let scores = vec![0; cells.len()];
        Ok(Self {
            options,
            baseline: Vec::new(),
            frame_layout: None,
            baseline_at: None,
            last_at: None,
            cells,
            scores,
            completed: VecDeque::new(),
            events: VecDeque::new(),
            next_id: 1,
            discarded_completed: 0,
            discarded_events: 0,
            discarded_since_drain: 0,
        })
    }

    pub fn push_frame(&mut self, at_ms: u64, frame: Frame<'_>) -> Result<FrameUpdate> {
        frame.validate()?;
        if self.last_at.is_some_and(|previous| at_ms < previous) {
            return Err(
                "timeline timestamps must be monotonic; out-of-order frame rejected".into(),
            );
        }
        let region = self.options.region;
        if u64::from(region.x) + u64::from(region.width) > u64::from(frame.width)
            || u64::from(region.y) + u64::from(region.height) > u64::from(frame.height)
        {
            return Err("timeline region lies outside the frame".into());
        }
        let layout = (frame.width, frame.height, frame.format);
        if self.frame_layout.is_some_and(|previous| previous != layout) {
            return Err(
                "timeline frame dimensions or pixel format changed; create a new scoped timeline"
                    .into(),
            );
        }
        if self.frame_layout.is_none() {
            let row_bytes = region.width as usize * frame.format.channels();
            let bytes = row_bytes
                .checked_mul(region.height as usize)
                .ok_or("timeline baseline size overflow")?;
            if bytes > MAX_BASELINE_BYTES {
                return Err("timeline baseline exceeds 32 MiB".into());
            }
            self.baseline
                .try_reserve_exact(bytes)
                .map_err(|_| "cannot allocate the bounded timeline baseline")?;
            for y in region.y..region.y + region.height {
                let start = y as usize * frame.stride + region.x as usize * frame.format.channels();
                self.baseline
                    .extend_from_slice(&frame.pixels[start..start + row_bytes]);
            }
            self.frame_layout = Some(layout);
            self.baseline_at = Some(at_ms);
            self.last_at = Some(at_ms);
            return Ok(FrameUpdate {
                at_ms,
                initialized_baseline: true,
                started: 0,
                finished: 0,
                active: 0,
            });
        }

        let channels = frame.format.channels();
        for (cell, score) in self.cells.iter().zip(&mut self.scores) {
            let mut difference = 0u64;
            for y in cell.bounds.y..cell.bounds.y + cell.bounds.height {
                let source = y as usize * frame.stride;
                let baseline_row = (y - region.y) as usize * region.width as usize;
                for x in cell.bounds.x..cell.bounds.x + cell.bounds.width {
                    let now = source + x as usize * channels;
                    let before = (baseline_row + (x - region.x) as usize) * channels;
                    difference += u64::from(pixel_difference(
                        frame.format,
                        &self.baseline[before..before + channels],
                        &frame.pixels[now..now + channels],
                    ));
                }
            }
            let count = u64::from(cell.bounds.width) * u64::from(cell.bounds.height);
            *score = (difference / count) as u8;
        }
        let starts = self
            .cells
            .iter()
            .zip(&self.scores)
            .filter(|(cell, score)| {
                cell.active.is_none() && **score >= self.options.enter_threshold
            })
            .count();
        self.next_id
            .checked_add(starts as u64)
            .ok_or("timeline episode identifiers exhausted")?;
        let mut finished = 0;
        for index in 0..self.cells.len() {
            let score = self.scores[index];
            if let Some(mut episode) = self.cells[index].active.take() {
                episode.duration_ms = at_ms - episode.appeared_ms;
                if score <= self.options.exit_threshold {
                    episode.disappeared_ms = Some(at_ms);
                    if self.completed.len() == self.options.completed_capacity {
                        self.completed.pop_front();
                        self.discarded_completed = self.discarded_completed.saturating_add(1);
                    }
                    self.completed.push_back(episode);
                    self.emit(TimelineEvent {
                        kind: EventKind::Disappeared,
                        at_ms,
                        episode,
                    });
                    finished += 1;
                } else {
                    episode.last_seen_ms = at_ms;
                    episode.visible_frames = episode.visible_frames.saturating_add(1);
                    episode.peak_score = episode.peak_score.max(score);
                    self.cells[index].active = Some(episode);
                }
            } else if score >= self.options.enter_threshold {
                let episode = Episode {
                    id: self.next_id,
                    cell: index,
                    column: (index % usize::from(self.options.columns)) as u16,
                    row: (index / usize::from(self.options.columns)) as u16,
                    bounds: self.cells[index].bounds,
                    appeared_ms: at_ms,
                    last_seen_ms: at_ms,
                    disappeared_ms: None,
                    duration_ms: 0,
                    visible_frames: 1,
                    peak_score: score,
                };
                self.next_id += 1;
                self.cells[index].active = Some(episode);
                self.emit(TimelineEvent {
                    kind: EventKind::Appeared,
                    at_ms,
                    episode,
                });
            }
        }
        self.last_at = Some(at_ms);
        Ok(FrameUpdate {
            at_ms,
            initialized_baseline: false,
            started: starts,
            finished,
            active: self
                .cells
                .iter()
                .filter(|cell| cell.active.is_some())
                .count(),
        })
    }

    fn emit(&mut self, event: TimelineEvent) {
        if self.events.len() == self.options.event_capacity {
            self.events.pop_front();
            self.discarded_events = self.discarded_events.saturating_add(1);
            self.discarded_since_drain = self.discarded_since_drain.saturating_add(1);
        }
        self.events.push_back(event);
    }
    /// Drain immediately; this never waits for an episode to end or an image to render.
    pub fn take_events(&mut self) -> EventBatch {
        EventBatch {
            events: self.events.drain(..).collect(),
            discarded_events: std::mem::take(&mut self.discarded_since_drain),
        }
    }
    pub fn pending_events(&self) -> usize {
        self.events.len()
    }
    pub fn baseline_bytes(&self) -> usize {
        self.baseline.len()
    }

    /// Copy bounded metadata only; frame pixels are not retained by a snapshot.
    pub fn snapshot(&self) -> TimelineSnapshot {
        let mut episodes = self.completed.iter().copied().collect::<Vec<_>>();
        episodes.extend(self.cells.iter().filter_map(|cell| cell.active));
        episodes.sort_unstable_by_key(|episode| episode.id);
        TimelineSnapshot {
            region: self.options.region,
            columns: self.options.columns,
            rows: self.options.rows,
            baseline_ms: self.baseline_at,
            observed_ms: self.last_at,
            episodes,
            total_episodes: self.next_id - 1,
            discarded_completed_episodes: self.discarded_completed,
            discarded_events: self.discarded_events,
        }
    }
}

fn grid_cells(options: &TimelineOptions) -> Vec<Region> {
    let mut cells = Vec::with_capacity(usize::from(options.columns) * usize::from(options.rows));
    for row in 0..options.rows {
        let y = u64::from(options.region.height) * u64::from(row) / u64::from(options.rows);
        let end_y =
            u64::from(options.region.height) * (u64::from(row) + 1) / u64::from(options.rows);
        for column in 0..options.columns {
            let x =
                u64::from(options.region.width) * u64::from(column) / u64::from(options.columns);
            let end_x = u64::from(options.region.width) * (u64::from(column) + 1)
                / u64::from(options.columns);
            cells.push(Region {
                x: options.region.x + x as u32,
                y: options.region.y + y as u32,
                width: (end_x - x) as u32,
                height: (end_y - y) as u32,
            });
        }
    }
    cells
}
fn pixel_difference(format: PixelFormat, before: &[u8], now: &[u8]) -> u8 {
    if format == PixelFormat::Gray8 {
        return before[0].abs_diff(now[0]);
    }
    let mut difference = before[3].abs_diff(now[3]);
    for channel in 0..3 {
        let premultiplied = |pixel: &[u8]| -> u8 {
            ((u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255) as u8
        };
        difference = difference.max(premultiplied(before).abs_diff(premultiplied(now)));
    }
    difference
}
fn optional_time(time: Option<u64>) -> String {
    time.map(|value| value.to_string())
        .unwrap_or_else(|| "null".into())
}
fn region_json(region: Region) -> String {
    format!(
        "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
        region.x, region.y, region.width, region.height
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineSnapshot {
    pub region: Region,
    pub columns: u16,
    pub rows: u16,
    pub baseline_ms: Option<u64>,
    pub observed_ms: Option<u64>,
    pub episodes: Vec<Episode>,
    pub total_episodes: u64,
    pub discarded_completed_episodes: u64,
    pub discarded_events: u64,
}
impl TimelineSnapshot {
    pub fn to_json(&self) -> String {
        let episodes = self
            .episodes
            .iter()
            .map(|episode| episode.to_json())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"region\":{},\"columns\":{},\"rows\":{},\"baseline_ms\":{},\"observed_ms\":{},\"total_episodes\":{},\"discarded_completed_episodes\":{},\"discarded_events\":{},\"episodes\":[{episodes}]}}",
            region_json(self.region),
            self.columns,
            self.rows,
            optional_time(self.baseline_ms),
            optional_time(self.observed_ms),
            self.total_episodes,
            self.discarded_completed_episodes,
            self.discarded_events
        )
    }
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "Visual episodes: {} total; {} retained; baseline {} ms; observed {} ms\n",
            self.total_episodes,
            self.episodes.len(),
            optional_time(self.baseline_ms),
            optional_time(self.observed_ms)
        );
        for episode in &self.episodes {
            let _ = writeln!(
                text,
                "#{} cell ({}, {}): appeared {} ms; duration {} ms; {} frames; {}",
                episode.id,
                episode.column,
                episode.row,
                episode.appeared_ms,
                episode.duration_ms,
                episode.visible_frames,
                if episode.is_active() {
                    "active"
                } else {
                    "ended"
                }
            );
        }
        if self.discarded_completed_episodes != 0 || self.discarded_events != 0 {
            let _ = writeln!(
                text,
                "Retention: {} completed episodes and {} queued events discarded",
                self.discarded_completed_episodes, self.discarded_events
            );
        }
        text
    }

    /// Render a detached, bounded spatial timeline. No capture, disk write,
    /// PNG rasterization, or rendering work happens during `push_frame`.
    pub fn render_svg(&self, options: SvgOptions) -> Result<String> {
        options.validate()?;
        TimelineOptions::new(self.region, self.columns, self.rows).validate()?;
        if self.baseline_ms.is_none()
            || self.region.width == 0
            || self.region.height == 0
            || self.columns == 0
            || self.rows == 0
        {
            return Err("SVG timeline requires an initialized, nonempty region".into());
        }
        let retained = self.episodes.len().min(options.max_episodes);
        let selected = &self.episodes[self.episodes.len() - retained..];
        let scale = (f64::from(options.width - 32) / f64::from(self.region.width))
            .min(f64::from(options.max_grid_height) / f64::from(self.region.height));
        let grid_width = f64::from(self.region.width) * scale;
        let grid_height = f64::from(self.region.height) * scale;
        let height = (grid_height + 90.0 + retained as f64 * 18.0).ceil() as u32;
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{height}\" viewBox=\"0 0 {} {height}\"><rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/><g font-family=\"monospace\" font-size=\"12\" fill=\"#202124\"><text x=\"16\" y=\"22\">Visual episodes; region ({}, {}) {} x {}; observed {} ms</text>",
            options.width,
            options.width,
            self.region.x,
            self.region.y,
            self.region.width,
            self.region.height,
            optional_time(self.observed_ms)
        );
        svg.push_str("<!-- baseline -->");
        let beginning = self.baseline_ms.unwrap_or(0);
        let ending = self
            .observed_ms
            .unwrap_or(beginning)
            .max(beginning.saturating_add(1));
        let shade = |at: u64| -> String {
            let fraction = at.saturating_sub(beginning) as f64
                / ending.saturating_sub(beginning).max(1) as f64;
            let gray = (210.0 - 170.0 * fraction.clamp(0.0, 1.0)).round() as u8;
            format!("#{gray:02x}{gray:02x}{gray:02x}")
        };
        let mut tracks = std::collections::BTreeMap::<usize, usize>::new();
        for episode in selected {
            let x = 16.0 + f64::from(episode.bounds.x.saturating_sub(self.region.x)) * scale;
            let y = 40.0 + f64::from(episode.bounds.y.saturating_sub(self.region.y)) * scale;
            let color = shade(episode.appeared_ms);
            let _ = write!(
                svg,
                "<rect x=\"{x:.3}\" y=\"{y:.3}\" width=\"{:.3}\" height=\"{:.3}\" fill=\"{color}\" fill-opacity=\"0.14\" stroke=\"{color}\" stroke-width=\"1.5\"><title>Episode {}; appeared {} ms; duration {} ms; {} frames</title></rect>",
                f64::from(episode.bounds.width) * scale,
                f64::from(episode.bounds.height) * scale,
                episode.id,
                episode.appeared_ms,
                episode.duration_ms,
                episode.visible_frames
            );
            let track = tracks.entry(episode.cell).or_default();
            *track += 1;
            let label = if *track <= 20 {
                char::from_u32(0x2460 + *track as u32 - 1)
                    .unwrap()
                    .to_string()
            } else {
                format!("#{track}")
            };
            let _ = write!(
                svg,
                "<text x=\"{:.3}\" y=\"{:.3}\" fill=\"{color}\">{label} +{}ms</text>",
                x + 5.0,
                y + 15.0 * *track as f64,
                episode.appeared_ms.saturating_sub(beginning)
            );
        }
        for column in 0..=self.columns {
            let logical =
                u64::from(self.region.width) * u64::from(column) / u64::from(self.columns);
            let x = 16.0 + logical as f64 * scale;
            let _ = write!(
                svg,
                "<path d=\"M{x:.3} 40V{:.3}\" stroke=\"#52525b\" stroke-width=\"0.5\"/>",
                40.0 + grid_height
            );
        }
        for row in 0..=self.rows {
            let logical = u64::from(self.region.height) * u64::from(row) / u64::from(self.rows);
            let y = 40.0 + logical as f64 * scale;
            let _ = write!(
                svg,
                "<path d=\"M16 {y:.3}H{:.3}\" stroke=\"#52525b\" stroke-width=\"0.5\"/>",
                16.0 + grid_width
            );
        }
        let _ = write!(
            svg,
            "<text x=\"16\" y=\"{:.3}\">Later appearances are darker. Showing {} of {} retained episodes; {} older completed episodes omitted</text>",
            grid_height + 61.0,
            retained,
            self.episodes.len(),
            self.discarded_completed_episodes
        );
        for (index, episode) in selected.iter().enumerate() {
            let color = shade(episode.appeared_ms);
            let _ = write!(
                svg,
                "<text x=\"16\" y=\"{:.3}\" fill=\"{color}\">#{} cell ({}, {}): appeared {} ms; duration {} ms; {}</text>",
                grid_height + 83.0 + index as f64 * 18.0,
                episode.id,
                episode.column,
                episode.row,
                episode.appeared_ms,
                episode.duration_ms,
                if episode.is_active() {
                    "active"
                } else {
                    "ended"
                }
            );
        }
        svg.push_str("</g></svg>");
        Ok(svg)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvgOptions {
    pub width: u32,
    pub max_grid_height: u32,
    pub max_episodes: usize,
}
impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            width: 1024,
            max_grid_height: 640,
            max_episodes: 128,
        }
    }
}
impl SvgOptions {
    pub fn validate(self) -> Result<()> {
        if !(256..=4096).contains(&self.width)
            || !(64..=4096).contains(&self.max_grid_height)
            || !(1..=MAX_SVG_EPISODES).contains(&self.max_episodes)
        {
            return Err(
                "SVG width must be 256..4096, grid height 64..4096, and episode limit 1..512"
                    .into(),
            );
        }
        Ok(())
    }
}
