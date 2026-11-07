//! The format through the SDK's contract: openexr-images against OpenEXR's
//! own reading (`tools/images_oracle.py`), a render's Image written and read
//! back whole, parts joined into one Image, Cryptomatte with a sidecar
//! manifest, and damaged files.

use kinogaki_exr_format::OpenExr;
use kinogaki_exr_format::exr::{
    self, Channel, ChannelSamples, Compression, Exr, Header, Part, Window,
};
use kinogaki_render_api::aov::cryptomatte;
use kinogaki_studio_api::geometry::image::{
    ColorTag, Encoding, Format, Image, Rect, Sample, metadata,
};
use kinogaki_studio_api::geometry::{Attribute, Domain, Meaning, Text};
use kinogaki_studio_api::{
    FileFormat, FileItem, Imported, Input, Purpose, Reading, Units, Value, Writing, standard,
};
use kinogaki_studio_api_testing::Unconverted;
use kinogaki_studio_api_testing::workers;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(input: &Input<'_>) -> Result<Imported, kinogaki_studio_api::Error> {
    let colors = Unconverted;
    let reading = Reading::new(Purpose::Value, Units::default(), &colors);
    OpenExr.read(input, &OpenExr.options(), &reading)
}

fn image(input: &Input<'_>) -> (Image, Vec<String>) {
    let read = read(input).unwrap_or_else(|e| panic!("{input}: {e}"));
    let data = read.items[0].data.as_ref().unwrap();
    let said = read.warnings.iter().map(|w| w.message.clone()).collect();
    (standard::image().get(data).unwrap().clone(), said)
}

/// `image` written with `parts` (`layers` or `one`): the file's bytes.
fn written(image: &Image, parts: &str) -> Vec<u8> {
    let writer = OpenExr.writer().unwrap();
    let mut options = writer.options();
    options.insert("parts".into(), Value::Choice(parts.into()));
    let dir = std::env::temp_dir().join(format!("kinogaki-exr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{parts}-{:?}.exr", std::thread::current().id()));
    let colors = Unconverted;
    let item = FileItem::top("frame", standard::image().wrap(image.clone()));
    let warnings = writer
        .write(
            &path,
            &[item],
            &Writing::new(Units::default(), &colors),
            &options,
        )
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    bytes
}

/// FNV-1a, 64 bits: what the oracle hashes samples with.
fn fnv(bytes: &[u8]) -> String {
    let hash = (bytes.iter()).fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// Channel `name` of `image`'s layer `layer` as little-endian sample bytes.
fn channel(image: &Image, layer: &str, name: &str) -> Vec<u8> {
    let at = image
        .layers()
        .iter()
        .position(|l| &*l.format().name == layer)
        .unwrap();
    let format = image.layers()[at].format();
    let k = format.channels.iter().position(|c| &**c == name).unwrap();
    let size = format.sample.bytes();
    let plane = image.plane(at).unwrap();
    (plane.chunks_exact(format.pixel_bytes()))
        .flat_map(|px| {
            let mut sample = px[k * size..][..size].to_vec();
            if cfg!(target_endian = "big") {
                sample.reverse();
            }
            sample
        })
        .collect()
}

#[test]
fn openexr_images_read_as_openexr_reads_them() {
    let expected = std::fs::read_to_string(fixtures().join("openexr-images/expected.txt")).unwrap();
    let mut current: Option<(String, Image)> = None;
    for line in expected.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let path = fixtures().join("openexr-images").join(fields[0]);
        if fields[1] == "error" {
            let why = read(&Input::file(&path)).unwrap_err();
            assert!(
                why.message().starts_with(&path.display().to_string()),
                "{why}"
            );
            continue;
        }
        if current.as_ref().is_none_or(|(name, _)| name != fields[0]) {
            current = Some((fields[0].to_owned(), image(&Input::file(&path)).0));
        }
        let (_, held) = current.as_ref().unwrap();
        match fields[1] {
            "part" => {
                let n: Vec<i32> = fields[2..].iter().map(|v| v.parse().unwrap()).collect();
                let window = Rect::new([n[0], n[1]], [n[2] + 1, n[3] + 1]).unwrap();
                assert_eq!(held.data(), window, "{}", fields[0]);
            }
            _ => {
                let bytes = channel(held, "", fields[2]);
                assert_eq!(fnv(&bytes), fields[4], "{} {}", fields[0], fields[2]);
            }
        }
    }
    let (wide, _) = image(&Input::file(
        &fixtures().join("openexr-images/WideColorGamut.exr"),
    ));
    let tag = &wide.layers()[0].format().encoding;
    assert!(
        matches!(tag, Encoding::Color { tag: ColorTag::Primaries(xy), premultiplied: true } if xy[0] == 0.64)
    );
    let (window, _) = image(&Input::file(&fixtures().join("openexr-images/t08.exr")));
    assert_eq!(window.display(), Rect::new([0, 0], [501, 401]).unwrap());
}

