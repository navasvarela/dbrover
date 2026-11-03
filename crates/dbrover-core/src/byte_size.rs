//! A number of bytes as people and settings files write one: `"500 MB"`, `"1 GiB"`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A number of bytes, as a report file or `settings.toml` writes it: `"500 MB"`, `"1 GiB"`,
/// or a number of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl ByteSize {
    /// The units a size can be written in, decimal and binary, as people and operating
    /// systems write them.
    const UNITS: [(&str, u64); 10] = [
        ("B", 1),
        ("BYTES", 1),
        ("KB", 1_000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("TB", 1_000_000_000_000),
        ("KIB", 1 << 10),
        ("MIB", 1 << 20),
        ("GIB", 1 << 30),
        ("TIB", 1 << 40),
    ];

    /// `"500 MB"`, `"1GiB"`, `"2 gb"`: a whole number and a unit, or a whole number alone.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let digits_end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (number, unit) = text.split_at(digits_end);
        let number: u64 = number.parse().ok()?;
        let unit = unit.trim().to_ascii_uppercase();
        let multiplier = if unit.is_empty() {
            1
        } else {
            Self::UNITS
                .iter()
                .find(|(name, _)| *name == unit)
                .map(|(_, multiplier)| *multiplier)?
        };
        number.checked_mul(multiplier).map(Self)
    }
}

/// In the largest unit it is a whole number of, as a report file would write it (`1 GiB`,
/// `500 MB`), so an error names the limit as it was set; else in bytes.
impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const LARGEST_FIRST: [(&str, u64); 8] = [
            ("TiB", 1 << 40),
            ("TB", 1_000_000_000_000),
            ("GiB", 1 << 30),
            ("GB", 1_000_000_000),
            ("MiB", 1 << 20),
            ("MB", 1_000_000),
            ("KiB", 1 << 10),
            ("KB", 1_000),
        ];
        match LARGEST_FIRST
            .iter()
            .find(|(_, unit)| self.0 >= *unit && self.0.is_multiple_of(*unit))
        {
            Some((name, unit)) => write!(f, "{} {name}", self.0 / unit),
            None => write!(f, "{} bytes", self.0),
        }
    }
}

/// As its text, `"1 GiB"`, so a settings file reads as a person would write it.
impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// From a size's text or a number of bytes. Zero is refused: no limit DbRover sets by a size
/// means anything at zero.
impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Text(String),
            Bytes(u64),
        }
        let size = match Written::deserialize(deserializer)? {
            Written::Text(text) => Self::parse(&text).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "{text:?} isn't a size such as \"500 MB\" or \"1 GiB\""
                ))
            })?,
            Written::Bytes(bytes) => Self(bytes),
        };
        if size.0 == 0 {
            return Err(serde::de::Error::custom("a size must be more than zero"));
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sizes_as_people_write_them() {
        let cases = [
            ("500 MB", Some(500_000_000)),
            ("1GiB", Some(1 << 30)),
            ("2 gb", Some(2_000_000_000)),
            ("4096", Some(4096)),
            ("12 B", Some(12)),
            ("1.5 GB", None),
            ("MB", None),
            ("-1 MB", None),
            ("5 parsecs", None),
            ("99999999999 TB", None),
        ];
        for (text, bytes) in cases {
            assert_eq!(ByteSize::parse(text), bytes.map(ByteSize), "{text:?}");
        }
        assert_eq!(ByteSize(1 << 30).to_string(), "1 GiB");
        assert_eq!(ByteSize(500_000_000).to_string(), "500 MB");
        assert_eq!(ByteSize(1_500).to_string(), "1500 bytes");
        assert_eq!(ByteSize::parse("1500 bytes"), Some(ByteSize(1_500)));
    }

    #[test]
    fn reads_and_writes_a_size_as_its_text() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Limits {
            size: ByteSize,
        }
        let cases = [
            ("size = \"2 GiB\"\n", Some(ByteSize(2 << 30))),
            ("size = 4096\n", Some(ByteSize(4096))),
            ("size = \"0 MB\"\n", None),
            ("size = 0\n", None),
            ("size = \"lots\"\n", None),
        ];
        for (text, size) in cases {
            let read = toml::from_str::<Limits>(text)
                .ok()
                .map(|limits| limits.size);
            assert_eq!(read, size, "{text:?}");
        }
        let written = toml::to_string(&Limits {
            size: ByteSize(1_500),
        })
        .unwrap();
        assert_eq!(written, "size = \"1500 bytes\"\n");
        assert_eq!(
            toml::from_str::<Limits>(&written).unwrap().size,
            ByteSize(1_500)
        );
    }
}
