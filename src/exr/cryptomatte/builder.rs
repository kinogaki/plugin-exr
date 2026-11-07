//! Writing a Cryptomatte layer from weighted samples.
//!
//! Each sample names the surfaces it sees with their coverages and has a
//! filter weight; what its coverages leave is the background (ID zero).
//! Contributions add up per pixel and ID; a pixel's ranks are its IDs by
//! descending weight (float bits break ties), each with its weight over
//! the pixel's total. Truncating to the layer's ranks never renormalizes
//! the ones kept, so transparency survives.
use super::hash::id_bits;
use super::{Cryptomatte, Manifest, Sidecar, check_sidecar, metadata_key, window};
use crate::exr::{Channel, ChannelSamples, Compression, Error, Exr, Header, Part, WriteOptions};
use std::path::Path;

/// One ID's weight at a pixel, in that pixel's list.
#[derive(Clone, Copy, Debug)]
struct Contribution {
    bits: u32,
    weight: f64,
    /// The pixel's next contribution, plus one (0 ends the list).
    next: u32,
}

/// Accumulates samples into a Cryptomatte layer.
#[derive(Clone, Debug)]
pub struct Builder {
    width: usize,
    height: usize,
    levels: usize,
    layer: String,
    /// Each pixel's first contribution, plus one.
    heads: Vec<u32>,
    weights: Vec<f64>,
    entries: Vec<Contribution>,
    manifest: Manifest,
}

impl Builder {
    /// A builder for a `width` × `height` layer called `layer` with
    /// `levels` channel quadruples (twice as many ranks).
    pub fn new(width: usize, height: usize, layer: &str, levels: usize) -> Result<Builder, Error> {
        if width == 0
            || height == 0
            || !(1..=256).contains(&levels)
            || layer.is_empty()
            || layer.len() > 240
            || layer.contains('\0')
        {
            return Err(Error::Invalid(
                "Cryptomatte size, name or levels out of range",
            ));
        }
        let pixels = width
            .checked_mul(height)
            .filter(|&p| p <= 1 << 28)
            .ok_or(Error::TooLarge)?;
        Ok(Builder {
            width,
            height,
            levels,
            layer: layer.to_owned(),
            heads: vec![0; pixels],
            weights: vec![0.0; pixels],
            entries: Vec::new(),
            manifest: Manifest::default(),
        })
    }

    /// The names seen so far, with their hashes.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Adds a sample at (`x`, `y`): surfaces `names` with `coverages` (each
    /// 0..=1, at most 1 together) and filter weight `weight` (0..=1e100).
    pub fn add_sample(
        &mut self,
        x: usize,
        y: usize,
        names: &[&str],
        coverages: &[f64],
        weight: f64,
    ) -> Result<(), Error> {
        if x >= self.width
            || y >= self.height
            || names.len() != coverages.len()
            || !weight.is_finite()
            || !(0.0..=1e100).contains(&weight)
        {
            return Err(Error::Invalid("invalid Cryptomatte sample"));
        }
        if coverages.iter().any(|c| !(0.0..=1.0).contains(c)) {
            return Err(Error::Invalid("sample coverage must be within 0-1"));
        }
        let covered: f64 = coverages.iter().sum();
        if covered > 1.000_000_000_001 {
            return Err(Error::Invalid("sample coverages add up to more than one"));
        }
        let mut bits = Vec::with_capacity(names.len());
        for name in names {
            bits.push(id_bits(self.manifest.ensure(name)?));
        }
        if self.entries.len() + names.len() + 1 > u32::MAX as usize - 1 {
            return Err(Error::TooLarge);
        }
        let pixel = y * self.width + x;
        self.weights[pixel] += weight;
        for (&b, &coverage) in bits.iter().zip(coverages) {
            self.add(pixel, b, coverage * weight);
        }
        self.add(pixel, 0, (1.0 - covered).max(0.0) * weight);
        Ok(())
    }

