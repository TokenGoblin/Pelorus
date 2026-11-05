//! System font discovery as a pure scan over an injected [`DirReader`].
//!
//! `/CLAUDE.md` allows filesystem access only in px-broker and px-store, so this
//! module never touches `std::fs`. It asks a [`DirReader`] for directory listings,
//! metadata and bytes; px-broker supplies the real one (`system_fonts` there) and
//! tests supply an in-memory fake. No fontconfig, no DirectWrite (ADR 036 (f)).
//!
//! # Bounds (hostile disks, hostile files)
//!
//! [`ScanLimits`]: directory depth, files examined, directories visited, bytes per
//! file, total bytes read, faces kept, and name length. A file or directory over a
//! bound is skipped, never an error; a scan that hits a global cap stops and keeps
//! what it has. Symlinks are **never followed** (the reader reports them as
//! [`EntryKind::Symlink`] and the scan skips them), so a link cannot lead out of
//! the tree. Directory entries are sorted by name before use, so the result
//! order is a pure function of the tree and not of the OS's listing order.
//!
//! # What is read
//!
//! `.ttf`, `.otf`, `.ttc` (and `.otc`; extension matched ASCII case-insensitively).
//! From each face: the `name` table (family, subfamily, typographic family and
//! subfamily, full name), `OS/2.usWeightClass` and `fsSelection`, `head.macStyle`.
//! Every read goes through bounds-checked helpers; hostile bytes give a skipped
//! face. Faces are not validated beyond that: a system face is never handed to
//! the deterministic render path (see the registry documentation).
//!
//! # Preference lists
//!
//! [`GenericFamily::preferences`] are our own data, in order; fontconfig aliases
//! are not interpreted. [`SystemFonts::resolve_generic`] returns the first
//! preference that is installed.

use std::io;
use std::path::{Path, PathBuf};

use crate::sfnt::{u16_at, u32_at};

/// What a directory entry is. Symlinks are reported as such, never resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// What [`DirReader::metadata`] knows about a path (without following links).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: EntryKind,
    pub len: u64,
}

/// The only way the scan reaches a file system.
pub trait DirReader {
    /// The names (not paths) of the entries of `dir`. Order is not relied on.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// The kind and length of `path`, **not** following a symlink.
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    /// At most `max_len` bytes of the file; `Err` if it is longer (so a reader
    /// never allocates more than the cap).
    fn read_bytes(&self, path: &Path, max_len: u64) -> io::Result<Vec<u8>>;
}

/// Caps on one scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanLimits {
    /// Deepest directory below a root that is entered (the root is depth 0).
    pub max_depth: u32,
    /// Most directories listed in total.
    pub max_dirs: u32,
    /// Most directory entries examined in total.
    pub max_files: u32,
    /// Largest font file read, in bytes.
    pub max_file_bytes: u64,
    /// Most bytes read in total.
    pub max_total_bytes: u64,
    /// Most faces kept.
    pub max_faces: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        ScanLimits {
            max_depth: 8,
            max_dirs: 4096,
            max_files: 20_000,
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024,
            max_faces: 8192,
        }
    }
}

/// Longest family or style string kept, in `char`s.
pub const MAX_NAME_CHARS: usize = 128;
/// Most faces read from one collection.
pub const MAX_FACES_PER_FILE: u32 = 64;

/// One installed face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemFace {
    pub path: PathBuf,
    /// Index within a collection (0 for a single font).
    pub index: u32,
    /// The family to match: the typographic family (name ID 16) if present,
    /// else the family (ID 1).
    pub family: String,
    /// The style name (ID 17, else ID 2), such as `Bold Italic`.
    pub style: String,
    /// The legacy family (ID 1), which differs from `family` for fonts such as
    /// `Foo Light` filed under `Foo`.
    pub legacy_family: String,
    /// `OS/2.usWeightClass`, 100..=900 (400 when the table is missing).
    pub weight: u16,
    pub italic: bool,
}

/// The CSS generic families with preference lists of our own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenericFamily {
    Serif,
    SansSerif,
    Monospace,
    Cursive,
    Fantasy,
    SystemUi,
}

