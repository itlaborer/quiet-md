//! 设置 → 同步: independent connections for FTP/FTPS, SFTP, SMB and S3, any
//! number of them enabled at once. A test button checks the selected protocol;
//! 立即同步 and 自动同步 visit every enabled protocol in turn.
//!
//! Jobs run on their own thread; the page polls a small static status for the
//! progress line and the outcome, so a slow server never stalls the UI.

use crate::app::{View, Workspace};
use crate::input::{TextField, TextFieldEvent};
use crate::settings::{self, button, desc, group, row, switch, Page};
use crate::theme::Theme;
use crate::ui::{self, ToastKind};
use fdl_sync::{Config, Kind, Settings};
use gpui::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// A library change since the last automatic sync.
static DIRTY: AtomicBool = AtomicBool::new(false);

pub(crate) fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
}

fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

struct Status {
    running: bool,
    /// The step shown while a job runs, or its result afterwards.
    message: String,
    outcome: Option<Outcome>,
    revision: u64,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
}

static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| {
    Mutex::new(Status { running: false, message: String::new(), outcome: None, revision: 0 })
});

/// Bumped on every progress step, so the page knows when to redraw.
pub(crate) fn revision() -> u64 {
    STATUS.lock().unwrap().revision
}

pub(crate) fn status() -> (bool, String) {
    let s = STATUS.lock().unwrap();
    (s.running, s.message.clone())
}

pub(crate) fn take_outcome() -> Option<Outcome> {
    STATUS.lock().unwrap().outcome.take()
}

fn set_message(msg: &str) {
    let mut s = STATUS.lock().unwrap();
    s.message = msg.to_string();
    s.revision += 1;
}

fn finish(result: Result<String, String>) {
    let mut s = STATUS.lock().unwrap();
    s.running = false;
    s.outcome = Some(match &result {
        Ok(m) => Outcome { ok: true, message: m.clone() },
        Err(e) => Outcome { ok: false, message: e.clone() },
    });
    s.message = result.unwrap_or_else(|e| e);
    s.revision += 1;
}

/// Run `job` on a thread unless one is already running.
fn spawn(what: &str, job: impl FnOnce() + Send + 'static) -> bool {
    {
        let mut s = STATUS.lock().unwrap();
        if s.running {
            return false;
        }
        s.running = true;
        s.message = what.to_string();
        s.outcome = None;
        s.revision += 1;
    }
    std::thread::spawn(job);
    true
}

pub(crate) fn start_sync(settings: Settings) -> bool {
    spawn(settings::tr("正在连接…", "Connecting…"), move || {
        let root = crate::store::Store::root_dir();
        let results = fdl_sync::sync_enabled(&root, &settings, set_message);
        if results.is_empty() {
            finish(Err(settings::tr("没有已启用的同步协议", "No sync protocol is enabled").to_string()));
            return;
        }
        let now = crate::time::now();
        // Stamp last_sync on the freshest on-disk settings so edits made while
        // the job ran survive; only protocols that succeeded are stamped.
        let mut current = Settings::load(&root);
        for (kind, r) in &results {
            if r.is_ok() { current.connection_mut(*kind).last_sync = now; }
        }
        let _ = current.save(&root);
        let (ok, msg) = multi_summary(&results);
        finish(if ok { Ok(msg) } else { Err(msg) });
    })
}

pub(crate) fn start_test(cfg: Config) -> bool {
    spawn(settings::tr("正在测试连接…", "Testing connection…"), move || {
        match fdl_sync::test(&cfg) {
            Ok(n) => {
                let msg = if settings::english() {
                    format!("Connected. The remote folder holds {n} library files.")
                } else {
                    format!("连接成功，远程目录中有 {n} 个同步文件。")
                };
                finish(Ok(msg))
            }
            Err(e) => finish(Err(format!("{e:#}"))),
        }
    })
}

