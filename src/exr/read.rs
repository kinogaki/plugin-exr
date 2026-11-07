//! Reading: the version field and headers, the chunk offset tables, and
//! each part's chunks decompressed on every processor into its planes.
//!
//! Work goes in bands of rows: a scanline chunk, or a row of tiles. Bands
//! go in at most [`GROUPS`] groups, each owning its rows of every channel
//! plane, so groups decode in parallel straight into the planes; a group's
//! bands go in order, each chunk polling the read's budget, and once a
//! chunk fails the groups after its own stop (the error is the first by
//! position, on any number of workers).
//!
//! Nothing is allocated for a part before its size is checked: its pixels
//! against the most a part may have, its planes against the read's budget.
use crate::exr::compression::{self, Shape};
use crate::exr::header::{self, DEEP, LONG_NAMES, MAGIC, MULTIPART, Parsed, Reader, TILED};
use crate::exr::parallel;
use crate::exr::{Channel, ChannelSamples, Error, Exr, Part, PartHeader, ReadOptions, SampleType};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The most groups of bands a part's work goes in: what each group holds
/// of every plane (a slice a channel) stays small whatever the bands and
/// channels a file claims.
const GROUPS: usize = 256;

/// The parsed headers and where the offset tables start.
struct Structure {
    parts: Vec<Parsed>,
    multipart: bool,
    tables: usize,
}

fn structure(data: &[u8]) -> Result<Structure, Error> {
    let mut r = Reader::new(data, 0);
    if r.u32()
        .map_err(|_| Error::corrupt("not an OpenEXR file", 0))?
        != MAGIC
    {
        return Err(Error::corrupt("not an OpenEXR file", 0));
    }
    let version = r.u32()?;
    if version & 0xFF != 2 {
        return Err(Error::Unsupported("file format version other than 2"));
    }
    if version & DEEP != 0 {
        return Err(Error::Unsupported("deep data"));
    }
    if version & !(0xFF | TILED | LONG_NAMES | MULTIPART) != 0 {
        return Err(Error::Unsupported("unknown version flags"));
    }
    let multipart = version & MULTIPART != 0;
    let mut parts = Vec::new();
    loop {
        if multipart && r.data.get(r.pos) == Some(&0) {
            r.pos += 1;
            break;
        }
        if parts.len() >= 4096 {
            return Err(Error::corrupt("more than 4096 parts", r.pos));
        }
        let at = r.pos;
        let parsed = header::parse(&mut r)?;
        if !multipart && (version & TILED != 0) != parsed.part.header.tiles.is_some() {
            return Err(Error::corrupt(
                "tiled flag and tiles attribute disagree",
                at,
            ));
        }
        if multipart && parsed.part.header.name.is_none() {
            return Err(Error::corrupt("multi-part header without a name", at));
        }
        parts.push(parsed);
        if !multipart {
            break;
        }
    }
    if parts.is_empty() {
        return Err(Error::corrupt("no parts", r.pos));
    }
    Ok(Structure {
        parts,
        multipart,
        tables: r.pos,
    })
}

/// The headers and channel lists of every part, without reading pixels.
pub fn read_headers(data: &[u8]) -> Result<Vec<PartHeader>, Error> {
    Ok(structure(data)?.parts.into_iter().map(|p| p.part).collect())
}

/// How a part's chunks tile its data window.
struct Layout {
    width: usize,
    height: usize,
    /// Chunk size (a scanline chunk spans the width).
    chunk_width: usize,
    chunk_height: usize,
    across: usize,
    down: usize,
    tiled: bool,
}

impl Layout {
    fn of(part: &PartHeader) -> Layout {
        let window = part.header.data_window;
        let (width, height) = (window.width(), window.height());
        let (chunk_width, chunk_height, tiled) = match part.header.tiles {
            Some(t) => (t.width as usize, t.height as usize, true),
            None => (width, part.header.compression.lines(), false),
        };
        Layout {
            width,
            height,
            chunk_width,
            chunk_height,
            across: width.div_ceil(chunk_width),
            down: height.div_ceil(chunk_height),
            tiled,
        }
    }

    /// How many chunks the part has: refused when that overflows (a
    /// window 2³² wide and tall in one-pixel tiles).
    fn chunks(&self) -> Result<usize, Error> {
        self.across.checked_mul(self.down).ok_or(Error::TooLarge)
    }
}

