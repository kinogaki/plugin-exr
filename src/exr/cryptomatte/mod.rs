//! Cryptomatte 1.2: ID mattes in EXR files.
//!
//! A layer is a set of FLOAT channel quadruples `<layer>NN.R/G/B/A`, each
//! holding two ranks (ID, coverage): an ID is a name's MurmurHash3 as
//! float bits. Its metadata lives under `cryptomatte/<key>/`: `name`,
//! `hash` (MurmurHash3_32), `conversion` (uint32_to_float32) and the
//! manifest of names to hashes, embedded (`manifest`) or in a sidecar
//! JSON file (`manif_file`).
//!
//! [`Cryptomatte`] reads a layer from a part: mattes of names or IDs
//! summed over every rank ([`Cryptomatte::extract`]), the ID or name at a
//! pixel ([`Cryptomatte::pick`]). [`Builder`] writes one from weighted
//! samples.
//!
//! ```
//! use kinogaki_exr_format::exr::cryptomatte::{Builder, Cryptomatte};
//! let mut builder = Builder::new(2, 1, "CryptoObject", 3).unwrap();
//! builder.add_sample(0, 0, &["hero", "floor"], &[0.25, 0.5], 1.0).unwrap();
//! builder.add_sample(0, 0, &["hero"], &[1.0], 3.0).unwrap();
//! let layer = builder.finish().unwrap();
//! assert_eq!(layer.pick_name(0, 0, 0).unwrap(), Some("hero"));
//! assert_eq!(layer.extract(&["hero"], false).unwrap().get(0, 0), 0.8125);
//! ```
mod builder;
mod hash;
mod manifest;

pub use builder::Builder;
pub use hash::{hash_to_id, id_bits, metadata_key, murmur3_32, name_id};
pub use manifest::Manifest;

use crate::exr::{
    Channel, ChannelSamples, Compression, Error, Exr, Header, Part, Window, WriteOptions,
};
use kinogaki_studio_api::format::files;
use kinogaki_studio_api::geometry::parallel;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The most bytes a sidecar manifest may take.
pub(crate) const MAX_SIDECAR: u64 = 64 << 20;
/// The most channel quadruples a layer may have.
const MAX_SETS: usize = 512;

/// The Cryptomatte layer names `header` declares, in attribute order.
pub fn layer_names(header: &Header) -> Vec<String> {
    header
        .attributes
        .iter()
        .filter(|a| is_name_key(&a.name) && a.kind == "string")
        .filter_map(|a| String::from_utf8(a.value.clone()).ok())
        .collect()
}

fn is_name_key(key: &str) -> bool {
    key.starts_with("cryptomatte/") && key.ends_with("/name")
}

/// Whether `name` is a safe sidecar path: relative, a descendant (no
/// `..`, `.` or empty components), no backslash, colon or NUL, ending in
/// `.json`. Lexical only: symbolic links are followed when read.
pub(crate) fn check_sidecar(name: &str) -> Result<(), Error> {
    let bad = name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', ':', '\0'])
        || !name.ends_with(".json")
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(Error::Invalid(
            "Cryptomatte sidecar must be a relative descendant .json path",
        ));
    }
    Ok(())
}

/// Where a layer's sidecar manifest may be read from.
#[derive(Clone, Copy, Debug)]
pub enum Sidecar<'a> {
    /// Never read one (untrusted files, or files without a directory).
    Ignore,
    /// Read it relative to this directory (the EXR's).
    Beside(&'a Path),
}

/// One Cryptomatte layer of a part: its ranks and manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct Cryptomatte {
    name: String,
    width: usize,
    height: usize,
    origin: (i32, i32),
    /// Each rank's IDs and coverages, rank 0 first.
    ranks: Vec<(Vec<f32>, Vec<f32>)>,
    manifest: Manifest,
}

