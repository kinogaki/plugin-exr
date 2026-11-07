//! Reading files OpenEXR wrote and writing files OpenEXR reads.
//!
//! `tests/fixtures/openexr` holds files written by OpenEXR itself
//! (`tools/exr_oracle.py fixtures`) from a pattern this test computes too.
//! With `KINOGAKI_EXR_ORACLE_DIR` set, every file this test writes goes
//! there for `tools/exr_oracle.py check`, which reads it with OpenEXR.

use kinogaki_exr_format::exr::{
    Channel, ChannelSamples, Compression, Error, Exr, Header, Part, Tiles, Window, WriteOptions,
    f32_to_f16,
};
use kinogaki_studio_api::format::{ReadBudget, ReadLimits};
use kinogaki_studio_api_testing::workers;
use std::path::{Path, PathBuf};

const CHANNELS: [(&str, char); 6] = [
    ("A", 'h'),
    ("B", 'f'),
    ("G", 'h'),
    ("R", 'h'),
    ("Z", 'f'),
    ("id", 'u'),
];

fn float(x: usize, y: usize, c: usize) -> f32 {
    (((x * 7 + y * 13 + c * 29) % 509) as f64 / 64.0 - 2.0 + ((x * y + c) % 17) as f64 / 1024.0)
        as f32
}

