//! One standard Image to an OpenEXR file ([`Writer`]).
use crate::IMAGE;
use crate::exr::{
    self, Attribute, Channel, ChannelSamples, Compression, Exr, Header, Part, Window,
};
use crate::{metadata, tiles};
use kinogaki_render_api::aov::{self, cryptomatte};
use kinogaki_studio_api::color::ColorService;
use kinogaki_studio_api::format::options;
use kinogaki_studio_api::geometry::Domain;
use kinogaki_studio_api::geometry::image::{ColorTag, Encoding, Image, Rect};
use kinogaki_studio_api::params::{Parameter, Schema};
use kinogaki_studio_api::{
    DataTypeId, Diagnostic, Error, FileItem, FormatOptions, FormatWriter, Value, Writing, standard,
};
use std::path::Path;

/// The option naming how chunks are compressed.
const COMPRESSION: &str = "compression";
/// The option naming how layers are laid out in parts: `layers` (a part
/// each) or `one` (one part of named layers).
const PARTS: &str = "parts";
/// The compressions, by key, label and codec's.
const COMPRESSIONS: [(&str, &str, Compression); 6] = [
    ("none", "None", Compression::None),
    ("rle", "RLE", Compression::Rle),
    ("zips", "ZIPS", Compression::Zips),
    ("zip", "ZIP", Compression::Zip),
    ("piz", "PIZ", Compression::Piz),
    ("pxr24", "PXR24 (lossy floats)", Compression::Pxr24),
];
/// A Cryptomatte output's keys.
const KEYS: [&str; 4] = ["name", "hash", "conversion", "manifest"];

/// The writer.
pub(crate) struct Writer;

impl FormatWriter for Writer {
    fn accepts(&self) -> &[DataTypeId] {
        IMAGE
    }
    fn options(&self) -> FormatOptions {
        FormatOptions::from([
            (COMPRESSION.to_owned(), Value::Choice("zip".into())),
            (PARTS.to_owned(), Value::Choice("layers".into())),
        ])
    }
    fn schema(&self, chosen: &FormatOptions) -> Schema {
        let key = |name| options::option(chosen, name, Value::choice).unwrap_or_default();
        let compressions: Vec<(&str, &str)> =
            COMPRESSIONS.iter().map(|(k, l, _)| (*k, *l)).collect();
        let parts = [("layers", "A Part a Layer"), ("one", "One Part")];
        Schema::default()
            .parameter(Parameter::choice(
                COMPRESSION,
                "Compression",
                &compressions,
                key(COMPRESSION),
            ))
            .parameter(Parameter::choice(PARTS, "Parts", &parts, key(PARTS)))
    }
    /// The first item's Image (see the crate docs); what else the items
    /// hold is left out with a warning.
    fn write(
        &self,
        path: &Path,
        items: &[FileItem],
        writing: &Writing<'_>,
        chosen: &FormatOptions,
    ) -> Result<Vec<Diagnostic>, Error> {
        writing.check()?;
        let named = options::option(chosen, COMPRESSION, Value::choice)?;
        let compression = (COMPRESSIONS.iter().find(|(k, ..)| *k == named))
            .map_or(Compression::Zip, |(.., c)| *c);
        let one = options::option(chosen, PARTS, Value::choice)? == "one";
        let mut images =
            (items.iter().filter_map(|i| i.data.as_ref())).filter_map(|d| standard::image().get(d));
        let image = images
            .next()
            .ok_or_else(|| Error::invalid("no image to write"))?;
        let mut warnings = Vec::new();
        let more = images.count();
        if more > 0 {
            warnings.push(Diagnostic::warning(format!(
                "an EXR holds one image: {more} more left out"
            )));
        }
        let at = |e: exr::Error| Error::invalid(format!("{}: {e}", path.display()));
        let mut file =
            file(image, (compression, one), writing.colors, &mut warnings).map_err(at)?;
        metadata::write(image.attributes(), &mut file.parts[0].header, &mut warnings);
        writing.check()?;
        let bytes = exr::write(&file, &Default::default()).map_err(at)?;
        writing.check()?;
        let all = |file: &mut std::fs::File| std::io::Write::write_all(file, &bytes);
        (kinogaki_studio_api::format::files::replace(path, all))
            .map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
        Ok(warnings)
    }
}

