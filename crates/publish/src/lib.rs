//! fdl_publish — 把笔记发布到 GitHub Pages 和内容平台。无 GPUI 依赖。
//!
//! GitHub 走 Contents API（新建/更新单文件）；微信公众号走官方 token →
//! 永久素材/图文草稿接口；头条、百家走各自的开放平台接口（凭证与端点可配，
//! 因为这类平台对图文发布普遍有资质审批，端点也可能调整）。
//!
//! 所有网络调用都是阻塞的：调用方（UI）在自己的工作线程里跑，和 fdl_sync
//! 一样的约定。HTTP 用 ureq + rustls，避免 Windows GNU 交叉编译引入
//! OpenSSL / nasm。

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod github;
pub mod wechat;
pub mod platform;

/// 发布目标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Github,
    Wechat,
    Toutiao,
    Baijiahao,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Github, Kind::Wechat, Kind::Toutiao, Kind::Baijiahao];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Github => "GitHub",
            Kind::Wechat => "微信公众号",
            Kind::Toutiao => "头条号",
            Kind::Baijiahao => "百家号",
        }
    }

    /// 是否需要 HTML 而不是 Markdown 作为正文。
    pub fn wants_html(self) -> bool {
        matches!(self, Kind::Wechat | Kind::Toutiao | Kind::Baijiahao)
    }
}

/// GitHub Pages 发布配置。`dir` 是仓库内的目标目录，如 `books`。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubConfig {
    /// Personal Access Token（需要 repo 或 contents:write 权限）。
    pub token: String,
    /// 仓库所有者，如 itlaborer。
    pub owner: String,
    /// 仓库名，如 itlaborer.github.io。
    pub repo: String,
    /// 分支，默认 main。
    pub branch: String,
    /// 目标目录，如 books（留空为根目录）。
    pub dir: String,
    /// 自定义 API 地址（GitHub Enterprise）；留空用 api.github.com。
    pub api_base: String,
}

/// 微信公众号配置。个人订阅号即可申请；需开发者密码与 IP 白名单。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WechatConfig {
    pub app_id: String,
    pub secret: String,
    /// 图文封面的永久素材 media_id（可选：留空则用正文中第一张图片的 media_id）。
    pub thumb_media_id: String,
    /// 作者名，可选。
    pub author: String,
    /// 原文链接，可选。
    pub content_source_url: String,
}

/// 头条 / 百家的开放平台配置。二者对图文发布普遍需要资质审批且端点会变，
/// 所以端点和额外的请求体字段都开放给用户填写。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformConfig {
    /// 发布接口完整 URL（照平台开放平台文档填）。
    pub endpoint: String,
    /// app_id / access_token 之类凭证的键名与值，按平台要求填在 extra_json。
    /// 这里只放最常被要求的两个，其余走 extra_json。
    pub app_id: String,
    pub token: String,
    /// 额外的 JSON 对象字符串，会合并进请求体，如 {"app_token":"..."}。
    pub extra_json: String,
    /// 请求里承载正文的字段名，默认 content。
    pub content_field: String,
    /// 请求里承载标题的字段名，默认 title。
    pub title_field: String,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        PlatformConfig {
            endpoint: String::new(),
            app_id: String::new(),
            token: String::new(),
            extra_json: String::new(),
            content_field: "content".into(),
            title_field: "title".into(),
        }
    }
}

/// 发布设置，持久化为文档库根目录的 `publish.json`。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub selected: Kind,
    pub github: GithubConfig,
    pub wechat: WechatConfig,
    pub toutiao: PlatformConfig,
    pub baijiahao: PlatformConfig,
}

/// `Kind` 无 Default，手工给设置选一个默认目标。
impl Default for Kind {
    fn default() -> Self { Kind::Github }
}

fn path(root: &Path) -> PathBuf { root.join("publish.json") }

impl Settings {
    pub fn load(root: &Path) -> Self {
        std::fs::read(path(root))
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default()
    }

    /// 原子写：先写 .tmp、sync、再 rename，避免中途断电留下半个配置（凭证很敏感）。
    pub fn save(&self, root: &Path) -> Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(root)?;
        let tmp = root.join("publish.json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path(root))?;
        Ok(())
    }

    pub fn config(&self, kind: Kind) -> &PlatformConfig {
        match kind {
            Kind::Toutiao => &self.toutiao,
            Kind::Baijiahao => &self.baijiahao,
            _ => static_platform(),
        }
    }
}

/// GitHub / 微信不用 PlatformConfig；返回一个共享的空配置供 `config()` 统一取用。
fn static_platform() -> &'static PlatformConfig {
    use std::sync::LazyLock;
    static EMPTY: LazyLock<PlatformConfig> = LazyLock::new(PlatformConfig::default);
    &EMPTY
}

/// 一篇待发布的笔记：标题、Markdown 正文、适合内容平台的 HTML 正文。
#[derive(Clone, Debug)]
pub struct Note {
    pub title: String,
    pub markdown: String,
    pub html: String,
}

