# 发布指南

quiet.md 可以把当前打开的笔记一键发布到多个平台。入口有两处：

- 文档顶部的 `…` 菜单 → **发布到…**
- 侧栏文档右键菜单 → **发布到…**

点击后选目标平台，确认即可。发布在后台线程进行，不卡界面；结果以界面右上角的提示条反馈。配置集中在 **设置 → 发布**。

所有凭证只保存在你本机的文档库 `publish.json`（Unix 下权限 0600），不会上传或发送到任何第三方，除了发布请求本身。

## GitHub Pages

把笔记导出成 Markdown，写成仓库里的一个文件。

| 字段 | 说明 |
|---|---|
| Token | Personal Access Token（经典版勾 `repo`，精细版给目标仓库 `Contents: write`） |
| 仓库所有者 | 如 `itlaborer` |
| 仓库名 | 如 `itlaborer.github.io` |
| 目标目录 | 仓库内目录，如 `books`；留空为根目录 |

- 文件路径为 `目标目录/标题.md`，标题里的 `/ : * ? " < > |` 会替换为 `_`。
- 同名发布会更新同一个文件（自动取当前 blob sha），不会重复创建。
- 发布成功后提示条会给出文件的在线链接。若用自定义域名或私有仓库，链接以 GitHub 返回为准。
- 用 GitHub Enterprise 可在 `publish.json` 里手动加 `"api_base": "https://你的域名/api/v3"`（UI 不暴露该字段）。

## 微信公众号

走官方 **草稿箱** 接口，创建一条图文草稿。

1. 到 [微信公众平台](https://mp.weixin.qq.com) → 设置与开发 → 基本配置，拿到 **AppID** 和 **AppSecret**。
2. 把本机公网 IP 加入 **IP 白名单**。
3. 在设置 → 发布里填 AppID / AppSecret。
4. （可选）用「新增永久素材」接口上传一张封面图，把返回的 `media_id` 填到 **封面素材**。留空则自动把正文第一张图片上传当封面；笔记里完全没有图片时会报错提示你填封面。
5. 在笔记的 `…` → 发布到 → 微信公众号。草稿建成后，到公众号后台 **草稿箱** 预览，确认无误再群发。

说明：

- 正文图片（本地图已内联为 data URI）会通过 `media/uploadimg` 上传到微信图床换成合法 URL；个别上传失败的图片保留原样，可能被微信丢弃，但草稿仍能建成。
- 标题、摘要（取正文前 110 字）自动填充；作者、原文链接可在 `publish.json` 里手动加 `author` / `content_source_url`。
- 草稿接口不返回公网文章链接，成功后提示条给出 `media_id`，请到后台查看。

## 头条号 / 百家号

这两家对图文发布普遍**需要资质审批**，且开放接口的地址和字段会随平台调整，没有对个人稳定开放的通用草稿端点。因此这里不写死假接口，而是让你按自己账号已开通的接口文档填写：

| 字段 | 说明 |
|---|---|
| 发布接口 | 平台开放文档给出的完整 URL（JSON POST） |
| Token | 基础凭证，写进请求体的 `access_token` |
| 额外字段 | 一个 JSON 对象，合并进请求体，如 `{"app_token":"..."}` |

请求体默认包含：`title`（标题）、`content`（HTML 正文）、`markdown`（Markdown 正文）、`app_id`、`access_token`，再合并「额外字段」。字段名可在 `publish.json` 里用 `title_field` / `content_field` 改。平台若在响应里返回文章链接（如 `data.url`），提示条会直接给出；否则提示你到后台确认。

## 凭证安全

- `publish.json` 在文档库根目录，和笔记放一起；不要把它提交到公开的笔记仓库。
- 需要同步笔记但不想同步凭证时，把 `publish.json` 排除在同步之外。
- Token / AppSecret 泄露后请立即在对应平台重置。

## 实现位置

- 核心库：`crates/publish`（`fdl_publish`，无 GPUI 依赖，可单独 `cargo test -p fdl_publish`）
  - `github.rs`：Contents API 新建 / 更新
  - `wechat.rs`：token → 图片上传 → draft/add
  - `platform.rs`：头条 / 百家通用 JSON POST
- 界面：`src/publish_ui.rs`；设置页在 `src/settings.rs`，入口在 `src/docmenu.rs` 和 `src/sidebar.rs`。
- HTTP 客户端用 ureq + rustls（纯 Rust TLS），Windows GNU 交叉编译无需 OpenSSL / nasm。
