//! OpenEXR reading and writing, with Cryptomatte.
//!
//! An [`Exr`] is one or more [`Part`]s; a part is a [`Header`] and its
//! channels, each a plane of samples over the data window (32-bit
//! unsigned integers, half floats as their bits, or floats), in the
//! file's own order: sorted by name. [`read`](fn@read) reads scanline and
//! tiled parts (single level) of every compression but B44 and DWA: none,
//! RLE, ZIPS, ZIP, PIZ and PXR24, within a read budget (the SDK's
//! `ReadBudget`, the one thing of the SDK it takes besides the workers);
//! [`write`](fn@write) writes all of those.
//! Chunks decompress and compress on the process's workers. [`Part::to_image`] and
//! [`Part::from_image`] convert to and from lib-image's interleaved
//! [`Image`]. Cryptomatte layers are in [`cryptomatte`].
//!
//! ```
//! use kinogaki_exr_format::exr::{Compression, Exr, Image, Part, Samples, WriteOptions};
//! use kinogaki_studio_api::format::ReadBudget;
//! let image = Image::new(2, 1, 4, Samples::F32(vec![1.0, 0.5, 0.25, 1.0, 0.0, 0.0, 0.0, 0.5])).unwrap();
//! let part = Part::from_image(&image, Compression::Zip).unwrap();
//! let bytes = kinogaki_exr_format::exr::write(&Exr { parts: vec![part] }, &WriteOptions::default()).unwrap();
//! let read = kinogaki_exr_format::exr::read(&bytes, &ReadBudget::default()).unwrap();
//! assert_eq!(read.parts[0].to_image().unwrap(), image);
//! ```
mod compression;
pub mod cryptomatte;
mod error;
mod header;
mod image;
mod read;
mod write;

pub use error::Error;
pub use kinogaki_deflate::Level;
pub use kinogaki_image::{Image, Samples, f16_to_f32, f32_to_f16};
pub use read::{read, read_headers};
pub use write::write;

/// An OpenEXR file: one part, or several (multi-part).
#[derive(Clone, Debug, PartialEq)]
pub struct Exr {
    /// The parts, in file order.
    pub parts: Vec<Part>,
}

/// One image of a file: its header and channels.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// What the header says.
    pub header: Header,
    /// Sorted by name, as the file stores them.
    pub channels: Vec<Channel>,
}

/// A part's header: the attributes this library interprets, and every
/// other attribute as the file has it.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    /// The pixels the part holds (inclusive bounds).
    pub data_window: Window,
    /// The picture's frame.
    pub display_window: Window,
    /// How the chunks are compressed.
    pub compression: Compression,
    /// Tiled storage (one level), or scanlines.
    pub tiles: Option<Tiles>,
    /// Width over height of a pixel.
    pub pixel_aspect_ratio: f32,
    /// The screen window's centre and width (perspective projection).
    pub screen_window_center: [f32; 2],
    /// See `screen_window_center`.
    pub screen_window_width: f32,
    /// The part's name (required in multi-part files).
    pub name: Option<String>,
    /// Every other attribute, in file order.
    pub attributes: Vec<Attribute>,
}

/// An inclusive pixel box (`box2i`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Left column.
    pub x_min: i32,
    /// Top row.
    pub y_min: i32,
    /// Right column (inclusive).
    pub x_max: i32,
    /// Bottom row (inclusive).
    pub y_max: i32,
}

impl Window {
    /// The box of `width` × `height` pixels at the origin.
    pub fn sized(width: u32, height: u32) -> Window {
        Window {
            x_min: 0,
            y_min: 0,
            x_max: width as i32 - 1,
            y_max: height as i32 - 1,
        }
    }

    /// Pixels across.
    pub fn width(&self) -> usize {
        (i64::from(self.x_max) - i64::from(self.x_min) + 1).max(0) as usize
    }

    /// Pixels down.
    pub fn height(&self) -> usize {
        (i64::from(self.y_max) - i64::from(self.y_min) + 1).max(0) as usize
    }
}

/// Tile size of a tiled part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiles {
    /// Pixels across a tile.
    pub width: u32,
    /// Pixels down a tile.
    pub height: u32,
}

/// How chunks are compressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// Stored as is.
    None,
    /// Run-length coding of the predicted bytes.
    Rle,
    /// zlib, one scanline a chunk.
    Zips,
    /// zlib, 16 scanlines a chunk.
    #[default]
    Zip,
    /// Wavelet and Huffman coding, 32 scanlines a chunk (lossless).
    Piz,
    /// Floats rounded to 24 bits, then zlib, 16 scanlines (lossy for
    /// FLOAT channels only).
    Pxr24,
}