/// `image` as an EXR's parts: a part a layer (a Cryptomatte output's
/// preview and ranks one part), or `one` part of them all, each
/// `compression`; a named space's primaries as `colors` gives them.
fn file(
    image: &Image,
    (compression, one): (Compression, bool),
    colors: &dyn ColorService,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Exr, exr::Error> {
    let header = || {
        let mut header = Header::new(1, 1, compression);
        header.data_window = window(image.data());
        header.display_window = window(image.display());
        header.pixel_aspect_ratio = image.aspect() as f32;
        header
    };
    let mut parts: Vec<Part> = Vec::new();
    // The Cryptomatte output whose part is being filled.
    let mut crypto: Option<&str> = None;
    for (at, layer) in image.layers().iter().enumerate() {
        let name = &*layer.format().name;
        let output = cryptomatte_of(image, name);
        let mut channels = channels(image, at)?;
        if (one || output.is_some()) && !name.is_empty() {
            for channel in &mut channels {
                channel.name = format!("{name}.{}", channel.name);
            }
        }
        let starts = match one {
            true => parts.is_empty(),
            false => output.is_none() || output != crypto,
        };
        if starts {
            let mut fresh = header();
            fresh.name = (!one).then(|| output.unwrap_or(aov::output(name)).to_owned());
            parts.push(Part {
                header: fresh,
                channels: Vec::new(),
            });
        }
        let part = parts.last_mut().expect("pushed above");
        match output {
            Some(output) => {
                for key in KEYS {
                    let key = cryptomatte::metadata(output, key);
                    part.header
                        .set_string(&key, text(image, &key).unwrap_or_default());
                }
            }
            None if starts || name.is_empty() => {
                tag(layer.format(), colors, &mut part.header, warnings)
            }
            None => {}
        }
        part.channels.extend(channels);
        crypto = output;
    }
    if parts.is_empty() {
        return Err(exr::Error::Invalid("an image of no layers"));
    }
    for part in &mut parts {
        part.channels.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(Exr { parts })
}

/// The Cryptomatte output layer `name` is one of, when the image's metadata
/// names that output: its preview (named as the output), or a rank (its
/// name less two digits).
fn cryptomatte_of<'a>(image: &Image, name: &'a str) -> Option<&'a str> {
    let named = |output| text(image, &cryptomatte::metadata(output, "name")).map(|_| output);
    let rank = (name.strip_suffix(|c: char| c.is_ascii_digit()))
        .and_then(|n| n.strip_suffix(|c: char| c.is_ascii_digit()));
    named(name).or_else(|| named(rank?))
}

/// The image's metadata text `name`.
fn text<'a>(image: &'a Image, name: &str) -> Option<&'a str> {
    image.attributes().text(Domain::Detail, name)?.get(0)
}

/// `header` tagged as a color layer of `format` says: `colorspace` its
/// space and `chromaticities` the space's primaries (as `colors` gives
/// them), or `chromaticities` its primaries alone; a profile an EXR can't
/// hold is said.
fn tag(
    format: &kinogaki_studio_api::geometry::image::Format,
    colors: &dyn ColorService,
    header: &mut Header,
    warnings: &mut Vec<Diagnostic>,
) {
    let Encoding::Color { tag, .. } = &format.encoding else {
        return;
    };
    let mut primaries = None;
    match tag {
        ColorTag::Space(space) => {
            header.set_string("colorspace", space);
            primaries = colors.primaries(space);
        }
        ColorTag::Primaries(xy) => primaries = Some(*xy),
        ColorTag::Icc(_) => warnings.push(Diagnostic::warning(
            "an EXR holds no ICC profile: its colors are written untagged",
        )),
        _ => {}
    }
    if let Some(xy) = primaries {
        header.set_attribute(Attribute {
            name: "chromaticities".to_owned(),
            kind: "chromaticities".to_owned(),
            value: xy.iter().flat_map(|v| v.to_le_bytes()).collect(),
        });
    }
}

/// Layer `at`'s channels, a plane each, as it holds them (8 and 16-bit
/// samples as floats).
fn channels(image: &Image, at: usize) -> Result<Vec<Channel>, exr::Error> {
    let format = image.layers()[at].format();
    let planes = tiles::planes(image, at).ok_or(exr::Error::Invalid(
        "a layer whose pixels the host holds isn't written; read it back first",
    ))?;
    Ok((format.channels.iter().zip(planes))
        .map(|(name, samples): (_, ChannelSamples)| Channel {
            name: name.to_string(),
            samples,
            linear: false,
        })
        .collect())
}

/// `rect` as an inclusive window.
fn window(rect: Rect) -> Window {
    let ([x0, y0], [x1, y1]) = (rect.min(), rect.max());
    Window {
        x_min: x0,
        y_min: y0,
        x_max: x1 - 1,
        y_max: y1 - 1,
    }
}