impl GenericFamily {
    /// Parse a CSS keyword (ASCII case-insensitive).
    pub fn from_keyword(s: &str) -> Option<GenericFamily> {
        const TABLE: [(&str, GenericFamily); 6] = [
            ("serif", GenericFamily::Serif),
            ("sans-serif", GenericFamily::SansSerif),
            ("monospace", GenericFamily::Monospace),
            ("cursive", GenericFamily::Cursive),
            ("fantasy", GenericFamily::Fantasy),
            ("system-ui", GenericFamily::SystemUi),
        ];
        TABLE
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(s))
            .map(|(_, g)| *g)
    }

    /// Family names, most preferred first. Windows names, then common Linux
    /// ones; all that are installed are candidates, the first wins.
    pub fn preferences(self) -> &'static [&'static str] {
        match self {
            GenericFamily::SansSerif => &[
                "Segoe UI",
                "Arial",
                "Helvetica",
                "Liberation Sans",
                "DejaVu Sans",
                "Noto Sans",
                "Roboto",
                "Open Sans",
                "Ubuntu",
                "Cantarell",
                "FreeSans",
            ],
            GenericFamily::Serif => &[
                "Times New Roman",
                "Georgia",
                "Liberation Serif",
                "DejaVu Serif",
                "Noto Serif",
                "Times",
                "FreeSerif",
            ],
            GenericFamily::Monospace => &[
                "Consolas",
                "Cascadia Mono",
                "Courier New",
                "Liberation Mono",
                "DejaVu Sans Mono",
                "Noto Sans Mono",
                "Ubuntu Mono",
                "FreeMono",
            ],
            GenericFamily::Cursive => &[
                "Comic Sans MS",
                "Segoe Script",
                "Comic Neue",
                "URW Chancery L",
                "Z003",
                "Lucida Handwriting",
            ],
            GenericFamily::Fantasy => &["Impact", "Papyrus", "Copperplate", "Haettenschweiler"],
            GenericFamily::SystemUi => &[
                "Segoe UI",
                "Cantarell",
                "Ubuntu",
                "Noto Sans",
                "Roboto",
                "DejaVu Sans",
                "Liberation Sans",
                "Arial",
            ],
        }
    }
}

/// The installed faces, in deterministic scan order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemFonts {
    faces: Vec<SystemFace>,
}

/// Host operating system, for [`default_roots`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
}

/// The directories to scan, in order, given an environment lookup.
///
/// Linux: `/usr/share/fonts`, `/usr/local/share/fonts`, `$HOME/.local/share/fonts`
/// (or `$XDG_DATA_HOME/fonts`), `$HOME/.fonts`, then `<dir>/fonts` for each entry
/// of `$XDG_DATA_DIRS`. Windows: `%SystemRoot%\Fonts`, `%LOCALAPPDATA%\Microsoft\
/// Windows\Fonts`. Duplicates are dropped, first occurrence kept. (The Windows
/// branch is untested on Windows here; it is a pure function of the lookup and
/// is unit-tested with a fake one.)
pub fn default_roots(platform: Platform, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    let nonempty = |k: &str| env(k).filter(|v| !v.is_empty());
    match platform {
        Platform::Linux => {
            push(PathBuf::from("/usr/share/fonts"));
            push(PathBuf::from("/usr/local/share/fonts"));
            if let Some(x) = nonempty("XDG_DATA_HOME") {
                push(Path::new(&x).join("fonts"));
            } else if let Some(h) = nonempty("HOME") {
                push(Path::new(&h).join(".local/share/fonts"));
            }
            if let Some(h) = nonempty("HOME") {
                push(Path::new(&h).join(".fonts"));
            }
            if let Some(dirs) = nonempty("XDG_DATA_DIRS") {
                for d in dirs.split(':').filter(|d| !d.is_empty()) {
                    push(Path::new(d).join("fonts"));
                }
            }
        }
        Platform::Windows => {
            if let Some(r) = nonempty("SystemRoot") {
                push(Path::new(&r).join("Fonts"));
            }
            if let Some(l) = nonempty("LOCALAPPDATA") {
                push(Path::new(&l).join("Microsoft").join("Windows").join("Fonts"));
            }
        }
    }
    out
}

