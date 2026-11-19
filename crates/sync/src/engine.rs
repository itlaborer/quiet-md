//! The incremental mirror: what changed here goes up, what changed there comes
//! down, deletions travel both ways, and the two JSON indexes merge.

use crate::backends;
use crate::{Backend, Config, RemoteFile};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[cfg(test)]
const STATE: &str = "sync-state.json";
const INDEX: &str = "index.json";
const TRASH_INDEX: &str = "trash/index.json";

/// What a sync moved, for the status line.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted_remote: usize,
    pub deleted_local: usize,
    pub merged: usize,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub failures: Vec<String>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.uploaded + self.downloaded + self.deleted_remote + self.deleted_local + self.merged > 0
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Tracked {
    /// FNV-1a of the contents last exchanged.
    hash: u64,
    /// Local size and mtime, so an unchanged file is not read again.
    size: u64,
    mtime: u64,
    remote_size: u64,
    /// 0: unknown (just uploaded; the next listing latches it).
    remote_mtime: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    #[serde(default = "state_version")]
    version: u32,
    #[serde(default)]
    files: BTreeMap<String, Tracked>,
}

fn state_version() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        State { version: state_version(), files: BTreeMap::new() }
    }
}

impl State {
    fn load(root: &Path, name: &str) -> State {
        let state: State = std::fs::read(root.join(name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if state.version == state_version() { state } else { State::default() }
    }

    fn save(&self, root: &Path, name: &str) -> Result<()> {
        let path = root.join(name);
        let tmp = root.join(format!("{name}.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

/// Run a sync. `progress` gets short human-readable steps ("上传 docs/…").
pub fn sync(root: &Path, cfg: &Config) -> Result<Report> {
    sync_with(root, cfg, |_| {})
}

pub fn sync_with(root: &Path, cfg: &Config, mut progress: impl FnMut(&str)) -> Result<Report> {
    cfg.validate()?;
    let _lock = lock(root)?;
    progress("正在连接…");
    let mut backend = backends::connect(cfg)?;
    // Each protocol keeps its own record of what was exchanged, so syncing the
    // same library to several targets does not let one skip the others' files.
    let state_name = format!("sync-state-{}.json", cfg.kind.id());
    run(root, &mut *backend, &mut progress, &state_name)
}

/// The sync itself, over any backend; tests drive it with a local folder.
fn run(root: &Path, backend: &mut dyn Backend, mut progress: &mut impl FnMut(&str), state_name: &str) -> Result<Report> {
    progress("正在读取远程列表…");
    let remote_list = backend.list().context("无法读取远程文件列表")?;
    let remote: BTreeMap<String, RemoteFile> =
        remote_list.into_iter().filter(|r| syncable(&r.path)).map(|r| (r.path.clone(), r)).collect();
    let mut local = BTreeMap::new();
    local_files(root, &mut local)?;
    let mut state = State::load(root, state_name);
    let mut report = Report::default();
    let mut seen = HashSet::new();
    // Remote copies we removed; the stale listing below must not re-download them.
    let mut removed = HashSet::new();

    for (rel, mtime) in &local {
        seen.insert(rel.clone());
        let result = if rel == INDEX || rel == TRASH_INDEX {
            sync_index(root, rel, backend, remote.get(rel), &mut state, &mut report, &mut progress)
        } else {
            sync_file(root, rel, *mtime, backend, remote.get(rel), &mut state, &mut report, &mut progress)
        };
        if let Err(e) = result {
            report.failures.push(format!("{rel}: {e:#}"));
        }
    }

    // Local deletions: remove the remote copy (or take back a newer remote one).
    let gone: Vec<String> = state.files.keys().filter(|rel| !seen.contains(*rel)).cloned().collect();
    for rel in gone {
        if !remote.is_empty() {
            match remote.get(&rel) {
                None => {
                    state.files.remove(&rel);
                }
                Some(r) => {
                    let s = state.files.get(&rel).copied().unwrap_or_default();
                    let remote_changed = s.remote_mtime == 0 && r.size != s.remote_size
                        || s.remote_mtime != 0 && (r.size != s.remote_size || r.mtime.abs_diff(s.remote_mtime) > 2);
                    let result = if remote_changed {
                        download(root, &rel, r, backend, &mut state, &mut report, &mut progress)
                    } else {
                        backend.delete(&rel).map(|()| {
                            state.files.remove(&rel);
                            removed.insert(rel.clone());
                            report.deleted_remote += 1;
                            progress(&format!("删除远程 {rel}"));
                        })
                    };
                    if let Err(e) = result {
                        report.failures.push(format!("{rel}: {e:#}"));
                    }
                }
            }
        } else {
            // The remote is empty. Never wipe the library on a misconfigured
            // folder: forget the entry, the next pass re-uploads the files.
            state.files.remove(&rel);
        }
    }

    // Files that exist only on the remote side.
    for (rel, r) in &remote {
        if seen.contains(rel) || state.files.contains_key(rel) || removed.contains(rel) {
            continue;
        }
        if root.join(rel).is_file() {
            continue;
        }
        if let Err(e) = download(root, rel, r, backend, &mut state, &mut report, &mut progress) {
            report.failures.push(format!("{rel}: {e:#}"));
        }
    }

    state.save(root, state_name)?;
    progress("完成");
    Ok(report)
}

fn lock(root: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(root.join(".sync.lock"))?;
    file.try_lock().map_err(|_| anyhow!("另一个同步正在进行，请稍后再试"))?;
    Ok(file)
}

fn sync_file(
    root: &Path,
    rel: &str,
    local_mtime: u64,
    backend: &mut dyn Backend,
    remote: Option<&RemoteFile>,
    state: &mut State,
    report: &mut Report,
    progress: &mut impl FnMut(&str),
) -> Result<()> {
    let path = root.join(rel);
    let meta = std::fs::metadata(&path)?;
    let size = meta.len();
    let tracked = state.files.get(rel).copied();
    let fast_unchanged = tracked.is_some_and(|s| s.size == size && s.mtime == local_mtime);
    let (hash, local_changed) = if fast_unchanged {
        (tracked.unwrap().hash, false)
    } else {
        let bytes = std::fs::read(&path)?;
        let h = fnv(&bytes);
        (h, tracked.is_none_or(|s| s.hash != h || s.size != size))
    };
    match remote {
        None => {
            if tracked.is_some() && !local_changed {
                // The remote copy went away; the deletion wins here too.
                std::fs::remove_file(&path)?;
                state.files.remove(rel);
                report.deleted_local += 1;
                progress(&format!("删除本地 {rel}"));
            } else {
                let bytes = std::fs::read(&path)?;
                backend.put(rel, &bytes)?;
                state.files.insert(rel.to_string(), Tracked { hash, size, mtime: local_mtime, remote_size: bytes.len() as u64, remote_mtime: 0 });
                report.uploaded += 1;
                report.bytes_up += bytes.len() as u64;
                progress(&format!("上传 {rel}"));
            }
        }
        Some(r) => {
            let remote_changed = match tracked {
                None => true,
                Some(s) if s.remote_mtime == 0 => r.size != s.remote_size,
                Some(s) => r.size != s.remote_size || r.mtime.abs_diff(s.remote_mtime) > 2,
            };
            if local_changed && remote_changed {
                // Both sides edited it: the newer file wins, ties go to the
                // file on this machine (the user is looking at it).
                if r.mtime != 0 && r.mtime > local_mtime {
                    download(root, rel, r, backend, state, report, progress)?;
                } else {
                    let bytes = std::fs::read(&path)?;
                    backend.put(rel, &bytes)?;
                    state.files.insert(rel.to_string(), Tracked { hash, size, mtime: local_mtime, remote_size: bytes.len() as u64, remote_mtime: 0 });
                    report.uploaded += 1;
                    report.bytes_up += bytes.len() as u64;
                    progress(&format!("上传 {rel}"));
                }
            } else if local_changed {
                let bytes = std::fs::read(&path)?;
                backend.put(rel, &bytes)?;
                state.files.insert(rel.to_string(), Tracked { hash, size, mtime: local_mtime, remote_size: bytes.len() as u64, remote_mtime: 0 });
                report.uploaded += 1;
                report.bytes_up += bytes.len() as u64;
                progress(&format!("上传 {rel}"));
            } else if remote_changed {
                download(root, rel, r, backend, state, report, progress)?;
            } else if let Some(mut s) = tracked {
                // Latch the real remote time after an upload so later remote
                // edits are noticed.
                if s.remote_mtime == 0 && r.mtime != 0 {
                    s.remote_mtime = r.mtime;
                    s.remote_size = r.size;
                    state.files.insert(rel.to_string(), s);
                }
            } else {
                state.files.insert(rel.to_string(), Tracked { hash, size, mtime: local_mtime, remote_size: r.size, remote_mtime: r.mtime });
            }
        }
    }
    Ok(())
}

fn sync_index(
    root: &Path,
    rel: &str,
    backend: &mut dyn Backend,
    remote: Option<&RemoteFile>,
    state: &mut State,
    report: &mut Report,
    progress: &mut impl FnMut(&str),
) -> Result<()> {
    let path = root.join(rel);
    let local = std::fs::read(&path).ok();
    match (local, remote) {
        (Some(bytes), None) => {
            backend.put(rel, &bytes)?;
            remember(root, rel, &bytes, bytes.len() as u64, 0, state)?;
            report.uploaded += 1;
            report.bytes_up += bytes.len() as u64;
            progress(&format!("上传 {rel}"));
        }
        (None, Some(r)) => download(root, rel, r, backend, state, report, progress)?,
        (Some(bytes), Some(r)) => {
            let remote_bytes = backend.get(rel)?;
            if bytes == remote_bytes {
                remember(root, rel, &bytes, r.size, r.mtime, state)?;
                return Ok(());
            }
            let merged = merge_json(&bytes, &remote_bytes)?;
            if merged != bytes {
                write_local(root, rel, &merged)?;
            }
            if merged != remote_bytes {
                backend.put(rel, &merged)?;
            }
            remember(root, rel, &merged, merged.len() as u64, 0, state)?;
            report.merged += 1;
            progress(&format!("合并 {rel}"));
        }
        (None, None) => {
            state.files.remove(rel);
        }
    }
    Ok(())
}

fn remember(root: &Path, rel: &str, bytes: &[u8], remote_size: u64, remote_mtime: u64, state: &mut State) -> Result<()> {
    let meta = std::fs::metadata(root.join(rel))?;
    state.files.insert(
        rel.to_string(),
        Tracked { hash: fnv(bytes), size: meta.len(), mtime: mtime_of(&meta), remote_size, remote_mtime },
    );
    Ok(())
}

fn download(
    root: &Path,
    rel: &str,
    r: &RemoteFile,
    backend: &mut dyn Backend,
    state: &mut State,
    report: &mut Report,
    progress: &mut impl FnMut(&str),
) -> Result<()> {
    let bytes = backend.get(rel)?;
    write_local(root, rel, &bytes)?;
    let meta = std::fs::metadata(root.join(rel))?;
    state.files.insert(
        rel.to_string(),
        Tracked { hash: fnv(&bytes), size: meta.len(), mtime: mtime_of(&meta), remote_size: r.size, remote_mtime: r.mtime },
    );
    report.downloaded += 1;
    report.bytes_down += bytes.len() as u64;
    progress(&format!("下载 {rel}"));
    Ok(())
}

fn write_local(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The library files that travel: documents, their images and attachments,
/// user themes, both indexes and the format marker. Caches (remote images,
/// thumbnails) stay behind.
pub(crate) fn syncable(rel: &str) -> bool {
    if matches!(rel, "format.json" | INDEX | TRASH_INDEX) {
        return true;
    }
    let flat = |rest: &str, ext: &str| {
        !rest.is_empty()
            && !rest.contains('/')
            && !rest.contains('\\')
            && !rest.starts_with('.')
            && rest.ends_with(ext)
    };
    if let Some(rest) = rel.strip_prefix("docs/") {
        return flat(rest, ".xml");
    }
    if let Some(rest) = rel.strip_prefix("trash/") {
        return flat(rest, ".xml");
    }
    if let Some(rest) = rel.strip_prefix("themes/") {
        return flat(rest, ".css");
    }
    if let Some(rest) = rel.strip_prefix("assets/") {
        if rest.is_empty() || rest.starts_with("remote/") || rest.starts_with("thumbs/") || rest.ends_with(".tmp") || rest.contains("..") {
            return false;
        }
        return !rest.split('/').any(|p| p.is_empty() || p == "." || p == "..");
    }
    false
}

fn local_files(root: &Path, out: &mut BTreeMap<String, u64>) -> Result<()> {
    for fixed in ["format.json", INDEX, TRASH_INDEX] {
        if let Ok(meta) = std::fs::metadata(root.join(fixed))
            && meta.is_file()
        {
            out.insert(fixed.to_string(), mtime_of(&meta));
        }
    }
    scan(root.join("docs"), "docs", Some("xml"), out);
    scan(root.join("trash"), "trash", Some("xml"), out);
    scan(root.join("themes"), "themes", Some("css"), out);
    scan_assets(root.join("assets"), "assets", true, out);
    out.retain(|rel, _| syncable(rel));
    Ok(())
}

fn scan(dir: PathBuf, prefix: &str, ext: Option<&str>, out: &mut BTreeMap<String, u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if let Some(ext) = ext
            && path.extension().and_then(|e| e.to_str()) != Some(ext)
        {
            continue;
        }
        out.insert(format!("{prefix}/{name}"), mtime_of(&meta));
    }
}

fn scan_assets(dir: PathBuf, prefix: &str, top: bool, out: &mut BTreeMap<String, u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if top && (name == "remote" || name == "thumbs") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let rel = format!("{prefix}/{name}");
        if meta.is_dir() {
            scan_assets(path, &rel, false, out);
        } else if meta.is_file() && !name.ends_with(".tmp") {
            out.insert(rel, mtime_of(&meta));
        }
    }
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Merge two JSON objects field by field: numbers take the larger value,
/// booleans OR, and the rest comes from the side with the newer `modified`
/// (or `deleted`) stamp. That keeps a 收藏 from one device and a 移动 from
/// another instead of letting one `index.json` overwrite the other.
pub(crate) fn merge_json(local: &[u8], remote: &[u8]) -> Result<Vec<u8>> {
    let mut a: Value = serde_json::from_slice(local).context("本地索引不是有效 JSON")?;
    let b: Value = serde_json::from_slice(remote).context("远程索引不是有效 JSON")?;
    merge_value(&mut a, &b);
    Ok(serde_json::to_vec_pretty(&a)?)
}

fn merge_value(local: &mut Value, remote: &Value) {
    let (Value::Object(l), Value::Object(r)) = (&mut *local, remote) else { return };
    let prefer_remote = number(r.get("modified")) > number(l.get("modified")) || number(r.get("deleted")) > number(l.get("deleted"));
    let keys: Vec<String> = r.keys().cloned().collect();
    for k in keys {
        let rv = &r[&k];
        match l.get_mut(&k) {
            None => {
                l.insert(k, rv.clone());
            }
            Some(lv) => match (&mut *lv, rv) {
                (Value::Object(_), Value::Object(_)) => merge_value(lv, rv),
                (Value::Number(_), Value::Number(_)) => {
                    if number(Some(rv)) > number(Some(lv)) {
                        *lv = rv.clone();
                    }
                }
                (Value::Bool(a), Value::Bool(b)) => *a = *a || *b,
                _ => {
                    if prefer_remote {
                        *lv = rv.clone();
                    }
                }
            },
        }
    }
}

fn number(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A stand-in for the remote side: a plain folder.
    struct LocalDir(PathBuf);

    impl LocalDir {
        fn new(p: &Path) -> Self {
            std::fs::create_dir_all(p).unwrap();
            Self(p.into())
        }
    }

    impl Backend for LocalDir {
        fn list(&mut self) -> Result<Vec<RemoteFile>> {
            fn walk(base: &Path, dir: &Path, out: &mut Vec<RemoteFile>) -> Result<()> {
                for e in std::fs::read_dir(dir)?.flatten() {
                    let p = e.path();
                    let meta = e.metadata()?;
                    if meta.is_dir() {
                        walk(base, &p, out)?;
                    } else {
                        let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                        out.push(RemoteFile { path: rel, size: meta.len(), mtime: mtime_of(&meta) });
                    }
                }
                Ok(())
            }
            let mut out = vec![];
            walk(&self.0, &self.0, &mut out)?;
            Ok(out)
        }

        fn get(&mut self, path: &str) -> Result<Vec<u8>> {
            Ok(std::fs::read(self.0.join(path))?)
        }

        fn put(&mut self, path: &str, data: &[u8]) -> Result<()> {
            let p = self.0.join(path);
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(p, data)?;
            Ok(())
        }

        fn delete(&mut self, path: &str) -> Result<()> {
            Ok(std::fs::remove_file(self.0.join(path))?)
        }
    }

    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let p = std::env::temp_dir().join(format!("fdl-sync-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Temp(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    fn put_at(root: &Path, rel: &str, bytes: &[u8], at: SystemTime) {
        put(root, rel, bytes);
        std::fs::File::options().write(true).open(root.join(rel)).unwrap().set_modified(at).unwrap();
    }

    fn read(root: &Path, rel: &str) -> Vec<u8> {
        std::fs::read(root.join(rel)).unwrap()
    }

    fn run_sync(lib: &Path, remote: &Path) -> Report {
        let mut backend = LocalDir::new(remote);
        let mut steps = vec![];
        let report = run(lib, &mut backend, &mut |s| steps.push(s.to_string()), STATE).unwrap();
        assert!(report.failures.is_empty(), "failures: {:?}", report.failures);
        report
    }

    fn index_entry(bytes: &[u8], id: &str) -> Value {
        let v: Value = serde_json::from_slice(bytes).unwrap();
        v.get(id).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn separate_state_files_track_independently() {
        let lib = Temp::new("lib");
        let r1 = Temp::new("remote");
        let r2 = Temp::new("remote");
        put(lib.path(), "docs/a.xml", b"<a/>");
        let mut b1 = LocalDir::new(r1.path());
        let first = run(lib.path(), &mut b1, &mut |_| {}, "sync-state-ftp.json").unwrap();
        assert_eq!(first.uploaded, 1);
        // A second protocol keeps its own state, so it still uploads the file.
        let mut b2 = LocalDir::new(r2.path());
        let second = run(lib.path(), &mut b2, &mut |_| {}, "sync-state-s3.json").unwrap();
        assert_eq!(second.uploaded, 1);
        assert!(lib.path().join("sync-state-ftp.json").is_file());
        assert!(lib.path().join("sync-state-s3.json").is_file());
        // The first protocol's state is intact: re-running it moves nothing.
        let mut b1 = LocalDir::new(r1.path());
        let again = run(lib.path(), &mut b1, &mut |_| {}, "sync-state-ftp.json").unwrap();
        assert!(!again.changed());
    }

    #[test]
    fn first_sync_copies_both_ways() {
        let lib = Temp::new("lib");
        let remote = Temp::new("remote");
        put(lib.path(), "docs/a.xml", b"<a/>");
        put(lib.path(), "themes/t.css", b"a{}");
        put(lib.path(), "assets/x.png", b"png");
        put(lib.path(), "assets/remote/cached.png", b"cache");
        put(lib.path(), "assets/thumbs/t.png", b"thumb");
        put(lib.path(), "index.json", br#"{"a":{"created":1,"modified":1}}"#);
        put(remote.path(), "docs/b.xml", b"<b/>");
        put(remote.path(), "index.json", br#"{"b":{"created":2,"modified":2}}"#);

        let report = run_sync(lib.path(), remote.path());
        assert!(report.changed());
        assert_eq!(report.uploaded, 3); // docs/a, themes/t, assets/x
        assert_eq!(report.downloaded, 1);
        assert_eq!(report.merged, 1);

        assert_eq!(read(remote.path(), "docs/a.xml"), b"<a/>");
        assert_eq!(read(remote.path(), "themes/t.css"), b"a{}");
        assert_eq!(read(remote.path(), "assets/x.png"), b"png");
        assert!(!remote.path().join("assets/remote/cached.png").exists());
        assert!(!remote.path().join("assets/thumbs/t.png").exists());
        assert_eq!(read(lib.path(), "docs/b.xml"), b"<b/>");
        assert_eq!(
            read(lib.path(), "index.json"),
            read(remote.path(), "index.json"),
            "both indexes must end up equal"
        );
        assert_ne!(index_entry(&read(lib.path(), "index.json"), "a"), Value::Null);
        assert_ne!(index_entry(&read(lib.path(), "index.json"), "b"), Value::Null);
    }

    #[test]
    fn second_sync_is_a_no_op() {
        let lib = Temp::new("lib2");
        let remote = Temp::new("remote2");
        put(lib.path(), "docs/a.xml", b"<a/>");
        put(lib.path(), "index.json", br#"{"a":{"created":1,"modified":1}}"#);
        let first = run_sync(lib.path(), remote.path());
        assert!(first.changed());
        let second = run_sync(lib.path(), remote.path());
        assert!(!second.changed(), "{:?}", second);
    }

    #[test]
    fn edits_and_deletions_travel() {
        let lib = Temp::new("lib3");
        let remote = Temp::new("remote3");
        put(lib.path(), "docs/a.xml", b"<a/>");
        put(lib.path(), "docs/b.xml", b"<b/>");
        put(lib.path(), "index.json", br#"{}"#);
        run_sync(lib.path(), remote.path());

        put(lib.path(), "docs/a.xml", b"<a2/>");
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.uploaded, 1);
        assert_eq!(read(remote.path(), "docs/a.xml"), b"<a2/>");

        std::fs::remove_file(lib.path().join("docs/a.xml")).unwrap();
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.deleted_remote, 1);
        assert!(!remote.path().join("docs/a.xml").exists());
        assert!(remote.path().join("docs/b.xml").exists());

        std::fs::remove_file(remote.path().join("docs/b.xml")).unwrap();
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.deleted_local, 1);
        assert!(!lib.path().join("docs/b.xml").exists());
    }

    #[test]
    fn indexes_merge_instead_of_overwriting() {
        let lib = Temp::new("lib4");
        let remote = Temp::new("remote4");
        put(lib.path(), "index.json", br#"{"a":{"modified":5,"pinned":false},"b":{"modified":1}}"#);
        put(remote.path(), "index.json", br#"{"a":{"modified":9,"pinned":true},"c":{"modified":3}}"#);
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.merged, 1);
        let merged = read(lib.path(), "index.json");
        assert_eq!(merged, read(remote.path(), "index.json"));
        let a = index_entry(&merged, "a");
        assert_eq!(a["modified"], 9);
        assert_eq!(a["pinned"], true);
        assert_ne!(index_entry(&merged, "b"), Value::Null);
        assert_ne!(index_entry(&merged, "c"), Value::Null);
    }

    #[test]
    fn conflicts_keep_the_newer_side() {
        let lib = Temp::new("lib5");
        let remote = Temp::new("remote5");
        put(lib.path(), "docs/c.xml", b"<c/>");
        put(lib.path(), "index.json", br#"{}"#);
        run_sync(lib.path(), remote.path());

        // Remote copy is older: this machine's edit wins.
        put(lib.path(), "docs/c.xml", b"<here/>");
        put_at(remote.path(), "docs/c.xml", b"<there/>", SystemTime::now() - Duration::from_secs(600));
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.uploaded, 1);
        assert_eq!(read(remote.path(), "docs/c.xml"), b"<here/>");

        // Remote copy is newer: it wins here.
        put(lib.path(), "docs/c.xml", b"<here again/>");
        put_at(remote.path(), "docs/c.xml", b"<there again/>", SystemTime::now() + Duration::from_secs(600));
        let r = run_sync(lib.path(), remote.path());
        assert_eq!(r.downloaded, 1);
        assert_eq!(read(lib.path(), "docs/c.xml"), b"<there again/>");
    }

    #[test]
    fn empty_remote_never_wipes_the_library() {
        let lib = Temp::new("lib6");
        let remote = Temp::new("remote6");
        put(lib.path(), "docs/a.xml", b"<a/>");
        // A state file that claims a second, now missing document.
        put(lib.path(), "sync-state.json", br#"{"version":1,"files":{"docs/gone.xml":{"hash":1,"size":1,"mtime":1,"remote_size":1,"remote_mtime":1}}}"#);
        let r = run_sync(lib.path(), remote.path());
        assert!(lib.path().join("docs/a.xml").exists());
        assert_eq!(read(remote.path(), "docs/a.xml"), b"<a/>");
        assert_eq!(r.uploaded, 1);
    }
}
