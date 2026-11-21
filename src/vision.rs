//! Optional, explicit template matching within an already captured image.
//!
//! This module does not capture the desktop, resolve windows, or send input.
//! Callers keep semantic targeting as the default and supply a window crop only
//! when a template was requested. Coordinates in the result are image pixels;
//! the caller must map them through the current window geometry and scale.
//!
//! Matching uses zero-mean normalized cross-correlation. Integral statistics
//! and conservative projection/Cauchy bounds prune work without selecting an
//! approximate subset of locations. Exceeding a limit returns an error, never a
//! unique match from an unfinished search. Edge mode tolerates an inverted
//! light/dark palette; it does not promise invariance to arbitrary icon changes.

use std::borrow::Cow;
use std::io::Read;
use std::path::Path;

/// Largest decoded/search image dimension, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 8_192;
/// Fits a 3840 × 2160 window while bounding integral-image allocations.
pub const MAX_IMAGE_PIXELS: usize = 8_388_608;
/// Applies to the template at every requested scale.
pub const MAX_TEMPLATE_SIDE: u32 = 256;
pub const MAX_TEMPLATE_PIXELS: usize = 65_536;
pub const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;

const MAX_SCALES: usize = 16;
const MAX_CANDIDATES: usize = 32;
const MAX_RAW_CANDIDATES: usize = 8_192;
// Counts visited pixels, positions, projection blocks and correlation samples.
// A caller can reduce the window crop or number of scales after this is reached.
const MAX_WORK: u64 = 160_000_000;
const MIN_TEMPLATE_SIDE: u32 = 5;
const MIN_TEMPLATE_VARIANCE: f64 = 4.0;
const NUMERIC_SLACK: f64 = 1e-8;