/// A final frame's Image as a host tags it for a file: a beauty in ACEScg
/// with its chromaticities, an albedo, depth, occlusion and object ids as
/// data, and a
/// Cryptomatte output of two rank sets with its manifest.
fn frame() -> Image {
    let window = Rect::new([0, 0], [3, 2]).unwrap();
    let pixels = 6;
    let format = |name: &str, channels: &[&str], sample: Sample, color: bool| Format {
        name: name.into(),
        channels: channels.iter().map(|&c| Arc::from(c)).collect(),
        sample,
        encoding: match color {
            true => Encoding::Color {
                tag: ColorTag::Space("ACEScg".into()),
                premultiplied: true,
            },
            false => Encoding::Raw,
        },
    };
    let floats = |count: usize, seed: f32| -> Vec<u8> {
        (0..count)
            .flat_map(|i| (seed + i as f32 * 0.25).to_ne_bytes())
            .collect()
    };
    let halves = |count: usize| -> Vec<u8> {
        (0..count)
            .flat_map(|i| {
                kinogaki_studio_api::geometry::image::f32_to_half(i as f32 / 8.0).to_ne_bytes()
            })
            .collect()
    };
    let ids: Vec<u8> = (0..pixels as u32)
        .flat_map(|i| (i * 7 + 1).to_ne_bytes())
        .collect();
    let xy = Attribute::tuples(
        vec![[
            0.713f32, 0.293, 0.165, 0.830, 0.128, 0.044, 0.32168, 0.33767,
        ]],
        Meaning::None,
    )
    .unwrap();
    let mut attributes = kinogaki_studio_api::geometry::Attributes::new()
        .with(Domain::Detail, metadata::CHROMATICITIES, xy)
        .unwrap();
    for (key, value) in [
        ("name", "CryptoObject"),
        ("hash", "MurmurHash3_32"),
        ("conversion", "uint32_to_float32"),
        ("manifest", r#"{"/hero":"0b1e7ad4"}"#),
    ] {
        let text = Attribute::text(Text::from_strs([value]));
        attributes = attributes
            .with(
                Domain::Detail,
                &cryptomatte::metadata("CryptoObject", key),
                text,
            )
            .unwrap();
    }
    let note = Attribute::text(Text::from_strs(["kept"]));
    attributes = attributes.with(Domain::Detail, "owner", note).unwrap();
    Image::builder(window)
        .attributes(attributes)
        .plane(
            format("", &["R", "G", "B", "A"], Sample::F32, true),
            &floats(pixels * 4, 0.5),
        )
        .plane(
            format("albedo", &["R", "G", "B"], Sample::F16, true),
            &halves(pixels * 3),
        )
        .plane(
            format("depth", &["Z"], Sample::F32, false),
            &floats(pixels, 4.0),
        )
        .plane(
            format("occlusion", &["Y"], Sample::F32, false),
            &floats(pixels, 0.25),
        )
        .plane(format("objectId", &["id"], Sample::U32, false), &ids)
        .plane(
            format("CryptoObject00", &["R", "G", "B", "A"], Sample::F32, false),
            &floats(pixels * 4, 1.0),
        )
        .plane(
            format("CryptoObject01", &["R", "G", "B", "A"], Sample::F32, false),
            &floats(pixels * 4, 2.0),
        )
        .build()
        .unwrap()
}

#[test]
fn a_frame_writes_a_part_an_output_and_reads_back_as_itself() {
    let frame = frame();
    let bytes = written(&frame, "layers");
    let file = exr::read(&bytes, &Default::default()).unwrap();
    let names: Vec<_> = file
        .parts
        .iter()
        .map(|p| p.header.name.clone().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "beauty",
            "albedo",
            "depth",
            "occlusion",
            "objectId",
            "CryptoObject"
        ]
    );
    assert_eq!(file.parts[0].header.string("colorspace"), Some("ACEScg"));
    assert!(file.parts[0].header.attribute("chromaticities").is_some());
    assert_eq!(file.parts[0].header.string("owner"), Some("kept"));
    // Data parts say no space: read back, a lone `Y` stays data.
    assert_eq!(file.parts[2].header.string("colorspace"), None);
    assert_eq!(file.parts[3].header.string("colorspace"), None);
    let key = cryptomatte::metadata("CryptoObject", "manifest");
    assert_eq!(
        file.parts[5].header.string(&key),
        Some(r#"{"/hero":"0b1e7ad4"}"#)
    );
    let (back, warnings) = image(&Input::memory("frame.exr", &bytes));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.layers(), frame.layers());
    for name in [metadata::CHROMATICITIES, "owner", key.as_str()] {
        assert_eq!(
            back.attributes().get(Domain::Detail, name),
            frame.attributes().get(Domain::Detail, name),
            "{name}"
        );
    }
}

