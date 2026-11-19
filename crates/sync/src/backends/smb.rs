//! SMB2/3 through the pure-Rust smb2 client.

use crate::{Backend, Config, RemoteFile};
use anyhow::{bail, Context, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

pub(crate) struct Smb {
    rt: Runtime,
    client: smb2::SmbClient,
    tree: smb2::Tree,
    root: String,
}

fn join(a: &str, b: &str) -> String {
    super::join(a, b)
}

fn secs(t: Option<SystemTime>) -> u64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

impl Smb {
    pub(super) fn connect(cfg: &Config) -> Result<Self> {
        let (share, sub) = cfg.smb_share().context("请填写共享名")?;
        let share = share.to_string();
        let share_label = share.clone();
        let root = sub.trim_matches('/').to_string();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().context("无法启动网络运行时")?;
        let addr = format!("{}:{}", cfg.host.trim(), cfg.port_or_default());
        let user = cfg.user.trim().to_string();
        let password = cfg.password.clone();
        let (client, tree) = rt.block_on(async move {
            let mut client = smb2::SmbClient::connect(smb2::ClientConfig {
                addr,
                timeout: Duration::from_secs(10),
                username: user,
                password,
                ..Default::default()
            })
            .await
            .context("无法连接 SMB 服务器")?;
            let tree = client.connect_share(&share).await.with_context(|| format!("无法打开共享 {share}"))?;
            Ok::<_, anyhow::Error>((client, tree))
        })?;
        let mut smb = Smb { rt, client, tree, root };
        if !smb.root.is_empty() {
            let root = smb.root.clone();
            if smb.rt.block_on(smb.client.stat(&mut smb.tree, &root)).is_err() {
                bail!("共享 {share_label} 中没有目录 {root}");
            }
        }
        Ok(smb)
    }

    fn walk(&mut self, dir: &str, rel: &str, out: &mut Vec<RemoteFile>) -> Result<()> {
        let entries = self.rt.block_on(self.client.list_directory(&mut self.tree, dir)).with_context(|| format!("无法列出远程目录 {dir}"))?;
        for e in entries {
            if e.name == "." || e.name == ".." {
                continue;
            }
            let child_dir = join(dir, &e.name);
            let child_rel = join(rel, &e.name);
            if e.is_directory {
                self.walk(&child_dir, &child_rel, out)?;
            } else {
                out.push(RemoteFile { path: child_rel, size: e.size, mtime: secs(e.modified.to_system_time()) });
            }
        }
        Ok(())
    }

    fn full(&self, path: &str) -> String {
        join(&self.root, path)
    }

    fn mkdirs(&mut self, path: &str) -> Result<()> {
        let Some((parent, _)) = path.rsplit_once('/') else { return Ok(()) };
        let mut acc = self.root.clone();
        for part in parent.split('/') {
            acc = join(&acc, part);
            let exists = self.rt.block_on(self.client.stat(&mut self.tree, &acc)).is_ok();
            if !exists {
                self.rt.block_on(self.client.create_directory(&self.tree, &acc)).with_context(|| format!("无法创建远程目录 {acc}"))?;
            }
        }
        Ok(())
    }
}

impl Backend for Smb {
    fn list(&mut self) -> Result<Vec<RemoteFile>> {
        let mut out = vec![];
        let root = self.root.clone();
        self.walk(&root, "", &mut out)?;
        Ok(out)
    }

    fn get(&mut self, path: &str) -> Result<Vec<u8>> {
        let full = self.full(path);
        self.rt.block_on(self.client.read_file(&mut self.tree, &full)).with_context(|| format!("无法下载 {path}"))
    }

    fn put(&mut self, path: &str, data: &[u8]) -> Result<()> {
        self.mkdirs(path)?;
        let full = self.full(path);
        self.rt.block_on(self.client.write_file(&self.tree, &full, data)).with_context(|| format!("无法上传 {path}")).map(|_| ())
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        let full = self.full(path);
        self.rt.block_on(self.client.delete_file(&self.tree, &full)).with_context(|| format!("无法删除远程文件 {path}"))
    }
}

impl Drop for Smb {
    fn drop(&mut self) {
        let _ = self.rt.block_on(self.client.disconnect_share(&self.tree));
    }
}
