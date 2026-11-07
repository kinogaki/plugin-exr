//! Headers: the magic and version field, attributes, the channel list,
//! and the attributes this library interprets.
use crate::exr::{
    Attribute, ChannelInfo, Compression, Error, Header, PartHeader, SampleType, Tiles, Window,
};

pub(crate) const MAGIC: u32 = 20_000_630;
/// Version field flags.
pub(crate) const TILED: u32 = 0x200;
pub(crate) const LONG_NAMES: u32 = 0x400;
pub(crate) const DEEP: u32 = 0x800;
pub(crate) const MULTIPART: u32 = 0x1000;

/// The attributes kept as typed fields of [`Header`] (or implied by the
/// file's structure), never as raw attributes.
const INTERPRETED: [&str; 13] = [
    "channels",
    "compression",
    "dataWindow",
    "displayWindow",
    "lineOrder",
    "pixelAspectRatio",
    "screenWindowCenter",
    "screenWindowWidth",
    "tiles",
    "name",
    "type",
    "chunkCount",
    "version",
];

/// Little-endian reads with the file offset of each failure.
pub(crate) struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let bytes = self
            .data
            .get(self.pos..self.pos.saturating_add(n))
            .ok_or(Error::corrupt("file ends early", self.pos))?;
        self.pos += n;
        Ok(bytes)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(self.u32()? as i32)
    }

    pub fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// A NUL-terminated name of at most 255 bytes.
    pub fn name(&mut self) -> Result<String, Error> {
        let at = self.pos;
        let rest = &self.data[self.pos.min(self.data.len())..];
        let end = rest
            .iter()
            .take(256)
            .position(|&b| b == 0)
            .ok_or(Error::corrupt("unterminated or overlong name", at))?;
        let name = std::str::from_utf8(&rest[..end])
            .map_err(|_| Error::corrupt("name is not UTF-8", at))?;
        self.pos += end + 1;
        Ok(name.to_owned())
    }
}

/// What a header says beyond [`Header`]: the part's channel list and the
/// structure attributes.
pub(crate) struct Parsed {
    pub part: PartHeader,
    /// The declared `chunkCount`.
    pub chunk_count: Option<u32>,
}

fn window(value: &[u8], at: usize) -> Result<Window, Error> {
    if value.len() != 16 {
        return Err(Error::corrupt("box2i attribute is not 16 bytes", at));
    }
    let mut r = Reader::new(value, 0);
    Ok(Window {
        x_min: r.i32()?,
        y_min: r.i32()?,
        x_max: r.i32()?,
        y_max: r.i32()?,
    })
}

fn channels(value: &[u8], at: usize) -> Result<Vec<ChannelInfo>, Error> {
    let mut r = Reader::new(value, 0);
    let mut list: Vec<ChannelInfo> = Vec::new();
    loop {
        if r.data.get(r.pos) == Some(&0) {
            break;
        }
        let name = r
            .name()
            .map_err(|_| Error::corrupt("bad channel name", at + r.pos))?;
        let fail = |r: &Reader| Error::corrupt("short channel list", at + r.pos);
        let kind = r.i32().map_err(|_| fail(&r))?;
        let linear = r.u8().map_err(|_| fail(&r))? != 0;
        r.take(3).map_err(|_| fail(&r))?;
        let (xs, ys) = (
            r.i32().map_err(|_| fail(&r))?,
            r.i32().map_err(|_| fail(&r))?,
        );
        let sample_type = match kind {
            0 => SampleType::U32,
            1 => SampleType::F16,
            2 => SampleType::F32,
            _ => return Err(Error::corrupt("unknown channel sample type", at + r.pos)),
        };
        if xs != 1 || ys != 1 {
            return Err(Error::Unsupported("subsampled channels"));
        }
        if name.is_empty() || list.last().is_some_and(|last| last.name >= name) {
            return Err(Error::corrupt(
                "channels not sorted or not unique",
                at + r.pos,
            ));
        }
        if list.len() >= 4096 {
            return Err(Error::corrupt("more than 4096 channels", at));
        }
        list.push(ChannelInfo {
            name,
            sample_type,
            linear,
        });
    }
    if list.is_empty() {
        return Err(Error::corrupt("no channels", at));
    }
    Ok(list)
}

