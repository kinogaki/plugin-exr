//! Cryptomatte: the builder's ranking and coverage, round trips through
//! files and sidecars, and Psyop's fixtures against OpenEXR's reading of
//! them (`tests/fixtures/cryptomatte/expected.txt`, written by
//! `tools/cryptomatte_oracle.py expected`). With
//! `KINOGAKI_CRYPTOMATTE_ORACLE_DIR` set, mattes are also written there for
//! `tools/cryptomatte_oracle.py check`, which compares every pixel.
use kinogaki_exr_format::exr::cryptomatte::{Builder, Cryptomatte, Sidecar, layer_names};
use kinogaki_exr_format::exr::{Compression, Error, ReadOptions};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cryptomatte")
}

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir()
        .join(format!("kinogaki-exr-cryptomatte-{}", std::process::id()))
        .join(name);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// A small layer: two samples at pixel 0, a background-only
/// pixel 1, eight equal surfaces at pixel 2.
fn builder() -> Builder {
    let mut builder = Builder::new(3, 1, "CryptoObject", 3).unwrap();
    builder
        .add_sample(0, 0, &["torus", "равнина"], &[0.25, 0.5], 1.0)
        .unwrap();
    builder.add_sample(0, 0, &["torus"], &[1.0], 3.0).unwrap();
    builder.add_sample(1, 0, &[], &[], 1.0).unwrap();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    builder.add_sample(2, 0, &names, &[0.125; 8], 1.0).unwrap();
    builder
}

#[test]
fn a_built_layer_ranks_picks_and_extracts_across_ranks() {
    let layer = builder().finish().unwrap();
    assert_eq!(layer.rank_count(), 6);
    assert_eq!(layer.manifest().len(), 10);
    assert_eq!(layer.pick_name(0, 0, 0).unwrap(), Some("torus"));
    assert_eq!(
        layer.extract(&["torus", "torus"], false).unwrap().get(0, 0),
        0.8125
    );
    assert_eq!(layer.extract(&["torus"], true).unwrap().get(0, 0), 0.1875);
    let background = layer.extract_ids(&[0.0], false).unwrap();
    assert_eq!((background.get(0, 0), background.get(1, 0)), (0.0625, 1.0));
    assert_eq!(
        layer
            .extract(&["torus", "равнина"], false)
            .unwrap()
            .get(0, 0),
        0.9375
    );
    // Six of eight surfaces fit the ranks; they are not renormalized.
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    assert_eq!(layer.extract(&names, false).unwrap().get(2, 0), 0.75);
    assert_eq!(
        layer.pick(9, 0, 0),
        Err(Error::Invalid("pixel outside the data window"))
    );
    assert!(layer.extract_ids(&[f32::NAN], false).is_err());
}

#[test]
fn bad_samples_are_refused() {
    let mut builder = Builder::new(2, 2, "CryptoObject", 1).unwrap();
    assert!(builder.add_sample(2, 0, &[], &[], 1.0).is_err());
    assert!(builder.add_sample(0, 0, &["a"], &[], 1.0).is_err());
    assert!(builder.add_sample(0, 0, &["a"], &[1.5], 1.0).is_err());
    assert!(
        builder
            .add_sample(0, 0, &["a", "b"], &[0.75, 0.5], 1.0)
            .is_err()
    );
    assert!(builder.add_sample(0, 0, &["a"], &[0.5], -1.0).is_err());
    assert!(builder.add_sample(0, 0, &["a"], &[0.5], f64::NAN).is_err());
    assert!(Builder::new(0, 1, "x", 1).is_err());
    assert!(Builder::new(1, 1, "", 1).is_err());
    assert!(Builder::new(1, 1, "x", 0).is_err());
}

