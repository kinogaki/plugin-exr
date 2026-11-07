//! Conversion between a part's planes and lib-image's interleaved image.
use crate::exr::{
    Channel, ChannelSamples, Compression, Error, Header, Image, Part, SampleType, Samples,
    f16_to_f32, f32_to_f16,
};

/// The channel names an image's channels map to, by channel count.
fn names(channels: u8) -> &'static [&'static str] {
    match channels {
        1 => &["Y"],
        2 => &["Y", "A"],
        3 => &["R", "G", "B"],
        _ => &["R", "G", "B", "A"],
    }
}

impl Part {
    /// The part as an interleaved image: R, G, B (and A) when it has them,
    /// else Y (and A), else its only channel. Samples stay half or float
    /// when every channel used is; mixed types become float.
    pub fn to_image(&self) -> Result<Image, Error> {
        let picked: Vec<&Channel> = if let (Some(r), Some(g), Some(b)) =
            (self.channel("R"), self.channel("G"), self.channel("B"))
        {
            [Some(r), Some(g), Some(b), self.channel("A")]
                .into_iter()
                .flatten()
                .collect()
        } else if let Some(y) = self.channel("Y") {
            [Some(y), self.channel("A")].into_iter().flatten().collect()
        } else if self.channels.len() == 1 {
            vec![&self.channels[0]]
        } else {
            return Err(Error::Invalid("the part has no R, G, B or Y channels"));
        };
        let (width, height) = self.size();
        let n = picked.len();
        let pixels = width * height;
        let same = |t: SampleType| picked.iter().all(|c| c.samples.sample_type() == t);
        let samples = if same(SampleType::F16) {
            let mut out = vec![0u16; pixels * n];
            for (c, channel) in picked.iter().enumerate() {
                let ChannelSamples::F16(v) = &channel.samples else {
                    unreachable!()
                };
                out.chunks_exact_mut(n).zip(v).for_each(|(p, &s)| p[c] = s);
            }
            Samples::F16(out)
        } else if same(SampleType::U32) {
            let mut out = vec![0u32; pixels * n];
            for (c, channel) in picked.iter().enumerate() {
                let ChannelSamples::U32(v) = &channel.samples else {
                    unreachable!()
                };
                out.chunks_exact_mut(n).zip(v).for_each(|(p, &s)| p[c] = s);
            }
            Samples::U32(out)
        } else {
            let mut out = vec![0f32; pixels * n];
            for (c, channel) in picked.iter().enumerate() {
                for (i, p) in out.chunks_exact_mut(n).enumerate() {
                    p[c] = channel.samples.get(i);
                }
            }
            Samples::F32(out)
        };
        Image::new(width as u32, height as u32, n as u8, samples)
            .map_err(|_| Error::Invalid("image size overflows"))
    }

    /// A scanline part holding `image` (half, float or 32-bit unsigned
    /// samples): channels Y, YA, RGB or RGBA.
    pub fn from_image(image: &Image, compression: Compression) -> Result<Part, Error> {
        let n = usize::from(image.channels);
        if !(1..=4).contains(&n)
            || image.sample_count() != Some(image.samples.len())
            || image.width == 0
            || image.height == 0
        {
            return Err(Error::Invalid(
                "image samples do not match its size and channels",
            ));
        }
        let plane = |c: usize| -> Result<ChannelSamples, Error> {
            Ok(match &image.samples {
                Samples::F16(v) => {
                    ChannelSamples::F16(v.iter().skip(c).step_by(n).copied().collect())
                }
                Samples::F32(v) => {
                    ChannelSamples::F32(v.iter().skip(c).step_by(n).copied().collect())
                }
                Samples::U32(v) => {
                    ChannelSamples::U32(v.iter().skip(c).step_by(n).copied().collect())
                }
                Samples::U8(_) | Samples::U16(_) => {
                    return Err(Error::Invalid(
                        "EXR stores half, float or 32-bit unsigned samples",
                    ));
                }
            })
        };
        let mut channels = names(image.channels)
            .iter()
            .enumerate()
            .map(|(c, name)| {
                Ok(Channel {
                    name: (*name).to_owned(),
                    samples: plane(c)?,
                    linear: false,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        channels.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Part {
            header: Header::new(image.width, image.height, compression),
            channels,
        })
    }
}

impl ChannelSamples {
    /// The samples as half floats (floats rounded to nearest, integers
    /// converted).
    pub fn to_f16(&self) -> Vec<u16> {
        match self {
            ChannelSamples::F16(v) => v.clone(),
            ChannelSamples::F32(v) => v.iter().map(|&s| f32_to_f16(s)).collect(),
            ChannelSamples::U32(v) => v.iter().map(|&s| f32_to_f16(s as f32)).collect(),
        }
    }

    /// The samples as floats.
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            ChannelSamples::F16(v) => v.iter().map(|&s| f16_to_f32(s)).collect(),
            ChannelSamples::F32(v) => v.clone(),
            ChannelSamples::U32(v) => v.iter().map(|&s| s as f32).collect(),
        }
    }
}
