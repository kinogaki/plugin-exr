//! An OpenEXR file to one standard Image ([`read`]): every part's channels
//! as layers over the parts' data windows joined (D4 of Studio's design).
use crate::exr::{self, ChannelSamples, Exr, Part, ReadOptions, Window};
use crate::metadata;
use crate::tiles::{Plane, tiles};
use kinogaki_render_api::aov::{self, cryptomatte};
use kinogaki_studio_api::geometry::image::{ColorTag, Encoding, Format, Image, Rect, Sample};
use kinogaki_studio_api::{Diagnostic, Error, FileItem, Imported, Input, Reading, standard};
use std::path::Path;
use std::sync::Arc;

/// The most pixels a part may have (2²⁸).
const MAX_PIXELS: u64 = 1 << 28;

/// `input` as one item holding its Image, named after the file.
pub(crate) fn read(input: &Input<'_>, reading: &Reading<'_>) -> Result<Imported, Error> {
    reading.check()?;
    let bytes = input.bytes()?;
    let at = |e: exr::Error| Error::invalid(format!("{input}: {e}"));
    let options = ReadOptions {
        max_pixels: MAX_PIXELS,
    };
    let file = exr::read(&bytes, &options).map_err(at)?;
    drop(bytes);
    reading.check()?;
    let mut warnings = Vec::new();
    let beside = input.path().and_then(Path::parent);
    let image = image(&file, beside, &mut warnings)
        .map_err(|e| Error::invalid(format!("{input}: {}", e.message())))?;
    let name = (Path::new(input.name()).file_stem())
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    Ok(Imported {
        items: vec![FileItem::top(name, standard::image().wrap(image))],
        warnings,
    })
}

/// One channel of a layer: its name in the layer, and where its samples are.
struct Source<'a> {
    name: String,
    samples: &'a ChannelSamples,
    window: Window,
}

/// A layer being gathered: its name, channels and the part they came from.
struct Gathered<'a> {
    name: String,
    channels: Vec<Source<'a>>,
    part: usize,
}

