//! One connection per sync run. Each backend works below a folder/prefix so
//! the engine only ever sees library-relative paths.

mod ftp;
mod s3;
mod sftp;
mod smb;

use crate::{Backend, Config};

pub(crate) fn connect(cfg: &Config) -> anyhow::Result<Box<dyn Backend>> {
    Ok(match cfg.kind {
        crate::Kind::Ftp => Box::new(ftp::Ftp::connect(cfg)?),
        crate::Kind::Sftp => Box::new(sftp::Sftp::connect(cfg)?),
        crate::Kind::Smb => Box::new(smb::Smb::connect(cfg)?),
        crate::Kind::S3 => Box::new(s3::S3::connect(cfg)?),
    })
}

/// `a/b` joined without a doubled slash.
pub(crate) fn join(a: &str, b: &str) -> String {
    let (a, b) = (a.trim_matches('/'), b.trim_matches('/'));
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}/{b}"),
    }
}