fn summary(r: &fdl_sync::Report) -> String {
    let mut parts = vec![];
    if r.uploaded > 0 { parts.push(format!("{} {}", settings::tr("上传", "up"), r.uploaded)) }
    if r.downloaded > 0 { parts.push(format!("{} {}", settings::tr("下载", "down"), r.downloaded)) }
    if r.deleted_remote + r.deleted_local > 0 { parts.push(format!("{} {}", settings::tr("删除", "deleted"), r.deleted_remote + r.deleted_local)) }
    if r.merged > 0 { parts.push(format!("{} {}", settings::tr("合并索引", "merged"), r.merged)) }
    let body = if parts.is_empty() { settings::tr("已经是最新的", "Already up to date").to_string() } else { parts.join(" · ") };
    match r.failures.len() {
        0 => body,
        n => if settings::english() { format!("{body}; {n} file(s) failed: {}", r.failures[0]) } else { format!("{body}；{n} 个文件失败：{}", r.failures[0]) },
    }
}

/// One line covering every protocol that ran; a protocol is named only when
/// more than one took part. Any failure marks the whole outcome unsuccessful.
fn multi_summary(results: &[(Kind, anyhow::Result<fdl_sync::Report>)]) -> (bool, String) {
    let multi = results.len() > 1;
    let mut parts = vec![];
    let mut ok = true;
    for (kind, r) in results {
        let (body, good) = match r {
            Ok(rep) => (summary(rep), rep.failures.is_empty()),
            Err(e) => (format!("{e:#}"), false),
        };
        ok &= good;
        parts.push(if multi { format!("{}: {body}", kind.label()) } else { body });
    }
    let sep = if settings::english() { "; " } else { "；" };
    (ok, parts.join(sep))
}

/// One protocol's connection fields; their text is copied into the matching
/// slot of `Workspace::sync_settings` as it changes, and saved there too.
pub struct Fields {
    host: Entity<TextField>,
    port: Entity<TextField>,
    user: Entity<TextField>,
    password: Entity<TextField>,
    path: Entity<TextField>,
    bucket: Entity<TextField>,
    region: Entity<TextField>,
    endpoint: Entity<TextField>,
    access_key: Entity<TextField>,
    secret_key: Entity<TextField>,
}

fn bind(field: &Entity<TextField>, kind: Kind, set: impl Fn(&mut Config, &str) + 'static, cx: &mut Context<Workspace>) -> Subscription {
    let f = field.clone();
    cx.subscribe(field, move |w: &mut Workspace, _, ev: &TextFieldEvent, cx| {
        if matches!(ev, TextFieldEvent::Changed) {
            set(w.sync_settings.connection_mut(kind), &f.read(cx).text);
            w.save_sync_settings();
            cx.notify();
        }
    })
}

impl Fields {
    pub fn new(kind: Kind, cfg: &Config, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let field = |text: &str, ph: &'static str, digits: bool, masked: bool, cx: &mut Context<Workspace>| {
            cx.new(|cx| {
                let mut f = TextField::new(ph, cx);
                f.set_text(text, cx);
                f.placeholder_color = Theme::text_placeholder();
                f.size = 13.;
                f.line_height = 20.;
                f.digits = digits;
                f.masked = masked;
                f
            })
        };
        let host = field(&cfg.host, settings::tr("example.com 或 192.168.1.10", "example.com or 192.168.1.10"), false, false, cx);
        let port = field(&if cfg.port == 0 { String::new() } else { cfg.port.to_string() }, settings::tr("端口", "Port"), true, false, cx);
        let user = field(&cfg.user, settings::tr("用户名", "User"), false, false, cx);
        let password = field(&cfg.password, settings::tr("密码", "Password"), false, true, cx);
        let path = field(&cfg.path, settings::tr("远程目录", "Remote folder"), false, false, cx);
        let bucket = field(&cfg.bucket, settings::tr("存储桶", "Bucket"), false, false, cx);
        let region = field(&cfg.region, "us-east-1", false, false, cx);
        let endpoint = field(&cfg.endpoint, settings::tr("留空为 AWS", "Empty for AWS"), false, false, cx);
        let access_key = field(&cfg.access_key, "Access Key", false, false, cx);
        let secret_key = field(&cfg.secret_key, "Secret Key", false, true, cx);
        let subs = vec![
            bind(&host, kind, |c, s| c.host = s.into(), cx),
            bind(&port, kind, |c, s| c.port = s.trim().parse().unwrap_or(0), cx),
            bind(&user, kind, |c, s| c.user = s.into(), cx),
            bind(&password, kind, |c, s| c.password = s.into(), cx),
            bind(&path, kind, |c, s| c.path = s.into(), cx),
            bind(&bucket, kind, |c, s| c.bucket = s.into(), cx),
            bind(&region, kind, |c, s| c.region = s.into(), cx),
            bind(&endpoint, kind, |c, s| c.endpoint = s.into(), cx),
            bind(&access_key, kind, |c, s| c.access_key = s.into(), cx),
            bind(&secret_key, kind, |c, s| c.secret_key = s.into(), cx),
        ];
        (Fields { host, port, user, password, path, bucket, region, endpoint, access_key, secret_key }, subs)
    }
}

