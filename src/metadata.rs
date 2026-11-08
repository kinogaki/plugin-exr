//! A file's header attributes as an Image's metadata ([`read`]), and back
//! ([`write`]).
use crate::exr::cryptomatte::{MAX_SIDECAR, check_sidecar};
use crate::exr::{Attribute as Raw, Exr, Header};
use kinogaki_studio_api::format::files::Named;
use kinogaki_studio_api::geometry::image::metadata::PIXELATED;
use kinogaki_studio_api::geometry::{
    Attribute, Attributes, Domain, Meaning, Text, Tuple, check_name,
};
use kinogaki_studio_api::{Diagnostic, Error};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The header attributes a part's color layers' tag holds
/// (`ColorTag`), never its metadata.
const TAGS: [&str; 2] = ["colorspace", "chromaticities"];

/// Every attribute of `file`'s parts but its colors' tag ([`TAGS`]) as
/// Detail attributes: the first part's by their names, a later part's by
/// its name too unless an earlier one holds another value there, then as
/// `<part>/<name>`; a
/// name an attribute's can't be held as [`held_name`] says. A
/// Cryptomatte manifest kept in a sidecar file (`manif_file`) is read
/// through `named` as its `manifest`, each file once and 64 MiB of them in
/// all. Attributes no metadata can hold (a preview image, a list of texts)
/// are left out, said once.
pub(crate) fn read(
    file: &Exr,
    named: &Named,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Attributes, Error> {
    let mut held = Attributes::new();
    let mut left_out: Vec<String> = Vec::new();
    let mut sidecars = Sidecars::default();
    for part in &file.parts {
        for raw in &part.header.attributes {
            if TAGS.contains(&raw.name.as_str()) {
                continue;
            }
            let Some(value) = value(raw) else {
                left_out.push(raw.name.clone());
                continue;
            };
            let name = match held.get(Domain::Detail, &held_name(&raw.name)) {
                None => held_name(&raw.name),
                Some(same) if *same == value => continue,
                Some(_) => held_name(&format!(
                    "{}/{}",
                    part.header.name.as_deref().unwrap_or("part"),
                    raw.name
                )),
            };
            if held.set(Domain::Detail, &name, value).is_err() {
                left_out.push(raw.name.clone());
            }
        }
        sidecars.read(&part.header, &mut held, named, warnings)?;
    }
    if !left_out.is_empty() {
        let shown = left_out.len().min(16);
        let more = match left_out.len() - shown {
            0 => String::new(),
            more => format!(" and {more} more"),
        };
        warnings.push(Diagnostic::warning(format!(
            "attributes an image's metadata can't hold left out: {}{more}",
            left_out[..shown].join(", ")
        )));
    }
    Ok(held)
}

/// The sidecar manifests a read has read: each file once, by its path,
/// and 64 MiB of manifests kept in all, however many layers name one.
#[derive(Default)]
struct Sidecars {
    read: HashMap<PathBuf, Arc<str>>,
    /// Bytes of manifests kept so far.
    kept: u64,
}

impl Sidecars {
    /// `held` with the manifest of each of `header`'s Cryptomatte layers that
    /// keeps its manifest in a sidecar file.
    fn read(
        &mut self,
        header: &Header,
        held: &mut Attributes,
        named: &Named,
        warnings: &mut Vec<Diagnostic>,
    ) -> Result<(), Error> {
        for raw in &header.attributes {
            let Some(key) = raw.name.strip_suffix("/manif_file") else {
                continue;
            };
            let manifest = format!("{key}/manifest");
            if header.attribute(&manifest).is_some() {
                continue;
            }
            let file = String::from_utf8_lossy(&raw.value);
            let text = match self.text(&file, named) {
                Ok(Some(text)) if self.kept + text.len() as u64 <= MAX_SIDECAR => text,
                Ok(None) => continue,
                found => {
                    let why = found
                        .err()
                        .unwrap_or_else(|| "over the 64 MiB a file's manifests may take".into());
                    warnings.push(Diagnostic::warning(format!(
                        "Cryptomatte manifest {file}: {why}"
                    )));
                    continue;
                }
            };
            self.kept += text.len() as u64;
            let text = Attribute::text(Text::from_strs([&*text]));
            held.set(Domain::Detail, &manifest, text)?;
        }
        Ok(())
    }

    /// The text of sidecar `file`, a relative `.json` path beside the file
    /// found through `named`; `None` where `named` left it unresolved (its
    /// warnings say so).
    fn text(&mut self, file: &str, named: &Named) -> Result<Option<Arc<str>>, String> {
        check_sidecar(file).map_err(|e| e.to_string())?;
        let Some(path) = named.file(file, None) else {
            return Ok(None);
        };
        if let Some(text) = self.read.get(&path) {
            return Ok(Some(text.clone()));
        }
        let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if self.kept + len > MAX_SIDECAR {
            return Err("over the 64 MiB a file's manifests may take".into());
        }
        let bytes = named.read(&path).map_err(|e| e.message().to_owned())?;
        let text: Arc<str> = String::from_utf8(bytes)
            .map_err(|_| "not UTF-8 text".to_owned())?
            .into();
        self.read.insert(path, text.clone());
        Ok(Some(text))
    }
}

/// Header attribute name `raw` as metadata names it: itself when an
/// attribute's name may be it and holds no `:`; else `_`, then `raw` with
/// each character an attribute's name can't hold where it is, and each
/// `:`, as `:` and its bytes in hex (`ColorSpace/Blue Primary` is
/// `_ColorSpace/Blue:20Primary`, `2d` is `_:32d`). [`raw_name`] gives it
/// back.
fn held_name(raw: &str) -> String {
    if check_name(raw).is_ok() && !raw.contains(':') {
        return raw.to_owned();
    }
    let mut held = String::from("_");
    for (at, c) in raw.chars().enumerate() {
        let kept = match at {
            0 => c.is_alphabetic() || matches!(c, '_' | '.'),
            _ => c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'),
        };
        match kept {
            true => held.push(c),
            false => {
                for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                    held.push_str(&format!(":{byte:02X}"));
                }
            }
        }
    }
    held
}

