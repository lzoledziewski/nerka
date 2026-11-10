//! Several windows, one configuration (owner, 2026-11-09, review U3).
//! Each `nerka-gui` window is a process of its own (`instance.rs`) that
//! reads the configuration at startup and writes whole files. So:
//!
//! - **Reload:** Settings → Reload configuration, and by itself when the
//!   window gets the focus back and a configuration file changed while it
//!   didn't have it ([`Stamp`]; nothing runs in between).
//! - **Merge on save:** a list kept in one file (checks, signals, prefs) is
//!   written as this window's changes applied to what is in the file now
//!   ([`merge3`]), not as this window's copy: another window's change made
//!   since isn't lost. Files of one item each (plots, presets, packs) are
//!   written only when that item is saved.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Three-way merge of named items (`(key, text)`, keys compared without
/// case): what this window changed since `base` (what it last read or
/// wrote) applied to `theirs` (the file now). An item both changed keeps
/// this window's version and is named in the second list. Order: theirs',
/// then the items only this window added, in its order.
pub fn merge3(
    base: &[(String, String)],
    mine: &[(String, String)],
    theirs: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let find =
        |v: &[(String, String)], k: &str| v.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, t)| t.clone());
    let mut out = Vec::new();
    let mut both = Vec::new();
    let keys = theirs.iter().map(|(k, _)| k).chain(mine.iter().map(|(k, _)| k));
    let mut seen: Vec<String> = Vec::new();
    for k in keys {
        let low = k.to_lowercase();
        if seen.contains(&low) {
            continue;
        }
        seen.push(low);
        let (b, m, t) = (find(base, k), find(mine, k), find(theirs, k));
        let take = if m == b {
            t
        } else {
            if t != b && t != m {
                both.push(k.clone());
            }
            m
        };
        if let Some(text) = take {
            out.push((k.clone(), text));
        }
    }
    (out, both)
}

/// `KEY=VALUE` lines as items, one per key (a key given on several lines,
/// as `route=`, is one item of all its values).
pub fn key_items(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((k, _)) = line.split_once('=') else { continue };
        let k = k.trim().to_owned();
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some((_, v)) => {
                v.push('\n');
                v.push_str(line);
            }
            None => out.push((k, line.to_owned())),
        }
    }
    out
}

/// What the configuration files are: each file's path, size and time, and
/// those of the files in each folder (packs, plots, presets), so a change
/// another window made shows.
#[derive(Debug, Default, PartialEq)]
pub struct Stamp(Vec<(PathBuf, u64, Option<SystemTime>)>);

impl Stamp {
    pub fn of(paths: &[PathBuf]) -> Stamp {
        let mut out = Vec::new();
        for p in paths {
            walk(p, 0, &mut out);
        }
        out.sort();
        Stamp(out)
    }
}

fn walk(p: &Path, depth: usize, out: &mut Vec<(PathBuf, u64, Option<SystemTime>)>) {
    let Ok(meta) = std::fs::metadata(p) else { return };
    if meta.is_dir() {
        // Packs may sit in subfolders; not deeper than that is enough.
        if depth > 2 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(p) else { return };
        for e in entries.flatten() {
            walk(&e.path(), depth + 1, out);
        }
    } else {
        out.push((p.to_owned(), meta.len(), meta.modified().ok()));
    }
}