/// 一次发布的结果，UI 据此弹 toast / 打开链接。
#[derive(Clone, Debug)]
pub struct Published {
    pub kind: Kind,
    /// 给用户的成功说明（可能含 URL）。
    pub message: String,
    /// 在线地址（GitHub blob / 微信草稿没有公网链接则为 None）。
    pub url: Option<String>,
}

/// 共享的阻塞 HTTP agent：20s 连接 / 120s 读取，避免慢平台卡死工作线程 indefinitely。
pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .build()
}

/// 把平台返回的 JSON 里的错误讲成人话（微信 errcode、通用 code/msg）。
pub(crate) fn api_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(code) = v.get("errcode").and_then(|c| c.as_i64())
        && code != 0
    {
        let msg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
        return Some(format!("微信接口错误 {code}：{msg}"));
    }
    if let Some(code) = v.get("code").and_then(|c| c.as_i64())
        && code != 0
        && code != 200
    {
        let msg = v.get("message").or_else(|| v.get("msg")).and_then(|m| m.as_str()).unwrap_or("");
        return Some(format!("接口错误 {code}：{msg}"));
    }
    None
}

/// 从 Markdown 正文里取一段摘要（给平台当 digest）。跳过标题、图片、标记行。
pub(crate) fn digest(markdown: &str) -> String {
    let mut out = String::new();
    for line in markdown.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("![") || t.starts_with("```") { continue }
        let clean = t.trim_start_matches(['>', '-', '*', ' ']).trim();
        if clean.is_empty() { continue }
        if !out.is_empty() { out.push(' ') }
        out.push_str(clean);
        let chars: Vec<char> = out.chars().collect();
        if chars.len() >= 110 {
            out = chars[..110].iter().collect();
            break;
        }
    }
    let chars: Vec<char> = out.chars().collect();
    if chars.len() > 110 { chars[..110].iter().collect() } else { out }
}

/// HTML 里的图片 data URI 用标准 base64 编码。
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 把标题变成安全的文件名（保留中文，替换路径分隔符与保留字符）。
pub fn safe_filename(title: &str) -> String {
    let t = title.trim();
    let t: String = t.chars().map(|c| match c {
        '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
        _ => c,
    }).collect();
    let t = t.trim_matches(['.', ' ']);
    if t.is_empty() { "未命名".into() } else { t.to_string() }
}

/// 统一入口：按目标分发。
pub fn publish(kind: Kind, note: &Note, settings: &Settings) -> Result<Published> {
    match kind {
        Kind::Github => github::publish(note, &settings.github),
        Kind::Wechat => wechat::publish(note, &settings.wechat),
        Kind::Toutiao => platform::publish(kind, note, &settings.toutiao),
        Kind::Baijiahao => platform::publish(kind, note, &settings.baijiahao),
    }
}

/// 校验某目标是否填好了必要配置，返回给用户的提示（Ok 表示可用）。
pub fn validate(kind: Kind, settings: &Settings) -> Result<()> {
    match kind {
        Kind::Github => {
            let c = &settings.github;
            if c.token.is_empty() { bail!("请填写 GitHub Personal Access Token") }
            if c.owner.is_empty() || c.repo.is_empty() { bail!("请填写 GitHub 仓库所有者与仓库名") }
            Ok(())
        }
        Kind::Wechat => {
            let c = &settings.wechat;
            if c.app_id.is_empty() || c.secret.is_empty() { bail!("请填写微信公众号 AppID 与 AppSecret") }
            Ok(())
        }
        Kind::Toutiao | Kind::Baijiahao => {
            let c = settings.config(kind);
            if c.endpoint.is_empty() { bail!("请填写 {} 的发布接口地址", kind.label()) }
            if !c.extra_json.is_empty() && serde_json::from_str::<serde_json::Value>(&c.extra_json).is_err() {
                bail!("额外请求字段不是合法的 JSON 对象")
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filename_keeps_chinese_and_replaces_reserved() {
        assert_eq!(safe_filename("  我的/笔记:一?  "), "我的_笔记_一");
        assert_eq!(safe_filename("  ...  "), "未命名");
        assert_eq!(safe_filename("README.md"), "README.md");
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("fdl-publish-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut s = Settings::default();
        assert_eq!(s.selected, Kind::Github);
        s.github.token = "secret-token".into();
        s.github.owner = "itlaborer".into();
        s.github.repo = "itlaborer.github.io".into();
        s.github.dir = "books".into();
        s.save(&dir).unwrap();
        let back = Settings::load(&dir);
        assert_eq!(back.github.token, "secret-token");
        assert_eq!(back.github.dir, "books");
        assert_eq!(back.toutiao.content_field, "content");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn api_error_reads_wechat_and_generic() {
        assert_eq!(api_error(r#"{"errcode":40001,"errmsg":"invalid credential"}"#), Some("微信接口错误 40001：invalid credential".into()));
        assert_eq!(api_error(r#"{"code":403,"message":"no permission"}"#), Some("接口错误 403：no permission".into()));
        assert_eq!(api_error(r#"{"media_id":"abc"}"#), None);
    }
}
