//! Chunk compression. Every codec works on a chunk's raw bytes as the file
//! defines them: for each scanline, for each channel (sorted by name),
//! that channel's samples across the chunk, little-endian. A chunk whose
//! compressed form is not smaller is stored raw, and is recognised on
//! reading by its size.
mod piz;
mod pxr24;
mod rle;
mod zip;

use crate::exr::{Compression, Error, SampleType};
use kinogaki_deflate::Level;

/// A chunk's size and channel types, as the codecs need them.
pub(crate) struct Shape<'a> {
    pub width: usize,
    pub height: usize,
    pub types: &'a [SampleType],
}

impl Shape<'_> {
    /// Bytes of the raw chunk.
    pub fn raw_len(&self) -> usize {
        self.width * self.height * self.types.iter().map(|t| t.bytes()).sum::<usize>()
    }
}

/// The raw bytes of a chunk stored as `data` (`at` its file offset, for
/// errors).
pub(crate) fn decompress(
    compression: Compression,
    shape: &Shape,
    data: &[u8],
    at: usize,
) -> Result<Vec<u8>, Error> {
    let raw_len = shape.raw_len();
    if data.len() == raw_len {
        return Ok(data.to_vec());
    }
    if data.len() > raw_len {
        return Err(Error::corrupt("chunk larger than its raw size", at));
    }
    let fail = |what| Error::corrupt(what, at);
    match compression {
        Compression::None => Err(fail("uncompressed chunk of the wrong size")),
        Compression::Rle => {
            let predicted = rle::decode(data, raw_len).map_err(fail)?;
            Ok(zip::unpredict(predicted))
        }
        Compression::Zips | Compression::Zip => {
            let predicted = kinogaki_deflate::zlib_decompress(data, raw_len)
                .map_err(|_| fail("zlib data does not decompress"))?;
            if predicted.len() != raw_len {
                return Err(fail("zlib data has the wrong size"));
            }
            Ok(zip::unpredict(predicted))
        }
        Compression::Piz => piz::decode(shape, data).map_err(fail),
        Compression::Pxr24 => pxr24::decode(shape, data).map_err(fail),
    }
}

/// The chunk `raw` compressed, or `None` when that is not smaller.
pub(crate) fn compress(
    compression: Compression,
    shape: &Shape,
    raw: &[u8],
    level: Level,
) -> Option<Vec<u8>> {
    let packed = match compression {
        Compression::None => return None,
        Compression::Rle => rle::encode(&zip::predict(raw)),
        Compression::Zips | Compression::Zip => {
            kinogaki_deflate::zlib_compress(&zip::predict(raw), level)
        }
        Compression::Piz => piz::encode(shape, raw),
        Compression::Pxr24 => pxr24::encode(shape, raw, level),
    };
    (packed.len() < raw.len()).then_some(packed)
}
