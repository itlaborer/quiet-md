//! GitHub Contents API：把单篇笔记写成仓库里的一个 Markdown 文件。
//!
//! 新建用 PUT /contents/{path}，更新需带上该路径当前的 blob sha（先 GET）。
//! 这样重复发布同一篇不会失败，而是更新同一个文件。

use crate::{Note, Published, Kind, GithubConfig, agent, api_error, safe_filename};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

fn base(cfg: &GithubConfig) -> &str {
    if cfg.api_base.is_empty() { "https://api.github.com" } else { cfg.api_base.trim_end_matches('/') }
}

fn branch(cfg: &GithubConfig) -> &str {
    if cfg.branch.is_empty() { "main" } else { &cfg.branch }
}

fn encoded_path(cfg: &GithubConfig, title: &str) -> String {
    let name = format!("{}.md", safe_filename(title));
    let dir = cfg.dir.trim_matches('/');
    if dir.is_empty() { name } else { format!("{dir}/{name}") }
}

/// 取目标路径当前的 sha；404 表示文件不存在（新建）。
fn current_sha(cfg: &GithubConfig, path: &str) -> Result<Option<String>> {
    let url = format!("{}/repos/{}/{}/contents/{}", base(cfg), cfg.owner, cfg.repo, path);
    let resp = agent().get(&url)
        .set("Authorization", &format!("Bearer {}", cfg.token))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .query("ref", branch(cfg))
        .call();
    match resp {
        Ok(r) => {
            let body = r.into_string().context("读取 GitHub 文件信息失败")?;
            if let Some(e) = api_error(&body) { bail!("{e}") }
            let v: Value = serde_json::from_str(&body).context("GitHub 返回内容无法解析")?;
            Ok(v.get("sha").and_then(|s| s.as_str()).map(str::to_string))
        }
        // 文件还不存在：正常的新建路径。ureq 把非 2xx 放进 Err(Status)。
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e).context("查询 GitHub 文件失败"),
    }
}

pub fn publish(note: &Note, cfg: &GithubConfig) -> Result<Published> {
    let path = encoded_path(cfg, &note.title);
    let sha = current_sha(cfg, &path)?;
    let url = format!("{}/repos/{}/{}/contents/{}", base(cfg), cfg.owner, cfg.repo, path);

    let mut body = json!({
        "message": format!("publish: {}", note.title),
        "content": crate::base64_encode(note.markdown.as_bytes()),
        "branch": branch(cfg),
    });
    if let Some(sha) = sha {
        body["sha"] = json!(sha);
    }

    let resp = agent().put(&url)
        .set("Authorization", &format!("Bearer {}", cfg.token))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .send_json(body)
        .context("提交到 GitHub 失败")?;
    let text = resp.into_string().context("读取 GitHub 提交结果失败")?;
    if let Some(e) = api_error(&text) { bail!("{e}") }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let html_url = v.get("content").and_then(|c| c.get("html_url")).and_then(|s| s.as_str()).map(str::to_string);

    let msg = match &html_url {
        Some(u) => format!("已发布到 GitHub：{}", u),
        None => format!("已发布到 GitHub：{}/{}", cfg.repo, path),
    };
    Ok(Published { kind: Kind::Github, message: msg, url: html_url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_with_dir() {
        let mut c = GithubConfig::default();
        c.dir = "books".into();
        assert_eq!(encoded_path(&c, "我的/笔记"), "books/我的_笔记.md");
        c.dir.clear();
        assert_eq!(encoded_path(&c, "README"), "README.md");
    }

    #[test]
    fn defaults_branch_and_host() {
        let c = GithubConfig::default();
        assert_eq!(branch(&c), "main");
        assert_eq!(base(&c), "https://api.github.com");
    }
}