#[test]
fn one_part_of_named_layers_reads_back_alike() {
    let frame = frame();
    let bytes = written(&frame, "one");
    let file = exr::read(&bytes, &Default::default()).unwrap();
    assert_eq!(file.parts.len(), 1);
    assert!(file.parts[0].channels.iter().any(|c| c.name == "albedo.G"));
    let (back, _) = image(&Input::memory("one.exr", &bytes));
    assert_eq!(back.layers().len(), frame.layers().len());
    for layer in frame.layers() {
        let found = back
            .layers()
            .iter()
            .find(|l| l.format().name == layer.format().name)
            .unwrap();
        assert_eq!(
            found.format().sample,
            layer.format().sample,
            "{:?}",
            layer.format().name
        );
        assert_eq!(found.tiles(), layer.tiles(), "{:?}", layer.format().name);
    }
}

#[test]
fn parts_of_other_windows_join_and_the_workers_never_change_a_read() {
    let part = |name: &str, window: Window, value: f32| {
        let mut header = Header::new(1, 1, Compression::Zip);
        header.data_window = window;
        header.display_window = Window::sized(8, 8);
        header.name = Some(name.into());
        let count = window.width() * window.height();
        let channel = Channel {
            name: "Y".into(),
            samples: ChannelSamples::F32(vec![value; count]),
            linear: false,
        };
        Part {
            header,
            channels: vec![channel],
        }
    };
    let file = Exr {
        parts: vec![
            part(
                "left",
                Window {
                    x_min: 0,
                    y_min: 0,
                    x_max: 3,
                    y_max: 7,
                },
                1.0,
            ),
            part(
                "right",
                Window {
                    x_min: 6,
                    y_min: 2,
                    x_max: 199,
                    y_max: 129,
                },
                2.0,
            ),
        ],
    };
    let bytes = exr::write(&file, &Default::default()).unwrap();
    workers::install(1);
    let (one, _) = image(&Input::memory("parts.exr", &bytes));
    workers::install(8);
    let (many, _) = image(&Input::memory("parts.exr", &bytes));
    assert_eq!(one, many);
    assert_eq!(one.data(), Rect::new([0, 0], [200, 130]).unwrap());
    let names: Vec<&str> = one.layers().iter().map(|l| &*l.format().name).collect();
    assert_eq!(names, ["left", "right"]);
    assert_eq!(one.read_pixel(0, 3, 7)[0], 1.0);
    assert_eq!(one.read_pixel(0, 4, 0)[0], 0.0);
    assert_eq!(one.read_pixel(1, 199, 129)[0], 2.0);
    assert_eq!(one.read_pixel(1, 5, 2)[0], 0.0);
}

