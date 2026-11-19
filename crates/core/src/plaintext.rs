//! 下载为 → 文本: the document as plain text for any editor. No markup
//! survives: styles are dropped, links keep their label, equations their
//! source and attachments their name. The structure stays readable through
//! blank lines, list markers and indentation.

use crate::model::*;

/// 导出为文本设置.
pub struct Options<'a> {
    /// Titles of the document's child pages (what 子页面列表 shows).
    pub sub_pages: &'a [String],
}

/// Indentation for nested content: code, quotes, callouts and children.
const PAD: &str = "    ";

pub fn export(doc: &Document, opts: &Options) -> String {
    let mut w = Writer { opts };
    let mut out = String::new();
    let title = doc.title.text.trim();
    if !title.is_empty() {
        out.push_str(title);
        out.push('\n');
    }
    let body = w.blocks(&doc.blocks);
    if !body.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&body);
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('\n');
    out
}

struct Writer<'a> {
    opts: &'a Options<'a>,
}

impl Writer<'_> {
    /// Blocks on their own paragraphs; consecutive list items stay tight.
    fn blocks(&mut self, bs: &[Block]) -> String {
        let mut out = String::new();
        let mut prev_list = false;
        let mut seq = 0u32;
        for b in bs {
            let list = b.kind.is_list();
            match &b.kind {
                BlockKind::Ordered { seq: Some(n) } => seq = *n,
                BlockKind::Ordered { seq: None } => seq = if prev_list && seq > 0 { seq + 1 } else { 1 },
                _ => seq = 0,
            }
            let s = self.block(b, seq);
            if s.is_empty() && !list {
                continue;
            }
            if !out.is_empty() {
                out.push_str(if list && prev_list { "\n" } else { "\n\n" });
            }
            out.push_str(&s);
            prev_list = list;
        }
        out
    }

    fn block(&mut self, b: &Block, seq: u32) -> String {
        let text = inline(&b.text);
        let nested = |w: &mut Self, head: String| -> String {
            if b.children.is_empty() {
                return head;
            }
            format!("{head}\n{}", indent(&w.blocks(&b.children), PAD))
        };
        match &b.kind {
            BlockKind::Text => nested(self, text),
            BlockKind::Heading { .. } => nested(self, text),
            BlockKind::Bullet => nested(self, format!("- {text}")),
            BlockKind::Ordered { .. } => nested(self, format!("{seq}. {text}")),
            BlockKind::Todo { done } => nested(self, format!("[{}] {text}", if *done { "x" } else { " " })),
            BlockKind::Code { .. } => indent(&b.text.text, PAD),
            BlockKind::Quote => indent(&self.blocks(&b.children), PAD),
            BlockKind::Callout { emoji, .. } => indent(&format!("{emoji}\n{}", self.blocks(&b.children)), PAD),
            BlockKind::Divider => "-".repeat(20),
            // 文本 keeps no pictures; a caption is the only thing worth its line.
            BlockKind::Image { caption, .. } => nested(self, caption.clone().unwrap_or_default()),
            BlockKind::Table(t) => table(t),
            BlockKind::Grid | BlockKind::Column { .. } => self.blocks(&b.children),
            BlockKind::Countdown(_) => String::new(),
            BlockKind::File(f) => f.name.clone(),
            BlockKind::SubPageList => self.opts.sub_pages.iter().map(|t| display_title(t).trim().to_string()).collect::<Vec<_>>().join("\n"),
            BlockKind::Raw { xml, .. } => match crate::docxml::raw_summary(xml) {
                (text, Some(link)) => if text.is_empty() { link } else { format!("{text} ({link})") },
                (text, None) => text,
            },
        }
    }
}

