//! The config-directory migration against populated profiles on real disks.
//!
//! The names below are synthetic. `px_brand::LEGACY_CONFIG_DIRS` is empty
//! today, so the rename is simulated by injecting a list, exactly as the
//! Phase 13 gate requires.
#![cfg(test)]

mod common;

mod migration {
    use super::common::fx::{TempDir, pk, psl};
    use px_net::cookie::Jar;
    use px_store::cookies::{load, save};
    use px_store::migrate::{
        MARKER_NAME, MigrateError, Outcome, STAGING_SUFFIX, migrate, migrate_default,
        migrate_with_hook,
    };
    use px_store::{Namespace, Store};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::time::Instant;

    const NEW: &str = "newprofile";
    const OLD: &str = "oldprofile";
    const OLDER: &str = "olderprofile";

    type Tree = BTreeMap<String, Option<Vec<u8>>>;

    fn snapshot(dir: &Path) -> Tree {
        fn walk(dir: &Path, prefix: &str, out: &mut Tree) {
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let name = e.file_name().into_string().unwrap();
                let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                let path = e.path();
                if e.file_type().unwrap().is_dir() {
                    out.insert(rel.clone(), None);
                    walk(&path, &rel, out);
                } else {
                    out.insert(rel, Some(fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Tree::new();
        walk(dir, "", &mut out);
        out
    }

    fn without_marker(mut t: Tree) -> Tree {
        t.remove(MARKER_NAME);
        t
    }

    /// Deterministic, non-repeating-looking bytes.
    fn pattern(len: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect()
    }

    /// A profile with a real store (page data and cookies in two partitions),
    /// nested directories, an empty file, an empty directory, unicode names,
    /// and a file of `big` bytes.
    fn build_profile(dir: &Path, big: usize) {
        let a = pk("a.example", "www.cdn.example");
        let b = pk("b.example", "www.cdn.example");
        let psl = psl();
        let mut s = Store::open(&dir.join("store")).unwrap();
        s.put(&a, Namespace::Storage, b"token", b"A-TOKEN").unwrap();
        s.put(&b, Namespace::Storage, b"token", b"B-TOKEN").unwrap();
        let mut jar = Jar::new();
        jar.store(&psl, &a, "sid=A-COOKIE; Path=/; Max-Age=100000").unwrap();
        let cookies = jar.cookies_for(&a, "/", false);
        save(&mut s, &a, &cookies, Instant::now(), 5_000).unwrap();
        drop(s);

        let nested = dir.join("cache/level1/level2/level3");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("blob.bin"), pattern(70_000)).unwrap();
        fs::write(dir.join("cache/level1/empty-file"), b"").unwrap();
        fs::create_dir_all(dir.join("cache/empty-dir")).unwrap();
        fs::create_dir_all(dir.join("профиль")).unwrap();
        fs::write(dir.join("профиль/日本語 ファイル.txt"), "unicode content ✓").unwrap();
        fs::write(dir.join("naïve file (1).json"), br#"{"k":"v"}"#).unwrap();
        fs::write(dir.join("large.bin"), pattern(big)).unwrap();
    }

    fn fresh(label: &str, big: usize) -> (TempDir, Tree) {
        let root = TempDir::new(label);
        fs::create_dir_all(root.path().join(OLD)).unwrap();
        build_profile(&root.path().join(OLD), big);
        let original = snapshot(&root.path().join(OLD));
        (root, original)
    }

    #[test]
    fn populated_profile_migrates_with_zero_data_loss() {
        let (root, original) = fresh("populated", 6 << 20);
        assert!(original.len() > 12, "profile should be populated: {}", original.len());
        let out = migrate(root.path(), NEW, &[OLD]).unwrap();
        match &out {
            Outcome::Migrated { from, files, source_removed, bytes, .. } => {
                assert_eq!(from, OLD);
                assert!(*source_removed);
                let want_files = original.values().filter(|v| v.is_some()).count();
                assert_eq!(*files, want_files);
                let want_bytes: u64 =
                    original.values().flatten().map(|v| v.len() as u64).sum();
                assert_eq!(*bytes, want_bytes);
            }
            other => panic!("{other:?}"),
        }
        // Byte-for-byte identical, every file, every directory.
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
        assert!(!root.path().join(OLD).exists());
        assert!(!root.path().join(format!("{NEW}{STAGING_SUFFIX}")).exists());

        // The migrated data is live: the store opens and partitions hold.
        let a = pk("a.example", "www.cdn.example");
        let b = pk("b.example", "www.cdn.example");
        let s = Store::open(&root.path().join(NEW).join("store")).unwrap();
        assert_eq!(s.get(&a, Namespace::Storage, b"token"), Some(&b"A-TOKEN"[..]));
        assert_eq!(s.get(&b, Namespace::Storage, b"token"), Some(&b"B-TOKEN"[..]));
        let mut jar = Jar::new();
        let r = load(&s, &psl(), &a, &mut jar, 5_100, true);
        assert_eq!(r.loaded, 1);
        assert_eq!(jar.cookies_for(&a, "/", false)[0].value, "A-COOKIE");

        // Idempotent.
        assert_eq!(migrate(root.path(), NEW, &[OLD]).unwrap(), Outcome::AlreadyCurrent);
    }

    #[test]
    fn default_names_are_empty_today_so_nothing_happens() {
        let root = TempDir::new("default");
        assert_eq!(migrate_default(root.path()).unwrap(), Outcome::NoLegacy);
        assert!(!root.path().join(px_brand::CONFIG_DIR).exists());
    }

    /// Fail before every single filesystem operation (a stand-in for a crash
    /// at that step), check the disk is in one of the two safe states, then
    /// resume and check the end state is the same as an uninterrupted run.
    #[test]
    fn fail_at_every_step() {
        // How many operations does an uninterrupted run perform?
        let (root, original) = fresh("steps-count", 100_000);
        let mut total = 0;
        migrate_with_hook(root.path(), NEW, &[OLD], &mut |n| {
            total = n + 1;
            false
        })
        .unwrap();
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
        assert!(total > 20, "expected a real number of steps, got {total}");

        for k in 0..total {
            let (root, original) = fresh("steps", 100_000);
            let old = root.path().join(OLD);
            let new = root.path().join(NEW);

            let r = migrate_with_hook(root.path(), NEW, &[OLD], &mut |n| n == k);
            assert!(matches!(r, Err(MigrateError::Injected(n)) if n == k), "k={k}: {r:?}");

            // Safe state 1: nothing committed, legacy completely intact.
            // Safe state 2: committed; the new profile is complete.
            if new.exists() {
                assert_eq!(without_marker(snapshot(&new)), original, "k={k}: committed but incomplete");
            } else {
                assert_eq!(snapshot(&old), original, "k={k}: legacy damaged before commit");
            }

            // Resume.
            let r = migrate(root.path(), NEW, &[OLD]).unwrap_or_else(|e| panic!("k={k}: resume {e}"));
            assert!(
                matches!(r, Outcome::Migrated { source_removed: true, .. } | Outcome::CleanedUp { source_removed: true }),
                "k={k}: {r:?}"
            );
            assert_eq!(without_marker(snapshot(&new)), original, "k={k}");
            assert!(!old.exists(), "k={k}: legacy left behind");
            assert!(!root.path().join(format!("{NEW}{STAGING_SUFFIX}")).exists());
            // And again: nothing to do.
            assert_eq!(migrate(root.path(), NEW, &[OLD]).unwrap(), Outcome::AlreadyCurrent, "k={k}");
        }
    }

    #[test]
    fn stale_staging_from_a_crash_is_replaced() {
        let (root, original) = fresh("staging", 1000);
        let staging = root.path().join(format!("{NEW}{STAGING_SUFFIX}"));
        fs::create_dir_all(staging.join("half")).unwrap();
        fs::write(staging.join("half/partial.bin"), b"torn").unwrap();
        fs::write(staging.join("junk"), b"x").unwrap();
        migrate(root.path(), NEW, &[OLD]).unwrap();
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
        assert!(!staging.exists());
    }

    #[test]
    fn an_existing_profile_is_never_overwritten_or_merged() {
        let (root, original) = fresh("existing", 1000);
        fs::create_dir_all(root.path().join(NEW)).unwrap();
        fs::write(root.path().join(NEW).join("mine.txt"), b"in use").unwrap();
        assert_eq!(migrate(root.path(), NEW, &[OLD]).unwrap(), Outcome::AlreadyCurrent);
        assert_eq!(snapshot(&root.path().join(OLD)), original, "legacy untouched");
        let new = snapshot(&root.path().join(NEW));
        assert_eq!(new.len(), 1);
        assert_eq!(new["mine.txt"].as_deref(), Some(&b"in use"[..]));
    }

    #[test]
    fn an_empty_new_directory_does_not_block_migration() {
        let (root, original) = fresh("empty-dest", 1000);
        fs::create_dir_all(root.path().join(NEW)).unwrap();
        migrate(root.path(), NEW, &[OLD]).unwrap();
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
    }

    #[test]
    fn newest_legacy_wins_and_older_ones_are_left_alone() {
        let (root, original) = fresh("multi", 1000);
        fs::create_dir_all(root.path().join(OLDER)).unwrap();
        fs::write(root.path().join(OLDER).join("ancient.txt"), b"older").unwrap();
        // The list is append-ordered: the last name is the most recent.
        migrate(root.path(), NEW, &[OLDER, OLD]).unwrap();
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
        assert_eq!(fs::read(root.path().join(OLDER).join("ancient.txt")).unwrap(), b"older");
    }

    #[test]
    fn nothing_to_do_without_a_legacy_directory() {
        let root = TempDir::new("none");
        assert_eq!(migrate(root.path(), NEW, &[OLD, OLDER]).unwrap(), Outcome::NoLegacy);
        assert!(!root.path().join(NEW).exists());
    }

    #[test]
    fn hostile_names_are_refused() {
        let root = TempDir::new("names");
        for bad in ["", "..", "a/b", "../x", "."] {
            assert!(matches!(
                migrate(root.path(), NEW, &[bad]),
                Err(MigrateError::InvalidConfigName(_))
            ), "{bad:?}");
            assert!(matches!(
                migrate(root.path(), bad, &[OLD]),
                Err(MigrateError::InvalidConfigName(_))
            ), "{bad:?}");
        }
    }

    /// If the source changes between copy and verification the migration
    /// stops, deletes nothing, and commits nothing.
    #[test]
    fn source_changing_during_the_copy_is_caught() {
        let (root, original) = fresh("changing", 1000);
        let old = root.path().join(OLD);
        // Operation 0 creates the staging root; operation i+1 copies manifest
        // entry i. `large.bin` sorts well before the last entry, so it has been
        // copied by the time the last copy starts. Change it then.
        let last_copy = original.len() as u32;
        let r = migrate_with_hook(root.path(), NEW, &[OLD], &mut |n| {
            if n == last_copy {
                fs::write(old.join("large.bin"), b"changed under the migration").unwrap();
            }
            false
        });
        assert!(matches!(r, Err(MigrateError::VerificationFailed(_))), "{r:?}");
        assert!(!root.path().join(NEW).exists(), "nothing may be committed");
        assert!(!root.path().join(format!("{NEW}{STAGING_SUFFIX}")).exists());
        // Nothing deleted: legacy is the original plus the edit.
        let mut expect = original;
        expect.insert("large.bin".into(), Some(b"changed under the migration".to_vec()));
        assert_eq!(snapshot(&old), expect);
    }

    /// After the new profile is committed, cleanup removes only files that
    /// still match what was copied. A file edited, and a file added, after the
    /// commit survive in the legacy directory.
    #[test]
    fn cleanup_never_deletes_what_it_did_not_copy() {
        // Find the index of the first cleanup operation.
        let (probe, _) = fresh("cleanup-probe", 1000);
        let mut total = 0;
        migrate_with_hook(probe.path(), NEW, &[OLD], &mut |n| {
            total = n + 1;
            false
        })
        .unwrap();
        let entries = snapshot(&probe.path().join(NEW)).len() - 1; // minus the marker
        let first_cleanup = total - (entries as u32 + 1);

        let (root, original) = fresh("cleanup", 1000);
        let old = root.path().join(OLD);
        let r = migrate_with_hook(root.path(), NEW, &[OLD], &mut |n| n == first_cleanup);
        assert!(matches!(r, Err(MigrateError::Injected(_))), "{r:?}");
        assert!(root.path().join(NEW).exists(), "must be committed");
        fs::write(old.join("naïve file (1).json"), b"edited after commit").unwrap();
        fs::write(old.join("added-later.txt"), b"new data").unwrap();

        let r = migrate(root.path(), NEW, &[OLD]).unwrap();
        assert_eq!(r, Outcome::CleanedUp { source_removed: false });
        let left = snapshot(&old);
        let keys: Vec<&str> = left.keys().map(String::as_str).collect();
        assert_eq!(keys, ["added-later.txt", "naïve file (1).json"]);
        assert_eq!(left["added-later.txt"].as_deref(), Some(&b"new data"[..]));
        // The new profile still holds the version that was verified.
        assert_eq!(without_marker(snapshot(&root.path().join(NEW))), original);
    }

    #[cfg(unix)]
    mod symlinks {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn symlink_inside_the_tree_refuses_the_whole_migration() {
            let (root, _) = fresh("symlink", 1000);
            let outside = TempDir::new("outside");
            fs::write(outside.path().join("secret.txt"), b"not part of the profile").unwrap();
            let old = root.path().join(OLD);
            symlink(outside.path(), old.join("cache/escape")).unwrap();
            let before = fs::read_link(old.join("cache/escape")).unwrap();

            let r = migrate(root.path(), NEW, &[OLD]);
            assert!(matches!(r, Err(MigrateError::SymlinkRefused(_))), "{r:?}");
            // Legacy intact, symlink still there, target untouched, nothing created.
            assert_eq!(fs::read_link(old.join("cache/escape")).unwrap(), before);
            assert!(old.join("large.bin").exists());
            assert!(old.join("store/store.log").exists());
            assert_eq!(fs::read(outside.path().join("secret.txt")).unwrap(), b"not part of the profile");
            assert!(!root.path().join(NEW).exists());
            assert!(!root.path().join(format!("{NEW}{STAGING_SUFFIX}")).exists());
        }

        #[test]
        fn symlinked_file_and_symlinked_root_are_refused() {
            let (root, _) = fresh("symlink-file", 1000);
            let outside = TempDir::new("outside-file");
            fs::write(outside.path().join("f"), b"x").unwrap();
            let old = root.path().join(OLD);
            symlink(outside.path().join("f"), old.join("link-to-file")).unwrap();
            assert!(matches!(
                migrate(root.path(), NEW, &[OLD]),
                Err(MigrateError::SymlinkRefused(_))
            ));

            let root2 = TempDir::new("symlink-root");
            let real = TempDir::new("real-profile");
            fs::write(real.path().join("data"), b"x").unwrap();
            symlink(real.path(), root2.path().join(OLD)).unwrap();
            assert!(matches!(
                migrate(root2.path(), NEW, &[OLD]),
                Err(MigrateError::SymlinkRefused(_))
            ));
            assert!(!root2.path().join(NEW).exists());
            assert_eq!(fs::read(real.path().join("data")).unwrap(), b"x");
        }

        #[test]
        fn cleanup_does_not_delete_through_a_swapped_in_symlink() {
            // Commit, then replace a legacy directory with a symlink to an
            // outside directory holding a file of the same name and content.
            let (probe, _) = fresh("swap-probe", 1000);
            let mut total = 0;
            migrate_with_hook(probe.path(), NEW, &[OLD], &mut |n| {
                total = n + 1;
                false
            })
            .unwrap();
            let entries = snapshot(&probe.path().join(NEW)).len() as u32 - 1;
            let first_cleanup = total - (entries + 1);

            let (root, _) = fresh("swap", 1000);
            let old = root.path().join(OLD);
            let _ = migrate_with_hook(root.path(), NEW, &[OLD], &mut |n| n == first_cleanup);
            let outside = TempDir::new("swap-outside");
            fs::create_dir_all(outside.path().join("level1/level2/level3")).unwrap();
            fs::write(outside.path().join("level1/level2/level3/blob.bin"), pattern(70_000)).unwrap();
            fs::remove_dir_all(old.join("cache")).unwrap();
            symlink(outside.path(), old.join("cache")).unwrap();

            migrate(root.path(), NEW, &[OLD]).unwrap();
            assert!(
                outside.path().join("level1/level2/level3/blob.bin").exists(),
                "a file outside the profile was deleted through a symlink"
            );
        }
    }
}