impl Workspace {
    pub(crate) fn save_sync_settings(&mut self) {
        let _ = self.sync_settings.save(&crate::store::Store::root_dir());
    }

    fn fields_for(&self, kind: Kind) -> &Fields {
        &self.sync_fields[Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0)]
    }

    pub(crate) fn set_sync_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.sync_settings.selected = kind;
        self.save_sync_settings();
        cx.notify();
    }

    pub(crate) fn toggle_sync_auto(&mut self, cx: &mut Context<Self>) {
        self.sync_settings.auto = !self.sync_settings.auto;
        if self.sync_settings.auto { self.autosynced = false }
        self.save_sync_settings();
        cx.notify();
    }

    pub(crate) fn toggle_sync_flag(&mut self, kind: Kind, which: fn(&mut Config), cx: &mut Context<Self>) {
        which(self.sync_settings.connection_mut(kind));
        self.save_sync_settings();
        cx.notify();
    }

    fn sync_ready(&mut self, kind: Kind, cx: &mut Context<Self>) -> bool {
        match self.sync_settings.connection(kind).validate() {
            Ok(()) => true,
            Err(e) => {
                self.show_toast_kind(ToastKind::Error, &format!("{e:#}"), cx);
                false
            }
        }
    }

    pub(crate) fn sync_test(&mut self, cx: &mut Context<Self>) {
        let kind = self.sync_settings.selected;
        if !self.sync_ready(kind, cx) { return }
        self.save_sync_settings();
        if !start_test(self.sync_settings.connection(kind).clone()) {
            self.show_toast(settings::tr("同步正在进行", "A sync is already running"), cx);
        }
        cx.notify();
    }

    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.sync_settings.enabled_kinds().is_empty() {
            self.show_toast(settings::tr("请先启用并配置至少一个协议", "Enable and configure at least one protocol first"), cx);
            return;
        }
        // Write pending edits first: the engine then sees them as this side's
        // version instead of downloading a remote copy underneath the editor.
        self.flush(cx);
        let _ = take_dirty();
        self.save_sync_settings();
        if start_sync(self.sync_settings.clone()) {
            self.autosynced = true;
        } else {
            self.show_toast(settings::tr("同步正在进行", "A sync is already running"), cx);
        }
        cx.notify();
    }

    /// One beat of 自动同步: a sync at launch, then whenever a document,
    /// image, theme or index changed on disk. Visits every enabled protocol.
    pub(crate) fn sync_tick(&mut self, cx: &mut Context<Self>) {
        if self.quick_note { return }
        if !self.sync_settings.auto {
            self.autosynced = false;
            let _ = take_dirty();
            return;
        }
        let (running, _) = status();
        if running { return }
        let due = !self.autosynced || take_dirty();
        self.autosynced = true;
        let enabled = self.sync_settings.enabled_kinds();
        if !due || enabled.is_empty() { return }
        if !enabled.iter().any(|k| self.sync_settings.connection(*k).validate().is_ok()) { return }
        // Pending edits go to disk first, for the reason in `sync_now`.
        self.flush(cx);
        let _ = take_dirty();
        if start_sync(self.sync_settings.clone()) { cx.notify() }
    }

    /// Redraw when a job reports, and fold its result into the library view.
    pub(crate) fn sync_poll(&mut self, rev: u64, cx: &mut Context<Self>) {
        if rev == self.sync_rev { return }
        self.sync_rev = rev;
        if let Some(o) = take_outcome() {
            self.entries = self.store.list();
            self.sort();
            self.show_toast_kind(if o.ok { ToastKind::Success } else { ToastKind::Error }, &o.message, cx);
        }
        if self.view == View::Settings && self.settings_page == Page::Sync { cx.notify() }
    }

    /// A masked field plus a 显示/隐藏 toggle that flips its mask. The real
    /// value stays in the field for editing and copy.
    fn secret_input(&self, field: &Entity<TextField>, id: &'static str, cx: &mut Context<Self>) -> Div {
        let fe = field.clone();
        let masked = field.read(cx).masked;
        div().flex().items_center().gap(px(6.))
            .child(div().w(px(216.)).h(px(30.)).px(px(10.)).flex().items_center().rounded(px(9.)).bg(Theme::bg_body())
                .border_1().border_color(Theme::line_border()).child(field.clone()))
            .child(settings::link_button(id, if masked { settings::tr("显示", "Show") } else { settings::tr("隐藏", "Hide") })
                .on_click(cx.listener(move |_, _, _, cx| {
                    fe.update(cx, |fld, cx| { fld.masked = !fld.masked; cx.notify(); });
                    cx.notify();
                })))
    }

    pub(crate) fn sync_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.sync_settings;
        let kind = s.selected;
        let cfg = s.connection(kind);
        let f = self.fields_for(kind);
        let input = |field: &Entity<TextField>, w: f32| {
            div().w(px(w)).h(px(30.)).px(px(10.)).flex().items_center().rounded(px(9.)).bg(Theme::bg_body())
                .border_1().border_color(Theme::line_border()).child(field.clone())
        };
        let what = |zh: &'static str, en: &'static str| settings::tr(zh, en);
        let (running, message) = status();
        let last = if cfg.last_sync > 0 {
            format!("{} {}", what("上次同步", "Last sync"), ui::format_time(cfg.last_sync))
        } else {
            what("还没有同步过", "Not synced yet").to_string()
        };
        let status_line = if message.is_empty() { last.clone() } else { format!("{last} · {message}") };
        let enabled: Vec<&str> = s.enabled_kinds().iter().map(|k| k.label()).collect();
        let enabled_line = if enabled.is_empty() {
            what("未启用任何协议", "No protocol enabled").to_string()
        } else {
            let sep = if settings::english() { ", " } else { "、" };
            format!("{}: {}", what("已启用", "Enabled"), enabled.join(sep))
        };
        let labels = ["FTP", "SFTP", "SMB", "S3"];
        let mut rows: Vec<AnyElement> = vec![
            row(what("协议", "Protocol"), None,
                settings::segmented("sync-kind", &labels, Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0),
                    |w, i, cx| w.set_sync_kind(Kind::ALL[i], cx), cx)).into_any_element(),
            row(what("自动同步", "Automatic sync"), desc(what("启动时同步一次，之后文档库有变动时同步。", "Sync at launch, then whenever the library changes.")),
                switch("sync-auto", s.auto).on_click(cx.listener(|w, _, _, cx| w.toggle_sync_auto(cx)))).into_any_element(),
            row(what("启用此协议", "Enable this protocol"), desc(what("立即同步与自动同步会依次同步所有已启用的协议。", "Sync now and automatic sync visit every enabled protocol in turn.")),
                switch("sync-enabled", cfg.enabled).on_click(cx.listener(move |w, _, _, cx| w.toggle_sync_flag(kind, |c| c.enabled = !c.enabled, cx)))).into_any_element(),
        ];
        if kind.uses_host() {
            rows.push(row(what("服务器", "Server"), None,
                div().flex().items_center().gap(px(8.))
                    .child(input(&f.host, 190.))
                    .child(input(&f.port, 64.)))
                .into_any_element());
            rows.push(row(what("用户名", "User"), None, input(&f.user, 262.)).into_any_element());
            rows.push(row(what("密码", "Password"), None, self.secret_input(&f.password, "sync-password-reveal", cx)).into_any_element());
            let (label, hint) = match kind {
                Kind::Smb => (what("共享/子目录", "Share / folder"), desc(what("例如 Documents 或 Documents/笔记。", "For example Documents or Documents/notes."))),
                _ => (what("远程目录", "Remote folder"), desc(what("留空为根目录。", "Empty for the root."))),
            };
            rows.push(row(label, hint, input(&f.path, 262.)).into_any_element());
            if kind == Kind::Ftp {
                rows.push(row(what("加密（FTPS）", "Encryption (FTPS)"), desc(what("服务器支持 AUTH TLS 时打开。", "On when the server supports AUTH TLS.")),
                    switch("sync-ftps", cfg.ftps).on_click(cx.listener(|w, _, _, cx| w.toggle_sync_flag(Kind::Ftp, |c| c.ftps = !c.ftps, cx)))).into_any_element());
            }
        } else {
            rows.push(row(what("存储桶", "Bucket"), None, input(&f.bucket, 262.)).into_any_element());
            rows.push(row(what("区域", "Region"), desc(what("AWS 的存储桶区域。", "The bucket's AWS region.")), input(&f.region, 262.)).into_any_element());
            rows.push(row(what("服务地址", "Endpoint"), desc(what("兼容 S3 的服务器地址，留空为 AWS。", "An S3-compatible server; empty for AWS.")), input(&f.endpoint, 262.)).into_any_element());
            rows.push(row("Access Key", None, input(&f.access_key, 262.)).into_any_element());
            rows.push(row("Secret Key", None, self.secret_input(&f.secret_key, "sync-secret-reveal", cx)).into_any_element());
            rows.push(row(what("路径风格", "Path-style URLs"), desc(what("MinIO、Ceph 等自建服务通常需要。", "Usually needed by MinIO, Ceph and other self-hosted servers.")),
                switch("sync-path-style", cfg.path_style).on_click(cx.listener(|w, _, _, cx| w.toggle_sync_flag(Kind::S3, |c| c.path_style = !c.path_style, cx)))).into_any_element());
            rows.push(row(what("前缀", "Prefix"), desc(what("远程目录里的子目录，可留空。", "A subfolder in the bucket; may be empty.")), input(&f.path, 262.)).into_any_element());
        }
        rows.push(row(what("同步状态", "Status"), Some(status_line.into()),
            div().flex().items_center().gap(px(8.))
                .child(button("sync-test", what("测试连接", "Test")).on_click(cx.listener(|w, _, _, cx| w.sync_test(cx))))
                .child(button("sync-now", if running { what("同步中…", "Syncing…") } else { what("立即同步", "Sync now") })
                    .on_click(cx.listener(|w, _, _, cx| w.sync_now(cx))))).into_any_element());
        div().w_full().flex().flex_col().gap(px(28.))
            .child(group(None, rows))
            .child(div().px(px(4.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_caption()).child(enabled_line))
            .child(div().px(px(4.)).text_size(px(12.)).line_height(px(18.)).text_color(Theme::text_caption())
                .child(what("每个协议的连接信息独立保存在文档库的 sync.json 中；同步会双向更新文档、图片、主题和回收站。", "Each protocol's settings are kept independently in sync.json in the library; a sync updates documents, images, themes and the trash in both directions.")))
            .into_any_element()
    }
}