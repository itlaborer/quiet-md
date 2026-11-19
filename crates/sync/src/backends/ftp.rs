//! FTP and explicit FTPS (AUTH TLS) through suppaftp.

use crate::{Backend, Config, RemoteFile};
use anyhow::{anyhow, Context, Result};
use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};
use suppaftp::list::ListParser;
use suppaftp::{FtpStream, NativeTlsFtpStream};

enum Conn {
    Plain(FtpStream),
    Tls(NativeTlsFtpStream),
}

macro_rules! on_conn {
    ($c:expr, $conn:ident => $body:expr) => {
        match $c {
            Conn::Plain($conn) => $body,
            Conn::Tls($conn) => $body,
        }
    };
}

pub(crate) struct Ftp {
    conn: Conn,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn join(a: &str, b: &str) -> String {
    super::join(a, b)
}

impl Ftp {
    pub(super) fn connect(cfg: &Config) -> Result<Self> {
        let host = cfg.host.trim();
        let port = cfg.port_or_default();
        let addr = (host, port);
        let mut conn = if cfg.ftps {
            let plain = NativeTlsFtpStream::connect(addr).with_context(|| format!("无法连接 {host}:{port}"))?;
            let tls = suppaftp::native_tls::TlsConnector::new().context("无法初始化 TLS")?;
            let conn = plain
                .into_secure(suppaftp::NativeTlsConnector::from(tls), host)
                .with_context(|| format!("{host}:{port} 的 TLS 握手失败"))?;
            Conn::Tls(conn)
        } else {
            Conn::Plain(FtpStream::connect(addr).with_context(|| format!("无法连接 {host}:{port}"))?)
        };
        on_conn!(&mut conn, ftp => {
            ftp.login(if cfg.user.trim().is_empty() { "anonymous" } else { cfg.user.trim() }, cfg.password.as_str())
                .context("FTP 登录失败，请检查用户名和密码")?;
            ftp.transfer_type(suppaftp::types::FileType::Binary).context("无法切换到二进制传输")?;
            let root = cfg.remote_folder();
            if !root.is_empty() {
                ftp.cwd(root).with_context(|| format!("远程目录不存在：{root}"))?;
            }
            Ok::<(), anyhow::Error>(())
        })?;
        Ok(Ftp { conn })
    }

    fn entries(&mut self, dir: &str) -> Result<Vec<(String, bool, u64, u64)>> {
        let opt = (!dir.is_empty()).then_some(dir);
        on_conn!(&mut self.conn, ftp => {
            // MLSD is the machine-readable listing; some servers only speak LIST.
            let lines = match ftp.mlsd(opt) {
                Ok(lines) => lines,
                Err(_) => ftp.list(opt).context("无法列出远程目录")?,
            };
            let mut out = vec![];
            for line in lines {
                let line = line.trim_end_matches('\r');
                if line.trim().is_empty() {
                    continue;
                }
                let file = if line.contains("type=") {
                    ListParser::parse_mlsd(line).ok()
                } else {
                    ListParser::parse_posix(line).or_else(|_| ListParser::parse_dos(line)).ok()
                };
                let Some(file) = file else { continue };
                let name = file.name().trim().trim_end_matches('/').to_string();
                if name.is_empty() || name == "." || name == ".." {
                    continue;
                }
                let mtime = if file.modified() > UNIX_EPOCH { secs(file.modified()) } else { 0 };
                out.push((name, file.is_directory(), file.size() as u64, mtime));
            }
            Ok(out)
        })
    }

    fn walk(&mut self, dir: &str, rel: &str, out: &mut Vec<RemoteFile>) -> Result<()> {
        for (name, is_dir, size, mtime) in self.entries(dir)? {
            let child_dir = join(dir, &name);
            let child_rel = join(rel, &name);
            if is_dir {
                self.walk(&child_dir, &child_rel, out)?;
            } else {
                out.push(RemoteFile { path: child_rel, size, mtime });
            }
        }
        Ok(())
    }

    fn mkdirs(&mut self, path: &str) -> Result<()> {
        let Some((parent, _)) = path.rsplit_once('/') else { return Ok(()) };
        let mut acc = String::new();
        for part in parent.split('/') {
            acc = join(&acc, part);
            on_conn!(&mut self.conn, ftp => {
                // 257 = created, 550 = already there; both are fine.
                let _ = ftp.mkdir(&acc);
                Ok::<(), anyhow::Error>(())
            })?;
        }
        Ok(())
    }
}

impl Backend for Ftp {
    fn list(&mut self) -> Result<Vec<RemoteFile>> {
        let mut out = vec![];
        self.walk("", "", &mut out)?;
        Ok(out)
    }

    fn get(&mut self, path: &str) -> Result<Vec<u8>> {
        on_conn!(&mut self.conn, ftp => {
            let mut buf = ftp.retr_as_buffer(path).with_context(|| format!("无法下载 {path}"))?;
            Ok(std::mem::take(buf.get_mut()))
        })
    }

    fn put(&mut self, path: &str, data: &[u8]) -> Result<()> {
        self.mkdirs(path)?;
        on_conn!(&mut self.conn, ftp => {
            let mut cursor = Cursor::new(data);
            ftp.put_file(path, &mut cursor).with_context(|| format!("无法上传 {path}"))?;
            Ok(())
        })
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        on_conn!(&mut self.conn, ftp => {
            match ftp.rm(path) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // Already gone is the goal.
                    if e.to_string().contains("550") { Ok(()) } else { Err(anyhow!(e)).with_context(|| format!("无法删除远程文件 {path}")) }
                }
            }
        })
    }
}