impl Compression {
    /// Scanlines a chunk of a scanline part holds.
    pub fn lines(self) -> usize {
        match self {
            Compression::None | Compression::Rle | Compression::Zips => 1,
            Compression::Zip | Compression::Pxr24 => 16,
            Compression::Piz => 32,
        }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            Compression::None => 0,
            Compression::Rle => 1,
            Compression::Zips => 2,
            Compression::Zip => 3,
            Compression::Piz => 4,
            Compression::Pxr24 => 5,
        }
    }
}

/// An attribute as the file stores it: name, type name and value bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// The attribute's name.
    pub name: String,
    /// Its type name (`string`, `int`, `v2f`...).
    pub kind: String,
    /// Its value bytes as the file stores them.
    pub value: Vec<u8>,
}

/// A channel: its name and its samples over the data window, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The channel's name (`R`, `Z`, `CryptoObject00.R`...).
    pub name: String,
    /// Its samples.
    pub samples: ChannelSamples,
    /// The `pLinear` hint (perceptually linear sampling).
    pub linear: bool,
}

/// A channel's samples, by type.
#[derive(Clone, Debug, PartialEq)]
pub enum ChannelSamples {
    /// 32-bit unsigned integers (UINT).
    U32(Vec<u32>),
    /// Half floats as their IEEE 754 binary16 bits.
    F16(Vec<u16>),
    /// 32-bit floats (FLOAT).
    F32(Vec<f32>),
}

impl ChannelSamples {
    /// How many samples there are.
    pub fn len(&self) -> usize {
        match self {
            ChannelSamples::U32(v) => v.len(),
            ChannelSamples::F16(v) => v.len(),
            ChannelSamples::F32(v) => v.len(),
        }
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The samples' type.
    pub fn sample_type(&self) -> SampleType {
        match self {
            ChannelSamples::U32(_) => SampleType::U32,
            ChannelSamples::F16(_) => SampleType::F16,
            ChannelSamples::F32(_) => SampleType::F32,
        }
    }

    /// Sample `i` as a float (UINT converted exactly where it can be).
    pub fn get(&self, i: usize) -> f32 {
        match self {
            ChannelSamples::U32(v) => v[i] as f32,
            ChannelSamples::F16(v) => f16_to_f32(v[i]),
            ChannelSamples::F32(v) => v[i],
        }
    }
}

/// A channel's sample type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleType {
    /// UINT.
    U32,
    /// HALF.
    F16,
    /// FLOAT.
    F32,
}

impl SampleType {
    /// Bytes a sample takes in the file.
    pub fn bytes(self) -> usize {
        match self {
            SampleType::F16 => 2,
            SampleType::U32 | SampleType::F32 => 4,
        }
    }
}

/// A channel as a header lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    /// The channel's name.
    pub name: String,
    /// Its sample type.
    pub sample_type: SampleType,
    /// The `pLinear` hint.
    pub linear: bool,
}

/// A part's header and channel list, read without its pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct PartHeader {
    /// The header.
    pub header: Header,
    /// The channel list, sorted by name.
    pub channels: Vec<ChannelInfo>,
}

impl Header {
    /// A scanline header for a `width` × `height` image at the origin.
    pub fn new(width: u32, height: u32, compression: Compression) -> Header {
        let window = Window::sized(width, height);
        Header {
            data_window: window,
            display_window: window,
            compression,
            tiles: None,
            pixel_aspect_ratio: 1.0,
            screen_window_center: [0.0, 0.0],
            screen_window_width: 1.0,
            name: None,
            attributes: Vec::new(),
        }
    }

    /// The attribute called `name`.
    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    /// The value of the `string` attribute called `name`, if it is one
    /// and is UTF-8.
    pub fn string(&self, name: &str) -> Option<&str> {
        self.attribute(name)
            .filter(|a| a.kind == "string")
            .and_then(|a| std::str::from_utf8(&a.value).ok())
    }

    /// Sets (or adds) the `string` attribute `name`.
    pub fn set_string(&mut self, name: &str, value: &str) {
        self.set_attribute(Attribute {
            name: name.to_owned(),
            kind: "string".to_owned(),
            value: value.as_bytes().to_vec(),
        });
    }

    /// Sets (or adds) an attribute, replacing one of the same name.
    pub fn set_attribute(&mut self, attribute: Attribute) {
        match self
            .attributes
            .iter_mut()
            .find(|a| a.name == attribute.name)
        {
            Some(existing) => *existing = attribute,
            None => self.attributes.push(attribute),
        }
    }
}

impl Part {
    /// The channel called `name`.
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.name == name)
    }

    /// Pixels across and down.
    pub fn size(&self) -> (usize, usize) {
        (
            self.header.data_window.width(),
            self.header.data_window.height(),
        )
    }
}

/// How a write runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    /// zlib effort for ZIP, ZIPS and PXR24: `Fast` by default (files
    /// within 1% of zlib level 6's size at several times its speed).
    pub level: Level,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { level: Level::Fast }
    }
}
