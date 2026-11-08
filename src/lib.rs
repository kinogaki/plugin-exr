//! OpenEXR, a format plugin: an EXR file reads as one standard
//! [`Image`](kinogaki_studio_api::geometry::image::Image), every part's
//! channels its layers, and an Image writes as one EXR, Cryptomatte
//! included. The codec is [`exr`], which knows no SDK but the process's
//! workers it decompresses and compresses on.
//!
//! - **Reading.** A channel `<layer>.<name>` is `<layer>`'s; one named
//!   alone is its part's: the main layer `""` in a file of one part, else
//!   the layer of the output its part is named after (`beauty` is `""`).
//!   Data windows join; half, float and integer channels stay as stored.
//!   A layer is color when its part says its space (`colorspace`, else
//!   `chromaticities`, its tag), or, in a file saying none, when its
//!   channels are `R G B A` or `Y`; Cryptomatte ranks and integers are
//!   data. Every other attribute is the Image's metadata (a later part's
//!   differing one under `<part>/<name>`, a name an attribute's can't be
//!   escaped), and a Cryptomatte sidecar manifest is read in as its
//!   `manifest`.
//! - **Writing.** A part a layer (option `parts`, `layers`) named by its
//!   output, a Cryptomatte output's preview and ranks one part with its
//!   four keys, or one part of `<layer>.<name>` channels (`one`); every
//!   part compressed as `compression` says (ZIP by default). A color
//!   layer's part carries its tag (`colorspace` and the space's
//!   `chromaticities` as the host's color service gives them, or the
//!   primaries alone), the first part the rest of the metadata, each
//!   attribute under the name it was read by.
pub mod exr;
mod metadata;
mod read;
mod tiles;
mod write;

use kinogaki_studio_api::{
    DataTypeId, Error, FileFormat, FormatId, FormatOptions, FormatWriter, Imported, Input, Package,
    PackageId, Reading, standard,
};

/// The package's id.
pub fn id() -> PackageId {
    PackageId::declared("kinogaki.exr")
}

/// The format's id.
pub fn format() -> FormatId {
    FormatId::declared("kinogaki.format.exr")
}

/// The package: the format alone.
pub fn package() -> Package {
    Package::new(id(), "OpenEXR").with_format(OpenExr)
}

/// What it reads and writes: the standard Image.
const IMAGE: &[DataTypeId] = &[standard::image().id()];

/// The format.
pub struct OpenExr;

impl FileFormat for OpenExr {
    fn id(&self) -> FormatId {
        format()
    }
    fn label(&self) -> &str {
        "OpenEXR"
    }
    fn extensions(&self) -> &[&str] {
        &["exr"]
    }
    fn detects(&self, head: &[u8]) -> bool {
        head.starts_with(&[0x76, 0x2F, 0x31, 0x01])
    }
    fn produces(&self) -> &[DataTypeId] {
        IMAGE
    }
    fn read(
        &self,
        input: &Input<'_>,
        _: &FormatOptions,
        reading: &Reading<'_>,
    ) -> Result<Imported, Error> {
        read::read(input, reading)
    }
    fn writer(&self) -> Option<&dyn FormatWriter> {
        Some(&write::Writer)
    }
}