/// Write `prefs` to `path` merged with what another window wrote there
/// since `base` (what this window last read or wrote); `prefs` and `base`
/// become what was written. A change from elsewhere is taken into `prefs`,
/// so the next save doesn't undo it.
pub fn save_prefs(prefs: &mut crate::prefs::SearchPrefs, base: &mut String, path: &Path) -> std::io::Result<()> {
    let theirs = std::fs::read_to_string(path).unwrap_or_default();
    let mine = prefs.serialize();
    let text = if theirs == *base || theirs.is_empty() {
        mine
    } else {
        // Read before the file was there: the defaults were the base.
        let was = if base.is_empty() { crate::prefs::SearchPrefs::default().serialize() } else { base.clone() };
        let (merged, _) = merge3(&key_items(&was), &key_items(&mine), &key_items(&theirs));
        let text: String = merged.iter().map(|(_, v)| format!("{v}\n")).collect();
        let protocols = std::mem::take(&mut prefs.protocols);
        *prefs = crate::prefs::SearchPrefs::parse(&text);
        prefs.protocols = protocols;
        nerka_core::time::set_local(prefs.local_time);
        prefs.serialize()
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    nerka_core::write_atomic(path, &text)?;
    *base = text;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(k, t)| (k.to_string(), t.to_string())).collect()
    }

    #[test]
    fn each_side_keeps_its_changes() {
        let base = items(&[("a", "a1"), ("b", "b1"), ("c", "c1")]);
        // Mine: a edited, c removed, d added. Theirs: b edited, e added.
        let mine = items(&[("a", "a2"), ("b", "b1"), ("d", "d1")]);
        let theirs = items(&[("a", "a1"), ("b", "b2"), ("c", "c1"), ("e", "e1")]);
        let (out, both) = merge3(&base, &mine, &theirs);
        assert_eq!(out, items(&[("a", "a2"), ("b", "b2"), ("e", "e1"), ("d", "d1")]));
        assert!(both.is_empty());
        // Removed there, untouched here: gone. Names without case.
        let (out, _) = merge3(&base, &items(&[("A", "a1"), ("b", "b1"), ("c", "c1")]), &items(&[("a", "a1")]));
        assert_eq!(out, items(&[("a", "a1")]));
    }

    #[test]
    fn both_changed_keeps_mine_and_says_so() {
        let base = items(&[("a", "a1")]);
        let (out, both) = merge3(&base, &items(&[("a", "mine")]), &items(&[("a", "theirs")]));
        assert_eq!((out, both), (items(&[("a", "mine")]), vec!["a".to_owned()]));
        // The same change on both sides isn't a clash.
        let (out, both) = merge3(&base, &items(&[("a", "same")]), &items(&[("a", "same")]));
        assert_eq!((out, both.len()), (items(&[("a", "same")]), 0));
    }

    #[test]
    fn prefs_lines_are_items_by_key() {
        let i = key_items("mode=auto\nroute=A:port=1\nroute=B:port=2\nnot a line\n");
        assert_eq!(i, items(&[("mode", "mode=auto"), ("route", "route=A:port=1\nroute=B:port=2")]));
    }

    #[test]
    fn prefs_saved_from_two_windows_keep_both_changes() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-sync-{}", std::process::id()));
        let path = dir.join("prefs");
        let mut one = crate::prefs::SearchPrefs::default();
        let mut two = one.clone();
        let (mut base1, mut base2) = (String::new(), String::new());
        save_prefs(&mut one, &mut base1, &path).unwrap();
        base2.clone_from(&base1);
        // Window one: notes in a column; window two, not knowing: merge by time.
        one.bookmark_notes = crate::view::NoteMode::Column;
        save_prefs(&mut one, &mut base1, &path).unwrap();
        two.merge_by_time = true;
        save_prefs(&mut two, &mut base2, &path).unwrap();
        let on_disk = crate::prefs::SearchPrefs::load(&path).unwrap();
        assert!(on_disk.merge_by_time && on_disk.bookmark_notes == crate::view::NoteMode::Column, "{on_disk:?}");
        assert_eq!(two.bookmark_notes, crate::view::NoteMode::Column, "taken into window two");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stamp_changes_with_a_file_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-stamp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plots")).unwrap();
        let paths = [dir.join("checks"), dir.join("plots")];
        let before = Stamp::of(&paths);
        assert_eq!(before, Stamp::of(&paths));
        std::fs::write(dir.join("plots").join("drive.nerka-plot"), "x").unwrap();
        let after = Stamp::of(&paths);
        assert_ne!(before, after);
        std::fs::write(dir.join("checks"), "a: lines /x/; exists\n").unwrap();
        assert_ne!(after, Stamp::of(&paths));
        std::fs::remove_dir_all(&dir).ok();
    }
}