/// Reads one header from `r` (through its terminating NUL).
pub(crate) fn parse(r: &mut Reader) -> Result<Parsed, Error> {
    let start = r.pos;
    let mut attributes: Vec<(Attribute, usize)> = Vec::new();
    let mut named = std::collections::HashSet::new();
    loop {
        if r.data.get(r.pos) == Some(&0) {
            r.pos += 1;
            break;
        }
        let at = r.pos;
        let name = r.name()?;
        let kind = r.name()?;
        let size = r.i32()?;
        if size < 0 {
            return Err(Error::corrupt("negative attribute size", at));
        }
        let value = r.take(size as usize)?.to_vec();
        if !named.insert(name.clone()) {
            return Err(Error::corrupt("duplicate attribute", at));
        }
        attributes.push((Attribute { name, kind, value }, r.pos - size as usize));
    }
    let get = |name: &str, kind: &str| -> Result<Option<(&[u8], usize)>, Error> {
        match attributes.iter().find(|(a, _)| a.name == name) {
            Some((a, at)) if a.kind == kind => Ok(Some((&a.value[..], *at))),
            Some((_, at)) => Err(Error::corrupt("attribute has the wrong type", *at)),
            None => Ok(None),
        }
    };
    let required = |name: &str, kind: &str| -> Result<(&[u8], usize), Error> {
        get(name, kind)?.ok_or(Error::corrupt("required attribute missing", start))
    };
    let (value, at) = required("channels", "chlist")?;
    let list = channels(value, at)?;
    let (value, at) = required("compression", "compression")?;
    let compression = match value.first() {
        Some(0) => Compression::None,
        Some(1) => Compression::Rle,
        Some(2) => Compression::Zips,
        Some(3) => Compression::Zip,
        Some(4) => Compression::Piz,
        Some(5) => Compression::Pxr24,
        Some(6..=9) => return Err(Error::Unsupported("B44 and DWA compression")),
        _ => return Err(Error::corrupt("unknown compression", at)),
    };
    let (value, at) = required("dataWindow", "box2i")?;
    let data_window = window(value, at)?;
    if data_window.x_max < data_window.x_min || data_window.y_max < data_window.y_min {
        return Err(Error::corrupt("empty or inverted data window", at));
    }
    let display_window = match get("displayWindow", "box2i")? {
        Some((value, at)) => window(value, at)?,
        None => data_window,
    };
    let float = |name: &str, default: f32| -> Result<f32, Error> {
        Ok(match get(name, "float")? {
            Some((value, at)) => Reader::new(value, 0)
                .f32()
                .map_err(|_| Error::corrupt("short float attribute", at))?,
            None => default,
        })
    };
    let pixel_aspect_ratio = float("pixelAspectRatio", 1.0)?;
    let screen_window_width = float("screenWindowWidth", 1.0)?;
    let screen_window_center = match get("screenWindowCenter", "v2f")? {
        Some((value, at)) => {
            let mut v = Reader::new(value, 0);
            let short = |_| Error::corrupt("short v2f attribute", at);
            [v.f32().map_err(short)?, v.f32().map_err(short)?]
        }
        None => [0.0, 0.0],
    };
    let tiles = match get("tiles", "tiledesc")? {
        Some((value, at)) => {
            let mut v = Reader::new(value, 0);
            let short = |_| Error::corrupt("short tiledesc attribute", at);
            let (width, height, mode) = (
                v.u32().map_err(short)?,
                v.u32().map_err(short)?,
                v.u8().map_err(short)?,
            );
            if mode & 15 != 0 {
                return Err(Error::Unsupported("mipmap and ripmap levels"));
            }
            if width == 0 || height == 0 || width > 1 << 20 || height > 1 << 20 {
                return Err(Error::corrupt("invalid tile size", at));
            }
            Some(Tiles { width, height })
        }
        None => None,
    };
    let name = match get("name", "string")? {
        Some((value, at)) => Some(
            String::from_utf8(value.to_vec())
                .map_err(|_| Error::corrupt("part name is not UTF-8", at))?,
        ),
        None => None,
    };
    let kind = get("type", "string")?.map(|(value, _)| value);
    if matches!(kind, Some(b"deepscanline" | b"deeptile")) {
        return Err(Error::Unsupported("deep data"));
    }
    let chunk_count = match get("chunkCount", "int")? {
        Some((value, at)) => Some(
            Reader::new(value, 0)
                .u32()
                .map_err(|_| Error::corrupt("short int attribute", at))?,
        ),
        None => None,
    };
    let header = Header {
        data_window,
        display_window,
        compression,
        tiles,
        pixel_aspect_ratio,
        screen_window_center,
        screen_window_width,
        name,
        attributes: attributes
            .into_iter()
            .map(|(a, _)| a)
            .filter(|a| !INTERPRETED.contains(&a.name.as_str()))
            .collect(),
    };
    Ok(Parsed {
        part: PartHeader {
            header,
            channels: list,
        },
        chunk_count,
    })
}

