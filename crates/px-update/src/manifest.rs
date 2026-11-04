//! The update manifest: a hand-written, bounded, canonical text format.
//!
//! Not JSON, on purpose. A JSON parser has to decide what duplicate keys,
//! number spellings, escapes, unicode normalisation, comments and trailing
//! garbage mean, and a signed document is only as unambiguous as its least
//! unambiguous reader. This format has one spelling for every document.
//!
//! # Format (version 1)
//!
//! ASCII only (`0x20..=0x7e` and `\n`), LF line endings, a final newline, one
//! `key SP value` record per line, at most 16 KiB and 64 lines. Lines appear in
//! exactly this order. A release manifest:
//!
//! ```text
//! manifest-format 1
//! type release
//! channel stable
//! key-epoch 1
//! sequence 7
//! issued 1790000000
//! expires 1790604800
//! version 1.2.3
//! artifact <platform> <size> <sha256> <sha512> <path>     (0..=16, platform ascending)
//! data <psl|hsts|roots> <yyyymmdd> <size> <sha256> <sha512> <path>   (0..=3, psl < hsts < roots)
//! ```
//!
//! A rotation manifest (see `trust`):
//!
//! ```text
//! manifest-format 1
//! type rotation
//! key-epoch 2
//! issued 1790000000
//! expires 1790604800
//! threshold 1
//! key <64 hex chars of an Ed25519 public key>             (1..=8, ascending)
//! ```
//!
//! Numbers are canonical decimals (no sign, no leading zero). Hashes are
//! lowercase hex. Paths are relative to the mirror root and cannot contain a
//! query string, fragment, scheme, `..`, a dot-segment or an empty segment.
//!
//! The SIGNED unit is the exact byte string. [`Manifest::parse`] additionally
//! requires `parse(b).encode() == b`, so the parser accepts a document if and
//! only if it is the canonical encoding of the value it returns: there is no
//! second spelling to smuggle a difference between signer and verifier.

use crate::hex;
use crate::version::{Version, date_to_unix};

/// Manifest format version understood by this build.
pub const FORMAT_VERSION: u32 = 1;
/// Hard cap on a manifest, checked before parsing and before hashing.
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_LINES: usize = 64;
const MAX_LINE_BYTES: usize = 512;
/// At most this many artifacts in one release.
pub const MAX_ARTIFACTS: usize = 16;
/// At most this many keys in a rotated root set.
pub const MAX_KEYS: usize = 8;
/// Largest installable artifact (checked against the declared size, and
/// enforced again while downloading).
pub const MAX_ARTIFACT_SIZE: u64 = 1 << 30;
/// Largest data file.
pub const MAX_DATA_SIZE: u64 = 64 << 20;
/// Longest validity window a manifest may claim: 90 days.
pub const MAX_VALIDITY_SECS: u64 = 90 * 86_400;
const MAX_PATH_BYTES: usize = 128;

/// Why a document was refused. The text names the rule, for logs and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "manifest rejected: {}", self.0)
    }
}

/// The data files that ride the update channel. Declaration order is the
/// order they must appear in a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataKind {
    Psl,
    Hsts,
    Roots,
}

impl DataKind {
    pub const ALL: [DataKind; 3] = [DataKind::Psl, DataKind::Hsts, DataKind::Roots];

