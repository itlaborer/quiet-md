//! 头条号 / 百家号：走各自的开放平台 HTTP 接口。
//!
//! 现实说明：这两家对「图文发布」普遍要求资质审批、且接口地址/字段会调整，
//! 不存在一个对个人稳定开放的通用草稿端点。所以这里不做硬编码的假接口，
//! 而是让用户按自己账号开通的接口文档填 endpoint 与额外字段：
//!   - endpoint：完整发布 URL
//!   - app_id / token：基础凭证（写进请求体）
//!   - extra_json：其余字段的 JSON 对象，会合并进请求体
//!   - title_field / content_field：标题与正文的字段名（默认 title/content）
//! 请求用 JSON POST，返回里的文章链接（若平台给了）展示给用户。

use crate::{Note, Published, Kind, PlatformConfig, agent, api_error};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub fn publish(kind: Kind, note: &Note, cfg: &PlatformConfig) -> Result<Published> {
    if cfg.endpoint.is_empty() { bail!("请先填写 {} 的发布接口地址", kind.label()) }

    let mut body = json!({});
    body[if cfg.title_field.is_empty() { "title" } else { &cfg.title_field }] = json!(note.title);
    body[if cfg.content_field.is_empty() { "content" } else { &cfg.content_field }] = json!(note.html);
    // 正文用 HTML（内容平台不吃 Markdown）；同时给一份纯 markdown 备用字段。
    body["markdown"] = json!(note.markdown);
    if !cfg.app_id.is_empty() { body["app_id"] = json!(cfg.app_id); }
    if !cfg.token.is_empty() { body["access_token"] = json!(cfg.token); }
    if !cfg.extra_json.is_empty() {
        let extra: Value = serde_json::from_str(&cfg.extra_json).context("额外请求字段不是合法 JSON")?;
        if !extra.is_object() { bail!("额外请求字段必须是一个 JSON 对象") }
        if let Some(obj) = extra.as_object() {
            let top = body.as_object_mut().unwrap();
            for (k, v) in obj { top.insert(k.clone(), v.clone()); }
        }
    }

    let resp = agent().post(&cfg.endpoint)
        .set("Content-Type", "application/json; charset=utf-8")
        .send_json(body)
        .context(format!("请求 {} 接口失败", kind.label()))?;
    let text = resp.into_string().context("读取平台响应失败")?;
    if let Some(e) = api_error(&text) { bail!("{e}") }

    // 常见平台把文章链接放在 data.url / url / data.share_url / item_url。
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let url = ["data.url", "url", "data.share_url", "share_url", "item_url"]
        .iter().find_map(|p| {
            let mut cur = &v;
            for seg in p.split('.') { cur = cur.get(seg)?; }
            cur.as_str().map(str::to_string)
        });
    let message = match &url {
        Some(u) => format!("已发布到 {}：{u}", kind.label()),
        None => format!("已提交到 {}，请到平台后台确认发布状态", kind.label()),
    };
    Ok(Published { kind, message, url })
}

#[cfg(test)]
mod tests {
    use crate::{Kind, PlatformConfig, Note};
    use super::*;

    #[test]
    fn rejects_empty_endpoint() {
        let note = Note { title: "t".into(), markdown: "m".into(), html: "h".into() };
        assert!(publish(Kind::Toutiao, &note, &PlatformConfig::default()).is_err());
    }
}