fn table(t: &Table) -> String {
    let cols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    let row = |r: &Vec<TableCell>| {
        let mut cells: Vec<String> = r.iter().map(|c| c.blocks.iter().map(|b| inline(&b.text)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ")).collect();
        cells.resize(cols, String::new());
        cells.join(" | ")
    };
    let mut lines = vec![];
    let mut rows = t.rows.iter();
    if let Some(r) = rows.next() {
        lines.push(row(r));
    }
    lines.extend(rows.map(row));
    lines.join("\n")
}

fn indent(s: &str, prefix: &str) -> String {
    s.lines().map(|l| if l.is_empty() { String::new() } else { format!("{prefix}{l}") }).collect::<Vec<_>>().join("\n")
}

/// The text of a run with its styles dropped: inline objects spelled out
/// instead of the placeholder character they occupy.
fn inline(t: &RichText) -> String {
    let mut out = String::new();
    for (r, m) in t.spans() {
        let obj = match m.inline.as_ref().and_then(Inline::equation) {
            Some(src) => Some(src.to_string()),
            None => match &m.inline {
                Some(Inline { kind: InlineKind::File(f), .. }) => Some(f.name.clone()),
                Some(obj) => Some(crate::docxml::raw_summary(&obj.xml).0),
                None => None,
            },
        };
        match obj {
            // An object occupies one placeholder character per glyph.
            Some(s) => for _ in t.text[r].chars() { out.push_str(&s) },
            None => out.push_str(&t.text[r]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_plain_text() {
        let mut bold = RichText::plain("a ");
        bold.push("bold ", Marks { bold: true, ..Default::default() });
        bold.push("link", Marks { link: Some("https://x.y".into()), ..Default::default() });
        let doc = Document {
            title: RichText::plain("标题"),
            blocks: vec![
                Block::new(BlockKind::Heading { level: 2, seq_auto: false }, RichText::plain("小节")),
                Block::new(BlockKind::Text, bold),
                Block::new(BlockKind::Ordered { seq: None }, RichText::plain("one")).with_children(vec![Block::new(BlockKind::Bullet, RichText::plain("sub"))]),
                Block::new(BlockKind::Ordered { seq: None }, RichText::plain("two")),
                Block::new(BlockKind::Todo { done: true }, RichText::plain("done")),
                Block::new(BlockKind::Code { lang: "Rust".into(), caption: None, wrap: false }, RichText::plain("fn main() {}")),
                Block::new(BlockKind::Quote, RichText::default()).with_children(vec![Block::text("q")]),
                Block::new(BlockKind::Divider, RichText::default()),
            ],
            ..Default::default()
        };
        assert_eq!(
            export(&doc, &Options { sub_pages: &[] }),
            "标题\n\n小节\n\na bold link\n\n1. one\n    - sub\n2. two\n[x] done\n\n    fn main() {}\n\n    q\n\n--------------------\n"
        );
    }

    #[test]
    fn drops_pictures_keeps_names() {
        let doc = Document {
            title: RichText::plain("t"),
            blocks: vec![
                Block::new(BlockKind::Image { src_kind: "path".into(), src: "a.png".into(), width: None, height: None, caption: Some("图一".into()), crop: None }, RichText::default()),
                Block::new(BlockKind::File(FileBlock { src_kind: "path".into(), src: "a.docx".into(), name: "报告.docx".into(), view: FileView::Card }), RichText::default()),
            ],
            ..Default::default()
        };
        assert_eq!(export(&doc, &Options { sub_pages: &[] }), "t\n\n图一\n\n报告.docx\n");
    }

    #[test]
    fn exports_tables_and_sub_pages() {
        let cell = |s: &str| TableCell { blocks: vec![Block::text(s)], ..TableCell::empty() };
        let t = Table { col_widths: vec![], header_rows: 1, header_col: false, rows: vec![vec![cell("a"), cell("b")], vec![cell("1")]] };
        let doc = Document {
            title: RichText::plain("t"),
            blocks: vec![Block::new(BlockKind::Table(t), RichText::default()), Block::new(BlockKind::SubPageList, RichText::default())],
            ..Default::default()
        };
        assert_eq!(export(&doc, &Options { sub_pages: &["子页一".into(), "子页二".into()] }), "t\n\na | b\n1 | \n\n子页一\n子页二\n");
    }
}