/// A matte: one coverage value a pixel, over the source's data window.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    /// Pixels across and down.
    pub width: usize,
    /// See `width`.
    pub height: usize,
    /// The data window's top left, as the source's.
    pub origin: (i32, i32),
    /// Coverage 0..=1, row by row.
    pub coverage: Vec<f32>,
}

impl Matte {
    /// The coverage at (`x`, `y`) of the window.
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.coverage[y * self.width + x]
    }

    /// The matte as a part of one FLOAT channel `Y` at its origin.
    pub fn to_part(&self, compression: Compression) -> Part {
        let mut header = Header::new(self.width as u32, self.height as u32, compression);
        header.data_window = window(self.origin, self.width, self.height);
        header.display_window = header.data_window;
        Part {
            header,
            channels: vec![Channel {
                name: "Y".to_owned(),
                samples: ChannelSamples::F32(self.coverage.clone()),
                linear: false,
            }],
        }
    }

    /// Writes the matte as an EXR file.
    pub fn save(&self, path: &Path, compression: Compression) -> Result<(), Error> {
        let bytes = crate::exr::write(
            &Exr {
                parts: vec![self.to_part(compression)],
            },
            &WriteOptions::default(),
        )?;
        files::replace(path, |file| std::io::Write::write_all(file, &bytes))?;
        Ok(())
    }
}

fn window((x, y): (i32, i32), width: usize, height: usize) -> Window {
    Window {
        x_min: x,
        y_min: y,
        x_max: x + width as i32 - 1,
        y_max: y + height as i32 - 1,
    }
}

impl Cryptomatte {
    /// Layer `layer` of `part`, which it takes its channels from; a sidecar
    /// manifest is read as `sidecar` allows. The layer's hash and conversion
    /// must be Cryptomatte 1.2's, its ranks complete FLOAT quadruples.
    pub fn from_part(part: Part, layer: &str, sidecar: Sidecar) -> Result<Cryptomatte, Error> {
        let header = &part.header;
        let mut prefix = None;
        for a in &header.attributes {
            if is_name_key(&a.name) && a.kind == "string" && a.value == layer.as_bytes() {
                if prefix.is_some() || a.name.len() != "cryptomatte/".len() + 7 + "/name".len() {
                    return Err(Error::Invalid(
                        "ambiguous or malformed Cryptomatte metadata key",
                    ));
                }
                prefix = Some(a.name[..a.name.len() - "name".len()].to_owned());
            }
        }
        let prefix = prefix.ok_or(Error::Invalid("Cryptomatte layer not found"))?;
        let meta = |key: &str| {
            header
                .string(&format!("{prefix}{key}"))
                .unwrap_or("")
                .to_owned()
        };
        if meta("hash") != "MurmurHash3_32" || meta("conversion") != "uint32_to_float32" {
            return Err(Error::Unsupported(
                "Cryptomatte hash or conversion other than MurmurHash3_32 / uint32_to_float32",
            ));
        }
        let manifest = match (meta("manifest"), meta("manif_file"), sidecar) {
            (embedded, _, _) if !embedded.is_empty() => Manifest::parse(&embedded)?,
            (_, file, Sidecar::Beside(directory)) if !file.is_empty() => {
                Manifest::parse(&self::sidecar(directory, &file)?)?
            }
            _ => Manifest::default(),
        };
        // The quadruples: `<layer>NN.<suffix>`, suffix R/G/B/A in any of
        // three spellings.
        let mut sets = 0usize;
        for channel in &part.channels {
            let Some(tail) = channel.name.strip_prefix(layer) else {
                continue;
            };
            let Some((number, suffix)) = tail.rsplit_once('.') else {
                continue;
            };
            if number.len() < 2
                || !number.bytes().all(|b| b.is_ascii_digit())
                || suffix_index(suffix).is_none()
            {
                continue;
            }
            let n: usize = number
                .parse()
                .map_err(|_| Error::Invalid("bad Cryptomatte channel number"))?;
            if n >= MAX_SETS {
                return Err(Error::TooLarge);
            }
            sets = sets.max(n + 1);
        }
        if sets == 0 {
            return Err(Error::Invalid("Cryptomatte layer has no ranked channels"));
        }
        let (width, height) = part.size();
        let origin = (header.data_window.x_min, header.data_window.y_min);
        let mut planes: Vec<Option<Vec<f32>>> = vec![None; sets * 4];
        for channel in part.channels {
            let Some(tail) = channel.name.strip_prefix(layer) else {
                continue;
            };
            let Some((number, suffix)) = tail.rsplit_once('.') else {
                continue;
            };
            let (Ok(n), Some(c)) = (number.parse::<usize>(), suffix_index(suffix)) else {
                continue;
            };
            if number.len() < 2 || n >= sets {
                continue;
            }
            match channel.samples {
                ChannelSamples::F32(v) => planes[n * 4 + c] = Some(v),
                _ => {
                    return Err(Error::Invalid(
                        "Cryptomatte channels must be lossless FLOAT",
                    ));
                }
            }
        }
        let mut planes = planes
            .into_iter()
            .map(|p| p.ok_or(Error::Invalid("Cryptomatte channel quadruple incomplete")))
            .collect::<Result<Vec<_>, Error>>()?
            .into_iter();
        let mut ranks = Vec::with_capacity(sets * 2);
        for _ in 0..sets * 2 {
            let ids = planes.next().expect("two planes a rank");
            let coverage = planes.next().expect("two planes a rank");
            ranks.push((ids, coverage));
        }
        Ok(Cryptomatte {
            name: layer.to_owned(),
            width,
            height,
            origin,
            ranks,
            manifest,
        })
    }