impl SystemFonts {
    /// Scan `roots` in order. Deterministic: same tree, same result.
    pub fn scan(reader: &dyn DirReader, roots: &[PathBuf], limits: &ScanLimits) -> SystemFonts {
        let mut st = Scan {
            reader,
            limits,
            dirs: 0,
            files: 0,
            bytes: 0,
            faces: Vec::new(),
        };
        for root in roots {
            if st.exhausted() {
                break;
            }
            // Iterative depth-first walk. The stack is pushed in reverse sorted
            // order so entries pop in sorted order.
            let mut stack: Vec<(PathBuf, u32)> = vec![(root.clone(), 0)];
            while let Some((dir, depth)) = stack.pop() {
                if st.exhausted() {
                    break;
                }
                st.visit_dir(&dir, depth, &mut stack);
            }
        }
        SystemFonts { faces: st.faces }
    }

    pub fn faces(&self) -> &[SystemFace] {
        &self.faces
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// The best face of `family` (ASCII case-insensitive, against both the
    /// typographic and the legacy family) for a weight and slant: smallest
    /// weight distance, an italic mismatch costing 1000, ties to scan order.
    pub fn best_match(&self, family: &str, weight: u16, italic: bool) -> Option<&SystemFace> {
        self.faces
            .iter()
            .filter(|f| {
                f.family.eq_ignore_ascii_case(family) || f.legacy_family.eq_ignore_ascii_case(family)
            })
            .min_by_key(|f| {
                let w = i32::from(f.weight) - i32::from(weight);
                w.unsigned_abs() + if f.italic == italic { 0 } else { 1000 }
            })
    }

    /// The face for a generic family: the first installed entry of its
    /// preference list, regular weight, upright.
    pub fn resolve_generic(&self, g: GenericFamily) -> Option<&SystemFace> {
        g.preferences()
            .iter()
            .find_map(|name| self.best_match(name, 400, false))
    }

    /// A family name or a generic keyword.
    pub fn lookup(&self, name: &str, weight: u16, italic: bool) -> Option<&SystemFace> {
        match GenericFamily::from_keyword(name) {
            Some(g) => self.resolve_generic(g),
            None => self.best_match(name, weight, italic),
        }
    }
}

struct Scan<'a> {
    reader: &'a dyn DirReader,
    limits: &'a ScanLimits,
    dirs: u32,
    files: u32,
    bytes: u64,
    faces: Vec<SystemFace>,
}

impl Scan<'_> {
    fn exhausted(&self) -> bool {
        self.dirs >= self.limits.max_dirs
            || self.files >= self.limits.max_files
            || self.bytes >= self.limits.max_total_bytes
            || self.faces.len() >= self.limits.max_faces
    }

    fn visit_dir(&mut self, dir: &Path, depth: u32, stack: &mut Vec<(PathBuf, u32)>) {
        self.dirs += 1;
        let Ok(mut names) = self.reader.list_dir(dir) else {
            return;
        };
        names.sort();
        names.dedup();
        let mut subdirs: Vec<PathBuf> = Vec::new();
        for name in names {
            // The directory cap was charged when this directory was entered, so
            // it does not cut short a directory already being read.
            if self.files >= self.limits.max_files
                || self.bytes >= self.limits.max_total_bytes
                || self.faces.len() >= self.limits.max_faces
            {
                break;
            }
            self.files += 1;
            // A name with a separator or a dot-dot would step outside `dir`.
            if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
                continue;
            }
            let path = dir.join(&name);
            let Ok(meta) = self.reader.metadata(&path) else {
                continue;
            };
            match meta.kind {
                EntryKind::Dir => {
                    if depth < self.limits.max_depth {
                        subdirs.push(path);
                    }
                }
                EntryKind::File => {
                    if is_font_extension(&name) {
                        self.read_font(&path, meta.len);
                    }
                }
                EntryKind::Symlink | EntryKind::Other => {}
            }
        }
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth + 1));
        }
    }

    fn read_font(&mut self, path: &Path, len: u64) {
        if len > self.limits.max_file_bytes
            || self.bytes.saturating_add(len) > self.limits.max_total_bytes
        {
            return;
        }
        let Ok(data) = self.reader.read_bytes(path, self.limits.max_file_bytes) else {
            return;
        };
        self.bytes = self.bytes.saturating_add(data.len() as u64);
        for (index, face) in parse_faces(&data) {
            if self.faces.len() >= self.limits.max_faces {
                break;
            }
            self.faces.push(SystemFace {
                path: path.to_owned(),
                index,
                family: face.family,
                style: face.style,
                legacy_family: face.legacy_family,
                weight: face.weight,
                italic: face.italic,
            });
        }
    }
}