/// A validated, tightly packed, row-major 8-bit grayscale image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let count = image_size(width, height)?;
        if pixels.len() != count {
            return Err(format!(
                "grayscale image has {} pixels, expected {count} for {width}x{height}",
                pixels.len()
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Decode an explicit PNG file. No screenshot or other I/O is inferred.
    ///
    /// PNG decoding currently requires Linux and system libpng 1.6 with its
    /// simplified API. The pure matcher works on all targets with `std`.
    /// Transparent pixels are composited on white before grayscale conversion.
    pub fn load_png(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|error| format!("cannot open template PNG {}: {error}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect template PNG {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err("template PNG must be a regular file".into());
        }
        if metadata.len() > MAX_PNG_BYTES as u64 {
            return Err(format!("PNG exceeds the {MAX_PNG_BYTES}-byte input limit"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_PNG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read template PNG {}: {error}", path.display()))?;
        Self::from_png(&bytes)
    }

    /// Decode PNG bytes under the same limits and alpha policy as `load_png`.
    pub fn from_png(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_PNG_BYTES {
            return Err(format!("PNG exceeds the {MAX_PNG_BYTES}-byte input limit"));
        }
        // Inspect IHDR before calling native code or allocating a pixel buffer.
        // libpng still verifies the complete stream, including CRCs.
        if bytes.len() < 33
            || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
            || &bytes[8..16] != b"\0\0\0\rIHDR"
        {
            return Err("invalid PNG: missing signature or IHDR".into());
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        image_size(width, height)?;
        png::decode(bytes)
    }
}

/// Image-space coordinates, before any window/desktop scale conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// Signed grayscale correlation: ignores brightness and positive contrast gain.
    Gray,
    /// Gradient magnitude correlation: also tolerates light/dark inversion.
    Edges,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchOptions {
    /// Template-to-search-image pixel scales; 1.25 is included by default.
    pub scales: Vec<f64>,
    pub mode: MatchMode,
    /// Minimum normalized correlation for a unique or ambiguous match.
    pub min_score: f64,
    /// A distinct runner-up this close to the best makes the result ambiguous.
    /// A runner-up slightly below `min_score` still participates in this check.
    pub min_margin: f64,
    /// Number of distinct candidates returned, from 2 through 32.
    pub max_candidates: usize,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            scales: vec![0.75, 1.0, 1.25, 1.5, 2.0],
            mode: MatchMode::Edges,
            min_score: 0.90,
            min_margin: 0.05,
            max_candidates: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchStatus {
    Unique,
    Ambiguous,
    NotFound,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub bounds: Bounds,
    pub score: f64,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchResult {
    pub status: MatchStatus,
    /// Highest score first; nearby responses of the same icon are merged.
    /// Spatially separate repeated icons remain separate candidates.
    pub candidates: Vec<Candidate>,
    /// All valid positions visited, including those rejected by exact bounds.
    pub evaluated_positions: usize,
}

/// Search all valid positions at the requested scales, or fail explicitly.
///
/// `Unique` is evidence for a caller to validate against fresh window state;
/// it is not permission to click. `Ambiguous` never chooses between candidates.
/// Constant/low-contrast templates, invalid options, and exhausted computation
/// or candidate limits are errors. A complete search below threshold is
/// `NotFound`; its candidates, if any, are suggestions below that threshold.
pub fn match_template(
    image: &GrayImage,
    template: &GrayImage,
    options: &MatchOptions,
) -> Result<MatchResult, String> {
    match_with_budget(image, template, options, MAX_WORK)
}

fn match_with_budget(
    image: &GrayImage,
    template: &GrayImage,
    options: &MatchOptions,
    work_limit: u64,
) -> Result<MatchResult, String> {
    validate_options(options)?;
    template_size(template.width, template.height)?;
    check_texture(&template.pixels)?;

    let mut raster_scales: Vec<(f64, u32, u32)> = Vec::new();
    for &scale in &options.scales {
        let width = (template.width as f64 * scale).round() as u32;
        let height = (template.height as f64 * scale).round() as u32;
        template_size(width, height)?;
        // Identical raster sizes produce identical resized templates, even if
        // the caller supplied several scales that round to the same size.
        if !raster_scales
            .iter()
            .any(|&(_, earlier_width, earlier_height)| {
                (earlier_width, earlier_height) == (width, height)
            })
        {
            raster_scales.push((scale, width, height));
        }
    }
    // A native-size hit tightens later bounds early, while every requested
    // raster size is still searched before a result is returned.
    raster_scales.sort_by(|left, right| {
        (left.0 - 1.0)
            .abs()
            .total_cmp(&(right.0 - 1.0).abs())
            .then_with(|| left.0.total_cmp(&right.0))
    });

    let mut budget = Budget(work_limit);
    budget.spend(image.pixels.len() as u64)?;
    let feature = match options.mode {
        MatchMode::Gray => Cow::Borrowed(image),
        MatchMode::Edges => Cow::Owned(edges(image)),
    };
    budget.spend(image.pixels.len() as u64)?;
    let integral = Integral::new(&feature);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut evaluated_positions = 0usize;
    let mut cutoff = options.min_score - options.min_margin;

    for (scale, width, height) in raster_scales {
        budget.spend(width as u64 * height as u64)?;
        let resized = resize(template, width, height);
        let model = Model::new(&resized, options.mode, image.width)?;
        if width > image.width || height > image.height {
            continue;
        }

        for y in 0..=image.height - height {
            for x in 0..=image.width - width {
                budget.spend(1)?;
                evaluated_positions += 1;
                let origin_x = x + model.border;
                let origin_y = y + model.border;
                let sum = integral.sum(origin_x, origin_y, model.width, model.height);
                let squares = integral.squares(origin_x, origin_y, model.width, model.height);
                let mean = sum / model.count;
                let energy = (squares - sum * mean).max(0.0);
                if energy <= f64::EPSILON {
                    continue;
                }
                let denominator = (energy * model.energy).sqrt();
                let tolerance = denominator * NUMERIC_SLACK + 1e-6;
                budget.spend(model.blocks.len() as u64)?;
                let upper = model.projection_upper(&integral, origin_x, origin_y, mean, energy);
                if upper + tolerance < cutoff * denominator {
                    continue;
                }
                let origin = origin_y as usize * image.width as usize + origin_x as usize;
                let score = model.correlate(
                    &feature.pixels,
                    origin,
                    mean,
                    energy,
                    denominator,
                    cutoff,
                    &mut budget,
                )?;
                let Some(score) = score else {
                    continue;
                };
                let next_cutoff = (score - options.min_margin).max(cutoff);
                if next_cutoff > cutoff {
                    cutoff = next_cutoff;
                    candidates.retain(|candidate| candidate.score + NUMERIC_SLACK >= cutoff);
                }
                if candidates.len() >= MAX_RAW_CANDIDATES {
                    return Err(format!(
                        "template matching exceeded {MAX_RAW_CANDIDATES} candidate responses; \
                         narrow the window crop or use a more distinctive template"
                    ));
                }
                candidates.push(Candidate {
                    bounds: Bounds {
                        x,
                        y,
                        width,
                        height,
                    },
                    score,
                    scale,
                });
            }
        }
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.bounds.y.cmp(&right.bounds.y))
            .then_with(|| left.bounds.x.cmp(&right.bounds.x))
            .then_with(|| left.bounds.width.cmp(&right.bounds.width))
            .then_with(|| left.bounds.height.cmp(&right.bounds.height))
            .then_with(|| left.scale.total_cmp(&right.scale))
    });
    let mut distinct: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if !distinct
            .iter()
            .any(|earlier| same_detection(earlier.bounds, candidate.bounds))
        {
            distinct.push(candidate);
            if distinct.len() == options.max_candidates {
                break;
            }
        }
    }
    let status = if distinct
        .first()
        .is_none_or(|best| best.score + NUMERIC_SLACK < options.min_score)
    {
        MatchStatus::NotFound
    } else if distinct.get(1).is_some_and(|second| {
        distinct[0].score - second.score <= options.min_margin + NUMERIC_SLACK
    }) {
        MatchStatus::Ambiguous
    } else {
        MatchStatus::Unique
    };
    Ok(MatchResult {
        status,
        candidates: distinct,
        evaluated_positions,
    })
}

fn image_size(width: u32, height: u32) -> Result<usize, String> {
    let count = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
        || count > MAX_IMAGE_PIXELS as u64
    {
        return Err(format!(
            "image dimensions {width}x{height} exceed limits: nonzero sides up to \
             {MAX_IMAGE_SIDE}, at most {MAX_IMAGE_PIXELS} pixels; crop the selected window"
        ));
    }
    Ok(count as usize)
}

fn template_size(width: u32, height: u32) -> Result<(), String> {
    if !(MIN_TEMPLATE_SIDE..=MAX_TEMPLATE_SIDE).contains(&width)
        || !(MIN_TEMPLATE_SIDE..=MAX_TEMPLATE_SIDE).contains(&height)
        || u64::from(width) * u64::from(height) > MAX_TEMPLATE_PIXELS as u64
    {
        return Err(format!(
            "template dimensions {width}x{height} are outside limits at a requested scale: \
             sides {MIN_TEMPLATE_SIDE}..={MAX_TEMPLATE_SIDE}, \
             at most {MAX_TEMPLATE_PIXELS} pixels"
        ));
    }
    Ok(())
}

fn validate_options(options: &MatchOptions) -> Result<(), String> {
    if options.scales.is_empty() || options.scales.len() > MAX_SCALES {
        return Err(format!(
            "template matching requires 1..={MAX_SCALES} scales"
        ));
    }
    if options
        .scales
        .iter()
        .any(|scale| !scale.is_finite() || !(0.25..=4.0).contains(scale))
    {
        return Err("template scales must be finite and in 0.25..=4.0".into());
    }
    if !options.min_score.is_finite() || !(0.0..=1.0).contains(&options.min_score) {
        return Err("template min_score must be finite and in 0..=1".into());
    }
    if !options.min_margin.is_finite()
        || options.min_margin < 0.0
        || options.min_margin > options.min_score
    {
        return Err("template min_margin must be finite and in 0..=min_score".into());
    }
    if !(2..=MAX_CANDIDATES).contains(&options.max_candidates) {
        return Err(format!(
            "template max_candidates must be in 2..={MAX_CANDIDATES} so ambiguity stays visible"
        ));
    }
    Ok(())
}

fn check_texture(pixels: &[u8]) -> Result<(f64, f64), String> {
    let count = pixels.len() as f64;
    let mean = pixels.iter().map(|&pixel| pixel as f64).sum::<f64>() / count;
    let energy = pixels
        .iter()
        .map(|&pixel| (pixel as f64 - mean).powi(2))
        .sum::<f64>();
    if energy / count < MIN_TEMPLATE_VARIANCE {
        return Err(
            "template has no usable texture or contrast; use a distinctive icon crop".into(),
        );
    }
    Ok((mean, energy))
}

fn resize(image: &GrayImage, width: u32, height: u32) -> GrayImage {
    if image.width == width && image.height == height {
        return image.clone();
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        let source_y = ((y as f64 + 0.5) * image.height as f64 / height as f64 - 0.5)
            .clamp(0.0, image.height as f64 - 1.0);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(image.height as usize - 1);
        let fy = source_y - y0 as f64;
        for x in 0..width {
            let source_x = ((x as f64 + 0.5) * image.width as f64 / width as f64 - 0.5)
                .clamp(0.0, image.width as f64 - 1.0);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(image.width as usize - 1);
            let fx = source_x - x0 as f64;
            let row = image.width as usize;
            let top = image.pixels[y0 * row + x0] as f64 * (1.0 - fx)
                + image.pixels[y0 * row + x1] as f64 * fx;
            let bottom = image.pixels[y1 * row + x0] as f64 * (1.0 - fx)
                + image.pixels[y1 * row + x1] as f64 * fx;
            pixels.push((top * (1.0 - fy) + bottom * fy).round() as u8);
        }
    }
    GrayImage {
        width,
        height,
        pixels,
    }
}

fn edges(image: &GrayImage) -> GrayImage {
    let width = image.width as usize;
    let height = image.height as usize;
    let mut pixels = vec![0; image.pixels.len()];
    for y in 0..height {
        let top = y.saturating_sub(1) * width;
        let bottom = (y + 1).min(height - 1) * width;
        for x in 0..width {
            let left = image.pixels[y * width + x.saturating_sub(1)];
            let right = image.pixels[y * width + (x + 1).min(width - 1)];
            let dx = left.abs_diff(right) as u16;
            let dy = image.pixels[top + x].abs_diff(image.pixels[bottom + x]) as u16;
            pixels[y * width + x] = ((dx + dy) / 2) as u8;
        }
    }
    GrayImage {
        width: image.width,
        height: image.height,
        pixels,
    }
}

struct Integral {
    stride: usize,
    sums: Vec<u32>,
    squares: Vec<u64>,
}

impl Integral {
    fn new(image: &GrayImage) -> Self {
        let stride = image.width as usize + 1;
        let length = stride * (image.height as usize + 1);
        let mut sums = vec![0u32; length];
        let mut squares = vec![0u64; length];
        for y in 0..image.height as usize {
            let mut row_sum = 0u32;
            let mut row_squares = 0u64;
            for x in 0..image.width as usize {
                let pixel = image.pixels[y * image.width as usize + x] as u32;
                row_sum += pixel;
                row_squares += u64::from(pixel * pixel);
                let above = y * stride + x + 1;
                let here = above + stride;
                // MAX_IMAGE_PIXELS * 255 is below u32::MAX.
                sums[here] = sums[above] + row_sum;
                squares[here] = squares[above] + row_squares;
            }
        }
        Self {
            stride,
            sums,
            squares,
        }
    }

    fn indices(&self, x: u32, y: u32, width: u32, height: u32) -> [usize; 4] {
        let top = y as usize * self.stride;
        let bottom = (y + height) as usize * self.stride;
        [
            top + x as usize,
            top + (x + width) as usize,
            bottom + x as usize,
            bottom + (x + width) as usize,
        ]
    }

    fn sum(&self, x: u32, y: u32, width: u32, height: u32) -> f64 {
        let [a, b, c, d] = self.indices(x, y, width, height);
        (u64::from(self.sums[d]) + u64::from(self.sums[a])
            - u64::from(self.sums[b])
            - u64::from(self.sums[c])) as f64
    }

    fn squares(&self, x: u32, y: u32, width: u32, height: u32) -> f64 {
        let [a, b, c, d] = self.indices(x, y, width, height);
        (self.squares[d] + self.squares[a] - self.squares[b] - self.squares[c]) as f64
    }
}

struct Sample {
    source_offset: usize,
    centered: f64,
}

struct Block {
    bounds: Bounds,
    count: f64,
    centered_mean: f64,
}

struct Model {
    border: u32,
    width: u32,
    height: u32,
    count: f64,
    energy: f64,
    blocks: Vec<Block>,
    residual_energy: f64,
    samples: Vec<Sample>,
    remaining_energy: Vec<f64>,
}

impl Model {
    fn new(image: &GrayImage, mode: MatchMode, source_width: u32) -> Result<Self, String> {
        let feature = match mode {
            MatchMode::Gray => Cow::Borrowed(image),
            MatchMode::Edges => Cow::Owned(edges(image)),
        };
        // Edge derivatives on the outermost template pixel would read outside
        // the crop in the source. Excluding this border makes the two agree.
        let border = if mode == MatchMode::Edges { 1 } else { 0 };
        let width = image.width - border * 2;
        let height = image.height - border * 2;
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for y in border..image.height - border {
            let start = y as usize * image.width as usize + border as usize;
            pixels.extend_from_slice(&feature.pixels[start..start + width as usize]);
        }
        let (mean, energy) = check_texture(&pixels)?;
        let count = pixels.len() as f64;
        let mut samples: Vec<Sample> = pixels
            .iter()
            .enumerate()
            .map(|(index, &pixel)| Sample {
                source_offset: index / width as usize * source_width as usize
                    + index % width as usize,
                centered: pixel as f64 - mean,
            })
            .collect();
        samples.sort_by(|left, right| right.centered.abs().total_cmp(&left.centered.abs()));
        let mut remaining_energy = vec![0.0; samples.len() + 1];
        for index in (0..samples.len()).rev() {
            remaining_energy[index] = remaining_energy[index + 1] + samples[index].centered.powi(2);
        }

        let columns = width.min(4);
        let rows = height.min(4);
        let mut blocks = Vec::with_capacity((columns * rows) as usize);
        let mut projected_energy = 0.0;
        for row in 0..rows {
            let y = row * height / rows;
            let next_y = (row + 1) * height / rows;
            for column in 0..columns {
                let x = column * width / columns;
                let next_x = (column + 1) * width / columns;
                let block_count = ((next_x - x) * (next_y - y)) as f64;
                let mut sum = 0u32;
                for dy in y..next_y {
                    for dx in x..next_x {
                        sum += pixels[dy as usize * width as usize + dx as usize] as u32;
                    }
                }
                let centered_mean = sum as f64 / block_count - mean;
                projected_energy += block_count * centered_mean.powi(2);
                blocks.push(Block {
                    bounds: Bounds {
                        x,
                        y,
                        width: next_x - x,
                        height: next_y - y,
                    },
                    count: block_count,
                    centered_mean,
                });
            }
        }
        Ok(Self {
            border,
            width,
            height,
            count,
            energy,
            blocks,
            residual_energy: (energy - projected_energy).max(0.0),
            samples,
            remaining_energy,
        })
    }

    // Orthogonal projection onto piecewise constant blocks plus the largest
    // possible residual dot product. This is an upper bound, not a heuristic
    // downsampled score; a true match cannot be discarded by the coarse pass.
    fn projection_upper(&self, integral: &Integral, x: u32, y: u32, mean: f64, energy: f64) -> f64 {
        let mut dot = 0.0;
        let mut projected_energy = 0.0;
        for block in &self.blocks {
            let bounds = block.bounds;
            let block_mean = integral.sum(x + bounds.x, y + bounds.y, bounds.width, bounds.height)
                / block.count
                - mean;
            dot += block.count * block.centered_mean * block_mean;
            projected_energy += block.count * block_mean.powi(2);
        }
        dot + (self.residual_energy * (energy - projected_energy).max(0.0)).sqrt()
    }

    #[allow(clippy::too_many_arguments)]
    fn correlate(
        &self,
        pixels: &[u8],
        origin: usize,
        mean: f64,
        energy: f64,
        denominator: f64,
        cutoff: f64,
        budget: &mut Budget,
    ) -> Result<Option<f64>, String> {
        let mut dot = 0.0;
        let mut seen_energy = 0.0;
        let mut seen = 0usize;
        for chunk in self.samples.chunks(16) {
            budget.spend(chunk.len() as u64)?;
            for sample in chunk {
                let centered = pixels[origin + sample.source_offset] as f64 - mean;
                dot += sample.centered * centered;
                seen_energy += centered * centered;
            }
            seen += chunk.len();
            let upper =
                dot + (self.remaining_energy[seen] * (energy - seen_energy).max(0.0)).sqrt();
            if upper + denominator * NUMERIC_SLACK + 1e-6 < cutoff * denominator {
                return Ok(None);
            }
        }
        let score = (dot / denominator).clamp(-1.0, 1.0);
        Ok((score + NUMERIC_SLACK >= cutoff).then_some(score))
    }
}

fn same_detection(left: Bounds, right: Bounds) -> bool {
    // Keep distinct spatial copies even if their scores and scales are equal.
    // A quarter of the smaller width/height permits raster/scale jitter only.
    let dx = (2 * left.x + left.width).abs_diff(2 * right.x + right.width);
    let dy = (2 * left.y + left.height).abs_diff(2 * right.y + right.height);
    if dx as f64 > left.width.min(right.width) as f64 * 0.5
        || dy as f64 > left.height.min(right.height) as f64 * 0.5
    {
        return false;
    }
    let width = (left.x + left.width)
        .min(right.x + right.width)
        .saturating_sub(left.x.max(right.x));
    let height = (left.y + left.height)
        .min(right.y + right.height)
        .saturating_sub(left.y.max(right.y));
    let intersection = width as f64 * height as f64;
    let union = left.width as f64 * left.height as f64 + right.width as f64 * right.height as f64
        - intersection;
    intersection >= union * 0.25
}

struct Budget(u64);

impl Budget {
    fn spend(&mut self, amount: u64) -> Result<(), String> {
        self.0 = self.0.checked_sub(amount).ok_or_else(|| {
            format!(
                "template matching reached its {MAX_WORK}-unit work limit before completing \
                 the search; narrow the window crop or request fewer scales"
            )
        })?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod png {
    use super::{GrayImage, image_size};
    use crate::platform::dylib::Library;
    use std::ffi::{c_char, c_int, c_void};
    use std::ptr;

    // libpng 1.6 png.h simplified API. The versioned public struct is the only
    // native object represented here; png_control remains opaque and private.
    #[repr(C)]
    struct PngImage {
        opaque: *mut c_void,
        version: u32,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
        colormap_entries: u32,
        warning_or_error: u32,
        message: [c_char; 64],
    }

    #[repr(C)]
    struct PngColor {
        red: u8,
        green: u8,
        blue: u8,
    }

    type Begin = unsafe extern "C" fn(*mut PngImage, *const c_void, usize) -> c_int;
    type Finish = unsafe extern "C" fn(
        *mut PngImage,
        *const PngColor,
        *mut c_void,
        i32,
        *mut c_void,
    ) -> c_int;
    type Free = unsafe extern "C" fn(*mut PngImage);

    struct Api {
        _library: Library,
        begin: Begin,
        finish: Finish,
        free: Free,
    }

    impl Api {
        fn open() -> Result<Self, String> {
            let library = Library::open(c"libpng16.so.16")
                .map_err(|error| format!("PNG decoding requires system libpng 1.6: {error}"))?;
            // SAFETY: these are the exact C ABIs of libpng's simplified API.
            // Api owns the library for every Image and function-pointer use.
            let begin = unsafe { library.symbol::<Begin>(c"png_image_begin_read_from_memory") }?;
            let finish = unsafe { library.symbol::<Finish>(c"png_image_finish_read") }?;
            let free = unsafe { library.symbol::<Free>(c"png_image_free") }?;
            Ok(Self {
                _library: library,
                begin,
                finish,
                free,
            })
        }
    }

    struct Image<'a> {
        raw: PngImage,
        api: &'a Api,
    }

    impl Image<'_> {
        fn error(&self) -> String {
            let bytes: Vec<u8> = self
                .raw
                .message
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|&byte| byte as u8)
                .collect();
            format!("cannot decode PNG: {}", String::from_utf8_lossy(&bytes))
        }
    }

    impl Drop for Image<'_> {
        fn drop(&mut self) {
            // SAFETY: raw was initialized for this API; free is also valid
            // after begin/finish failure or when finish already freed opaque.
            unsafe { (self.api.free)(&mut self.raw) };
        }
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<GrayImage, String> {
        let api = Api::open()?;
        let mut image = Image {
            raw: PngImage {
                opaque: ptr::null_mut(),
                version: 1, // PNG_IMAGE_VERSION
                width: 0,
                height: 0,
                format: 0,
                flags: 0,
                colormap_entries: 0,
                warning_or_error: 0,
                message: [0; 64],
            },
            api: &api,
        };
        // SAFETY: the byte slice and initialized struct remain valid through
        // finish_read; no libpng longjmp interface or Rust callbacks are used.
        if unsafe { (api.begin)(&mut image.raw, bytes.as_ptr().cast(), bytes.len()) } == 0 {
            return Err(image.error());
        }
        let count = image_size(image.raw.width, image.raw.height)?;
        image.raw.format = 0x03; // PNG_FORMAT_RGBA = COLOR | ALPHA, 8-bit
        let mut rgba = vec![0u8; count * 4];
        // SAFETY: RGBA needs exactly width*height*4 bytes; row_stride=0 selects
        // that packed layout. RGBA output retains alpha, so background and
        // colormap are unused and can be null, including for palette inputs.
        if unsafe {
            (api.finish)(
                &mut image.raw,
                ptr::null(),
                rgba.as_mut_ptr().cast(),
                0,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(image.error());
        }
        let pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| {
                let alpha = u32::from(pixel[3]);
                let channel =
                    |value: u8| (u32::from(value) * alpha + 255 * (255 - alpha) + 127) / 255;
                ((77 * channel(pixel[0]) + 150 * channel(pixel[1]) + 29 * channel(pixel[2]) + 128)
                    >> 8) as u8
            })
            .collect();
        GrayImage::new(image.raw.width, image.raw.height, pixels)
    }
}

#[cfg(not(target_os = "linux"))]
mod png {
    use super::GrayImage;

    pub(super) fn decode(_bytes: &[u8]) -> Result<GrayImage, String> {
        Err("PNG template decoding is unsupported on this platform; supply GrayImage pixels".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_work_budget_is_an_error_instead_of_an_incomplete_result() {
        let pixels: Vec<u8> = (0..81).map(|index| (index * 73 % 251) as u8).collect();
        let template = GrayImage::new(9, 9, pixels).unwrap();
        let mut pixels = vec![127; 40 * 20];
        for y in 0..9 {
            pixels[y * 40..y * 40 + 9].copy_from_slice(&template.pixels[y * 9..y * 9 + 9]);
        }
        let image = GrayImage::new(40, 20, pixels).unwrap();
        let options = MatchOptions {
            scales: vec![1.0],
            mode: MatchMode::Gray,
            ..MatchOptions::default()
        };
        assert_eq!(
            match_template(&image, &template, &options).unwrap().status,
            MatchStatus::Unique
        );
        // Enough work to encounter the top-left match, but not to finish the
        // rest of the window. Returning that early hit as unique is unsafe.
        let error = match_with_budget(&image, &template, &options, 1_800).unwrap_err();
        assert!(error.contains("before completing the search"));
        assert!(error.contains("narrow the window crop"));
    }

    #[test]
    fn projection_bound_never_excludes_the_exact_correlation() {
        let mut seed = 17u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        let template = GrayImage::new(13, 11, (0..143).map(|_| next()).collect()).unwrap();
        let mut pixels: Vec<u8> = (0..667).map(|_| next()).collect();
        for y in 0..11 {
            pixels[(y + 5) * 29 + 7..(y + 5) * 29 + 20]
                .copy_from_slice(&template.pixels[y * 13..(y + 1) * 13]);
        }
        let image = GrayImage::new(29, 23, pixels).unwrap();
        let model = Model::new(&template, MatchMode::Gray, image.width).unwrap();
        let integral = Integral::new(&image);
        for y in 0..=image.height - template.height {
            for x in 0..=image.width - template.width {
                let sum = integral.sum(x, y, model.width, model.height);
                let mean = sum / model.count;
                let energy = integral.squares(x, y, model.width, model.height) - sum * mean;
                let origin = y as usize * image.width as usize + x as usize;
                let dot: f64 = model
                    .samples
                    .iter()
                    .map(|sample| {
                        sample.centered
                            * (image.pixels[origin + sample.source_offset] as f64 - mean)
                    })
                    .sum();
                assert!(model.projection_upper(&integral, x, y, mean, energy) + 1e-6 >= dot);
                let denominator = (energy * model.energy).sqrt();
                let exact = dot / denominator;
                let mut budget = Budget(MAX_WORK);
                let found = model
                    .correlate(
                        &image.pixels,
                        origin,
                        mean,
                        energy,
                        denominator,
                        exact - 1e-6,
                        &mut budget,
                    )
                    .unwrap();
                assert!(found.is_some_and(|score| (score - exact).abs() < 1e-8));
            }
        }
    }
}
