//! 设置 → 发布 和 文档「…」→ 发布到：把当前笔记发到 GitHub / 微信 / 头条 / 百家。
//!
//! 和 同步 一样：任务跑在自己的线程里（网络阻塞），页面轮询一个静态状态拿
//! 进度与结果；慢平台不会卡住 UI。配置存文档库的 publish.json。

use crate::app::{View, Workspace};
use crate::input::{TextField, TextFieldEvent};
use crate::settings::{self, Page};
use crate::theme::Theme;
use crate::ui::ToastKind;
use fdl_publish::{Kind, Note, Published, Settings};
use gpui::*;
use std::sync::{LazyLock, Mutex};

// ---- 后台任务状态（和 sync_ui 同款） ----

struct Status {
    running: bool,
    message: String,
    outcome: Option<Outcome>,
    revision: u64,
}

#[derive(Clone, Debug)]
struct Outcome {
    ok: bool,
    message: String,
}

static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| {
    Mutex::new(Status { running: false, message: String::new(), outcome: None, revision: 0 })
});

pub(crate) fn revision() -> u64 { STATUS.lock().unwrap().revision }
pub(crate) fn status() -> (bool, String) {
    let s = STATUS.lock().unwrap();
    (s.running, s.message.clone())
}
fn take_outcome() -> Option<Outcome> { STATUS.lock().unwrap().outcome.take() }
fn finish(result: anyhow::Result<Published>) {
    let mut s = STATUS.lock().unwrap();
    s.running = false;
    s.outcome = Some(match &result {
        Ok(p) => Outcome { ok: true, message: p.message.clone() },
        Err(e) => Outcome { ok: false, message: format!("{e:#}") },
    });
    s.message = match &result {
        Ok(p) => p.message.clone(),
        Err(e) => format!("{e:#}"),
    };
    s.revision += 1;
}

/// 在工作线程发布一篇笔记。`note` 是主线程算好的纯字符串，Send。
pub(crate) fn start_publish(kind: Kind, note: Note, settings: Settings) -> bool {
    {
        let mut s = STATUS.lock().unwrap();
        if s.running { return false }
        s.running = true;
        s.message = settings::tr("正在发布…", "Publishing…").to_string();
        s.outcome = None;
        s.revision += 1;
    }
    std::thread::spawn(move || {
        let res = fdl_publish::publish(kind, &note, &settings)
            .map_err(|e| anyhow::anyhow!("{}：{e:#}", kind.label()));
        finish(res);
    });
    true
}

/// 打开的发布对话框：笔记 id + 目标。
pub struct PublishDialog {
    pub id: String,
    pub kind: Kind,
}

// ---- 设置页字段 ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    // GitHub
    GhToken, GhOwner, GhRepo, GhDir,
    // 微信
    WxAppId, WxSecret, WxThumb,
    // 头条 / 百家
    PtEndpoint, PtToken, PtExtra,
}

/// 一个输入框，改动写回 publish_settings 并保存。
struct PubField {
    field: Field,
    entity: Entity<TextField>,
}

/// 某个目标的全部字段，按 Kind 索引存进 Workspace。
pub struct PublishFields(Vec<PubField>);

fn specs(kind: Kind) -> Vec<(Field, &'static str, bool)> {
    match kind {
        Kind::Github => vec![
            (Field::GhToken, "ghp_…", true),
            (Field::GhOwner, "itlaborer", false),
            (Field::GhRepo, "itlaborer.github.io", false),
            (Field::GhDir, "books", false),
        ],
        Kind::Wechat => vec![
            (Field::WxAppId, "wx…", false),
            (Field::WxSecret, "AppSecret", true),
            (Field::WxThumb, settings::tr("封面素材 media_id（留空用正文首图）", "Cover media_id (empty = first inline image)"), false),
        ],
        Kind::Toutiao | Kind::Baijiahao => vec![
            (Field::PtEndpoint, "https://…", false),
            (Field::PtToken, "access_token / app_token", true),
            (Field::PtExtra, "{\"app_token\":\"…\"}", false),
        ],
    }
}