/// Appends one attribute.
fn attribute(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) {
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(kind.as_bytes());
    out.push(0);
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

fn window_bytes(w: &Window) -> Vec<u8> {
    [w.x_min, w.y_min, w.x_max, w.y_max]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

/// Whether a header needs the long-names flag (names past 31 bytes).
pub(crate) fn long_names(header: &Header, channels: &[ChannelInfo]) -> bool {
    channels.iter().any(|c| c.name.len() > 31)
        || header
            .attributes
            .iter()
            .any(|a| a.name.len() > 31 || a.kind.len() > 31)
}

/// Checks what a header will write: names that fit, attributes that do not
/// shadow the interpreted ones.
pub(crate) fn check(header: &Header, channels: &[ChannelInfo]) -> Result<(), Error> {
    let bad = |name: &str| name.is_empty() || name.len() > 255 || name.contains('\0');
    if channels.iter().any(|c| bad(&c.name)) {
        return Err(Error::Invalid(
            "channel names must be 1 to 255 bytes without NUL",
        ));
    }
    if header
        .attributes
        .iter()
        .any(|a| bad(&a.name) || bad(&a.kind) || INTERPRETED.contains(&a.name.as_str()))
    {
        return Err(Error::Invalid(
            "attribute names and types must be 1 to 255 bytes, and not one the header writes itself",
        ));
    }
    Ok(())
}

/// Appends a header: the interpreted attributes, then the others, then
/// the terminating NUL. `multipart` adds `name`, `type` and `chunkCount`.
pub(crate) fn write(
    out: &mut Vec<u8>,
    header: &Header,
    channels: &[ChannelInfo],
    multipart: Option<u32>,
) {
    let mut list = Vec::new();
    for c in channels {
        list.extend_from_slice(c.name.as_bytes());
        list.push(0);
        let kind: i32 = match c.sample_type {
            SampleType::U32 => 0,
            SampleType::F16 => 1,
            SampleType::F32 => 2,
        };
        list.extend_from_slice(&kind.to_le_bytes());
        list.extend_from_slice(&[u8::from(c.linear), 0, 0, 0]);
        list.extend_from_slice(&1i32.to_le_bytes());
        list.extend_from_slice(&1i32.to_le_bytes());
    }
    list.push(0);
    attribute(out, "channels", "chlist", &list);
    attribute(
        out,
        "compression",
        "compression",
        &[header.compression.code()],
    );
    attribute(
        out,
        "dataWindow",
        "box2i",
        &window_bytes(&header.data_window),
    );
    attribute(
        out,
        "displayWindow",
        "box2i",
        &window_bytes(&header.display_window),
    );
    attribute(out, "lineOrder", "lineOrder", &[0]);
    attribute(
        out,
        "pixelAspectRatio",
        "float",
        &header.pixel_aspect_ratio.to_le_bytes(),
    );
    let center: Vec<u8> = header
        .screen_window_center
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    attribute(out, "screenWindowCenter", "v2f", &center);
    attribute(
        out,
        "screenWindowWidth",
        "float",
        &header.screen_window_width.to_le_bytes(),
    );
    if let Some(tiles) = header.tiles {
        let mut value = Vec::with_capacity(9);
        value.extend_from_slice(&tiles.width.to_le_bytes());
        value.extend_from_slice(&tiles.height.to_le_bytes());
        value.push(0);
        attribute(out, "tiles", "tiledesc", &value);
    }
    if let Some(chunks) = multipart {
        let name = header.name.clone().unwrap_or_default();
        attribute(out, "name", "string", name.as_bytes());
        let kind = if header.tiles.is_some() {
            "tiledimage"
        } else {
            "scanlineimage"
        };
        attribute(out, "type", "string", kind.as_bytes());
        attribute(out, "chunkCount", "int", &chunks.to_le_bytes());
    } else if let Some(name) = &header.name {
        attribute(out, "name", "string", name.as_bytes());
    }
    for a in &header.attributes {
        attribute(out, &a.name, &a.kind, &a.value);
    }
    out.push(0);
}