/// A channel plane's rows of one band.
enum Rows<'a> {
    U32(&'a mut [u32]),
    F16(&'a mut [u16]),
    F32(&'a mut [f32]),
}

/// Reads every part of the EXR in `data`.
pub fn read(data: &[u8], options: &ReadOptions) -> Result<Exr, Error> {
    let structure = structure(data)?;
    let mut table = Reader::new(data, structure.tables);
    let mut offsets = Vec::with_capacity(structure.parts.len());
    for parsed in &structure.parts {
        let layout = Layout::of(&parsed.part);
        let count = layout.chunks()?;
        if parsed.chunk_count.is_some_and(|c| c as usize != count) {
            return Err(Error::corrupt(
                "chunkCount disagrees with the data window",
                structure.tables,
            ));
        }
        let bytes = table.take(count.checked_mul(8).ok_or(Error::TooLarge)?)?;
        let part: Vec<u64> = (bytes.as_chunks::<8>().0.iter())
            .map(|&b| u64::from_le_bytes(b))
            .collect();
        // Every chunk's offset in the file before any part is allocated.
        if let Some(&outside) = part.iter().find(|&&o| o == 0 || o >= data.len() as u64) {
            return Err(Error::corrupt(
                "chunk offset outside the file",
                outside as usize,
            ));
        }
        offsets.push(part);
    }
    let mut parts = Vec::with_capacity(structure.parts.len());
    for (index, (parsed, offsets)) in structure.parts.into_iter().zip(offsets).enumerate() {
        let multipart = structure.multipart.then_some(index as u32);
        parts.push(read_part(data, parsed.part, &offsets, multipart, options)?);
    }
    Ok(Exr { parts })
}

fn read_part(
    data: &[u8],
    part: PartHeader,
    offsets: &[u64],
    multipart: Option<u32>,
    options: &ReadOptions,
) -> Result<Part, Error> {
    let layout = Layout::of(&part);
    let pixels = (layout.width as u64).saturating_mul(layout.height as u64);
    let bytes = (part.channels.iter())
        .map(|c| c.sample_type.bytes() as u64)
        .sum::<u64>()
        .saturating_mul(pixels);
    if pixels > options.max_pixels || bytes > options.max_pixels.saturating_mul(64) {
        return Err(Error::TooLarge);
    }
    let types: Vec<SampleType> = part.channels.iter().map(|c| c.sample_type).collect();
    for t in &types {
        options.budget.allocate(pixels * t.bytes() as u64)?;
    }
    let mut planes: Vec<ChannelSamples> = types
        .iter()
        .map(|t| match t {
            SampleType::U32 => ChannelSamples::U32(vec![0; pixels as usize]),
            SampleType::F16 => ChannelSamples::F16(vec![0; pixels as usize]),
            SampleType::F32 => ChannelSamples::F32(vec![0.0; pixels as usize]),
        })
        .collect();
    {
        // Each group's rows of every plane: `per` bands of `band_len`
        // samples.
        let band_len = layout.width * layout.chunk_height;
        let per = layout.down.div_ceil(GROUPS).max(1);
        let groups = layout.down.div_ceil(per);
        let mut rows: Vec<Vec<Rows>> = (0..groups)
            .map(|_| Vec::with_capacity(types.len()))
            .collect();
        for plane in &mut planes {
            match plane {
                ChannelSamples::U32(v) => v
                    .chunks_mut(band_len * per)
                    .zip(&mut rows)
                    .for_each(|(r, g)| g.push(Rows::U32(r))),
                ChannelSamples::F16(v) => v
                    .chunks_mut(band_len * per)
                    .zip(&mut rows)
                    .for_each(|(r, g)| g.push(Rows::F16(r))),
                ChannelSamples::F32(v) => v
                    .chunks_mut(band_len * per)
                    .zip(&mut rows)
                    .for_each(|(r, g)| g.push(Rows::F32(r))),
            }
        }
        let slots: Vec<Mutex<Option<Vec<Rows>>>> =
            rows.into_iter().map(|g| Mutex::new(Some(g))).collect();
        let header = &part.header;
        // The first group a chunk failed in.
        let failed = AtomicUsize::new(usize::MAX);
        parallel::map(groups, |group| {
            let mut rows = slots[group]
                .lock()
                .expect("group")
                .take()
                .expect("each group once");
            let first = group * per;
            for band in first..(first + per).min(layout.down) {
                for column in 0..layout.across {
                    if failed.load(Ordering::Relaxed) < group {
                        return Ok(());
                    }
                    let read = options.budget.check().map_err(Error::from).and_then(|()| {
                        let at = (band - first) * band_len;
                        let chunk = (column, band);
                        read_chunk(
                            data, header, &layout, &types, offsets, multipart, chunk, at, &mut rows,
                        )
                    });
                    if read.is_err() {
                        failed.fetch_min(group, Ordering::Relaxed);
                        return read;
                    }
                }
            }
            Ok::<(), Error>(())
        })?;
    }
    let channels = part
        .channels
        .into_iter()
        .zip(planes)
        .map(|(info, samples)| Channel {
            name: info.name,
            samples,
            linear: info.linear,
        })
        .collect();
    Ok(Part {
        header: part.header,
        channels,
    })
}

/// Reads chunk (`column`, `band`) into `rows`, its group's plane rows,
/// the band's first sample at `at` in them.
#[allow(clippy::too_many_arguments)]
fn read_chunk(
    data: &[u8],
    header: &crate::exr::Header,
    layout: &Layout,
    types: &[SampleType],
    offsets: &[u64],
    multipart: Option<u32>,
    (column, band): (usize, usize),
    at: usize,
    rows: &mut [Rows],
) -> Result<(), Error> {
    let index = band * layout.across + column;
    let offset = offsets[index] as usize;
    let mut r = Reader::new(data, offset);
    if offset == 0 || offset >= data.len() {
        return Err(Error::corrupt("chunk offset outside the file", offset));
    }
    if let Some(part) = multipart
        && r.u32()? != part
    {
        return Err(Error::corrupt("chunk belongs to another part", offset));
    }
    if layout.tiled {
        let (tx, ty, lx, ly) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        if (tx as usize, ty as usize, lx, ly) != (column, band, 0, 0) {
            return Err(Error::corrupt(
                "tile coordinates disagree with the offset table",
                offset,
            ));
        }
    } else {
        let y = i64::from(r.i32()?);
        if y != i64::from(header.data_window.y_min) + (band * layout.chunk_height) as i64 {
            return Err(Error::corrupt(
                "scanline chunk's y disagrees with the offset table",
                offset,
            ));
        }
    }
    let size = r.i32()?;
    if size < 0 {
        return Err(Error::corrupt("negative chunk size", r.pos));
    }
    let start = r.pos;
    let packed = r.take(size as usize)?;
    let x0 = column * layout.chunk_width;
    let width = layout.chunk_width.min(layout.width - x0);
    let height = layout
        .chunk_height
        .min(layout.height - band * layout.chunk_height);
    let shape = Shape {
        width,
        height,
        types,
    };
    let raw = compression::decompress(header.compression, &shape, packed, start)?;
    // Scatter: per scanline, per channel, `width` little-endian samples.
    let mut source = raw.as_slice();
    for y in 0..height {
        let start = at + y * layout.width + x0;
        for plane in rows.iter_mut() {
            match plane {
                Rows::U32(p) => {
                    let (bytes, rest) = source.split_at(width * 4);
                    for (o, b) in p[start..start + width]
                        .iter_mut()
                        .zip(bytes.as_chunks::<4>().0)
                    {
                        *o = u32::from_le_bytes(*b);
                    }
                    source = rest;
                }
                Rows::F16(p) => {
                    let (bytes, rest) = source.split_at(width * 2);
                    for (o, b) in p[start..start + width]
                        .iter_mut()
                        .zip(bytes.as_chunks::<2>().0)
                    {
                        *o = u16::from_le_bytes(*b);
                    }
                    source = rest;
                }
                Rows::F32(p) => {
                    let (bytes, rest) = source.split_at(width * 4);
                    for (o, b) in p[start..start + width]
                        .iter_mut()
                        .zip(bytes.as_chunks::<4>().0)
                    {
                        *o = f32::from_le_bytes(*b);
                    }
                    source = rest;
                }
            }
        }
    }
    Ok(())
}
