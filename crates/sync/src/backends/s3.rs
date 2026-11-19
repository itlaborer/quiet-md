//! S3 and S3-compatible object storage through rust-s3's blocking client.

use crate::{Backend, Config, RemoteFile};
use anyhow::{Context, Result};
use chrono::DateTime;

pub(crate) struct S3 {
    bucket: Box<s3::Bucket>,
    prefix: String,
}

fn key(prefix: &str, path: &str) -> String {
    super::join(prefix, path)
}

fn secs(t: &str) -> u64 {
    DateTime::parse_from_rfc3339(t).ok().map(|d| d.timestamp().max(0) as u64).unwrap_or(0)
}

impl S3 {
    pub(super) fn connect(cfg: &Config) -> Result<Self> {
        let region = s3::Region::Custom { region: cfg.region_or_default().to_string(), endpoint: cfg.endpoint_or_default() };
        let creds = s3::creds::Credentials::new(Some(cfg.access_key.trim()), Some(cfg.secret_key.trim()), None, None, None)
            .context("S3 凭据无效")?;
        let bucket = s3::Bucket::new(cfg.bucket.trim(), region, creds).context("无法创建 S3 客户端")?;
        let bucket = if cfg.path_style { bucket.with_path_style() } else { bucket };
        let prefix = cfg.remote_folder().to_string();
        Ok(S3 { bucket, prefix })
    }
}

impl Backend for S3 {
    fn list(&mut self) -> Result<Vec<RemoteFile>> {
        let mut out = vec![];
        let listing = self.bucket.list(self.prefix.clone(), None).context("无法列出存储桶")?;
        for page in listing {
            for obj in page.contents {
                let rel = match obj.key.strip_prefix(&self.prefix) {
                    Some(r) => r.trim_start_matches('/').to_string(),
                    None => continue,
                };
                if rel.is_empty() || obj.key.ends_with('/') {
                    continue;
                }
                out.push(RemoteFile { path: rel, size: obj.size, mtime: secs(&obj.last_modified) });
            }
        }
        Ok(out)
    }

    fn get(&mut self, path: &str) -> Result<Vec<u8>> {
        let key = key(&self.prefix, path);
        let resp = self.bucket.get_object(&key).with_context(|| format!("无法下载 {path}"))?;
        Ok(resp.to_vec())
    }

    fn put(&mut self, path: &str, data: &[u8]) -> Result<()> {
        let key = key(&self.prefix, path);
        self.bucket.put_object(&key, data).with_context(|| format!("无法上传 {path}"))?;
        Ok(())
    }

    fn delete(&mut self, path: &str) -> Result<()> {
        let key = key(&self.prefix, path);
        self.bucket.delete_object(&key).with_context(|| format!("无法删除远程文件 {path}"))?;
        Ok(())
    }
}
