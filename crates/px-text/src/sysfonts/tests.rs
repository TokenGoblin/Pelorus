use super::*;

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn name_table(records: &[(u16, u16, u16, u16, &[u8])]) -> Vec<u8> {
    // (platform, encoding, language, id, bytes)
    let mut t = Vec::new();
    be16(&mut t, 0);
    be16(&mut t, records.len() as u16);
    be16(&mut t, 6 + 12 * records.len() as u16);
    let mut storage = Vec::new();
    for (p, e, l, id, b) in records {
        be16(&mut t, *p);
        be16(&mut t, *e);
        be16(&mut t, *l);
        be16(&mut t, *id);
        be16(&mut t, b.len() as u16);
        be16(&mut t, storage.len() as u16);
        storage.extend_from_slice(b);
    }
    t.extend_from_slice(&storage);
    t
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A font with only `name`, `OS/2` and `head`, at `base` inside a larger buffer
/// when wrapped by `collection`.
fn tables(name: Vec<u8>, weight: u16, fs_selection: u16) -> Vec<(&'static [u8; 4], Vec<u8>)> {
    let mut os2 = vec![0u8; 78];
    os2[4..6].copy_from_slice(&weight.to_be_bytes());
    os2[62..64].copy_from_slice(&fs_selection.to_be_bytes());
    vec![(b"OS/2", os2), (b"head", vec![0u8; 54]), (b"name", name)]
}

/// One sfnt whose table offsets are absolute in the final file when placed at
/// `base`.
fn sfnt_at(base: usize, tabs: &[(&'static [u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, tabs.len() as u16);
    be16(&mut out, 0);
    be16(&mut out, 0);
    be16(&mut out, 0);
    let mut off = base + 12 + 16 * tabs.len();
    let mut body = Vec::new();
    for (tag, data) in tabs {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, off as u32);
        be32(&mut out, data.len() as u32);
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        off = base + 12 + 16 * tabs.len() + body.len();
    }
    out.extend_from_slice(&body);
    out
}

fn font(family: &str, style: &str, weight: u16, italic: bool) -> Vec<u8> {
    let f = utf16(family);
    let s = utf16(style);
    let name = name_table(&[(3, 1, 0x409, 1, &f), (3, 1, 0x409, 2, &s)]);
    sfnt_at(0, &tables(name, weight, if italic { 1 } else { 0 }))
}

#[test]
fn parses_family_style_weight_italic() {
    let faces = parse_faces(&font("Test Sans", "Bold Italic", 700, true));
    assert_eq!(faces.len(), 1);
    let (i, f) = &faces[0];
    assert_eq!(*i, 0);
    assert_eq!(f.family, "Test Sans");
    assert_eq!(f.style, "Bold Italic");
    assert_eq!(f.weight, 700);
    assert!(f.italic);
}

#[test]
fn typographic_family_wins_and_legacy_is_kept() {
    let a = utf16("Foo Light");
    let b = utf16("Foo");
    let c = utf16("Light");
    let name = name_table(&[
        (3, 1, 0x409, 1, &a),
        (3, 1, 0x409, 16, &b),
        (3, 1, 0x409, 17, &c),
    ]);
    let data = sfnt_at(0, &tables(name, 300, 0));
    let (_, f) = parse_faces(&data).remove(0);
    assert_eq!(f.family, "Foo");
    assert_eq!(f.legacy_family, "Foo Light");
    assert_eq!(f.style, "Light");
}

#[test]
fn windows_english_beats_other_languages_and_mac_needs_ascii() {
    let de = utf16("Deutsch");
    let en = utf16("English");
    let name = name_table(&[
        (3, 1, 0x407, 1, &de),
        (3, 1, 0x409, 1, &en),
        (1, 0, 0, 1, b"Mac"),
    ]);
    let (_, f) = parse_faces(&sfnt_at(0, &tables(name, 400, 0))).remove(0);
    assert_eq!(f.family, "English");
    // Mac Roman with a high byte is not decoded.
    let name = name_table(&[(1, 0, 0, 1, &[0x41, 0xC4])]);
    assert!(parse_faces(&sfnt_at(0, &tables(name, 400, 0))).is_empty());
}

#[test]
fn collections_yield_each_face() {
    let one = tables(name_table(&[(3, 1, 0x409, 1, &utf16("One"))]), 400, 0);
    let two = tables(name_table(&[(3, 1, 0x409, 1, &utf16("Two"))]), 700, 0);
    let header = 12 + 8;
    let f1 = sfnt_at(header, &one);
    let f2 = sfnt_at(header + f1.len(), &two);
    let mut file = b"ttcf".to_vec();
    be32(&mut file, 0x0001_0000);
    be32(&mut file, 2);
    be32(&mut file, header as u32);
    be32(&mut file, (header + f1.len()) as u32);
    file.extend_from_slice(&f1);
    file.extend_from_slice(&f2);
    let faces = parse_faces(&file);
    let names: Vec<_> = faces
        .iter()
        .map(|(i, f)| (*i, f.family.as_str(), f.weight))
        .collect();
    assert_eq!(names, [(0, "One", 400), (1, "Two", 700)]);
}

#[test]
fn hostile_bytes_never_panic() {
    let good = font("Test Sans", "Regular", 400, false);
    for cut in 0..good.len() {
        let _ = parse_faces(&good[..cut]);
    }
    for i in 0..good.len() {
        for v in [0u8, 0xFF, 0x80] {
            let mut bad = good.clone();
            bad[i] = v;
            let _ = parse_faces(&bad);
        }
    }
    // A collection claiming four billion faces, with offsets out of range.
    let mut c = b"ttcf".to_vec();
    be32(&mut c, 0x0001_0000);
    be32(&mut c, u32::MAX);
    c.extend_from_slice(&[0xFF; 64]);
    assert!(parse_faces(&c).is_empty());
    // Odd-length UTF-16 and lone surrogates are skipped, not decoded.
    let name = name_table(&[(3, 1, 0x409, 1, &[0xD8, 0x00, 0x00, 0x41])]);
    assert!(parse_faces(&sfnt_at(0, &tables(name, 400, 0))).is_empty());
}

fn fake() -> FakeFs {
    let mut fs = FakeFs::new();
    fs.file("/fonts/b/Bold.ttf", font("Zeta", "Bold", 700, false));
    fs.file("/fonts/a/Regular.TTF", font("Zeta", "Regular", 400, false));
    fs.file("/fonts/a/Italic.otf", font("Zeta", "Italic", 400, true));
    fs.file("/fonts/DejaVu.ttf", font("DejaVu Sans", "Book", 400, false));
    fs.file("/fonts/notes.txt", b"not a font".to_vec());
    fs.file("/fonts/broken.ttf", vec![1, 2, 3]);
    fs.symlink("/fonts/link.ttf");
    fs
}

#[test]
fn scan_is_sorted_and_skips_non_fonts_and_symlinks() {
    let fs = fake();
    let s = SystemFonts::scan(&fs, &[PathBuf::from("/fonts")], &ScanLimits::default());
    let got: Vec<_> = s
        .faces()
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect();
    // Sorted by name within a directory, files and directories interleaved by
    // name: "DejaVu.ttf", then directory a, then directory b.
    assert_eq!(
        got,
        [
            "/fonts/DejaVu.ttf",
            "/fonts/a/Italic.otf",
            "/fonts/a/Regular.TTF",
            "/fonts/b/Bold.ttf"
        ]
    );
    let again = SystemFonts::scan(&fs, &[PathBuf::from("/fonts")], &ScanLimits::default());
    assert_eq!(s, again);
}

#[test]
fn missing_root_is_empty_not_an_error() {
    let s = SystemFonts::scan(
        &FakeFs::new(),
        &[PathBuf::from("/nope")],
        &ScanLimits::default(),
    );
    assert!(s.is_empty());
}

#[test]
fn best_match_weighs_weight_then_slant() {
    let fs = fake();
    let s = SystemFonts::scan(&fs, &[PathBuf::from("/fonts")], &ScanLimits::default());
    assert_eq!(
        s.best_match("zeta", 400, false).map(|f| f.style.as_str()),
        Some("Regular")
    );
    assert_eq!(
        s.best_match("ZETA", 600, false).map(|f| f.style.as_str()),
        Some("Bold")
    );
    assert_eq!(
        s.best_match("Zeta", 400, true).map(|f| f.style.as_str()),
        Some("Italic")
    );
    assert!(s.best_match("Absent", 400, false).is_none());
}

#[test]
fn generic_families_use_our_preference_lists() {
    let fs = fake();
    let s = SystemFonts::scan(&fs, &[PathBuf::from("/fonts")], &ScanLimits::default());
    // Only DejaVu Sans is installed; it is in the sans-serif list.
    assert_eq!(
        s.lookup("sans-serif", 400, false)
            .map(|f| f.family.as_str()),
        Some("DejaVu Sans")
    );
    assert_eq!(
        s.lookup("SANS-SERIF", 400, false)
            .map(|f| f.family.as_str()),
        Some("DejaVu Sans")
    );
    assert!(s.lookup("monospace", 400, false).is_none());
    assert_eq!(
        GenericFamily::from_keyword("system-ui"),
        Some(GenericFamily::SystemUi)
    );
    assert_eq!(GenericFamily::from_keyword("Zeta"), None);
    // Preference order, not scan order, decides.
    let mut fs = FakeFs::new();
    fs.file("/f/a.ttf", font("Noto Sans", "Regular", 400, false));
    fs.file("/f/b.ttf", font("Arial", "Regular", 400, false));
    let s = SystemFonts::scan(&fs, &[PathBuf::from("/f")], &ScanLimits::default());
    assert_eq!(
        s.resolve_generic(GenericFamily::SansSerif)
            .map(|f| f.family.as_str()),
        Some("Arial")
    );
}

#[test]
fn limits_bound_the_scan() {
    let fs = fake();
    let root = [PathBuf::from("/fonts")];
    let d = ScanLimits::default();
    let l = ScanLimits { max_depth: 0, ..d };
    assert_eq!(
        SystemFonts::scan(&fs, &root, &l).len(),
        1,
        "depth 0 reads only the root's files"
    );
    let l = ScanLimits { max_faces: 2, ..d };
    assert_eq!(SystemFonts::scan(&fs, &root, &l).len(), 2);
    let l = ScanLimits {
        max_file_bytes: 10,
        ..d
    };
    assert!(SystemFonts::scan(&fs, &root, &l).is_empty());
    let l = ScanLimits { max_files: 1, ..d };
    assert!(SystemFonts::scan(&fs, &root, &l).len() <= 1);
    let l = ScanLimits { max_dirs: 1, ..d };
    assert_eq!(SystemFonts::scan(&fs, &root, &l).len(), 1);
}

#[test]
fn deep_trees_stop_at_the_depth_cap() {
    let mut fs = FakeFs::new();
    let mut p = String::from("/r");
    for i in 0..20 {
        p.push_str(&format!("/d{i}"));
    }
    fs.file(
        &format!("{p}/deep.ttf"),
        font("Deep", "Regular", 400, false),
    );
    let s = SystemFonts::scan(&fs, &[PathBuf::from("/r")], &ScanLimits::default());
    assert!(s.is_empty());
}

#[test]
fn roots_linux() {
    let env = |k: &str| match k {
        "HOME" => Some("/home/u".to_owned()),
        "XDG_DATA_DIRS" => Some("/usr/share:/opt/x::/usr/local/share".to_owned()),
        _ => None,
    };
    let r = default_roots(Platform::Linux, &env);
    let r: Vec<_> = r.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    assert_eq!(
        r,
        [
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/home/u/.local/share/fonts",
            "/home/u/.fonts",
            "/opt/x/fonts",
        ]
    );
}

#[test]
fn roots_windows_from_a_fake_environment() {
    let env = |k: &str| match k {
        "SystemRoot" => Some("C:\\Windows".to_owned()),
        "LOCALAPPDATA" => Some("C:\\Users\\u\\AppData\\Local".to_owned()),
        _ => None,
    };
    let r = default_roots(Platform::Windows, &env);
    assert_eq!(r.len(), 2);
    assert!(r[0].ends_with("Fonts"));
    assert!(r[1].ends_with("Fonts"));
    assert!(default_roots(Platform::Windows, &|_| None).is_empty());
}