#[test]
fn files_and_sidecars_round_trip() {
    let directory = scratch("roundtrip");
    let builder = builder();
    let path = directory.join("cryptomatte.exr");
    builder.save(&path, Compression::Zip, (0, 0), None).unwrap();
    let read = Cryptomatte::open(&path, "CryptoObject", true).unwrap();
    assert_eq!(read, builder.finish().unwrap());
    let data = std::fs::read(&path).unwrap();
    let headers = kinogaki_exr_format::exr::read_headers(&data).unwrap();
    assert_eq!(layer_names(&headers[0].header), ["CryptoObject"]);

    let sidecar = directory.join("sidecar.exr");
    builder
        .save(&sidecar, Compression::Piz, (-10, 20), Some("sidecar.json"))
        .unwrap();
    let with = Cryptomatte::open(&sidecar, "CryptoObject", true).unwrap();
    assert_eq!(with.manifest().len(), 10);
    assert_eq!(with.extract(&["torus"], false).unwrap().origin, (-10, 20));
    let without = Cryptomatte::open(&sidecar, "CryptoObject", false).unwrap();
    assert!(without.manifest().is_empty());
    // Without the manifest names still resolve by the standard hash.
    assert_eq!(
        without.extract(&["torus"], false).unwrap().get(0, 0),
        0.8125
    );

    for escape in [
        "../escape.json",
        "/abs.json",
        "a//b.json",
        "x.txt",
        "c:\\x.json",
    ] {
        assert!(
            builder
                .save(
                    &directory.join("escape.exr"),
                    Compression::Zip,
                    (0, 0),
                    Some(escape)
                )
                .is_err(),
            "{escape}"
        );
    }
    assert!(
        builder
            .save(
                &directory.join("lossy.exr"),
                Compression::Pxr24,
                (0, 0),
                None
            )
            .is_err()
    );

    let matte = with.extract(&["torus"], false).unwrap();
    let matte_path = directory.join("mask.exr");
    matte.save(&matte_path, Compression::Zip).unwrap();
    let back = kinogaki_exr_format::exr::read(
        &std::fs::read(&matte_path).unwrap(),
        &ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(back.parts[0].header.data_window.x_min, -10);
    with.write_manifest(&directory.join("written.json"))
        .unwrap();
    let json = std::fs::read_to_string(directory.join("written.json")).unwrap();
    assert_eq!(
        kinogaki_exr_format::exr::cryptomatte::Manifest::parse(&json).unwrap(),
        *with.manifest()
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_sidecar_that_climbs_out_is_refused() {
    let mut part = builder().part((0, 0), None).unwrap();
    let key = format!(
        "cryptomatte/{}/",
        kinogaki_exr_format::exr::cryptomatte::metadata_key("CryptoObject")
    );
    part.header
        .attributes
        .retain(|a| a.name != format!("{key}manifest"));
    part.header
        .set_string(&format!("{key}manif_file"), "../escape.json");
    let here = std::env::temp_dir();
    assert!(Cryptomatte::from_part(part.clone(), "CryptoObject", Sidecar::Beside(&here)).is_err());
    assert!(Cryptomatte::from_part(part, "CryptoObject", Sidecar::Ignore).is_ok());
}

#[test]
fn psyop_fixtures_match_openexr() {
    let expected = std::fs::read_to_string(fixtures().join("expected.txt")).unwrap();
    let oracle = std::env::var("KINOGAKI_CRYPTOMATTE_ORACLE_DIR").ok();
    let mut layers: Vec<(String, String, Cryptomatte)> = Vec::new();
    let mut checked = 0;
    for line in expected.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (file, layer, index) = (fields[0], fields[1], fields[2].parse::<usize>().unwrap());
        let (count, sum): (usize, f64) = (fields[3].parse().unwrap(), fields[4].parse().unwrap());
        if !layers.iter().any(|(f, l, _)| f == file && l == layer) {
            let opened = Cryptomatte::open(&fixtures().join(file), layer, true)
                .unwrap_or_else(|e| panic!("{file} {layer}: {e}"));
            layers.push((file.to_owned(), layer.to_owned(), opened));
        }
        let cryptomatte = &layers
            .iter()
            .find(|(f, l, _)| f == file && l == layer)
            .unwrap()
            .2;
        let (name, _) = cryptomatte.manifest().entries().nth(index).unwrap();
        let matte = cryptomatte.extract(&[name], false).unwrap();
        let got_count = matte.coverage.iter().filter(|&&c| c > 0.0).count();
        let got_sum: f64 = matte.coverage.iter().map(|&c| f64::from(c)).sum();
        assert_eq!(got_count, count, "{file} {layer} {name}");
        assert!(
            (got_sum - sum).abs() <= 1e-4 * sum.max(1.0),
            "{file} {layer} {name}: {got_sum} vs {sum}"
        );
        if let Some(directory) = &oracle {
            std::fs::create_dir_all(directory).unwrap();
            matte
                .save(
                    &Path::new(directory).join(format!("{file}~{layer}~{index}.exr")),
                    Compression::Zip,
                )
                .unwrap();
        }
        checked += 1;
    }
    assert_eq!(checked, 425);
    // Five layers over three files, one manifest in a sidecar.
    assert_eq!(layers.len(), 5);
}