/// `file` as one Image; a Cryptomatte sidecar manifest is read from
/// `beside` (the file's folder) when there is one.
fn image(
    file: &Exr,
    beside: Option<&Path>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Image, Error> {
    let first = file
        .parts
        .first()
        .ok_or_else(|| Error::invalid("no image"))?;
    let display = rect(first.header.display_window)?;
    let windows = file.parts.iter().map(|p| p.header.data_window);
    let joined = windows
        .filter(|w| w.width() > 0 && w.height() > 0)
        .reduce(|a, b| Window {
            x_min: a.x_min.min(b.x_min),
            y_min: a.y_min.min(b.y_min),
            x_max: a.x_max.max(b.x_max),
            y_max: a.y_max.max(b.y_max),
        })
        .unwrap_or(first.header.data_window);
    let data = rect(joined)?;
    let attributes = metadata::read(file, beside, warnings)?;
    let tagged = file.parts.iter().any(|p| tag(p).is_some());
    let mut builder = Image::builder(display)
        .data(data)
        .aspect(f64::from(first.header.pixel_aspect_ratio));
    for layer in gather(file, warnings) {
        let part = &file.parts[layer.part];
        let sample = sample(&layer, warnings);
        let names: Vec<&str> = layer.channels.iter().map(|c| c.name.as_str()).collect();
        let crypto = is_cryptomatte(&attributes, &layer.name);
        let color = !crypto
            && sample != Sample::U32
            && match tagged {
                true => tag(part).is_some(),
                false => names.iter().all(|n| colored(n) || *n == "Y"),
            };
        let encoding = match color {
            true => Encoding::Color {
                tag: tag(part).unwrap_or(ColorTag::Unknown { linear: true }),
                premultiplied: true,
            },
            false => Encoding::Raw,
        };
        let format = Format {
            name: layer.name.as_str().into(),
            channels: names.iter().map(|&n| Arc::from(n)).collect(),
            sample,
            encoding,
        };
        let planes: Vec<Plane<'_>> = (layer.channels.iter())
            .map(|c| Plane {
                samples: c.samples,
                window: c.window,
            })
            .collect();
        builder = builder.tiles(format.clone(), tiles(&format, joined, &planes));
    }
    builder.attributes(attributes).build()
}

/// What a part's colors are, as its header says: its `colorspace`, else
/// its `chromaticities`; none when it says neither.
fn tag(part: &Part) -> Option<ColorTag> {
    if let Some(space) = part.header.string("colorspace") {
        return Some(ColorTag::Space(space.into()));
    }
    let xy = part.header.attribute("chromaticities")?;
    let values: Vec<f32> = (xy.value.as_chunks::<4>().0.iter())
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Some(ColorTag::Primaries(values.try_into().ok()?))
}

/// Whether layer `name` is a Cryptomatte rank set (`CryptoObject00`): its
/// output's name is in the metadata.
fn is_cryptomatte(attributes: &kinogaki_studio_api::geometry::Attributes, name: &str) -> bool {
    let output = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = name.len() - output.len();
    let key = cryptomatte::metadata(output, "name");
    digits == 2
        && attributes
            .text(kinogaki_studio_api::geometry::Domain::Detail, &key)
            .is_some()
}

/// The layers of `file`: a channel `<layer>.<name>` is `<layer>`'s; one
/// named alone is its part's layer, the main one (`""`) in a file of one
/// part, else the layer its part's name is the output of (`beauty`'s is
/// `""`). A layer holds at most four channels: past the colors, a main
/// layer's `Z` is `depth` and another channel a layer of its own. Within a
/// layer, colors come first (`R G B`, then `X Y Z`, the rest by name, `A`
/// last).
fn gather<'a>(file: &'a Exr, warnings: &mut Vec<Diagnostic>) -> Vec<Gathered<'a>> {
    let mut layers: Vec<Gathered<'a>> = Vec::new();
    let alone = file.parts.len() == 1;
    for (at, part) in file.parts.iter().enumerate() {
        let own = match alone {
            true => "",
            false => aov::layer(part.header.name.as_deref().unwrap_or("")),
        };
        for channel in &part.channels {
            let (layer, name) = match channel.name.rsplit_once('.') {
                Some((layer, name)) => (layer, name),
                None => (own, channel.name.as_str()),
            };
            let source = Source {
                name: name.to_owned(),
                samples: &channel.samples,
                window: part.header.data_window,
            };
            match layers.iter_mut().find(|l| l.name == layer) {
                Some(found) if found.channels.iter().any(|c| c.name == name) => {
                    warnings.push(Diagnostic::warning(format!(
                        "channel {} is in two parts: the first is read",
                        channel.name
                    )));
                }
                Some(found) => found.channels.push(source),
                None => layers.push(Gathered {
                    name: layer.to_owned(),
                    channels: vec![source],
                    part: at,
                }),
            }
        }
    }
    let mut split = Vec::with_capacity(layers.len());
    for mut layer in layers {
        layer
            .channels
            .sort_by(|a, b| order(&a.name).cmp(&order(&b.name)));
        let extra = match layer.channels.len() > 4 {
            true => {
                let colors = |c: &Source| colored(&c.name);
                let (kept, extra): (Vec<_>, Vec<_>) = layer.channels.into_iter().partition(colors);
                layer.channels = kept;
                extra
            }
            false => Vec::new(),
        };
        let (name, part) = (layer.name.clone(), layer.part);
        split.push(layer);
        for channel in extra {
            let alone = match (name.as_str(), channel.name.as_str()) {
                ("", "Z") => "depth".to_owned(),
                ("", other) => other.to_owned(),
                (layer, other) => format!("{layer}.{other}"),
            };
            split.push(Gathered {
                name: alone,
                channels: vec![channel],
                part,
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    split.retain(|l| {
        let fresh = !l.channels.is_empty() && seen.insert(l.name.clone());
        if !fresh {
            warnings.push(Diagnostic::warning(format!(
                "layer {:?} is named twice: the first is read",
                l.name
            )));
        }
        fresh
    });
    split
}

/// Where channel `name` sorts in its layer (Cryptomatte spells red as
/// `R`, `r` or `red`).
fn order(name: &str) -> (u8, &str) {
    let rank = match name {
        "R" | "r" | "red" => 0,
        "G" | "g" | "green" => 1,
        "B" | "b" | "blue" => 2,
        "X" => 3,
        "Y" => 4,
        "Z" => 5,
        "A" | "a" | "alpha" => 9,
        _ => 8,
    };
    (rank, name)
}

/// Whether channel `name` is a color's or its alpha.
fn colored(name: &str) -> bool {
    matches!(order(name).0, 0..=2 | 9)
}

/// The one sample `layer` holds: its channels', floats widened when they
/// mix (said when integers are among them).
fn sample(layer: &Gathered<'_>, warnings: &mut Vec<Diagnostic>) -> Sample {
    let of = |c: &Source<'_>| match c.samples {
        ChannelSamples::U32(_) => Sample::U32,
        ChannelSamples::F16(_) => Sample::F16,
        ChannelSamples::F32(_) => Sample::F32,
    };
    let first = of(&layer.channels[0]);
    if layer.channels.iter().all(|c| of(c) == first) {
        return first;
    }
    if layer.channels.iter().any(|c| of(c) == Sample::U32) {
        warnings.push(Diagnostic::warning(format!(
            "layer {:?} mixes integer and float channels: read as floats",
            layer.name
        )));
    }
    Sample::F32
}

/// `window` (inclusive) as an Image's window.
fn rect(window: Window) -> Result<Rect, Error> {
    let end = |v: i32| {
        v.checked_add(1)
            .ok_or_else(|| Error::invalid("a window too large"))
    };
    Rect::new(
        [window.x_min, window.y_min],
        [end(window.x_max)?, end(window.y_max)?],
    )
}
