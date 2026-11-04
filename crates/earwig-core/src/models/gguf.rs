//! Just enough of the GGUF format (v2/v3, little-endian) to read one
//! metadata string from the start of a file, so search results can be
//! checked against the engine before anything is downloaded.

/// Outcome of scanning a header prefix for a string key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    Found(String),
    /// The buffer ends before the key was reached; at least this many bytes
    /// from the start of the file are needed to get further.
    NeedMore(usize),
    /// The metadata has no such key (or it is not a string).
    Absent,
    Invalid(&'static str),
}

const MAGIC: &[u8; 4] = b"GGUF";
/// Sanity bounds so a corrupt or hostile header cannot make us ask for
/// gigabytes.
const MAX_KEYS: u64 = 1 << 20;
const MAX_STRING: u64 = 1 << 24;
const MAX_ARRAY: u64 = 1 << 28;

const TYPE_STRING: u32 = 8;
const TYPE_ARRAY: u32 = 9;

/// Scan `buf` (the first bytes of a GGUF file) for the string value of
/// `key`, e.g. `general.architecture`. Stops at the key.
pub fn find_string(buf: &[u8], key: &str) -> Scan {
    let mut r = Reader { buf, pos: 0 };
    match scan(&mut r, key) {
        Ok(scan) => scan,
        Err(Stop::Short(need)) => Scan::NeedMore(need),
        Err(Stop::Invalid(why)) => Scan::Invalid(why),
    }
}

