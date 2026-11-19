//! The sync connection, stored as `sync.json` in the library root.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Ftp,
    Sftp,
    Smb,
    S3,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Ftp, Kind::Sftp, Kind::Smb, Kind::S3];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Ftp => "FTP",
            Kind::Sftp => "SFTP",
            Kind::Smb => "SMB",
            Kind::S3 => "S3",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Kind::Ftp => "ftp",
            Kind::Sftp => "sftp",
            Kind::Smb => "smb",
            Kind::S3 => "s3",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Ftp => 21,
            Kind::Sftp => 22,
            Kind::Smb => 445,
            Kind::S3 => 443,
        }
    }

    /// 是否使用主机名 + 端口 + 账号这一组字段.
    pub fn uses_host(self) -> bool {
        self != Kind::S3
    }

    pub fn is_s3(self) -> bool {
        self == Kind::S3
    }
}

/// One protocol's connection. The library keeps one of these per protocol (see
/// [`Settings`]), edited in 设置 → 同步. Passwords live in `sync.json` as plain
/// text, readable only by the account that owns the library (mode 0600).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub kind: Kind,
    /// 启用此协议：立即同步 / 自动同步依次同步所有已启用的协议。
    pub enabled: bool,
    pub host: String,
    /// 0: the protocol's default port.
    pub port: u16,
    pub user: String,
    pub password: String,
    /// FTP/SFTP: the remote folder. SMB: `share/子目录`. S3: key prefix.
    pub path: String,
    /// FTP over explicit TLS (FTPS).
    pub ftps: bool,
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    /// S3-compatible servers (MinIO, Ceph …) usually need path-style URLs.
    pub path_style: bool,
    /// Unix seconds of the last successful sync (shown in 设置).
    pub last_sync: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            kind: Kind::Ftp,
            enabled: false,
            host: String::new(),
            port: 0,
            user: String::new(),
            password: String::new(),
            path: String::new(),
            ftps: false,
            bucket: String::new(),
            region: "us-east-1".into(),
            endpoint: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            path_style: false,
            last_sync: 0,
        }
    }
}

impl Config {
    pub fn port_or_default(&self) -> u16 {
        if self.port == 0 { self.kind.default_port() } else { self.port }
    }

    pub fn region_or_default(&self) -> &str {
        if self.region.trim().is_empty() { "us-east-1" } else { self.region.trim() }
    }

    /// The HTTPS endpoint for a custom S3 endpoint or plain AWS.
    pub fn endpoint_or_default(&self) -> String {
        let e = self.endpoint.trim().trim_end_matches('/');
        if !e.is_empty() {
            if e.starts_with("http://") || e.starts_with("https://") { e.to_string() } else { format!("https://{e}") }
        } else if self.region_or_default() == "us-east-1" {
            "https://s3.amazonaws.com".into()
        } else {
            format!("https://s3.{}.amazonaws.com", self.region_or_default())
        }
    }

    /// `share/子目录` for SMB: the first part is the share name.
    pub fn smb_share(&self) -> Option<(&str, &str)> {
        let p = self.path.trim().trim_matches('/');
        if p.is_empty() { return None }
        match p.split_once('/') {
            Some((share, sub)) => Some((share, sub)),
            None => Some((p, "")),
        }
    }

    pub fn remote_folder(&self) -> &str {
        self.path.trim().trim_matches('/')
    }

    pub fn validate(&self) -> Result<()> {
        let host = self.host.trim();
        match self.kind {
            Kind::Ftp | Kind::Sftp => {
                if host.is_empty() { bail!("请填写服务器地址") }
                if self.kind == Kind::Sftp && self.user.trim().is_empty() { bail!("请填写 SSH 用户名") }
            }
            Kind::Smb => {
                if host.is_empty() { bail!("请填写服务器地址") }
                if self.smb_share().is_none() { bail!("请填写共享名，例如 Documents 或 Documents/笔记") }
                if self.user.trim().is_empty() { bail!("请填写用户名") }
            }
            Kind::S3 => {
                if self.bucket.trim().is_empty() { bail!("请填写存储桶名称") }
                if self.access_key.trim().is_empty() || self.secret_key.trim().is_empty() { bail!("请填写 Access Key 和 Secret Key") }
            }
        }
        Ok(())
    }
}

/// All four protocols' connections plus the shared automatic-sync flag, stored
/// together as `sync.json`. Each protocol is configured independently and any
/// number may be enabled at once; a sync visits each enabled one in turn.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 自动同步: on launch and after local changes (applies to all enabled).
    pub auto: bool,
    /// The protocol shown in 设置 → 同步.
    pub selected: Kind,
    pub ftp: Config,
    pub sftp: Config,
    pub smb: Config,
    pub s3: Config,
}