    fn add(&mut self, pixel: usize, bits: u32, weight: f64) {
        if weight == 0.0 {
            return;
        }
        let mut index = self.heads[pixel];
        while index != 0 {
            let entry = &mut self.entries[index as usize - 1];
            if entry.bits == bits {
                entry.weight += weight;
                return;
            }
            index = entry.next;
        }
        self.entries.push(Contribution {
            bits,
            weight,
            next: self.heads[pixel],
        });
        self.heads[pixel] = self.entries.len() as u32;
    }

    /// The layer as a part with its data window at `origin`: the manifest
    /// embedded, or with `sidecar` only named (`manif_file`) for the caller
    /// to write there.
    pub fn part(&self, origin: (i32, i32), sidecar: Option<&str>) -> Result<Part, Error> {
        if let Some(name) = sidecar {
            check_sidecar(name)?;
        }
        let pixels = self.width * self.height;
        let ranks = self.levels * 2;
        let mut planes = vec![vec![0.0f32; pixels]; ranks * 2];
        let mut list: Vec<Contribution> = Vec::new();
        for (pixel, (&head, &total)) in self.heads.iter().zip(&self.weights).enumerate() {
            list.clear();
            let mut index = head;
            while index != 0 {
                let entry = self.entries[index as usize - 1];
                list.push(entry);
                index = entry.next;
            }
            list.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.bits.cmp(&b.bits)));
            for (rank, entry) in list.iter().take(ranks).enumerate() {
                planes[rank * 2][pixel] = f32::from_bits(entry.bits);
                planes[rank * 2 + 1][pixel] = (entry.weight / total) as f32;
            }
        }
        let mut header = Header::new(self.width as u32, self.height as u32, Compression::Zip);
        header.data_window = window(origin, self.width, self.height);
        header.display_window = header.data_window;
        let prefix = format!("cryptomatte/{}/", metadata_key(&self.layer));
        header.set_string(&format!("{prefix}name"), &self.layer);
        header.set_string(&format!("{prefix}hash"), "MurmurHash3_32");
        header.set_string(&format!("{prefix}conversion"), "uint32_to_float32");
        match sidecar {
            Some(name) => header.set_string(&format!("{prefix}manif_file"), name),
            None => header.set_string(&format!("{prefix}manifest"), &self.manifest.to_json()),
        }
        let suffixes = ["R", "G", "B", "A"];
        let mut channels: Vec<Channel> = planes
            .into_iter()
            .enumerate()
            .map(|(i, samples)| Channel {
                name: format!("{}{:02}.{}", self.layer, i / 4, suffixes[i % 4]),
                samples: ChannelSamples::F32(samples),
                linear: false,
            })
            .collect();
        channels.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Part { header, channels })
    }

    /// The layer as built so far, read back as any other.
    pub fn finish(&self) -> Result<Cryptomatte, Error> {
        Cryptomatte::from_part(self.part((0, 0), None)?, &self.layer, Sidecar::Ignore)
    }

    /// Writes the layer to the EXR at `path` with its data window at
    /// `origin`; with `sidecar` the manifest goes to that file beside it.
    /// Use a lossless compression (ZIP or PIZ): IDs are exact float bits.
    pub fn save(
        &self,
        path: &Path,
        compression: Compression,
        origin: (i32, i32),
        sidecar: Option<&str>,
    ) -> Result<(), Error> {
        if compression == Compression::Pxr24 {
            return Err(Error::Invalid("PXR24 would lose Cryptomatte IDs"));
        }
        let mut part = self.part(origin, sidecar)?;
        part.header.compression = compression;
        if let Some(name) = sidecar {
            let directory = path.parent().unwrap_or(Path::new(""));
            std::fs::write(directory.join(name), self.manifest.to_json())?;
        }
        let bytes = crate::exr::write(&Exr { parts: vec![part] }, &WriteOptions::default())?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}