    pub fn name(self) -> &'static str {
        match self {
            DataKind::Psl => "psl",
            DataKind::Hsts => "hsts",
            DataKind::Roots => "roots",
        }
    }

    fn parse(s: &str) -> Option<DataKind> {
        DataKind::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// A file the manifest points at, with the checks a download must pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRef {
    pub size: u64,
    pub sha256: [u8; 32],
    pub sha512: [u8; 64],
    /// Path relative to the mirror root.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub platform: String,
    pub file: FileRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataEntry {
    pub kind: DataKind,
    /// `YYYYMMDD` of the upstream fetch (the `fetched` date in `data/*.version`).
    pub version: u32,
    pub file: FileRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub channel: String,
    pub key_epoch: u32,
    pub sequence: u64,
    pub issued: u64,
    pub expires: u64,
    pub version: Version,
    pub artifacts: Vec<Artifact>,
    pub data: Vec<DataEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rotation {
    /// The epoch this rotation creates; must be the current epoch + 1.
    pub key_epoch: u32,
    pub issued: u64,
    pub expires: u64,
    /// Signatures from this many distinct keys of the NEW set are required
    /// from the next rotation on.
    pub threshold: u8,
    pub keys: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Manifest {
    Release(Release),
    Rotation(Rotation),
}

// ---- parsing ---------------------------------------------------------------

fn err<T>(why: &'static str) -> Result<T, ParseError> {
    Err(ParseError(why))
}

fn dec_u64(v: &str) -> Result<u64, ParseError> {
    let b = v.as_bytes();
    if b.is_empty() || b.len() > 20 || !b.iter().all(u8::is_ascii_digit) {
        return err("number is not a canonical decimal");
    }
    if b.len() > 1 && b.first() == Some(&b'0') {
        return err("number has a leading zero");
    }
    v.parse().or(err("number out of range"))
}

fn dec_u32(v: &str) -> Result<u32, ParseError> {
    u32::try_from(dec_u64(v)?).or(err("number out of range"))
}

fn is_token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PATH_BYTES
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && p.split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

fn file_ref(
    size: &str,
    sha256: &str,
    sha512: &str,
    path: &str,
    cap: u64,
) -> Result<FileRef, ParseError> {
    let size = dec_u64(size)?;
    if size == 0 || size > cap {
        return err("file size is zero or above the cap");
    }
    let Some(sha256) = hex::decode_fixed::<32>(sha256) else {
        return err("sha256 is not 64 lowercase hex characters");
    };
    let Some(sha512) = hex::decode_fixed::<64>(sha512) else {
        return err("sha512 is not 128 lowercase hex characters");
    };
    if !valid_path(path) {
        return err("path is not a plain relative path");
    }
    Ok(FileRef {
        size,
        sha256,
        sha512,
        path: path.to_string(),
    })
}

struct Lines<'a> {
    rest: std::vec::IntoIter<(&'a str, &'a str)>,
    peeked: Option<(&'a str, &'a str)>,
}

impl<'a> Lines<'a> {
    fn peek(&mut self) -> Option<(&'a str, &'a str)> {
        if self.peeked.is_none() {
            self.peeked = self.rest.next();
        }
        self.peeked
    }
    fn next(&mut self) -> Option<(&'a str, &'a str)> {
        self.peek();
        self.peeked.take()
    }
    fn expect(&mut self, key: &'static str) -> Result<&'a str, ParseError> {
        match self.next() {
            Some((k, v)) if k == key => Ok(v),
            _ => err("a required field is missing or out of order"),
        }
    }
}

impl Manifest {
    /// Parse and validate. The only way to obtain a [`Manifest`] from bytes.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, ParseError> {
        if bytes.is_empty() {
            return err("empty");
        }
        if bytes.len() > MAX_MANIFEST_BYTES {
            return err("larger than the manifest size cap");
        }
        if !bytes
            .iter()
            .all(|b| *b == b'\n' || (0x20..=0x7e).contains(b))
        {
            return err("byte outside printable ASCII and LF");
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return err("not ASCII");
        };
        let Some(body) = text.strip_suffix('\n') else {
            return err("no final newline");
        };
        let mut recs = Vec::new();
        for line in body.split('\n') {
            if recs.len() >= MAX_LINES {
                return err("too many lines");
            }
            if line.len() > MAX_LINE_BYTES {
                return err("line too long");
            }
            let Some((k, v)) = line.split_once(' ') else {
                return err("line is not `key value`");
            };
            if k.is_empty() || v.is_empty() {
                return err("empty key or value");
            }
            recs.push((k, v));
        }
        let mut lines = Lines {
            rest: recs.into_iter(),
            peeked: None,
        };
        if lines.expect("manifest-format")? != FORMAT_VERSION.to_string() {
            return err("unsupported manifest format version");
        }
        let manifest = match lines.expect("type")? {
            "release" => Manifest::Release(parse_release(&mut lines)?),
            "rotation" => Manifest::Rotation(parse_rotation(&mut lines)?),
            _ => return err("unknown manifest type"),
        };
        if lines.next().is_some() {
            return err("unexpected trailing field");
        }
        if manifest.encode().as_bytes() != bytes {
            return err("not the canonical encoding");
        }
        Ok(manifest)
    }

    /// The canonical encoding. `parse(encode(m)) == m` for every valid `m`.
    pub fn encode(&self) -> String {
        let mut s = format!("manifest-format {FORMAT_VERSION}\n");
        match self {
            Manifest::Release(r) => {
                s.push_str("type release\n");
                s.push_str(&format!("channel {}\n", r.channel));
                s.push_str(&format!("key-epoch {}\n", r.key_epoch));
                s.push_str(&format!("sequence {}\n", r.sequence));
                s.push_str(&format!("issued {}\n", r.issued));
                s.push_str(&format!("expires {}\n", r.expires));
                s.push_str(&format!("version {}\n", r.version));
                for a in &r.artifacts {
                    s.push_str(&format!(
                        "artifact {} {}\n",
                        a.platform,
                        file_fields(&a.file)
                    ));
                }
                for d in &r.data {
                    s.push_str(&format!(
                        "data {} {} {}\n",
                        d.kind.name(),
                        d.version,
                        file_fields(&d.file)
                    ));
                }
            }
            Manifest::Rotation(r) => {
                s.push_str("type rotation\n");
                s.push_str(&format!("key-epoch {}\n", r.key_epoch));
                s.push_str(&format!("issued {}\n", r.issued));
                s.push_str(&format!("expires {}\n", r.expires));
                s.push_str(&format!("threshold {}\n", r.threshold));
                for k in &r.keys {
                    s.push_str(&format!("key {}\n", hex::encode(k)));
                }
            }
        }
        s
    }
}

fn file_fields(f: &FileRef) -> String {
    format!(
        "{} {} {} {}",
        f.size,
        hex::encode(&f.sha256),
        hex::encode(&f.sha512),
        f.path
    )
}

fn validity(issued: u64, expires: u64) -> Result<(), ParseError> {
    if issued == 0 || expires <= issued {
        return err("expires must be after issued");
    }
    if expires - issued > MAX_VALIDITY_SECS {
        return err("validity window exceeds 90 days");
    }
    Ok(())
}

fn parse_release(lines: &mut Lines<'_>) -> Result<Release, ParseError> {
    let channel = lines.expect("channel")?;
    if !is_token(channel, 16) {
        return err("channel is not a short lowercase token");
    }
    let key_epoch = dec_u32(lines.expect("key-epoch")?)?;
    let sequence = dec_u64(lines.expect("sequence")?)?;
    let issued = dec_u64(lines.expect("issued")?)?;
    let expires = dec_u64(lines.expect("expires")?)?;
    let Some(version) = Version::parse(lines.expect("version")?) else {
        return err("version is not MAJOR.MINOR.PATCH");
    };
    if key_epoch == 0 || sequence == 0 {
        return err("key-epoch and sequence start at 1");
    }
    validity(issued, expires)?;

    let mut artifacts: Vec<Artifact> = Vec::new();
    while let Some(("artifact", _)) = lines.peek() {
        let Some((_, v)) = lines.next() else { break };
        if artifacts.len() >= MAX_ARTIFACTS {
            return err("too many artifacts");
        }
        let f: Vec<&str> = v.split(' ').collect();
        let [platform, size, s256, s512, path] = f.as_slice() else {
            return err("artifact needs platform size sha256 sha512 path");
        };
        if !is_token(platform, 32) {
            return err("platform is not a short lowercase token");
        }
        if artifacts
            .last()
            .is_some_and(|p| p.platform.as_str() >= *platform)
        {
            return err("artifacts are not in strictly ascending platform order");
        }
        artifacts.push(Artifact {
            platform: (*platform).to_string(),
            file: file_ref(size, s256, s512, path, MAX_ARTIFACT_SIZE)?,
        });
    }

    let mut data: Vec<DataEntry> = Vec::new();
    while let Some(("data", _)) = lines.peek() {
        let Some((_, v)) = lines.next() else { break };
        let f: Vec<&str> = v.split(' ').collect();
        let [kind, version, size, s256, s512, path] = f.as_slice() else {
            return err("data needs kind version size sha256 sha512 path");
        };
        let Some(kind) = DataKind::parse(kind) else {
            return err("unknown data kind");
        };
        if data.last().is_some_and(|p| p.kind >= kind) {
            return err("data entries are not in strictly ascending kind order");
        }
        let version = dec_u32(version)?;
        if date_to_unix(version).is_none() {
            return err("data version is not a YYYYMMDD date");
        }
        data.push(DataEntry {
            kind,
            version,
            file: file_ref(size, s256, s512, path, MAX_DATA_SIZE)?,
        });
    }

    Ok(Release {
        channel: channel.to_string(),
        key_epoch,
        sequence,
        issued,
        expires,
        version,
        artifacts,
        data,
    })
}

fn parse_rotation(lines: &mut Lines<'_>) -> Result<Rotation, ParseError> {
    let key_epoch = dec_u32(lines.expect("key-epoch")?)?;
    let issued = dec_u64(lines.expect("issued")?)?;
    let expires = dec_u64(lines.expect("expires")?)?;
    let threshold = dec_u32(lines.expect("threshold")?)?;
    if key_epoch < 2 {
        return err("a rotation creates epoch 2 or later");
    }
    validity(issued, expires)?;
    let mut keys: Vec<[u8; 32]> = Vec::new();
    while let Some(("key", _)) = lines.peek() {
        let Some((_, v)) = lines.next() else { break };
        if keys.len() >= MAX_KEYS {
            return err("too many keys");
        }
        let Some(k) = hex::decode_fixed::<32>(v) else {
            return err("key is not 64 lowercase hex characters");
        };
        if keys.last().is_some_and(|p| *p >= k) {
            return err("keys are not in strictly ascending order");
        }
        keys.push(k);
    }
    let threshold = u8::try_from(threshold).or(err("threshold out of range"))?;
    if keys.is_empty() || threshold == 0 || usize::from(threshold) > keys.len() {
        return err("threshold must be between 1 and the number of keys");
    }
    Ok(Rotation {
        key_epoch,
        issued,
        expires,
        threshold,
        keys,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn file(path: &str, fill: u8) -> FileRef {
        FileRef {
            size: 1234,
            sha256: [fill; 32],
            sha512: [fill.wrapping_add(1); 64],
            path: path.to_string(),
        }
    }

    pub(crate) fn release() -> Release {
        Release {
            channel: "stable".into(),
            key_epoch: 1,
            sequence: 7,
            issued: 1_790_000_000,
            expires: 1_790_604_800,
            version: Version::new(1, 2, 3),
            artifacts: vec![
                Artifact {
                    platform: "linux-x86_64".into(),
                    file: file("releases/1.2.3/app-linux-x86_64.tar.zst", 0x11),
                },
                Artifact {
                    platform: "windows-x86_64".into(),
                    file: file("releases/1.2.3/app-windows-x86_64.zip", 0x22),
                },
            ],
            data: vec![
                DataEntry {
                    kind: DataKind::Psl,
                    version: 20_260_909,
                    file: file("data/psl-20260909.dat", 0x33),
                },
                DataEntry {
                    kind: DataKind::Hsts,
                    version: 20_260_909,
                    file: file("data/hsts-20260909.txt", 0x44),
                },
                DataEntry {
                    kind: DataKind::Roots,
                    version: 20_260_801,
                    file: file("data/roots-20260801.pem", 0x55),
                },
            ],
        }
    }

    pub(crate) fn rotation() -> Rotation {
        Rotation {
            key_epoch: 2,
            issued: 1_790_000_000,
            expires: 1_790_604_800,
            threshold: 1,
            keys: vec![[1u8; 32], [2u8; 32]],
        }
    }

    #[test]
    fn exact_canonical_bytes() {
        let text = Manifest::Release(release()).encode();
        let expected = "manifest-format 1\n\
type release\n\
channel stable\n\
key-epoch 1\n\
sequence 7\n\
issued 1790000000\n\
expires 1790604800\n\
version 1.2.3\n\
artifact linux-x86_64 1234 1111111111111111111111111111111111111111111111111111111111111111 12121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212 releases/1.2.3/app-linux-x86_64.tar.zst\n";
        assert!(text.starts_with(expected), "{text}");
        assert!(text.contains("\ndata psl 20260909 1234 3333"));
        assert!(text.ends_with(".pem\n"));
    }

    #[test]
    fn round_trips() {
        for m in [Manifest::Release(release()), Manifest::Rotation(rotation())] {
            assert_eq!(Manifest::parse(m.encode().as_bytes()), Ok(m));
        }
        let mut r = release();
        r.artifacts.clear();
        r.data.clear();
        let m = Manifest::Release(r);
        assert_eq!(Manifest::parse(m.encode().as_bytes()), Ok(m));
    }

    fn mutate(from: &str, to: &str) -> Vec<u8> {
        let t = Manifest::Release(release()).encode();
        assert!(t.contains(from), "{from}");
        t.replacen(from, to, 1).into_bytes()
    }

    #[test]
    fn rejects_non_canonical_and_malformed() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("leading zero", mutate("sequence 7", "sequence 07")),
            ("plus sign", mutate("sequence 7", "sequence +7")),
            ("hex upper", mutate("1111111111111111", "111111111111111A")),
            (
                "short hash",
                mutate(
                    "artifact linux-x86_64 1234 11",
                    "artifact linux-x86_64 1234 1",
                ),
            ),
            ("double space", mutate("channel stable", "channel  stable")),
            (
                "trailing space",
                mutate("channel stable\n", "channel stable \n"),
            ),
            ("crlf", mutate("channel stable\n", "channel stable\r\n")),
            ("tab", mutate("channel stable", "channel\tstable")),
            (
                "field order",
                mutate(
                    "sequence 7\nissued 1790000000\n",
                    "issued 1790000000\nsequence 7\n",
                ),
            ),
            (
                "duplicate field",
                mutate("sequence 7\n", "sequence 7\nsequence 7\n"),
            ),
            (
                "unknown field",
                mutate("version 1.2.3\n", "version 1.2.3\nfoo bar\n"),
            ),
            ("bad version", mutate("version 1.2.3", "version 1.2")),
            ("format 2", mutate("manifest-format 1", "manifest-format 2")),
            ("unknown type", mutate("type release", "type other")),
            (
                "query in path",
                mutate("app-windows-x86_64.zip", "app-windows-x86_64.zip?x=1"),
            ),
            (
                "dotdot path",
                mutate("releases/1.2.3/app-windows", "releases/../app-windows"),
            ),
            (
                "absolute path",
                mutate(
                    " releases/1.2.3/app-windows",
                    " /releases/1.2.3/app-windows",
                ),
            ),
            (
                "scheme path",
                mutate("releases/1.2.3/app-windows", "https://x/app-windows"),
            ),
            (
                "empty segment",
                mutate("releases/1.2.3/app-windows", "releases//app-windows"),
            ),
            ("zero size", mutate("1234 2222", "0 2222")),
            ("huge size", mutate("1234 2222", "99999999999 2222")),
            (
                "expires before issued",
                mutate("expires 1790604800", "expires 1789999999"),
            ),
            (
                "validity too long",
                mutate("expires 1790604800", "expires 1890604800"),
            ),
            ("zero sequence", mutate("sequence 7", "sequence 0")),
            ("platform order", mutate("linux-x86_64", "xlinux-x86_64")),
            (
                "uppercase channel",
                mutate("channel stable", "channel Stable"),
            ),
            ("bad date", mutate("psl 20260909", "psl 20261309")),
            ("unknown data kind", mutate("data psl", "data dns")),
            ("no final newline", {
                let mut v = Manifest::Release(release()).encode().into_bytes();
                v.pop();
                v
            }),
            ("non-ascii", mutate("channel stable", "channel stablé")),
            ("nul", mutate("channel stable", "channel sta\0ble")),
            ("empty", Vec::new()),
            ("only newline", b"\n".to_vec()),
        ];
        for (name, bytes) in cases {
            assert!(Manifest::parse(&bytes).is_err(), "accepted: {name}");
        }
    }

    #[test]
    fn data_order_and_duplicates_rejected() {
        let mut r = release();
        r.data.swap(0, 1);
        assert!(Manifest::parse(Manifest::Release(r).encode().as_bytes()).is_err());
        let mut r = release();
        r.artifacts.push(r.artifacts[1].clone());
        assert!(Manifest::parse(Manifest::Release(r).encode().as_bytes()).is_err());
        let mut r = release();
        r.data.push(r.data[2].clone());
        assert!(Manifest::parse(Manifest::Release(r).encode().as_bytes()).is_err());
    }

    #[test]
    fn bounds() {
        let mut r = release();
        r.artifacts = (0..=MAX_ARTIFACTS)
            .map(|i| Artifact {
                platform: format!("p{i:02}"),
                file: file("a/b", 1),
            })
            .collect();
        assert!(Manifest::parse(Manifest::Release(r).encode().as_bytes()).is_err());
        let mut r = release();
        r.artifacts = (0..MAX_ARTIFACTS)
            .map(|i| Artifact {
                platform: format!("p{i:02}"),
                file: file("a/b", 1),
            })
            .collect();
        assert!(Manifest::parse(Manifest::Release(r).encode().as_bytes()).is_ok());
        assert!(Manifest::parse(&vec![b'a'; MAX_MANIFEST_BYTES + 1]).is_err());
        let mut long = Manifest::Release(release()).encode();
        long.push_str(&"x ".repeat(MAX_MANIFEST_BYTES));
        assert_eq!(
            Manifest::parse(long.as_bytes()),
            Err(ParseError("larger than the manifest size cap"))
        );
    }

    #[test]
    fn rotation_rules() {
        let mut r = rotation();
        r.threshold = 0;
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
        let mut r = rotation();
        r.threshold = 3;
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
        let mut r = rotation();
        r.keys.reverse();
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
        let mut r = rotation();
        r.keys.clear();
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
        let mut r = rotation();
        r.key_epoch = 1;
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
        let mut r = rotation();
        r.keys = (1..=9u8).map(|i| [i; 32]).collect();
        assert!(Manifest::parse(Manifest::Rotation(r).encode().as_bytes()).is_err());
    }

    #[test]
    fn hostile_bytes_never_panic() {
        // A cheap deterministic stir over the canonical text: every single
        // byte replaced by every printable value and by control bytes.
        let base = Manifest::Release(release()).encode().into_bytes();
        for i in 0..base.len() {
            for b in [0u8, b'\n', b' ', b'0', b'z', 0x7f, 0xff] {
                let mut m = base.clone();
                m[i] = b;
                if let Ok(parsed) = Manifest::parse(&m) {
                    // Anything accepted must be canonical.
                    assert_eq!(parsed.encode().as_bytes(), &m[..]);
                }
            }
            let mut m = base.clone();
            m.truncate(i);
            let _ = Manifest::parse(&m);
        }
    }
}