    /// Layer `layer` of the EXR at `path` (the first part that has it); a
    /// sidecar manifest is read from beside it when `load_sidecar`.
    pub fn open(path: &Path, layer: &str, load_sidecar: bool) -> Result<Cryptomatte, Error> {
        let bytes = std::fs::read(path)?;
        let exr = crate::exr::read(&bytes, &kinogaki_studio_api::format::ReadBudget::default())?;
        let directory: PathBuf = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let sidecar = if load_sidecar {
            Sidecar::Beside(&directory)
        } else {
            Sidecar::Ignore
        };
        let part = exr
            .parts
            .into_iter()
            .find(|p| layer_names(&p.header).iter().any(|n| n == layer))
            .ok_or(Error::Invalid("Cryptomatte layer not found"))?;
        Cryptomatte::from_part(part, layer, sidecar)
    }

    /// The layer's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ranks: two a channel quadruple.
    pub fn rank_count(&self) -> usize {
        self.ranks.len()
    }

    /// Pixels across and down, and the data window's top left.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The data window's top left.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// The manifest (empty when the file has none or its sidecar was not
    /// read).
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The ID of `name`: the manifest's hash, else the standard hash.
    pub fn id_for(&self, name: &str) -> f32 {
        hash_to_id(
            self.manifest
                .hash(name)
                .unwrap_or_else(|| murmur3_32(name.as_bytes())),
        )
    }

    /// The ID at (`x`, `y`) of the window in rank `rank`.
    pub fn pick(&self, x: usize, y: usize, rank: usize) -> Result<f32, Error> {
        if x >= self.width || y >= self.height {
            return Err(Error::Invalid("pixel outside the data window"));
        }
        let (ids, _) = self
            .ranks
            .get(rank)
            .ok_or(Error::Invalid("Cryptomatte rank out of range"))?;
        Ok(ids[y * self.width + x])
    }

    /// The manifest's name for the ID at (`x`, `y`) in rank `rank`.
    pub fn pick_name(&self, x: usize, y: usize, rank: usize) -> Result<Option<&str>, Error> {
        Ok(self.manifest.name_for(self.pick(x, y, rank)?.to_bits()))
    }