fn is_font_extension(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    ["ttf", "otf", "ttc", "otc"]
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

/// What the name/OS/2/head tables of one face say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceInfo {
    pub family: String,
    pub legacy_family: String,
    pub style: String,
    pub weight: u16,
    pub italic: bool,
}

/// The faces of a font file: one for `ttf`/`otf`, up to
/// [`MAX_FACES_PER_FILE`] for a collection. Faces without a usable family name
/// are skipped. Never panics.
pub fn parse_faces(data: &[u8]) -> Vec<(u32, FaceInfo)> {
    let mut out = Vec::new();
    if data.get(..4) == Some(b"ttcf") {
        let count = u32_at(data, 8).unwrap_or(0).min(MAX_FACES_PER_FILE);
        for i in 0..count {
            let off = u32_at(data, 12 + (i as usize) * 4).map(|o| o as usize);
            if let Some(info) = off.and_then(|o| parse_face_at(data, o)) {
                out.push((i, info));
            }
        }
    } else if let Some(info) = parse_face_at(data, 0) {
        out.push((0, info));
    }
    out
}

/// Find a table of the font whose offset table starts at `base`.
fn table<'d>(data: &'d [u8], base: usize, tag: &[u8; 4]) -> Option<&'d [u8]> {
    let count = usize::from(u16_at(data, base.checked_add(4)?)?);
    for i in 0..count {
        let rec = base.checked_add(12)?.checked_add(i.checked_mul(16)?)?;
        if data.get(rec..rec.checked_add(4)?)? == tag {
            let off = u32_at(data, rec.checked_add(8)?)? as usize;
            let len = u32_at(data, rec.checked_add(12)?)? as usize;
            return data.get(off..off.checked_add(len)?);
        }
    }
    None
}

fn parse_face_at(data: &[u8], base: usize) -> Option<FaceInfo> {
    // Version: TrueType, `true`, `OTTO`.
    match u32_at(data, base)? {
        0x0001_0000 | 0x7472_7565 | 0x4F54_544F => {}
        _ => return None,
    }
    let name = table(data, base, b"name")?;
    let family16 = name_string(name, 16);
    let family1 = name_string(name, 1);
    let legacy_family = family1.clone().or_else(|| family16.clone())?;
    let family = family16.or(family1)?;
    let style = name_string(name, 17)
        .or_else(|| name_string(name, 2))
        .unwrap_or_default();

    let os2 = table(data, base, b"OS/2");
    let weight = os2
        .and_then(|t| u16_at(t, 4))
        .filter(|w| (1..=1000).contains(w))
        .unwrap_or(400);
    let fs_selection = os2.and_then(|t| u16_at(t, 62));
    let mac_style = table(data, base, b"head").and_then(|t| u16_at(t, 44));
    let italic = match fs_selection {
        Some(fs) => fs & 1 != 0 || fs & (1 << 9) != 0,
        None => mac_style.is_some_and(|m| m & 2 != 0),
    };
    Some(FaceInfo {
        family,
        legacy_family,
        style,
        weight,
        italic,
    })
}

