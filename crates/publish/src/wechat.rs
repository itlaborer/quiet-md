//! 微信公众号：官方 草稿箱 接口。
//!
//! 流程：appid+secret 换 access_token →（可选）上传封面永久素材 →
//! draft/add 新建图文草稿。草稿可在公众号后台「草稿箱」查看、预览后群发。
//!
//! 注意：微信正文里的图片必须是 mmbiz.qpic.cn 的 URL。quiet.md 导出的 HTML
//! 把本地图内联成 data: URI，微信不接受；这里对 data: URI 尝试用
//! media/uploadimg 换成微信 URL，换不了的保留 data URI（微信可能丢弃该图，
//! 但草稿仍能建成）。公网 http(s) 图则原样保留。

use crate::{Note, Published, Kind, WechatConfig, agent, api_error};
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

const API: &str = "https://api.weixin.qq.com/cgi-bin";

/// 取 access_token。微信 tokens 2 小时有效，每次发布重新取，简单可靠。
fn access_token(cfg: &WechatConfig) -> Result<String> {
    let url = format!("{API}/token");
    let resp = agent().get(&url)
        .query("grant_type", "client_credential")
        .query("appid", &cfg.app_id)
        .query("secret", &cfg.secret)
        .call()
        .context("连接微信接口失败（请检查网络与 AppID）")?;
    let body = resp.into_string().context("读取微信 token 响应失败")?;
    if let Some(e) = api_error(&body) { bail!("{e}（请核对 AppID/AppSecret 与 IP 白名单）") }
    let v: Value = serde_json::from_str(&body).context("微信 token 响应无法解析")?;
    v.get("access_token").and_then(|s| s.as_str()).map(str::to_string)
        .with_context(|| format!("微信未返回 access_token：{body}"))
}

/// 上传一张图片到微信图床，返回可在正文里用的 URL。
fn upload_content_image(token: &str, data_uri: &str) -> Result<String> {
    // data:<mime>;base64,<payload>
    let (_, b64) = data_uri.split_once(',').with_context(|| "图片 data URI 格式错误")?;
    let bytes = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(b64).context("图片 base64 解码失败")?
    };
    let url = format!("{API}/media/uploadimg?access_token={token}");
    let resp = agent().post(&url)
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&bytes)
        .context("上传正文图片失败")?;
    let body = resp.into_string().context("读取图片上传结果失败")?;
    if let Some(e) = api_error(&body) { bail!("{e}") }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    v.get("url").and_then(|s| s.as_str()).map(str::to_string)
        .with_context(|| format!("微信未返回图片 URL：{body}"))
}

/// 上传封面永久素材，返回 media_id。草稿的 thumb_media_id 必填。
fn upload_thumb(token: &str, data_uri: &str) -> Result<String> {
    let (_, b64) = data_uri.split_once(',').with_context(|| "封面 data URI 格式错误")?;
    let bytes = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(b64).context("封面 base64 解码失败")?
    };
    // multipart/form-data: media 字段，文件名 cover.png
    let boundary = "----QuietMdBoundary7MA4YWxk";
    let mut part = Vec::new();
    part.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"media\"; filename=\"cover.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    part.extend_from_slice(&bytes);
    part.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let url = format!("{API}/material/add_material?access_token={token}&type=image");
    let resp = agent().post(&url)
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send_bytes(&part)
        .context("上传封面素材失败")?;
    let body = resp.into_string().context("读取封面上传结果失败")?;
    if let Some(e) = api_error(&body) { bail!("{e}") }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    v.get("media_id").and_then(|s| s.as_str()).map(str::to_string)
        .with_context(|| format!("微信未返回封面 media_id：{body}"))
}

/// 从 HTML 里找第一个 data: 图片（用于当封面）。
fn first_data_image(html: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data:image/[^"')]+;base64,[A-Za-z0-9+/=]+"#).unwrap());
    RE.find(html).map(|m| m.as_str().to_string())
}

/// 把正文里的 data URI 图片替换成微信图床 URL。上传失败的图片原样留下。
fn inline_images(token: &str, html: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data:image/[^"')]+;base64,[A-Za-z0-9+/=]+"#).unwrap());
    let mut out = html.to_string();
    // 去重：同一张图可能出现在封面和正文，只传一次。
    let mut seen = std::collections::HashSet::new();
    let uris: Vec<String> = RE.find_iter(html).map(|m| m.as_str().to_string()).collect();
    for uri in uris {
        if !seen.insert(uri.clone()) { continue }
        match upload_content_image(token, &uri) {
            Ok(wechat_url) => { out = out.replace(&uri, &wechat_url); }
            Err(_) => { /* 保留原 data URI，草稿仍可建 */ }
        }
    }
    out
}

pub fn publish(note: &Note, cfg: &WechatConfig) -> Result<Published> {
    let token = access_token(cfg)?;

    // 封面：优先用配置里的永久素材 media_id；否则取正文第一张图上传。
    let thumb = if !cfg.thumb_media_id.is_empty() {
        cfg.thumb_media_id.clone()
    } else if let Some(img) = first_data_image(&note.html) {
        upload_thumb(&token, &img).context("自动上传封面失败，请在设置里填写封面素材 media_id")?
    } else {
        bail!("微信图文草稿需要封面：请填写封面素材 media_id，或在笔记里放一张图片");
    };

    let content = inline_images(&token, &note.html);
    let mut article = json!({
        "title": note.title,
        "author": cfg.author,
        "digest": crate::digest(&note.markdown),
        "content": content,
        "thumb_media_id": thumb,
        "need_open_comment": 0,
        "only_fans_can_comment": 0,
    });
    if !cfg.content_source_url.is_empty() {
        article["content_source_url"] = json!(cfg.content_source_url);
    }

    let url = format!("{API}/draft/add?access_token={token}");
    let resp = agent().post(&url).send_json(json!({ "articles": [article] }))
        .context("创建微信草稿失败")?;
    let body = resp.into_string().context("读取微信草稿结果失败")?;
    if let Some(e) = api_error(&body) { bail!("{e}") }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let media_id = v.get("media_id").and_then(|s| s.as_str()).unwrap_or("");

    Ok(Published {
        kind: Kind::Wechat,
        message: format!("已创建微信图文草稿（media_id: {media_id}），请在公众号后台草稿箱预览后发布"),
        url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_first_data_image() {
        let html = r#"<p>x</p><img src="data:image/png;base64,iVBORw0KGgo="/>"#;
        assert_eq!(first_data_image(html), Some("data:image/png;base64,iVBORw0KGgo=".into()));
        assert_eq!(first_data_image("<p>no image</p>"), None);
    }
}