    /// The coverage of `names` together (each counted once) over every
    /// rank; `invert` gives the rest.
    pub fn extract(&self, names: &[&str], invert: bool) -> Result<Matte, Error> {
        let ids: Vec<f32> = names.iter().map(|n| self.id_for(n)).collect();
        self.extract_ids(&ids, invert)
    }

    /// The coverage of IDs `ids` (exact float bits; 0 is the background)
    /// together over every rank; `invert` gives the rest. Coverage must be
    /// finite and within 0..=1.
    pub fn extract_ids(&self, ids: &[f32], invert: bool) -> Result<Matte, Error> {
        if ids.iter().any(|id| !id.is_finite()) {
            return Err(Error::Invalid(
                "Cryptomatte selection holds a non-finite ID",
            ));
        }
        let mut wanted: Vec<u32> = ids.iter().map(|id| id.to_bits()).collect();
        wanted.sort_unstable();
        wanted.dedup();
        let selected = |bits: u32| {
            if wanted.len() <= 8 {
                wanted.contains(&bits)
            } else {
                wanted.binary_search(&bits).is_ok()
            }
        };
        let rows = self.height;
        let band = 64;
        let bands = parallel::map(rows.div_ceil(band), |b| {
            let (first, last) = (
                b * band * self.width,
                ((b + 1) * band).min(rows) * self.width,
            );
            // Summed in f64, rounded once.
            let mut sums = vec![0.0f64; last - first];
            for (ids, coverage) in &self.ranks {
                for ((sum, &id), &amount) in sums
                    .iter_mut()
                    .zip(&ids[first..last])
                    .zip(&coverage[first..last])
                {
                    if !(0.0..=1.0).contains(&amount) {
                        return Err(Error::Invalid("Cryptomatte coverage outside 0-1"));
                    }
                    if selected(id.to_bits()) {
                        *sum += f64::from(amount);
                    }
                }
            }
            Ok(sums
                .into_iter()
                .map(|sum| {
                    let sum = sum.clamp(0.0, 1.0);
                    (if invert { 1.0 - sum } else { sum }) as f32
                })
                .collect::<Vec<f32>>())
        })
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()?;
        Ok(Matte {
            width: self.width,
            height: self.height,
            origin: self.origin,
            coverage: bands.concat(),
        })
    }

    /// Writes the manifest as JSON to `path`.
    pub fn write_manifest(&self, path: &Path) -> Result<(), Error> {
        let manifest = self.manifest.to_json();
        files::replace(path, |file| {
            std::io::Write::write_all(file, manifest.as_bytes())
        })?;
        Ok(())
    }
}

/// R, G, B, A in any of three spellings, as 0..=3.
fn suffix_index(suffix: &str) -> Option<usize> {
    match suffix {
        "R" | "r" | "red" => Some(0),
        "G" | "g" | "green" => Some(1),
        "B" | "b" | "blue" => Some(2),
        "A" | "a" | "alpha" => Some(3),
        _ => None,
    }
}

/// The text of sidecar manifest `file` (a `manif_file` value) beside an EXR
/// in `directory`: refused unless it is a relative descendant `.json` path,
/// and when it is over 64 MB.
pub fn sidecar(directory: &Path, file: &str) -> Result<String, Error> {
    check_sidecar(file)?;
    read_sidecar(&directory.join(file))
}

/// The text of the sidecar at `path`: a regular file (a pipe or a device
/// is never opened: a pipe waits for a writer, a device never ends), read
/// no further than [`MAX_SIDECAR`].
fn read_sidecar(path: &Path) -> Result<String, Error> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(Error::Invalid("Cryptomatte sidecar is not a regular file"));
    }
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_SIDECAR {
        return Err(Error::TooLarge);
    }
    let mut text = String::new();
    file.take(MAX_SIDECAR).read_to_string(&mut text)?;
    Ok(text)
}
