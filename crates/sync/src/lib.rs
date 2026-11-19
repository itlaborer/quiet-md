//! Remote mirror of the quiet.md library: one folder per device stays in step
//! with FTP/FTPS, SFTP, SMB or S3 storage.
//!
//! The engine is incremental. A small `sync-state.json` in the library records
//! what was last exchanged, so only files that really changed travel, local
//! deletions remove their remote copy and remote deletions remove theirs. When
//! both sides changed the newer file wins; the two JSON indexes (`index.json`,
//! `trash/index.json`) are merged field by field instead of overwritten.

mod backends;
mod config;
mod engine;

pub use config::{Config, Kind, Settings};
pub use engine::{sync, sync_with, Report};

use std::path::Path;

/// Sync every enabled connection in turn, each against its own state file. The
/// progress line is prefixed with the protocol label. Returns one result per
/// enabled connection; a failing server is reported without stopping the rest.
pub fn sync_enabled(root: &Path, settings: &Settings, mut progress: impl FnMut(&str)) -> Vec<(Kind, anyhow::Result<Report>)> {
    let mut out = Vec::new();
    for kind in settings.enabled_kinds() {
        let cfg = settings.connection(kind);
        let label = kind.label();
        progress(&format!("{label}: 正在连接…"));
        let r = sync_with(root, cfg, |s| progress(&format!("{label}: {s}")));
        out.push((kind, r));
    }
    out
}

/// Connect and list the remote folder without changing anything (设置 →
/// 同步 → 测试连接). Returns how many library files it can see.
pub fn test(cfg: &Config) -> anyhow::Result<usize> {
    cfg.validate()?;
    let mut backend = backends::connect(cfg)?;
    let files = backend.list()?;
    Ok(files.iter().filter(|f| engine::syncable(&f.path)).count())
}

/// A file as the remote side lists it.
#[derive(Clone, Debug)]
pub struct RemoteFile {
    /// Path relative to the configured remote folder, with `/` separators.
    pub path: String,
    pub size: u64,
    /// Modification time in Unix seconds; 0 when the server does not say.
    pub mtime: u64,
}

/// The four protocols behind one small interface. Everything is blocking; the
/// UI runs a sync on its own thread.
pub(crate) trait Backend {
    fn list(&mut self) -> anyhow::Result<Vec<RemoteFile>>;
    fn get(&mut self, path: &str) -> anyhow::Result<Vec<u8>>;
    fn put(&mut self, path: &str, data: &[u8]) -> anyhow::Result<()>;
    fn delete(&mut self, path: &str) -> anyhow::Result<()>;
}
