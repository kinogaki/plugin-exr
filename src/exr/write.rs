//! Writing: headers, the offset tables and every chunk, compressed on
//! every processor. Chunks go in increasing y (lineOrder INCREASING_Y);
//! a chunk that does not compress is stored raw.
use crate::exr::compression::{self, Shape};
use crate::exr::header::{self, LONG_NAMES, MAGIC, MULTIPART, TILED};
use crate::exr::{ChannelInfo, ChannelSamples, Error, Exr, Part, SampleType, WriteOptions};
use kinogaki_studio_api::geometry::parallel;

/// A part's chunk grid.
struct Grid {
    width: usize,
    height: usize,
    chunk_width: usize,
    chunk_height: usize,
    across: usize,
    down: usize,
}

impl Grid {
    fn of(part: &Part) -> Grid {
        let (width, height) = part.size();
        let (chunk_width, chunk_height) = match part.header.tiles {
            Some(t) => (t.width as usize, t.height as usize),
            None => (width, part.header.compression.lines()),
        };
        Grid {
            width,
            height,
            chunk_width,
            chunk_height,
            across: width.div_ceil(chunk_width),
            down: height.div_ceil(chunk_height),
        }
    }
}

/// Checks a part can be written; its channels in name order.
fn check(part: &Part, multipart: bool) -> Result<Vec<usize>, Error> {
    let (width, height) = part.size();
    if width == 0 || height == 0 {
        return Err(Error::Invalid("empty data window"));
    }
    if part.channels.is_empty() {
        return Err(Error::Invalid("a part needs at least one channel"));
    }
    if part
        .channels
        .iter()
        .any(|c| c.samples.len() != width * height)
    {
        return Err(Error::Invalid(
            "channel samples do not fill the data window",
        ));
    }
    if part
        .header
        .tiles
        .is_some_and(|t| t.width == 0 || t.height == 0)
    {
        return Err(Error::Invalid("zero tile size"));
    }
    if multipart && part.header.name.as_deref().is_none_or(str::is_empty) {
        return Err(Error::Invalid(
            "every part of a multi-part file needs a name",
        ));
    }
    let mut order: Vec<usize> = (0..part.channels.len()).collect();
    order.sort_by(|&a, &b| part.channels[a].name.cmp(&part.channels[b].name));
    if order
        .windows(2)
        .any(|w| part.channels[w[0]].name == part.channels[w[1]].name)
    {
        return Err(Error::Invalid("channel names must be unique"));
    }
    Ok(order)
}

/// Writes `exr` as an OpenEXR file (multi-part when it has several parts).
pub fn write(exr: &Exr, options: &WriteOptions) -> Result<Vec<u8>, Error> {
    if exr.parts.is_empty() {
        return Err(Error::Invalid("no parts"));
    }
    let multipart = exr.parts.len() > 1;
    if multipart {
        let mut names: Vec<_> = exr.parts.iter().map(|p| &p.header.name).collect();
        names.sort();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return Err(Error::Invalid("part names must be unique"));
        }
    }
    let mut orders = Vec::with_capacity(exr.parts.len());
    let mut infos = Vec::with_capacity(exr.parts.len());
    for part in &exr.parts {
        let order = check(part, multipart)?;
        let list: Vec<ChannelInfo> = order
            .iter()
            .map(|&i| ChannelInfo {
                name: part.channels[i].name.clone(),
                sample_type: part.channels[i].samples.sample_type(),
                linear: part.channels[i].linear,
            })
            .collect();
        header::check(&part.header, &list)?;
        orders.push(order);
        infos.push(list);
    }
    let long = exr
        .parts
        .iter()
        .zip(&infos)
        .any(|(p, l)| header::long_names(&p.header, l));
    let tiled = !multipart && exr.parts[0].header.tiles.is_some();
    let version = 2
        | if tiled { TILED } else { 0 }
        | if long { LONG_NAMES } else { 0 }
        | if multipart { MULTIPART } else { 0 };
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    let grids: Vec<Grid> = exr.parts.iter().map(Grid::of).collect();
    for ((part, list), grid) in exr.parts.iter().zip(&infos).zip(&grids) {
        let chunks = multipart.then_some((grid.across * grid.down) as u32);
        header::write(&mut out, &part.header, list, chunks);
    }
    if multipart {
        out.push(0);
    }
    let table = out.len();
    let total: usize = grids.iter().map(|g| g.across * g.down).sum();
    out.resize(table + total * 8, 0);
    let mut entry = 0;
    for (index, ((part, order), grid)) in exr.parts.iter().zip(&orders).zip(&grids).enumerate() {
        let types: Vec<SampleType> = order
            .iter()
            .map(|&i| part.channels[i].samples.sample_type())
            .collect();
        let count = grid.across * grid.down;
        let chunks = parallel::map(count, |i| {
            chunk(
                part,
                order,
                &types,
                grid,
                i,
                multipart.then_some(index as u32),
                options,
            )
        });
        for bytes in chunks {
            let offset = out.len() as u64;
            out[table + entry * 8..table + entry * 8 + 8].copy_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&bytes);
            entry += 1;
        }
    }
    Ok(out)
}

/// Chunk `index` of `part`: its header fields and its (compressed) data.
fn chunk(
    part: &Part,
    order: &[usize],
    types: &[SampleType],
    grid: &Grid,
    index: usize,
    multipart: Option<u32>,
    options: &WriteOptions,
) -> Vec<u8> {
    let (column, band) = (index % grid.across, index / grid.across);
    let (x0, y0) = (column * grid.chunk_width, band * grid.chunk_height);
    let width = grid.chunk_width.min(grid.width - x0);
    let height = grid.chunk_height.min(grid.height - y0);
    let shape = Shape {
        width,
        height,
        types,
    };
    let mut raw = Vec::with_capacity(shape.raw_len());
    for y in y0..y0 + height {
        let start = y * grid.width + x0;
        for &c in order {
            match &part.channels[c].samples {
                ChannelSamples::U32(v) => v[start..start + width]
                    .iter()
                    .for_each(|s| raw.extend_from_slice(&s.to_le_bytes())),
                ChannelSamples::F16(v) => v[start..start + width]
                    .iter()
                    .for_each(|s| raw.extend_from_slice(&s.to_le_bytes())),
                ChannelSamples::F32(v) => v[start..start + width]
                    .iter()
                    .for_each(|s| raw.extend_from_slice(&s.to_le_bytes())),
            }
        }
    }
    let packed = compression::compress(part.header.compression, &shape, &raw, options.level);
    let data = packed.as_deref().unwrap_or(&raw);
    let mut out = Vec::with_capacity(data.len() + 28);
    if let Some(part) = multipart {
        out.extend_from_slice(&part.to_le_bytes());
    }
    if part.header.tiles.is_some() {
        for value in [column as u32, band as u32, 0, 0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    } else {
        out.extend_from_slice(
            &(part.header.data_window.y_min.wrapping_add(y0 as i32)).to_le_bytes(),
        );
    }
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}
