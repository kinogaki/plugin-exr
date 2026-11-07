//! A file's channel planes as an Image layer's tiles ([`tiles`]), made
//! straight from the planes on the process's workers, and back ([`planes`]).
use crate::exr::{ChannelSamples, Window};
use kinogaki_studio_api::geometry::image::{
    Format, Image, Sample, TILE, Tile, f32_to_half, half_to_f32, tiles_of,
};
use kinogaki_studio_api::geometry::parallel;
use std::convert::Infallible;

/// One channel of a layer as the file holds it: its samples over its
/// part's data window (inclusive), row by row.
pub(crate) struct Plane<'a> {
    pub samples: &'a ChannelSamples,
    pub window: Window,
}

/// Tiles made in one block of work.
const TILES_A_BLOCK: usize = 8;

/// The tiles of a layer of `format` over window `data`, its channels from
/// `planes` in order, each converted to the layer's sample (a float as
/// itself, an integer as its value); a pixel no plane covers is zero, and
/// a tile of zeros is absent.
pub(crate) fn tiles(format: &Format, data: Window, planes: &[Plane<'_>]) -> Vec<Option<Tile>> {
    let [across, down] = tiles_of([data.width() as u32, data.height() as u32]).map(|n| n as usize);
    let blocks = parallel::map_blocks(across * down, TILES_A_BLOCK, |range| {
        Ok::<_, Infallible>(
            range
                .map(|at| tile(format, data, planes, at % across, at / across))
                .collect::<Vec<_>>(),
        )
    });
    match blocks {
        Ok(blocks) => blocks.into_iter().flatten().collect(),
        Err(never) => match never {},
    }
}

/// Tile (`tx`, `ty`).
fn tile(format: &Format, data: Window, planes: &[Plane<'_>], tx: usize, ty: usize) -> Option<Tile> {
    let side = TILE as usize;
    let (size, pixel) = (format.sample.bytes(), format.pixel_bytes());
    let mut bytes = vec![0u8; format.tile_bytes()];
    let left = i64::from(data.x_min) + (tx * side) as i64;
    let top = i64::from(data.y_min) + (ty * side) as i64;
    for row in 0..side {
        let y = top + row as i64;
        if y > i64::from(data.y_max) {
            break;
        }
        for (k, plane) in planes.iter().enumerate() {
            let w = plane.window;
            if y < i64::from(w.y_min) || y > i64::from(w.y_max) {
                continue;
            }
            let from = left.max(i64::from(w.x_min));
            let to = (left + side as i64 - 1)
                .min(i64::from(w.x_max))
                .min(i64::from(data.x_max));
            if from > to {
                continue;
            }
            let source = (y - i64::from(w.y_min)) as usize * w.width()
                + (from - i64::from(w.x_min)) as usize;
            let target = (row * side + (from - left) as usize) * pixel + k * size;
            copy(
                plane.samples,
                source,
                (to - from + 1) as usize,
                format.sample,
                &mut bytes[target..],
                pixel,
            );
        }
    }
    bytes.iter().any(|&b| b != 0).then(|| Tile::new(bytes))
}

/// `count` samples of `samples` from `first`, as `sample`, into `out` a
/// value every `stride` bytes.
fn copy(
    samples: &ChannelSamples,
    first: usize,
    count: usize,
    sample: Sample,
    out: &mut [u8],
    stride: usize,
) {
    let slots = out.chunks_mut(stride).take(count);
    match (samples, sample) {
        (ChannelSamples::F16(v), Sample::F16) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..2].copy_from_slice(&value.to_ne_bytes());
            }
        }
        (ChannelSamples::F32(v), Sample::F32) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..4].copy_from_slice(&value.to_ne_bytes());
            }
        }
        (ChannelSamples::U32(v), Sample::U32) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..4].copy_from_slice(&value.to_ne_bytes());
            }
        }
        (ChannelSamples::F16(v), _) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..4].copy_from_slice(&half_to_f32(*value).to_ne_bytes());
            }
        }
        (ChannelSamples::U32(v), _) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..4].copy_from_slice(&(*value as f32).to_ne_bytes());
            }
        }
        (ChannelSamples::F32(v), _) => {
            for (slot, value) in slots.zip(&v[first..first + count]) {
                slot[..2].copy_from_slice(&f32_to_half(*value).to_ne_bytes());
            }
        }
    }
}

/// Layer `at` of `image` as a file's channels, a plane each over the data
/// window: half floats, floats and integers as held, 8 and 16-bit samples
/// as floats of 0 to 1. None while the host holds the layer's pixels.
pub(crate) fn planes(image: &Image, at: usize) -> Option<Vec<ChannelSamples>> {
    let format = image.layers()[at].format();
    let bytes = image.plane(at)?;
    let (size, pixel) = (format.sample.bytes(), format.pixel_bytes());
    let channel = |k: usize| {
        let values = bytes
            .chunks_exact(pixel)
            .map(move |px| &px[k * size..][..size]);
        match format.sample {
            Sample::F16 => {
                ChannelSamples::F16(values.map(|b| u16::from_ne_bytes([b[0], b[1]])).collect())
            }
            Sample::U32 => ChannelSamples::U32(
                values
                    .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
            ),
            sample => ChannelSamples::F32(values.map(|b| sample.read(b)).collect()),
        }
    };
    Some((0..format.channels.len()).map(channel).collect())
}
