//! How a filesystem path becomes JSON, and back.
//!
//! These encoders sit behind `#[serde(with = ...)]` on the packet, so a path
//! that is not UTF-8 survives into the JSON instead of being lost.
//!
//! A valid-UTF-8 path is a plain JSON string. On Unix a path that is not valid
//! UTF-8 still has to round-trip exactly, so it serializes as
//! `{"bytes_hex": "..."}` — lossy decoding would silently rename a file inside
//! an artifact that claims to describe it.
//!
//! `display` escapes control characters and invalid bytes, so a filename cannot
//! forge a heading or an ANSI escape in human output.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::ser::SerializeSeq as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Deserialize)]
#[serde(untagged)]
enum EncodedPath {
    Text(String),
    Bytes { bytes_hex: String },
}

struct PathRef<'a>(&'a Path);

impl Serialize for PathRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(text) = self.0.to_str() {
            serializer.serialize_str(text)
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt as _;
                #[derive(Serialize)]
                struct Bytes {
                    bytes_hex: String,
                }
                Bytes {
                    bytes_hex: hex(self.0.as_os_str().as_bytes()),
                }
                .serialize(serializer)
            }
            #[cfg(not(unix))]
            {
                Err(serde::ser::Error::custom("path is not valid UTF-8"))
            }
        }
    }
}

fn decode<E: serde::de::Error>(encoded: EncodedPath) -> Result<PathBuf, E> {
    match encoded {
        EncodedPath::Text(text) => Ok(PathBuf::from(text)),
        EncodedPath::Bytes { bytes_hex } => {
            let bytes = unhex(&bytes_hex).map_err(E::custom)?;
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt as _;
                Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
            }
            #[cfg(not(unix))]
            {
                let _ = bytes;
                Err(E::custom("raw path bytes are only supported on Unix"))
            }
        }
    }
}

pub mod one {
    use super::*;

    pub fn serialize<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        PathRef(path).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        decode(EncodedPath::deserialize(deserializer)?)
    }
}

pub mod option {
    use super::*;

    pub fn serialize<S>(path: &Option<PathBuf>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        path.as_deref().map(PathRef).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<EncodedPath>::deserialize(deserializer)?
            .map(decode)
            .transpose()
    }
}

pub mod vec {
    use super::*;

    pub fn serialize<S>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(paths.len()))?;
        for path in paths {
            sequence.serialize_element(&PathRef(path))?;
        }
        sequence.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<EncodedPath>::deserialize(deserializer)?
            .into_iter()
            .map(decode)
            .collect()
    }
}

pub fn from_git_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

pub fn display(path: &Path) -> String {
    if let Some(text) = path.to_str() {
        return text.chars().fold(String::new(), |mut output, character| {
            match character {
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                '\\' => output.push_str("\\\\"),
                value if value.is_control() => {
                    let _ = write!(output, "\\u{{{:x}}}", value as u32);
                }
                value => output.push(value),
            }
            output
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str()
            .as_bytes()
            .iter()
            .fold(String::new(), |mut output, byte| {
                if byte.is_ascii_graphic() && *byte != b'\\' {
                    output.push(*byte as char);
                } else if *byte == b' ' {
                    output.push(' ');
                } else {
                    let _ = write!(output, "\\x{byte:02x}");
                }
                output
            })
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn unhex(text: &str) -> Result<Vec<u8>, &'static str> {
    if !text.len().is_multiple_of(2) {
        return Err("path bytes_hex must contain an even number of digits");
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digits = std::str::from_utf8(pair).map_err(|_| "path bytes_hex is not ASCII")?;
            u8::from_str_radix(digits, 16).map_err(|_| "path bytes_hex contains a non-hex digit")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_round_trips_through_the_encoded_form() {
        use std::os::unix::ffi::OsStringExt as _;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0xff, b'\n']));
        let json = serde_json::to_string(&PathRef(&path)).unwrap();
        assert_eq!(json, r#"{"bytes_hex":"61ff0a"}"#);
        let decoded = decode::<serde_json::Error>(serde_json::from_str(&json).unwrap()).unwrap();
        assert_eq!(decoded, path);
        assert_eq!(display(&path), r"a\xff\x0a");
    }
}