/// The oracle's pattern for channel `c` of a `width` × `height` part.
fn pattern(width: usize, height: usize, c: usize, kind: char) -> ChannelSamples {
    let at = |i: usize| (i % width, i / width);
    let n = width * height;
    match kind {
        'u' => ChannelSamples::U32(
            (0..n)
                .map(|i| {
                    let (x, y) = at(i);
                    ((x as u64 * 2_654_435_761) ^ (y as u64 * 40_503 + c as u64)) as u32
                })
                .collect(),
        ),
        'f' => ChannelSamples::F32((0..n).map(|i| float(at(i).0, at(i).1, c)).collect()),
        _ => ChannelSamples::F16(
            (0..n)
                .map(|i| f32_to_f16(float(at(i).0, at(i).1, c)))
                .collect(),
        ),
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openexr")
}

/// Checks every pattern channel `part` has; PXR24 floats to 24 bits.
fn check_part(part: &Part, lossy: bool, what: &str) {
    let (width, height) = part.size();
    let mut seen = 0;
    for (c, &(name, kind)) in CHANNELS.iter().enumerate() {
        let Some(channel) = part.channel(name) else {
            continue;
        };
        seen += 1;
        let want = pattern(width, height, c, kind);
        match (&channel.samples, &want) {
            (ChannelSamples::F32(got), ChannelSamples::F32(want)) if lossy => {
                for (g, w) in got.iter().zip(want) {
                    assert!(
                        (g - w).abs() <= w.abs() * 2f32.powi(-15),
                        "{what} {name}: {g} vs {w}"
                    );
                }
            }
            _ => assert_eq!(channel.samples, want, "{what} {name}"),
        }
    }
    assert!(seen > 0, "{what}");
}

#[test]
fn files_openexr_wrote_read_exactly() {
    let mut count = 0;
    for entry in std::fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let data = std::fs::read(&path).unwrap();
        let exr = kinogaki_exr_format::exr::read(&data, &ReadBudget::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for part in &exr.parts {
            check_part(part, name.contains("pxr24"), &name);
        }
        let headers = kinogaki_exr_format::exr::read_headers(&data).unwrap();
        assert_eq!(headers.len(), exr.parts.len());
        assert_eq!(headers[0].header, exr.parts[0].header);
        count += 1;
    }
    assert_eq!(count, 12);
    let multipart = kinogaki_exr_format::exr::read(
        &std::fs::read(fixtures().join("multipart.exr")).unwrap(),
        &ReadBudget::default(),
    )
    .unwrap();
    let names: Vec<_> = multipart
        .parts
        .iter()
        .map(|p| p.header.name.clone().unwrap())
        .collect();
    assert_eq!(names, ["beauty", "depth"]);
    assert_eq!(
        multipart.parts[1].header.tiles,
        Some(Tiles {
            width: 8,
            height: 8
        })
    );
    assert_eq!(multipart.parts[1].header.compression, Compression::Piz);
}

fn part(
    width: usize,
    height: usize,
    compression: Compression,
    tiles: Option<Tiles>,
    name: Option<&str>,
) -> Part {
    let mut header = Header::new(width as u32, height as u32, compression);
    header.tiles = tiles;
    header.name = name.map(str::to_owned);
    let channels = CHANNELS
        .iter()
        .enumerate()
        .map(|(c, &(name, kind))| Channel {
            name: name.to_owned(),
            samples: pattern(width, height, c, kind),
            linear: false,
        })
        .collect();
    Part { header, channels }
}

fn oracle_write(name: &str, bytes: &[u8]) {
    if let Ok(directory) = std::env::var("KINOGAKI_EXR_ORACLE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(Path::new(&directory).join(name), bytes).unwrap();
    }
}

const ALL: [Compression; 6] = [
    Compression::None,
    Compression::Rle,
    Compression::Zips,
    Compression::Zip,
    Compression::Piz,
    Compression::Pxr24,
];

#[test]
fn every_compression_round_trips_scanline_and_tiled() {
    for compression in ALL {
        for (width, height) in [(1, 1), (67, 45), (160, 120)] {
            for tiles in [
                None,
                Some(Tiles {
                    width: 16,
                    height: 12,
                }),
                Some(Tiles {
                    width: 64,
                    height: 64,
                }),
            ] {
                let exr = Exr {
                    parts: vec![part(width, height, compression, tiles, None)],
                };
                workers::install(width % 5 + 1);
                let bytes =
                    kinogaki_exr_format::exr::write(&exr, &WriteOptions::default()).unwrap();
                let back = kinogaki_exr_format::exr::read(&bytes, &ReadBudget::default()).unwrap();
                let what = format!("{compression:?} {width}x{height} {tiles:?}");
                check_part(&back.parts[0], compression == Compression::Pxr24, &what);
                assert_eq!(back.parts[0].header, exr.parts[0].header, "{what}");
                let tag = tiles.map_or("scanline".to_owned(), |t| format!("tiled{}", t.width));
                oracle_write(
                    &format!(
                        "{}-{width}x{height}-{tag}.exr",
                        format!("{compression:?}").to_lowercase()
                    ),
                    &bytes,
                );
            }
        }
    }
}

#[test]
fn multipart_files_round_trip() {
    let exr = Exr {
        parts: vec![
            part(40, 30, Compression::Zip, None, Some("beauty")),
            part(
                40,
                30,
                Compression::Piz,
                Some(Tiles {
                    width: 8,
                    height: 8,
                }),
                Some("depth"),
            ),
            part(40, 30, Compression::Rle, None, Some("mask")),
        ],
    };
    let bytes = kinogaki_exr_format::exr::write(&exr, &WriteOptions::default()).unwrap();
    oracle_write("multipart.exr", &bytes);
    let back = kinogaki_exr_format::exr::read(&bytes, &ReadBudget::default()).unwrap();
    assert_eq!(back, exr);
    let mut unnamed = exr.clone();
    unnamed.parts[1].header.name = None;
    assert_eq!(
        kinogaki_exr_format::exr::write(&unnamed, &WriteOptions::default()),
        Err(Error::Invalid(
            "every part of a multi-part file needs a name"
        ))
    );
}

#[test]
fn windows_attributes_and_long_names_survive() {
    let mut p = part(9, 7, Compression::Zip, None, None);
    p.header.data_window = Window {
        x_min: -3,
        y_min: 10,
        x_max: 5,
        y_max: 16,
    };
    p.header.display_window = Window {
        x_min: 0,
        y_min: 0,
        x_max: 99,
        y_max: 99,
    };
    p.header.pixel_aspect_ratio = 2.0;
    p.header.set_string("owner", "kinogaki");
    p.header
        .set_string("a.very.long.attribute.name.past.thirty.one.bytes", "long");
    p.channels.push(Channel {
        name: "layer.with.a.name.longer.than.thirty.one.bytes.R".to_owned(),
        samples: pattern(9, 7, 0, 'h'),
        linear: true,
    });
    p.channels.sort_by(|a, b| a.name.cmp(&b.name));
    let exr = Exr { parts: vec![p] };
    let bytes = kinogaki_exr_format::exr::write(&exr, &WriteOptions::default()).unwrap();
    oracle_write("windows.exr", &bytes);
    let back = kinogaki_exr_format::exr::read(&bytes, &ReadBudget::default()).unwrap();
    assert_eq!(back, exr);
    assert_eq!(back.parts[0].header.string("owner"), Some("kinogaki"));
}

#[test]
fn bad_files_fail_cleanly() {
    let data = std::fs::read(fixtures().join("scanline-zip.exr")).unwrap();
    assert!(matches!(
        kinogaki_exr_format::exr::read(b"not an exr", &ReadBudget::default()),
        Err(Error::Corrupt { .. })
    ));
    let fewer = ReadBudget::new(ReadLimits {
        pixels: 100,
        ..ReadLimits::default()
    });
    let refused = kinogaki_exr_format::exr::read(&data, &fewer);
    assert!(
        matches!(&refused, Err(Error::Budget(why)) if why.contains("over the read's limit of 100")),
        "{refused:?}"
    );
    for name in [
        "scanline-zip.exr",
        "tiled-piz.exr",
        "scanline-pxr24.exr",
        "scanline-rle.exr",
        "multipart.exr",
        "piz-wide.exr",
    ] {
        let data = std::fs::read(fixtures().join(name)).unwrap();
        let step = (data.len() / 200).max(1);
        for cut in (0..data.len() - 1).step_by(step) {
            assert!(
                kinogaki_exr_format::exr::read(&data[..cut], &ReadBudget::default()).is_err(),
                "{name} cut at {cut}"
            );
        }
        let mut seed = 0x9E37_79B9u32;
        for _ in 0..300 {
            let mut mutated = data.clone();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let at = seed as usize % mutated.len();
                mutated[at] ^= 1 << (seed >> 29);
            }
            // A small pixel limit: a mutated window must not allocate gigabytes.
            let budget = ReadBudget::new(ReadLimits {
                pixels: 1 << 22,
                ..ReadLimits::default()
            });
            let _ = kinogaki_exr_format::exr::read(&mutated, &budget);
        }
    }
}