/// Name ID `id` from a `name` table: prefer Windows English (3, any, 0x409),
/// then any Windows record, then Unicode platform 0, then Mac Roman (1, 0) only
/// if pure ASCII. Bounds-checked throughout.
fn name_string(name: &[u8], id: u16) -> Option<String> {
    let count = usize::from(u16_at(name, 2)?);
    let storage = usize::from(u16_at(name, 4)?);
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let rec = 6usize.checked_add(i.checked_mul(12)?)?;
        if u16_at(name, rec + 6)? != id {
            continue;
        }
        let platform = u16_at(name, rec)?;
        let language = u16_at(name, rec + 4)?;
        let len = usize::from(u16_at(name, rec + 8)?);
        let off = storage.checked_add(usize::from(u16_at(name, rec + 10)?))?;
        let Some(raw) = name.get(off..off.checked_add(len)?) else {
            continue;
        };
        let (rank, text) = match platform {
            3 => (if language == 0x409 { 4 } else { 3 }, utf16be(raw)),
            0 => (2, utf16be(raw)),
            1 if raw.is_ascii() => (1, Some(String::from_utf8_lossy(raw).into_owned())),
            _ => continue,
        };
        let Some(text) = text.map(|t| clean(&t)).filter(|t| !t.is_empty()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, t)| t)
}

fn utf16be(raw: &[u8]) -> Option<String> {
    if raw.len() % 2 != 0 {
        return None;
    }
    let units = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)]));
    char::decode_utf16(units)
        .take(MAX_NAME_CHARS * 2)
        .collect::<Result<String, _>>()
        .ok()
}

/// Trim, drop control characters, cap the length.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// An in-memory [`DirReader`] for tests: paths map to directories or files.
#[derive(Clone, Debug, Default)]
pub struct FakeFs {
    entries: std::collections::BTreeMap<PathBuf, FakeEntry>,
}

#[derive(Clone, Debug)]
enum FakeEntry {
    Dir,
    File(Vec<u8>),
    Symlink,
}

impl FakeFs {
    pub fn new() -> FakeFs {
        FakeFs::default()
    }

    /// Add a file, creating its parent directories.
    pub fn file(&mut self, path: &str, bytes: Vec<u8>) -> &mut FakeFs {
        self.parents(Path::new(path));
        self.entries.insert(PathBuf::from(path), FakeEntry::File(bytes));
        self
    }

    /// Add a symlink (never followed by the scan).
    pub fn symlink(&mut self, path: &str) -> &mut FakeFs {
        self.parents(Path::new(path));
        self.entries.insert(PathBuf::from(path), FakeEntry::Symlink);
        self
    }

    pub fn dir(&mut self, path: &str) -> &mut FakeFs {
        self.parents(Path::new(path));
        self.entries.insert(PathBuf::from(path), FakeEntry::Dir);
        self
    }

    fn parents(&mut self, p: &Path) {
        for a in p.ancestors().skip(1) {
            if a.as_os_str().is_empty() {
                continue;
            }
            self.entries.entry(a.to_owned()).or_insert(FakeEntry::Dir);
        }
    }
}

impl DirReader for FakeFs {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        match self.entries.get(dir) {
            Some(FakeEntry::Dir) => Ok(self
                .entries
                .keys()
                .filter(|p| p.parent() == Some(dir))
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect()),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        match self.entries.get(path) {
            Some(FakeEntry::Dir) => Ok(Meta {
                kind: EntryKind::Dir,
                len: 0,
            }),
            Some(FakeEntry::File(b)) => Ok(Meta {
                kind: EntryKind::File,
                len: b.len() as u64,
            }),
            Some(FakeEntry::Symlink) => Ok(Meta {
                kind: EntryKind::Symlink,
                len: 0,
            }),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn read_bytes(&self, path: &Path, max_len: u64) -> io::Result<Vec<u8>> {
        match self.entries.get(path) {
            Some(FakeEntry::File(b)) if (b.len() as u64) <= max_len => Ok(b.clone()),
            Some(FakeEntry::File(_)) => Err(io::Error::from(io::ErrorKind::InvalidData)),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }
}

#[cfg(test)]
mod tests;