enum Stop {
    Short(usize),
    Invalid(&'static str),
}

fn scan(r: &mut Reader<'_>, key: &str) -> Result<Scan, Stop> {
    if r.take(4)? != MAGIC {
        return Err(Stop::Invalid("not a GGUF file"));
    }
    let version = r.u32()?;
    if !matches!(version, 2 | 3) {
        return Err(Stop::Invalid("unsupported GGUF version"));
    }
    let _tensors = r.u64()?;
    let keys = r.u64()?;
    if keys > MAX_KEYS {
        return Err(Stop::Invalid("implausible metadata key count"));
    }
    for _ in 0..keys {
        let name = r.string()?;
        let ty = r.u32()?;
        if name == key.as_bytes() {
            if ty != TYPE_STRING {
                return Ok(Scan::Absent);
            }
            let value = r.string()?;
            return String::from_utf8(value.to_vec())
                .map(Scan::Found)
                .map_err(|_| Stop::Invalid("value is not UTF-8"));
        }
        r.skip_value(ty)?;
    }
    Ok(Scan::Absent)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Stop> {
        let end = self.pos.checked_add(n).ok_or(Stop::Invalid("overflow"))?;
        let bytes = self.buf.get(self.pos..end).ok_or(Stop::Short(end))?;
        self.pos = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, Stop> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    fn u64(&mut self) -> Result<u64, Stop> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    fn string(&mut self) -> Result<&'a [u8], Stop> {
        let len = self.u64()?;
        if len > MAX_STRING {
            return Err(Stop::Invalid("implausible string length"));
        }
        self.take(usize::try_from(len).map_err(|_| Stop::Invalid("overflow"))?)
    }

    fn skip_value(&mut self, ty: u32) -> Result<(), Stop> {
        match ty {
            TYPE_STRING => {
                self.string()?;
            }
            TYPE_ARRAY => {
                let item = self.u32()?;
                let count = self.u64()?;
                if count > MAX_ARRAY {
                    return Err(Stop::Invalid("implausible array length"));
                }
                if let Some(width) = scalar_width(item) {
                    let bytes = count
                        .checked_mul(width as u64)
                        .and_then(|b| usize::try_from(b).ok())
                        .ok_or(Stop::Invalid("overflow"))?;
                    self.take(bytes)?;
                } else {
                    for _ in 0..count {
                        self.skip_value(item)?;
                    }
                }
            }
            _ => {
                let width = scalar_width(ty).ok_or(Stop::Invalid("unknown value type"))?;
                self.take(width)?;
            }
        }
        Ok(())
    }
}

fn scalar_width(ty: u32) -> Option<usize> {
    Some(match ty {
        0 | 1 | 7 => 1, // u8, i8, bool
        2 | 3 => 2,     // u16, i16
        4..=6 => 4,     // u32, i32, f32
        10..=12 => 8,   // u64, i64, f64
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GGUF v3 header with the given metadata, values pre-encoded.
    fn header(version: u32, kvs: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(version.to_le_bytes());
        out.extend(7u64.to_le_bytes());
        out.extend((kvs.len() as u64).to_le_bytes());
        for (key, ty, value) in kvs {
            out.extend(str_bytes(key));
            out.extend(ty.to_le_bytes());
            out.extend(value);
        }
        out
    }

    fn str_bytes(s: &str) -> Vec<u8> {
        let mut out = (s.len() as u64).to_le_bytes().to_vec();
        out.extend(s.as_bytes());
        out
    }

    fn string_array(items: &[&str]) -> Vec<u8> {
        let mut out = TYPE_STRING.to_le_bytes().to_vec();
        out.extend((items.len() as u64).to_le_bytes());
        for item in items {
            out.extend(str_bytes(item));
        }
        out
    }

    fn sample() -> Vec<u8> {
        let mut floats = 6u32.to_le_bytes().to_vec();
        floats.extend(3u64.to_le_bytes());
        floats.extend([0u8; 12]);
        header(
            3,
            &[
                ("general.alignment", 4, 32u32.to_le_bytes().to_vec()),
                ("general.name", TYPE_STRING, str_bytes("tiny")),
                (
                    "tokenizer.tokens",
                    TYPE_ARRAY,
                    string_array(&["a", "bc", "def"]),
                ),
                ("audio.window", TYPE_ARRAY, floats),
                ("general.flag", 7, vec![1]),
                ("general.architecture", TYPE_STRING, str_bytes("parakeet")),
                ("after", 10, 1u64.to_le_bytes().to_vec()),
            ],
        )
    }

    #[test]
    fn finds_key_after_other_keys_of_every_shape() {
        assert_eq!(
            find_string(&sample(), "general.architecture"),
            Scan::Found("parakeet".into())
        );
        assert_eq!(
            find_string(&sample(), "general.name"),
            Scan::Found("tiny".into())
        );
    }

    #[test]
    fn version_2_is_read() {
        let buf = header(
            2,
            &[("general.architecture", TYPE_STRING, str_bytes("whisper"))],
        );
        assert_eq!(
            find_string(&buf, "general.architecture"),
            Scan::Found("whisper".into())
        );
    }

    #[test]
    fn missing_or_non_string_key_is_absent() {
        assert_eq!(find_string(&sample(), "nope"), Scan::Absent);
        assert_eq!(find_string(&sample(), "general.alignment"), Scan::Absent);
    }

    #[test]
    fn truncated_headers_ask_for_more_until_the_value_is_complete() {
        let full = sample();
        let value_end = full.len() - (8 + "after".len() + 4 + 8);
        for cut in 0..full.len() {
            match find_string(&full[..cut], "general.architecture") {
                Scan::NeedMore(need) => {
                    assert!(cut < value_end, "cut {cut} past the value asked for more");
                    assert!(need > cut && need <= value_end, "cut {cut}: need {need}");
                }
                Scan::Found(arch) => {
                    assert!(cut >= value_end, "cut {cut} found before the value ended");
                    assert_eq!(arch, "parakeet");
                }
                other => panic!("cut {cut}: {other:?}"),
            }
        }
    }

    #[test]
    fn garbage_is_invalid() {
        assert!(matches!(
            find_string(b"GGML\0\0\0\0", "k"),
            Scan::Invalid(_)
        ));
        let mut v1 = sample();
        v1[4] = 1;
        assert!(matches!(find_string(&v1, "k"), Scan::Invalid(_)));
        let mut huge = header(3, &[]);
        huge[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(find_string(&huge, "k"), Scan::Invalid(_)));
        let unknown = header(
            3,
            &[
                ("x", 99, vec![]),
                ("general.architecture", 8, str_bytes("p")),
            ],
        );
        assert!(matches!(
            find_string(&unknown, "general.architecture"),
            Scan::Invalid(_)
        ));
    }
}
