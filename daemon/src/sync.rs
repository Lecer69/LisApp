//! The shell's todo, goals, notes and calendar files, which stay the only copy on this PC. The shell watches
//! them, and what comes from a phone replaces them atomically, the same way the shell saves.

use lis_core::sync::{check, empty, file_name, rev, KINDS};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const BACKUPS_KEPT: usize = 10;

#[derive(Clone, PartialEq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    inode: u64,
}

#[derive(Default)]
struct Seen {
    stamp: Option<Stamp>,
    rev: Option<String>,
}

pub enum Applied {
    Written { rev: String },
    /// The PC's copy moved on, so the phone has to merge this one.
    Stale { rev: String, doc: Value },
}

pub struct SyncStore {
    dir: PathBuf,
    backups: PathBuf,
    seen: Mutex<HashMap<String, Seen>>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(Stamp { modified: metadata.modified().ok(), len: metadata.len(), inode: metadata.ino() })
}

impl SyncStore {
    pub fn new(dir: PathBuf, backups: PathBuf) -> Self {
        Self { dir, backups, seen: Mutex::new(HashMap::new()) }
    }

    fn path(&self, kind: &str) -> Result<PathBuf, String> {
        file_name(kind).map(|name| self.dir.join(name)).ok_or_else(|| "Not something that syncs".to_string())
    }

    /// A file that's there but can't be understood is left alone, as the shell treats it.
    fn read(&self, kind: &str) -> Result<(String, Value), String> {
        let path = self.path(kind)?;
        let doc = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| format!("{} isn't valid JSON", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => empty(kind),
            Err(error) => return Err(format!("Can't read {}: {error}", path.display())),
        };
        let usable = if kind == "todo" { doc["tasks"].is_array() } else { doc.is_array() };
        if !usable {
            return Err(format!("{} isn't in the shell's format", path.display()));
        }
        Ok((rev(&doc), doc))
    }

    fn mark(&self, kind: &str, stamp: Option<Stamp>, rev: &str) {
        let mut seen = self.seen.lock().unwrap();
        let entry = seen.entry(kind.to_string()).or_default();
        entry.stamp = stamp;
        entry.rev = Some(rev.to_string());
    }

    /// What a phone is greeted with, which counts as seen.
    pub fn current(&self, kind: &str) -> Result<(String, Value), String> {
        let before = stamp(&self.path(kind)?);
        let (rev, doc) = self.read(kind)?;
        self.mark(kind, before, &rev);
        Ok((rev, doc))
    }

    /// The documents that really changed since last asked. The stamp comes first, so a save during
    /// the read is seen again next time.
    pub fn changed(&self) -> Vec<(&'static str, String, Value)> {
        let mut found = Vec::new();
        for kind in KINDS {
            let Ok(path) = self.path(kind) else { continue };
            let now = stamp(&path);
            {
                let mut seen = self.seen.lock().unwrap();
                let entry = seen.entry(kind.to_string()).or_default();
                if entry.stamp == now {
                    continue;
                }
                entry.stamp = now;
            }
            let Ok((rev, doc)) = self.read(kind) else { continue };
            let mut seen = self.seen.lock().unwrap();
            let entry = seen.entry(kind.to_string()).or_default();
            if entry.rev.as_deref() != Some(rev.as_str()) {
                entry.rev = Some(rev.clone());
                found.push((kind, rev, doc));
            }
        }
        found
    }

    pub fn apply(&self, kind: &str, based_on: &str, doc: &Value) -> Result<Applied, String> {
        let path = self.path(kind)?;
        let before = stamp(&path);
        let (current_rev, current) = self.read(kind)?;
        if current_rev != based_on {
            return Ok(Applied::Stale { rev: current_rev, doc: current });
        }
        check(kind, doc)?;
        let new_rev = rev(doc);
        if new_rev == current_rev {
            return Ok(Applied::Written { rev: new_rev });
        }

        self.backup(kind, &path);
        let tmp = write_tmp(&path, doc.to_string().as_bytes()).map_err(|error| format!("Can't save {}: {error}", path.display()))?;

        // Getting that onto disk takes a moment, and the shell may have saved meanwhile. It doesn't
        // take locks, so the most this can do is look once more, right before the rename.
        if stamp(&path) != before {
            let _ = fs::remove_file(&tmp);
            let (rev, doc) = self.read(kind)?;
            return Ok(Applied::Stale { rev, doc });
        }
        fs::rename(&tmp, &path).map_err(|error| format!("Can't save {}: {error}", path.display()))?;
        self.mark(kind, stamp(&path), &new_rev);
        Ok(Applied::Written { rev: new_rev })
    }