#[test]
fn a_cryptomatte_sidecar_is_read_beside_its_file_and_said_missing_in_memory() {
    let path = fixtures().join("cryptomatte/sidecar_manifest.exr");
    let (beside, _) = image(&Input::file(&path));
    let key = cryptomatte::metadata("crypto_asset", "manifest");
    let manifest = beside
        .attributes()
        .text(Domain::Detail, &key)
        .and_then(|t| t.get(0).map(str::to_owned));
    assert!(manifest.is_some_and(|m| m.starts_with('{')), "{key}");
    let ranks = beside
        .layers()
        .iter()
        .find(|l| &*l.format().name == "crypto_asset00")
        .unwrap();
    assert_eq!(ranks.format().encoding, Encoding::Raw);
    let names: Vec<&str> = ranks.format().channels.iter().map(|c| &**c).collect();
    assert_eq!(names, ["red", "green", "blue", "alpha"]);
    let bytes = std::fs::read(&path).unwrap();
    let (held, warnings) = image(&Input::memory("sidecar_manifest.exr", &bytes));
    assert!(held.attributes().text(Domain::Detail, &key).is_none());
    assert!(
        warnings.iter().any(|w| w.contains("held in memory")),
        "{warnings:?}"
    );
}

#[test]
fn psyops_cryptomatte_files_write_back_as_they_read() {
    let outputs = [
        ("multichannel", "crypto_object"),
        ("special_chars", "special_chars"),
        ("sidecar_manifest", "crypto_asset"),
    ];
    for (name, output) in outputs {
        let path = fixtures().join(format!("cryptomatte/{name}.exr"));
        let (read, warnings) = image(&Input::file(&path));
        // Every attribute held, its name spaces and all.
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
        for parts in ["layers", "one"] {
            let bytes = written(&read, parts);
            let (back, warnings) = image(&Input::memory("back.exr", &bytes));
            assert!(warnings.is_empty(), "{name}, {parts}: {warnings:?}");
            // The same layers; an EXR orders its channels by name.
            let sorted = |image: &Image| {
                let mut layers = image.layers().to_vec();
                layers.sort_by(|a, b| a.format().name.cmp(&b.format().name));
                layers
            };
            assert_eq!(sorted(&back), sorted(&read), "{name}, {parts}");
            for (domain, attribute, value) in read.attributes().iter() {
                let held = back.attributes().get(domain, attribute);
                // A one-part file's header names no part; written as
                // parts, it does. A sidecar's manifest is written in the
                // file.
                let renamed = parts == "layers" && attribute == "name";
                let inline = attribute.ends_with("/manif_file");
                let same = renamed || inline || held == Some(value);
                assert!(same, "{name}, {parts}: {attribute}");
            }
        }
        // The preview and the ranks of an output are one part.
        let file = exr::read(&written(&read, "layers"), &Default::default()).unwrap();
        let part = (file.parts.iter())
            .find(|p| p.header.name.as_deref() == Some(output))
            .unwrap();
        let channels: Vec<&str> = part.channels.iter().map(|c| &*c.name).collect();
        for layer in [format!("{output}."), format!("{output}00.")] {
            assert!(
                channels.iter().any(|c| c.starts_with(&layer)),
                "{channels:?}"
            );
        }
        let primary = "ColorSpace/Blue Primary";
        assert!(file.parts[0].header.attribute(primary).is_some(), "{name}");
    }
}

#[test]
fn a_damaged_or_truncated_file_is_an_error_naming_it() {
    let bytes = written(&frame(), "layers");
    for cut in [0, 4, 100, bytes.len() / 2, bytes.len() - 1] {
        let why = read(&Input::memory("cut.exr", &bytes[..cut])).unwrap_err();
        assert!(why.message().starts_with("cut.exr: "), "{why}");
    }
    assert!(OpenExr.detects(&bytes[..4]) && !OpenExr.detects(b"\x89PNG"));
}