/// The header attribute name metadata `name` holds ([`held_name`]'s
/// inverse): `name` itself unless it holds a `:`.
fn raw_name(name: &str) -> String {
    let Some(escaped) = name.strip_prefix('_').filter(|_| name.contains(':')) else {
        return name.to_owned();
    };
    let text = escaped.as_bytes();
    let mut bytes = Vec::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let hex = (text.get(at + 1..at + 3))
            .filter(|_| text[at] == b':')
            .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match hex {
            Some(byte) => {
                bytes.push(byte);
                at += 3;
            }
            None => {
                bytes.push(text[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `raw` as an attribute, when one can hold it.
fn value(raw: &Raw) -> Option<Attribute> {
    let bytes = &raw.value;
    let words = |count: usize| -> Option<Vec<[u8; 4]>> {
        let words: Vec<[u8; 4]> = bytes.as_chunks::<4>().0.to_vec();
        (bytes.len() == count * 4).then_some(words)
    };
    let ints = |count: usize| {
        Some(
            words(count)?
                .into_iter()
                .map(i32::from_le_bytes)
                .collect::<Vec<_>>(),
        )
    };
    let floats = |count: usize| {
        Some(
            words(count)?
                .into_iter()
                .map(f32::from_le_bytes)
                .collect::<Vec<_>>(),
        )
    };
    let doubles = |count: usize| -> Option<Vec<f64>> {
        (bytes.len() == count * 8).then(|| {
            (bytes.as_chunks::<8>().0.iter())
                .map(|b| f64::from_le_bytes(*b))
                .collect()
        })
    };
    match raw.kind.as_str() {
        "string" => Some(Attribute::text(Text::from_strs([String::from_utf8_lossy(
            bytes,
        )
        .as_ref()]))),
        "int" => one(ints(1)?),
        "v2i" | "rational" | "timecode" => tuple::<i32, 2>(ints(2)?),
        "v3i" => tuple::<i32, 3>(ints(3)?),
        "box2i" => tuple::<i32, 4>(ints(4)?),
        "keycode" => tuple::<i32, 7>(ints(7)?),
        "float" => one(floats(1)?),
        "v2f" => tuple::<f32, 2>(floats(2)?),
        "v3f" => tuple::<f32, 3>(floats(3)?),
        "box2f" => tuple::<f32, 4>(floats(4)?),
        "chromaticities" => tuple::<f32, 8>(floats(8)?),
        "m33f" => tuple::<f32, 9>(floats(9)?),
        "m44f" => tuple::<f32, 16>(floats(16)?),
        "double" => one(doubles(1)?),
        "v2d" => tuple::<f64, 2>(doubles(2)?),
        "v3d" => tuple::<f64, 3>(doubles(3)?),
        "m33d" => tuple::<f64, 9>(doubles(9)?),
        "m44d" => tuple::<f64, 16>(doubles(16)?),
        "compression" | "lineOrder" | "envmap" | "deepImageState" => {
            one(vec![i32::from(*bytes.first()?)])
        }
        _ => None,
    }
}

/// `attribute`'s one tuple of `N` scalars, each as `bytes` gives it.
fn le<S: Copy, const N: usize>(
    attribute: &Attribute,
    bytes: impl Fn(S) -> Vec<u8>,
) -> Option<Vec<u8>>
where
    [S; N]: Tuple,
{
    let first = attribute.as_tuples::<[S; N]>()?.first()?;
    Some(first.iter().flat_map(|v| bytes(*v)).collect())
}

/// One scalar as an attribute.
fn one<T: Tuple>(values: Vec<T>) -> Option<Attribute> {
    Attribute::tuples(values, Meaning::None).ok()
}

/// `values`, `N` of them, as one tuple.
fn tuple<T: Copy + Default, const N: usize>(values: Vec<T>) -> Option<Attribute>
where
    [T; N]: Tuple,
{
    let tuple: [T; N] = values.try_into().ok()?;
    Attribute::tuples(vec![tuple], Meaning::None).ok()
}

/// `metadata`'s attributes into `header`, as the kind of EXR attribute their
/// type is: every one but those a writer handles itself (a color layer's
/// tag, Cryptomatte's keys) and those no EXR attribute holds (`pixelated`),
/// said by name.
pub(crate) fn write(metadata: &Attributes, header: &mut Header, warnings: &mut Vec<Diagnostic>) {
    let mut left_out = Vec::new();
    for (domain, name, attribute) in metadata.iter() {
        let handled = TAGS.contains(&name) || name == PIXELATED || name.starts_with("cryptomatte/");
        if domain != Domain::Detail || handled {
            continue;
        }
        match raw(metadata, name, attribute) {
            Some((kind, value)) => header.set_attribute(Raw {
                name: raw_name(name),
                kind: kind.to_owned(),
                value,
            }),
            None => left_out.push(name.to_owned()),
        }
    }
    if !left_out.is_empty() {
        warnings.push(Diagnostic::warning(format!(
            "metadata an EXR attribute can't hold left out: {}",
            left_out.join(", ")
        )));
    }
}

/// `attribute` (metadata `name`) as an EXR attribute's kind and bytes.
fn raw(
    metadata: &Attributes,
    name: &str,
    attribute: &Attribute,
) -> Option<(&'static str, Vec<u8>)> {
    if let Some(text) = metadata.text(Domain::Detail, name) {
        return Some(("string", text.get(0)?.as_bytes().to_vec()));
    }
    let int = |v: i32| v.to_le_bytes().to_vec();
    let float = |v: f32| v.to_le_bytes().to_vec();
    let double = |v: f64| v.to_le_bytes().to_vec();
    let found = [
        ("int", le::<i32, 1>(attribute, int)),
        ("v2i", le::<i32, 2>(attribute, int)),
        ("v3i", le::<i32, 3>(attribute, int)),
        ("box2i", le::<i32, 4>(attribute, int)),
        ("float", le::<f32, 1>(attribute, float)),
        ("v2f", le::<f32, 2>(attribute, float)),
        ("v3f", le::<f32, 3>(attribute, float)),
        ("box2f", le::<f32, 4>(attribute, float)),
        ("m33f", le::<f32, 9>(attribute, float)),
        ("m44f", le::<f32, 16>(attribute, float)),
        ("double", le::<f64, 1>(attribute, double)),
        ("v2d", le::<f64, 2>(attribute, double)),
        ("v3d", le::<f64, 3>(attribute, double)),
    ];
    found
        .into_iter()
        .find_map(|(kind, bytes)| Some((kind, bytes?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_an_attributes_cant_be_are_held_and_given_back() {
        let raws = [
            "ColorSpace/Blue Primary",
            "2d",
            "a:b",
            "_:3A",
            "x (y)",
            "π r²",
            "owner",
        ];
        for raw in raws {
            let held = held_name(raw);
            assert!(check_name(&held).is_ok(), "{held}");
            assert_eq!(raw_name(&held), raw, "{held}");
        }
        assert_eq!(
            held_name("ColorSpace/Blue Primary"),
            "_ColorSpace/Blue:20Primary"
        );
        assert_eq!(held_name("owner"), "owner");
    }
}