    /// A phone can replace a whole file, so the last few are kept in case that goes wrong.
    fn backup(&self, kind: &str, path: &Path) {
        let Ok(bytes) = fs::read(path) else { return };
        let saved = fs::create_dir_all(&self.backups).and_then(|()| {
            let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_millis());
            lis_core::identity::write_private(&self.backups.join(format!("{kind}-{millis:015}.json")), &bytes)
        });
        if let Err(error) = saved {
            eprintln!("lis-daemon: can't back up {kind}: {error}");
            return;
        }
        let Ok(entries) = fs::read_dir(&self.backups) else { return };
        let prefix = format!("{kind}-");
        let mut names: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(&prefix)))
            .collect();
        names.sort();
        let extra = names.len().saturating_sub(BACKUPS_KEPT);
        for old in &names[..extra] {
            let _ = fs::remove_file(old);
        }
    }
}

/// A complete copy next to the file, for renaming over it. It has the file's permissions, and is
/// private when the file is new.
fn write_tmp(path: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mode = fs::metadata(path).map_or(0o600, |metadata| metadata.permissions().mode() & 0o777);
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("sync");
    let tmp = path.with_file_name(format!(".{name}.lis-tmp"));
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(dir: &tempfile::TempDir) -> SyncStore {
        SyncStore::new(dir.path().join("lis"), dir.path().join("backups"))
    }

    fn notes(body: &str) -> Value {
        json!([{ "id": 1, "title": "t", "body": body, "created": 1, "updated": 1 }])
    }

    #[test]
    fn a_missing_file_is_an_empty_document() {
        let dir = tempfile::tempdir().unwrap();
        let (rev_empty, doc) = store(&dir).current("todo").unwrap();
        assert_eq!(doc, empty("todo"));
        assert_eq!(rev_empty, rev(&empty("todo")));
    }

    #[test]
    fn a_push_on_the_current_copy_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (rev_before, _) = store.current("notes").unwrap();
        assert!(matches!(store.apply("notes", &rev_before, &notes("a")).unwrap(), Applied::Written { .. }));
        let written: Value = serde_json::from_slice(&fs::read(dir.path().join("lis/notes.json")).unwrap()).unwrap();
        assert_eq!(written, notes("a"));
    }

    #[test]
    fn a_push_on_an_old_copy_gets_the_current_one_instead() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        fs::create_dir_all(dir.path().join("lis")).unwrap();
        fs::write(dir.path().join("lis/notes.json"), notes("on the pc").to_string()).unwrap();
        match store.apply("notes", "old", &notes("from the phone")).unwrap() {
            Applied::Stale { doc, .. } => assert_eq!(doc, notes("on the pc")),
            Applied::Written { .. } => panic!("overwrote a newer file"),
        }
        let kept: Value = serde_json::from_slice(&fs::read(dir.path().join("lis/notes.json")).unwrap()).unwrap();
        assert_eq!(kept, notes("on the pc"));
    }

    #[test]
    fn a_bad_document_or_file_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let (rev_empty, _) = store.current("notes").unwrap();
        assert!(store.apply("notes", &rev_empty, &json!({ "not": "notes" })).is_err());
        assert!(!dir.path().join("lis/notes.json").exists());

        fs::create_dir_all(dir.path().join("lis")).unwrap();
        fs::write(dir.path().join("lis/notes.json"), "{broken").unwrap();
        assert!(store.current("notes").is_err());
        assert!(store.apply("notes", &rev_empty, &notes("a")).is_err());
        assert_eq!(fs::read_to_string(dir.path().join("lis/notes.json")).unwrap(), "{broken");
    }

    #[test]
    fn only_real_changes_count() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        fs::create_dir_all(dir.path().join("lis")).unwrap();
        let path = dir.path().join("lis/notes.json");
        fs::write(&path, notes("a").to_string()).unwrap();
        store.current("notes").unwrap();
        store.current("goals").unwrap();
        store.current("todo").unwrap();
        assert!(store.changed().is_empty());

        // Saved again with the same content, in another layout
        fs::write(&path, serde_json::to_vec_pretty(&notes("a")).unwrap()).unwrap();
        assert!(store.changed().is_empty());

        fs::write(&path, notes("b").to_string()).unwrap();
        let changed = store.changed();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, "notes");
        assert_eq!(changed[0].2, notes("b"));
    }

    #[test]
    fn its_own_writes_are_not_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store.changed();
        let (rev_empty, _) = store.current("notes").unwrap();
        store.apply("notes", &rev_empty, &notes("a")).unwrap();
        assert!(store.changed().is_empty());
    }

    #[test]
    fn writes_keep_the_files_permissions_and_a_few_backups() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        fs::create_dir_all(dir.path().join("lis")).unwrap();
        let path = dir.path().join("lis/notes.json");
        fs::write(&path, notes("0").to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        for round in 1..=(BACKUPS_KEPT + 3) {
            let (current, _) = store.current("notes").unwrap();
            store.apply("notes", &current, &notes(&round.to_string())).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);

        let backups = fs::read_dir(dir.path().join("backups")).unwrap().count();
        assert_eq!(backups, BACKUPS_KEPT);
        assert!(!dir.path().join("lis/.notes.json.lis-tmp").exists());
    }
}