impl Default for Settings {
    fn default() -> Self {
        let mut s = Settings { auto: false, selected: Kind::Ftp, ftp: Config::default(), sftp: Config::default(), smb: Config::default(), s3: Config::default() };
        s.ftp.kind = Kind::Ftp;
        s.sftp.kind = Kind::Sftp;
        s.smb.kind = Kind::Smb;
        s.s3.kind = Kind::S3;
        s
    }
}

impl Settings {
    pub fn connection(&self, k: Kind) -> &Config {
        match k { Kind::Ftp => &self.ftp, Kind::Sftp => &self.sftp, Kind::Smb => &self.smb, Kind::S3 => &self.s3 }
    }

    pub fn connection_mut(&mut self, k: Kind) -> &mut Config {
        match k { Kind::Ftp => &mut self.ftp, Kind::Sftp => &mut self.sftp, Kind::Smb => &mut self.smb, Kind::S3 => &mut self.s3 }
    }

    /// The enabled protocols, in a fixed order.
    pub fn enabled_kinds(&self) -> Vec<Kind> {
        Kind::ALL.into_iter().filter(|k| self.connection(*k).enabled).collect()
    }

    /// Read `sync.json`, migrating the original single-connection format (a
    /// top-level `kind`) into the matching protocol slot and carrying its
    /// incremental state file across.
    pub fn load(root: &Path) -> Settings {
        let Ok(bytes) = std::fs::read(root.join("sync.json")) else { return Settings::default() };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Settings::default() };
        if v.get("kind").is_some() && v.get("ftp").is_none() {
            let auto = v.get("auto").and_then(|x| x.as_bool()).unwrap_or(false);
            let mut cfg: Config = serde_json::from_value(v).unwrap_or_default();
            let kind = cfg.kind;
            cfg.enabled = true;
            let mut s = Settings { auto, selected: kind, ..Settings::default() };
            *s.connection_mut(kind) = cfg;
            let old = root.join("sync-state.json");
            let new = root.join(format!("sync-state-{}.json", kind.id()));
            if old.exists() && !new.exists() { let _ = std::fs::rename(old, new); }
            return s;
        }
        let mut s: Settings = serde_json::from_value(v).unwrap_or_default();
        s.ftp.kind = Kind::Ftp;
        s.sftp.kind = Kind::Sftp;
        s.smb.kind = Kind::Smb;
        s.s3.kind = Kind::S3;
        s
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = root.join("sync.json");
        let tmp = root.join("sync.json.tmp");
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("无法写入 {}", tmp.display()))?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("fdl-sync-cfg-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn old_single_connection_migrates_to_its_slot() {
        let root = temp("migrate");
        std::fs::write(root.join("sync.json"), br#"{"kind":"sftp","auto":true,"host":"h.example","user":"u","password":"test-pass","path":"notes"}"#).unwrap();
        std::fs::write(root.join("sync-state.json"), br#"{"version":1}"#).unwrap();
        let s = Settings::load(&root);
        assert!(s.auto);
        assert_eq!(s.selected, Kind::Sftp);
        assert_eq!(s.enabled_kinds(), vec![Kind::Sftp]);
        let c = s.connection(Kind::Sftp);
        assert!(c.enabled);
        assert_eq!(c.host, "h.example");
        assert_eq!(c.password, "test-pass");
        assert_eq!(c.path, "notes");
        assert_eq!(s.connection(Kind::Ftp).host, "");
        assert!(!s.connection(Kind::Ftp).enabled);
        assert!(!root.join("sync-state.json").exists());
        assert!(root.join("sync-state-sftp.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_round_trip_keeps_each_protocol() {
        let root = temp("roundtrip");
        let mut s = Settings::default();
        s.auto = true;
        s.selected = Kind::S3;
        s.ftp.host = "ftp.example".into();
        s.ftp.enabled = true;
        s.smb.host = "nas".into();
        s.s3.bucket = "b".into();
        s.s3.enabled = true;
        s.s3.secret_key = "test-key".into();
        s.save(&root).unwrap();
        let l = Settings::load(&root);
        assert!(l.auto);
        assert_eq!(l.selected, Kind::S3);
        assert_eq!(l.enabled_kinds(), vec![Kind::Ftp, Kind::S3]);
        assert_eq!(l.ftp.host, "ftp.example");
        assert_eq!(l.smb.host, "nas");
        assert!(!l.smb.enabled);
        assert_eq!(l.s3.secret_key, "test-key");
        assert_eq!(l.sftp.kind, Kind::Sftp);
        let _ = std::fs::remove_dir_all(&root);
    }
}
