//! Cryptomatte manifests: a JSON object of names to eight hex digits of
//! hash. The parser is strict and bounded: 64 MiB of text, names of at
//! most 1 MiB, a million entries; UTF-8, escapes and surrogate pairs as
//! JSON defines them; duplicates and anything but string members refused.
use super::hash::{id_bits, murmur3_32};
use crate::exr::Error;
use std::collections::HashMap;

const MAX_TEXT: usize = 64 << 20;
const MAX_NAME: usize = 1 << 20;
const MAX_ENTRIES: usize = 1 << 20;

/// Names and their hashes, in manifest order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<(String, u32)>,
    index: HashMap<String, usize>,
    /// The first entry of each ID (float bits).
    ids: HashMap<u32, usize>,
}

impl Manifest {
    /// Parses a manifest's JSON text.
    pub fn parse(text: &str) -> Result<Manifest, Error> {
        if text.len() > MAX_TEXT {
            return Err(Error::Invalid("manifest larger than 64 MiB"));
        }
        let mut p = Parser {
            bytes: text.as_bytes(),
            pos: 0,
        };
        let mut manifest = Manifest::default();
        p.space();
        p.expect(b'{', "manifest is not a JSON object")?;
        p.space();
        if p.peek() == Some(b'}') {
            p.pos += 1;
        } else {
            loop {
                let name = p.string()?;
                p.space();
                p.expect(b':', "manifest member without a colon")?;
                p.space();
                let at = p.pos;
                let value = p.string()?;
                if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::corrupt("manifest hash is not eight hex digits", at));
                }
                let hash = u32::from_str_radix(&value, 16).expect("hex digits");
                manifest
                    .add(name, hash)
                    .map_err(|_| Error::corrupt("duplicate or too many manifest names", at))?;
                p.space();
                match p.next() {
                    Some(b'}') => break,
                    Some(b',') => p.space(),
                    _ => {
                        return Err(Error::corrupt(
                            "manifest members not separated by commas",
                            p.pos,
                        ));
                    }
                }
            }
        }
        p.space();
        if p.pos != p.bytes.len() {
            return Err(Error::corrupt("text after the manifest", p.pos));
        }
        Ok(manifest)
    }

    /// The manifest as compact JSON.
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(self.entries.len() * 24 + 2);
        out.push('{');
        for (i, (name, hash)) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            quote(name, &mut out);
            out.push_str(&format!(":\"{hash:08x}\""));
        }
        out.push('}');
        out
    }

    /// Adds `name` with `hash`; a name already present or past the limits
    /// is refused.
    pub fn add(&mut self, name: String, hash: u32) -> Result<(), Error> {
        if self.index.contains_key(&name) {
            return Err(Error::Invalid("duplicate manifest name"));
        }
        if self.entries.len() >= MAX_ENTRIES || name.len() > MAX_NAME {
            return Err(Error::Invalid(
                "manifest name or entry count past its limit",
            ));
        }
        self.index.insert(name.clone(), self.entries.len());
        self.ids.entry(id_bits(hash)).or_insert(self.entries.len());
        self.entries.push((name, hash));
        Ok(())
    }

    /// The hash of `name`, adding it by the standard hash when absent.
    pub fn ensure(&mut self, name: &str) -> Result<u32, Error> {
        if let Some(hash) = self.hash(name) {
            return Ok(hash);
        }
        let hash = murmur3_32(name.as_bytes());
        self.add(name.to_owned(), hash)?;
        Ok(hash)
    }

    /// The hash the manifest gives `name`.
    pub fn hash(&self, name: &str) -> Option<u32> {
        self.index.get(name).map(|&i| self.entries[i].1)
    }

    /// The name whose ID has float bits `bits`.
    pub fn name_for(&self, bits: u32) -> Option<&str> {
        self.ids.get(&bits).map(|&i| self.entries[i].0.as_str())
    }

    /// Entries in manifest order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, u32)> {
        self.entries
            .iter()
            .map(|(name, hash)| (name.as_str(), *hash))
    }

    /// How many names there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A JSON string of `text`, escaping quotes, backslashes and controls.
fn quote(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 32 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8, what: &'static str) -> Result<(), Error> {
        if self.next() != Some(byte) {
            return Err(Error::corrupt(what, self.pos));
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let at = self.pos;
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or(Error::corrupt("short \\u escape", at))?;
        let text = std::str::from_utf8(digits).map_err(|_| Error::corrupt("bad \\u escape", at))?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| Error::corrupt("bad \\u escape", at))?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::corrupt("bad \\u escape", at));
        }
        self.pos += 4;
        Ok(value)
    }

    /// A JSON string, unescaped.
    fn string(&mut self) -> Result<String, Error> {
        self.expect(b'"', "manifest member is not a string")?;
        let mut out = Vec::new();
        loop {
            let at = self.pos;
            let b = self
                .next()
                .ok_or(Error::corrupt("unterminated string", at))?;
            match b {
                b'"' => break,
                0..=31 => return Err(Error::corrupt("control character in a string", at)),
                b'\\' => {
                    let escaped = self
                        .next()
                        .ok_or(Error::corrupt("unterminated escape", at))?;
                    match escaped {
                        b'"' | b'\\' | b'/' => out.push(escaped),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut scalar = self.hex4()?;
                            if (0xD800..0xDC00).contains(&scalar) {
                                if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                                    return Err(Error::corrupt(
                                        "high surrogate without a low one",
                                        at,
                                    ));
                                }
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err(Error::corrupt("invalid low surrogate", at));
                                }
                                scalar = 0x10000 + ((scalar - 0xD800) << 10) + (low - 0xDC00);
                            }
                            let c = char::from_u32(scalar)
                                .ok_or(Error::corrupt("lone surrogate", at))?;
                            let mut buffer = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                        }
                        _ => return Err(Error::corrupt("invalid escape", at)),
                    }
                }
                _ => out.push(b),
            }
            if out.len() > MAX_NAME {
                return Err(Error::corrupt("manifest string longer than 1 MiB", at));
            }
        }
        String::from_utf8(out).map_err(|_| Error::corrupt("manifest string is not UTF-8", self.pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_surrogates_round_trip() {
        let manifest = Manifest::parse("{\"a\\\"b\\\\c\\n\":\"f19c6cfd\",\"\\uD83D\\uDE80\":\"12345678\",\"\\u0000\":\"87654321\"}").unwrap();
        assert_eq!(manifest.len(), 3);
        assert_eq!(manifest.hash("a\"b\\c\n"), Some(0xF19C_6CFD));
        assert_eq!(manifest.hash("🚀"), Some(0x1234_5678));
        assert_eq!(manifest.hash("\0"), Some(0x8765_4321));
        let again = Manifest::parse(&manifest.to_json()).unwrap();
        assert_eq!(again, manifest);
    }

    #[test]
    fn malformed_manifests_are_refused() {
        for text in [
            "[]",
            "{",
            "{\"x\":3}",
            "{\"x\":\"123\"}",
            "{\"x\":\"gggggggg\"}",
            "{\"x\":\"12345678\",}",
            "{}x",
            "{\"x\":\"12345678\",\"x\":\"12345678\"}",
            "{\"\\uD800\":\"12345678\"}",
            "{\"\\uDC00\":\"12345678\"}",
            "{\"\\x\":\"12345678\"}",
            "{\"\n\":\"12345678\"}",
            "{\"x\":\"+2345678\"}",
        ] {
            assert!(Manifest::parse(text).is_err(), "{text}");
        }
        assert!(Manifest::parse(" { } ").unwrap().is_empty());
    }
}