fn field_value(s: &Settings, kind: Kind, f: Field) -> String {
    match (kind, f) {
        (Kind::Github, Field::GhToken) => s.github.token.clone(),
        (Kind::Github, Field::GhOwner) => s.github.owner.clone(),
        (Kind::Github, Field::GhRepo) => s.github.repo.clone(),
        (Kind::Github, Field::GhDir) => s.github.dir.clone(),
        (Kind::Wechat, Field::WxAppId) => s.wechat.app_id.clone(),
        (Kind::Wechat, Field::WxSecret) => s.wechat.secret.clone(),
        (Kind::Wechat, Field::WxThumb) => s.wechat.thumb_media_id.clone(),
        (Kind::Toutiao, Field::PtEndpoint) => s.toutiao.endpoint.clone(),
        (Kind::Toutiao, Field::PtToken) => s.toutiao.token.clone(),
        (Kind::Toutiao, Field::PtExtra) => s.toutiao.extra_json.clone(),
        (Kind::Baijiahao, Field::PtEndpoint) => s.baijiahao.endpoint.clone(),
        (Kind::Baijiahao, Field::PtToken) => s.baijiahao.token.clone(),
        (Kind::Baijiahao, Field::PtExtra) => s.baijiahao.extra_json.clone(),
        _ => String::new(),
    }
}

fn set_field(s: &mut Settings, kind: Kind, f: Field, v: String) {
    match (kind, f) {
        (Kind::Github, Field::GhToken) => s.github.token = v,
        (Kind::Github, Field::GhOwner) => s.github.owner = v,
        (Kind::Github, Field::GhRepo) => s.github.repo = v,
        (Kind::Github, Field::GhDir) => s.github.dir = v,
        (Kind::Wechat, Field::WxAppId) => s.wechat.app_id = v,
        (Kind::Wechat, Field::WxSecret) => s.wechat.secret = v,
        (Kind::Wechat, Field::WxThumb) => s.wechat.thumb_media_id = v,
        (Kind::Toutiao, Field::PtEndpoint) => s.toutiao.endpoint = v,
        (Kind::Toutiao, Field::PtToken) => s.toutiao.token = v,
        (Kind::Toutiao, Field::PtExtra) => s.toutiao.extra_json = v,
        (Kind::Baijiahao, Field::PtEndpoint) => s.baijiahao.endpoint = v,
        (Kind::Baijiahao, Field::PtToken) => s.baijiahao.token = v,
        (Kind::Baijiahao, Field::PtExtra) => s.baijiahao.extra_json = v,
        _ => {}
    }
}

impl PublishFields {
    pub fn new(kind: Kind, settings: &Settings, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let mut fields = Vec::new();
        let mut subs = Vec::new();
        for (field, ph, masked) in specs(kind) {
            let entity = cx.new(|cx| {
                let mut f = TextField::new(ph, cx);
                f.set_text(&field_value(settings, kind, field), cx);
                f.placeholder_color = Theme::text_placeholder();
                f.size = 13.;
                f.line_height = 20.;
                f.masked = masked;
                f
            });
            let e = entity.clone();
            subs.push(cx.subscribe(&entity, move |w: &mut Workspace, _, ev: &TextFieldEvent, cx| {
                if matches!(ev, TextFieldEvent::Changed) {
                    set_field(&mut w.publish_settings, kind, field, e.read(cx).text.clone());
                    w.save_publish_settings();
                    cx.notify();
                }
            }));
            fields.push(PubField { field, entity });
        }
        (PublishFields(fields), subs)
    }

    fn get(&self, field: Field) -> Entity<TextField> {
        self.0.iter().find(|f| f.field == field).map(|f| f.entity.clone()).unwrap()
    }
}

impl Workspace {
    pub(crate) fn save_publish_settings(&mut self) {
        let _ = self.publish_settings.save(&crate::store::Store::root_dir());
    }

