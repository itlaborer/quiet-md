//! SFTP through russh + russh-sftp (pure Rust, no OpenSSL to cross-build).

use crate::{Backend, Config, RemoteFile};
use anyhow::{bail, Context, Result};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Runtime;

struct Client;

impl russh::client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, _key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        // The user typed this server's address and credentials; the same
        // trust model as ssh with 首次连接确认. A changed host key is not
        // detected — pin it later if that matters for the deployment.
        Ok(true)
    }
}

pub(crate) struct Sftp {
    rt: Runtime,
    session: russh::client::Handle<Client>,
    sftp: SftpSession,
    root: String,
}

fn join(a: &str, b: &str) -> String {
    super::join(a, b)
}

impl Sftp {
    pub(super) fn connect(cfg: &Config) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().context("无法启动网络运行时")?;
        let host = cfg.host.trim().to_string();
        let port = cfg.port_or_default();
        let user = cfg.user.trim().to_string();
        let password = cfg.password.clone();
        let (session, sftp) = rt.block_on(async move {
            let mut session = russh::client::connect(Arc::new(russh::client::Config::default()), (host.as_str(), port), Client)
                .await
                .with_context(|| format!("无法连接 {host}:{port}"))?;
            let auth = session.authenticate_password(user, password).await.context("SSH 认证失败")?;
            if !auth.success() {
                bail!("SSH 认证失败，请检查用户名和密码");
            }
            let channel = session.channel_open_session().await.context("无法打开 SSH 会话")?;
            channel.request_subsystem(true, "sftp").await.context("服务器没有 SFTP 子系统")?;
            let sftp = SftpSession::new(channel.into_stream()).await.context("SFTP 初始化失败")?;
            Ok::<_, anyhow::Error>((session, sftp))
        })?;
        let root = cfg.remote_folder().to_string();
        if !root.is_empty() {
            let exists = rt.block_on(sftp.metadata(&root)).is_ok();
            if !exists { bail!("远程目录不存在：{root}") }
        }
        Ok(Sftp { rt, session, sftp, root })
    }

    fn walk(&mut self, dir: &str, rel: &str, out: &mut Vec<RemoteFile>) -> Result<()> {
        let entries = self.rt.block_on(self.sftp.read_dir(dir)).with_context(|| format!("无法列出远程目录 {dir}"))?;
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let meta = entry.metadata();
            let child_dir = join(dir, &name);
            let child_rel = join(rel, &name);
            if entry.file_type().is_dir() {
                self.walk(&child_dir, &child_rel, out)?;
            } else {
                out.push(RemoteFile { path: child_rel, size: meta.size.unwrap_or(0), mtime: meta.mtime.map(u64::from).unwrap_or(0) });
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
            if self.rt.block_on(self.sftp.metadata(&acc)).is_err() {
                self.rt.block_on(self.sftp.create_dir(&acc)).with_context(|| format!("无法创建远程目录 {acc}"))?;
            }
        }
        Ok(())
    }
}

impl Backend for Sftp {
    fn list(&mut self) -> Result<Vec<RemoteFile>> {
        let mut out = vec![];
        let root = if self.root.is_empty() { ".".to_string() } else { self.root.clone() };
        self.walk(&root, "", &mut out)?;
        Ok(out)
    }

    fn get(&mut self, path: &str) -> Result<Vec<u8>> {
        let full = self.full(path);
        self.rt.block_on(self.sftp.read(&full)).with_context(|| format!("无法下载 {path}"))
    }

    fn put(&mut self, path: &str, data: &[u8]) -> Result<()> {
        self.mkdirs(path)?;
        let full = self.full(path);
        self.rt.block_on(async {
            let mut file = self.sftp.open_with_flags(&full, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
            file.write_all(data).await?;
            file.shutdown().await?;
            Ok::<(), anyhow::Error>(())
        }).with_context(|| format!("无法上传 {path}"))
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        let full = self.full(path);
        self.rt.block_on(self.sftp.remove_file(&full)).with_context(|| format!("无法删除远程文件 {path}"))
    }
}

impl Drop for Sftp {
    fn drop(&mut self) {
        let _ = self.rt.block_on(self.session.disconnect(russh::Disconnect::ByApplication, "", "bye"));
    }
}