    pub(crate) fn set_publish_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.publish_settings.selected = kind;
        self.save_publish_settings();
        cx.notify();
    }

    /// 「…」→ 发布到：打开对话框确认目标。
    pub fn open_publish(&mut self, id: &str, cx: &mut Context<Self>) {
        self.close_menus();
        self.publish_dialog = Some(PublishDialog { id: id.to_string(), kind: self.publish_settings.selected });
        cx.notify();
    }

    /// 对话框里「发布」：算出 Markdown + HTML，交给工作线程。
    fn do_publish(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(PublishDialog { id, kind }) = self.publish_dialog.take() else { return };
        cx.notify();
        let Some(doc) = self.doc_of(&id, cx) else { return };
        let subs = self.sub_page_titles(&id);
        let title = crate::settings::display_title(&doc.title.text);
        let markdown = crate::markdown::export(&doc, &crate::markdown::Options { sub_pages: &subs, assets: None }).text;
        let html = crate::html::to_html(&doc, &crate::html::Options { sub_pages: &subs });
        if let Err(e) = fdl_publish::validate(kind, &self.publish_settings) {
            self.show_toast_kind(ToastKind::Error, &format!("{e}"), cx);
            return;
        }
        let note = Note { title, markdown, html };
        let settings = self.publish_settings.clone();
        if !start_publish(kind, note, settings) {
            self.show_toast(settings::tr("已有发布任务正在进行", "A publish is already running"), cx);
        }
        cx.notify();
    }

    /// 每 400ms 轮询：结果出来则弹 toast。
    pub(crate) fn publish_poll(&mut self, rev: u64, cx: &mut Context<Self>) {
        if rev == self.publish_rev { return }
        self.publish_rev = rev;
        if let Some(o) = take_outcome() {
            self.show_toast_kind(if o.ok { ToastKind::Success } else { ToastKind::Error }, &o.message, cx);
        }
        if self.view == View::Settings && self.settings_page == Page::Publish { cx.notify(); }
    }

    /// 发布对话框（模态），仿 导出 Markdown 设置。
    pub(crate) fn render_publish(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let data = self.publish_dialog.as_ref()?;
        let kind = data.kind;
        let doc = self.entries.iter().find(|e| e.id == data.id).map(|e| e.title.clone())
            .unwrap_or_else(|| data.id.clone());
        let labels = ["GitHub", "微信公众号", "头条号", "百家号"];
        let selected = Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        let segmented = settings::segmented("publish-kind", &labels, selected,
            |w, i, cx| {
                if let Some(d) = w.publish_dialog.as_mut() { d.kind = Kind::ALL[i]; }
                cx.notify();
            }, cx);
        let dialog = crate::trash::dialog("publish-dlg")
            .relative()
            .w(px(560.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div().pt(px(24.)).pb(px(20.)).pl(px(24.)).pr(px(56.))
                    .border_b_1().border_color(Theme::line_divider())
                    .text_size(px(15.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD)
                    .child(settings::tr("发布笔记", "Publish note")),
            )
            .child(crate::trash::close_button("publish-close").absolute().top(px(27.)).right(px(25.)).on_click(cx.listener(|w, _, _, cx| {
                w.publish_dialog = None; cx.notify();
            })))
            .child(
                div().px(px(24.)).pt(px(20.)).pb(px(8.)).flex().flex_col().gap(px(14.))
                    .child(div().text_size(px(13.)).text_color(Theme::text_caption())
                        .child(crate::localized_format!("笔记：{}", "Note: {}", doc)))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(settings::tr("发布到", "Publish to")))
                    .child(segmented)
                    .child(div().text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_caption())
                        .child(settings::tr("凭证与目标在「设置 → 发布」里配置。GitHub 写成仓库中的一个 Markdown 文件；微信创建图文草稿，需到后台预览后群发。", "Credentials are set in Settings → Publish. GitHub writes one Markdown file to the repo; WeChat creates a draft to preview and send from the backend."))),
            )
            .child(
                div().px(px(24.)).pb(px(24.)).pt(px(12.)).flex().justify_end().gap(px(12.))
                    .child(crate::trash::secondary_button("publish-cancel", settings::tr("取消", "Cancel")).min_w(px(80.)).on_click(cx.listener(|w, _, _, cx| {
                        w.publish_dialog = None; cx.notify();
                    })))
                    .child(crate::trash::primary_button("publish-ok", settings::tr("发布", "Publish")).min_w(px(80.)).on_click(cx.listener(|w, _, window, cx| w.do_publish(window, cx)))),
            );
        Some(crate::transfer::mask("publish-mask", dialog, |w, cx| { w.publish_dialog = None; cx.notify(); }, cx))
    }

    /// 发布设置页。凭证只在本地 publish.json，不上传。
    pub(crate) fn publish_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.publish_settings.selected;
        let idx = Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        let fields = &self.publish_fields[idx];
        let input = |field: Field, w: f32| {
            div().w(px(w)).h(px(30.)).px(px(10.)).flex().items_center().rounded(px(9.)).bg(Theme::bg_body())
                .border_1().border_color(Theme::line_border()).child(fields.get(field))
        };
        let secret_input = |field: Field, id: &'static str, f: &PublishFields, cx: &mut Context<Self>| {
            let e = f.get(field);
            let masked = e.read(cx).masked;
            let ee = e.clone();
            div().flex().items_center().gap(px(6.))
                .child(div().w(px(300.)).h(px(30.)).px(px(10.)).flex().items_center().rounded(px(9.)).bg(Theme::bg_body())
                    .border_1().border_color(Theme::line_border()).child(e))
                .child(settings::link_button(id, if masked { settings::tr("显示", "Show") } else { settings::tr("隐藏", "Hide") })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        ee.update(cx, |fld, cx| { fld.masked = !fld.masked; cx.notify(); });
                        cx.notify();
                    })))
        };
        let what = settings::tr;
        let labels = ["GitHub", "微信公众号", "头条号", "百家号"];
        let mut rows: Vec<AnyElement> = vec![
            settings::row(what("发布目标", "Target"), None,
                settings::segmented("publish-target", &labels, idx, |w, i, cx| w.set_publish_kind(Kind::ALL[i], cx), cx)).into_any_element(),
        ];
        match kind {
            Kind::Github => {
                rows.push(settings::row("Token", settings::desc(what("需要 repo 或 contents:write 权限的 Personal Access Token", "A Personal Access Token with repo or contents:write scope")), secret_input(Field::GhToken, "pub-gh-token-reveal", fields, cx)).into_any_element());
                rows.push(settings::row(what("仓库所有者", "Owner"), None, input(Field::GhOwner, 300.)).into_any_element());
                rows.push(settings::row(what("仓库名", "Repository"), settings::desc(what("如 itlaborer.github.io", "e.g. itlaborer.github.io")), input(Field::GhRepo, 300.)).into_any_element());
                rows.push(settings::row(what("目标目录", "Folder"), settings::desc(what("仓库内目录，如 books；留空为根目录", "Folder inside the repo, e.g. books; empty for root")), input(Field::GhDir, 300.)).into_any_element());
            }
            Kind::Wechat => {
                rows.push(settings::row("AppID", None, input(Field::WxAppId, 300.)).into_any_element());
                rows.push(settings::row("AppSecret", settings::desc(what("需把本机公网 IP 加入公众号 IP 白名单", "Add this machine's public IP to the account's whitelist")), secret_input(Field::WxSecret, "pub-wx-secret-reveal", fields, cx)).into_any_element());
                rows.push(settings::row(what("封面素材", "Cover"), None, input(Field::WxThumb, 300.)).into_any_element());
            }
            Kind::Toutiao | Kind::Baijiahao => {
                rows.push(settings::row(what("发布接口", "Endpoint"), settings::desc(what("按平台开放平台文档填完整 URL", "The full URL from the platform's open-platform docs")), input(Field::PtEndpoint, 300.)).into_any_element());
                rows.push(settings::row("Token", None, secret_input(Field::PtToken, "pub-pt-token-reveal", fields, cx)).into_any_element());
                rows.push(settings::row(what("额外字段", "Extra JSON"), settings::desc(what("合并进请求体的 JSON 对象，如 {\"app_token\":\"…\"}", "JSON object merged into the request body")), input(Field::PtExtra, 300.)).into_any_element());
            }
        }
        let (running, message) = status();
        let status_line = if message.is_empty() {
            what("尚未发布", "Not published yet").to_string()
        } else { message };
        rows.push(settings::row(what("发布状态", "Status"), Some(status_line.into()),
            settings::button("pub-check", what("检查配置", "Check")).on_click(cx.listener(move |w, _, _, cx| {
                let k = w.publish_settings.selected;
                match fdl_publish::validate(k, &w.publish_settings) {
                    Ok(()) => w.show_toast_kind(ToastKind::Success, what("配置完整，可以发布", "Configuration looks good"), cx),
                    Err(e) => w.show_toast_kind(ToastKind::Error, &format!("{e}"), cx),
                }
            }))).into_any_element());

        let note = match kind {
            Kind::Github => what("在笔记的「…」菜单选「发布到」，把当前笔记写成仓库里的一个 Markdown 文件（同名重复发布会更新该文件）。凭证只保存在本地 publish.json。", "Use a note's … menu → Publish to write it as a Markdown file in the repo (republishing the same title updates that file). Credentials stay in the local publish.json."),
            Kind::Wechat => what("个人订阅号即可开通开发者接口。发布会创建一条图文草稿，请到公众号后台「草稿箱」预览后群发。正文图片会上传到微信图床，失败的图片会保留原样。", "A personal subscription account can enable the developer API. Publishing creates a draft; preview and send it from the backend. Inline images are uploaded to WeChat; failures stay as-is."),
            Kind::Toutiao | Kind::Baijiahao => what("这两家对图文发布普遍需要资质审批、接口地址也会调整，因此端点与字段开放填写。发布用 JSON POST，请用你账号开通的接口文档核对字段。", "These platforms generally require approval for article publishing and may change endpoints, so the endpoint and fields are configurable. Publishing is a JSON POST; match the fields to your account's docs."),
        };
        div().w_full().flex().flex_col().gap(px(28.))
            .child(settings::group(None, rows))
            .child(div().px(px(4.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_caption())
                .child(if running { what("正在发布，请稍候…", "Publishing, please wait…") } else { note }))
            .into_any_element()
    }
}
