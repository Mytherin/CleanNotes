//! Super notes: Standard Notes' rich-text notes, stored as the editor
//! state of Lexical (the editor Super is built on) as JSON. Converted to and
//! from the app's [`Doc`] so they can be edited here like any note, and
//! saved back as Super notes.
//!
//! Only what the app can show is converted: headings (h1–h3), paragraphs,
//! quotes, bulleted / numbered / check lists (nested), code blocks, and
//! bold / italic / underline / strikethrough / inline code and links.
//! Anything else (tables, images, files, embeds, colours, alignment,
//! highlights, dividers, …) makes [`to_doc`] fail, so the note stays
//! read-only rather than losing it.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::doc::{Block, Doc, Kind, Style, MAX_INDENT};

// Lexical's text format bits.
const BOLD: u64 = 1;
const ITALIC: u64 = 1 << 1;
const STRIKE: u64 = 1 << 2;
const UNDERLINE: u64 = 1 << 3;
const CODE: u64 = 1 << 4;
const SUPPORTED_FORMAT: u64 = BOLD | ITALIC | STRIKE | UNDERLINE | CODE;

type Result<T> = std::result::Result<T, String>;

fn ty(n: &Value) -> &str {
    n["type"].as_str().unwrap_or("")
}

fn children(n: &Value) -> &[Value] {
    n["children"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// Elements must be laid out the default way (no alignment, no indent).
fn plain_element(n: &Value, what: &str) -> Result<()> {
    match n["format"].as_str().unwrap_or("") {
        "" | "left" | "start" => {}
        other => return Err(format!("{what} aligned {other}")),
    }
    if n["indent"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("an indented {what}"));
    }
    Ok(())
}

/// A Super note's text as a [`Doc`]; `Err` names the first thing in it the
/// app can't show.
pub fn to_doc(json: &str) -> Result<Doc> {
    if json.trim().is_empty() {
        return Ok(Doc::default());
    }
    let v: Value = serde_json::from_str(json).map_err(|e| format!("unreadable ({e})"))?;
    let root = &v["root"];
    if ty(root) != "root" {
        return Err("no root".into());
    }
    let mut blocks = Vec::new();
    for c in children(root) {
        block(c, &mut blocks)?;
    }
    Ok(Doc::new(blocks))
}

fn block(n: &Value, out: &mut Vec<Block>) -> Result<()> {
    match ty(n) {
        "paragraph" => {
            plain_element(n, "paragraph")?;
            out.push(inline_block(n, Kind::Body, "paragraph")?);
        }
        "heading" => {
            plain_element(n, "heading")?;
            let kind = match n["tag"].as_str() {
                Some("h1") => Kind::Title,
                Some("h2") => Kind::Heading,
                Some("h3") => Kind::Subheading,
                other => return Err(format!("a heading of level {}", other.unwrap_or("?"))),
            };
            out.push(inline_block(n, kind, "heading")?);
        }
        "quote" => {
            if !matches!(n["format"].as_str().unwrap_or(""), "" | "left" | "start") {
                return Err("an aligned quote".into());
            }
            let mut b = inline_block(n, Kind::Quote, "quote")?;
            b.indent = indent_of(n["indent"].as_u64().unwrap_or(0))?;
            out.push(b);
        }
        "list" => list(n, 0, out)?,
        "table" => table(n, out)?,
        "code" => {
            if n["language"].as_str().is_some_and(|l| !l.is_empty()) {
                return Err("a code block set to a language".into());
            }
            let mut line = String::new();
            for c in children(n) {
                match ty(c) {
                    "linebreak" => out.push(Block::new(Kind::Monospaced, &std::mem::take(&mut line))),
                    "tab" => line.push('\t'),
                    "code-highlight" | "text" => line.push_str(c["text"].as_str().unwrap_or("")),
                    other => return Err(format!("{other} in a code block")),
                }
            }
            out.push(Block::new(Kind::Monospaced, &line));
        }
        other => return Err(describe(other)),
    }
    Ok(())
}

fn indent_of(n: u64) -> Result<u8> {
    if n > MAX_INDENT as u64 {
        return Err("lists nested too deep".into());
    }
    Ok(n as u8)
}

fn list(n: &Value, depth: u64, out: &mut Vec<Block>) -> Result<()> {
    // (A list's indent follows from its nesting; only alignment matters.)
    if !matches!(n["format"].as_str().unwrap_or(""), "" | "left" | "start") {
        return Err("an aligned list".into());
    }
    let kind = |checked: bool| match n["listType"].as_str() {
        Some("bullet") => Ok(Kind::Bulleted),
        Some("number") => Ok(Kind::Numbered),
        Some("check") => Ok(Kind::Checklist(checked)),
        other => Err(format!("a {} list", other.unwrap_or("?"))),
    };
    if n["listType"] == "number" && n["start"].as_u64().unwrap_or(1) != 1 {
        return Err("a numbered list that doesn't start at 1".into());
    }
    for item in children(n) {
        if ty(item) != "listitem" {
            return Err(describe(ty(item)));
        }
        if !matches!(item["format"].as_str().unwrap_or(""), "" | "left" | "start") {
            return Err("an aligned list item".into());
        }
        let kids = children(item);
        // A list item holding only a list: the next level of nesting.
        if !kids.is_empty() && kids.iter().all(|k| ty(k) == "list") {
            for k in kids {
                list(k, depth + 1, out)?;
            }
            continue;
        }
        if kids.iter().any(|k| ty(k) == "list") {
            return Err("a list item with text and a list in it".into());
        }
        let mut b = inline_block(item, kind(item["checked"].as_bool() == Some(true))?, "list item")?;
        b.indent = indent_of(depth)?;
        out.push(b);
    }
    Ok(())
}

// Lexical's table header states.
const HEADER_ROW: u64 = 1;
const HEADER_COLUMN: u64 = 2;

/// A table: cells row by row; each cell's paragraphs as lines of it.
/// Headers may be in the first row and/or first column (as Super makes
/// them); merged, coloured or aligned cells, frozen or striped rows the app
/// can't show.
fn table(n: &Value, out: &mut Vec<Block>) -> Result<()> {
    if n["frozenColumnCount"].as_u64().unwrap_or(0) > 0 || n["frozenRowCount"].as_u64().unwrap_or(0) > 0 {
        return Err("a table with frozen rows or columns".into());
    }
    if n["rowStriping"].as_bool() == Some(true) {
        return Err("a table with striped rows".into());
    }
    if out.last().is_some_and(|b| b.kind.is_cell()) {
        return Err("two tables in a row".into());
    }
    let rows: Vec<&[Value]> = children(n).iter().map(|r| if ty(r) == "tablerow" { Ok(children(r)) } else { Err(describe(ty(r))) }).collect::<Result<_>>()?;
    let cols = rows.first().map_or(0, |r| r.len());
    if cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return Err("a table with merged cells".into());
    }
    let state = |r: usize, c: usize| rows[r][c]["headerState"].as_u64().unwrap_or(0);
    let header_row = (0..cols).all(|c| state(0, c) & HEADER_ROW != 0);
    let header_col = (0..rows.len()).all(|r| state(r, 0) & HEADER_COLUMN != 0);
    let kind = Kind::Cell { cols: cols as u16, header_row, header_col };
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if ty(cell) != "tablecell" {
                return Err(describe(ty(cell)));
            }
            let expected = if r == 0 && header_row { HEADER_ROW } else { 0 } | if c == 0 && header_col { HEADER_COLUMN } else { 0 };
            if state(r, c) != expected {
                return Err("table headers in an unusual place".into());
            }
            if cell["colSpan"].as_u64().unwrap_or(1) != 1 || cell["rowSpan"].as_u64().unwrap_or(1) != 1 {
                return Err("a table with merged cells".into());
            }
            if !cell["backgroundColor"].is_null() {
                return Err("coloured table cells".into());
            }
            if cell.get("verticalAlign").is_some_and(|v| !v.is_null()) {
                return Err("aligned table cells".into());
            }
            let mut b = Block::new(kind, "");
            for (i, para) in children(cell).iter().enumerate() {
                if ty(para) != "paragraph" {
                    return Err(format!("{} in a table cell", describe(ty(para))));
                }
                plain_element(para, "paragraph")?;
                if i > 0 {
                    push(&mut b, "\n", &Style::default());
                }
                for k in children(para) {
                    if ty(k) == "linebreak" {
                        push(&mut b, "\n", &Style::default());
                    } else {
                        inline(k, &Style::default(), &mut b, "table cell")?;
                    }
                }
            }
            out.push(b);
        }
    }
    Ok(())
}

/// A block of inline content (text, links, tabs).
fn inline_block(n: &Value, kind: Kind, what: &str) -> Result<Block> {
    let mut b = Block::new(kind, "");
    for c in children(n) {
        inline(c, &Style::default(), &mut b, what)?;
    }
    Ok(b)
}

fn inline(n: &Value, outer: &Style, b: &mut Block, what: &str) -> Result<()> {
    match ty(n) {
        "text" | "hashtag" => {
            if !matches!(n["mode"].as_str().unwrap_or("normal"), "normal") {
                return Err("special text".into());
            }
            if !n["style"].as_str().unwrap_or("").is_empty() {
                return Err("coloured or styled text".into());
            }
            let format = n["format"].as_u64().unwrap_or(0);
            if format & !SUPPORTED_FORMAT != 0 {
                return Err("highlighted, subscript or superscript text".into());
            }
            let style = Style {
                bold: format & BOLD != 0,
                italic: format & ITALIC != 0,
                strike: format & STRIKE != 0,
                underline: format & UNDERLINE != 0,
                code: format & CODE != 0,
                link: outer.link.clone(),
            };
            push(b, n["text"].as_str().unwrap_or(""), &style);
        }
        "tab" => push(b, "\t", outer),
        "link" | "autolink" => {
            if outer.link.is_some() {
                return Err("a link inside a link".into());
            }
            if n["isUnlinked"].as_bool() == Some(true) {
                return Err("an unlinked link".into());
            }
            let url = n["url"].as_str().unwrap_or("");
            let style = Style { link: Some(Arc::from(url)), ..outer.clone() };
            for c in children(n) {
                inline(c, &style, b, what)?;
            }
        }
        "linebreak" => return Err(format!("a line break inside a {what}")),
        other => return Err(describe(other)),
    }
    Ok(())
}

fn push(b: &mut Block, text: &str, style: &Style) {
    b.text.push_str(text);
    b.styles.extend(std::iter::repeat(style.clone()).take(text.chars().count()));
}

/// What a node is, for the reason a note stays read-only.
fn describe(t: &str) -> String {
    match t {
        "table" | "tablerow" | "tablecell" => "a table".into(),
        "horizontalrule" => "a divider".into(),
        "snfile" | "inline-file" | "file-export" => "an attached file".into(),
        "unencrypted-image" | "remote-image" => "an image".into(),
        "tweet" | "youtube" => "an embedded post or video".into(),
        "collapsible-container" | "collapsible-title" | "collapsible-content" => "a collapsible section".into(),
        "snbubble" => "a link to another note".into(),
        "mark" => "a comment mark".into(),
        "" => "something unknown".into(),
        other => format!("a {other}"),
    }
}

// ---- Writing --------------------------------------------------------------

fn element(ty: &str, children: Vec<Value>) -> Value {
    json!({
        "children": children,
        "direction": "ltr",
        "format": "",
        "indent": 0,
        "type": ty,
        "version": 1,
    })
}

fn text_node(ty: &str, text: &str, style: &Style) -> Value {
    let format = (style.bold as u64) * BOLD
        | (style.italic as u64) * ITALIC
        | (style.strike as u64) * STRIKE
        | (style.underline as u64) * UNDERLINE
        | (style.code as u64) * CODE;
    json!({ "detail": 0, "format": format, "mode": "normal", "style": "", "text": text, "type": ty, "version": 1 })
}

/// The inline nodes of a block: runs of text by style, links around theirs,
/// tabs as tab nodes.
fn inlines(b: &Block) -> Vec<Value> {
    let chars: Vec<char> = b.text.chars().collect();
    let mut out: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let link = b.styles[i].link.clone();
        let mut j = i;
        while j < chars.len() && b.styles[j].link == link {
            j += 1;
        }
        let mut nodes = Vec::new();
        let mut k = i;
        while k < j {
            if chars[k] == '\t' {
                nodes.push(json!({ "detail": 2, "format": 0, "mode": "normal", "style": "", "text": "\t", "type": "tab", "version": 1 }));
                k += 1;
                continue;
            }
            let style = &b.styles[k];
            let mut m = k;
            while m < j && chars[m] != '\t' && b.styles[m] == *style {
                m += 1;
            }
            let text: String = chars[k..m].iter().collect();
            nodes.push(text_node("text", &text, &Style { link: None, ..style.clone() }));
            k = m;
        }
        match link {
            Some(url) => {
                let mut l = element("link", nodes);
                l["rel"] = json!("noreferrer");
                l["target"] = Value::Null;
                l["title"] = Value::Null;
                l["url"] = json!(url.as_ref());
                out.push(l);
            }
            None => out.extend(nodes),
        }
        i = j;
    }
    out
}

/// A block with its inline content, and the text format Lexical records on
/// paragraphs (the first text's).
fn inline_element(ty: &str, b: &Block) -> Value {
    let kids = inlines(b);
    let first = kids.iter().find(|k| k["type"] == "text").map(|k| k["format"].clone()).unwrap_or(json!(0));
    let mut e = element(ty, kids);
    if b.text.is_empty() {
        e["direction"] = Value::Null;
    }
    if ty == "paragraph" {
        e["textFormat"] = first;
        e["textStyle"] = json!("");
    }
    e
}

fn list_type(k: Kind) -> &'static str {
    match k {
        Kind::Numbered => "number",
        Kind::Checklist(_) => "check",
        _ => "bullet",
    }
}

/// A list from `blocks` (list items at `depth` or deeper, all of the type of
/// the first): its node, and how many blocks it took.
fn list_node(blocks: &[Block], depth: u8) -> (Value, usize) {
    let lt = list_type(blocks[0].kind);
    let mut items = Vec::new();
    let mut i = 0;
    let mut value = 1;
    while i < blocks.len() {
        let b = &blocks[i];
        if !b.kind.is_list() || b.indent < depth || (b.indent == depth && list_type(b.kind) != lt) {
            break;
        }
        if b.indent > depth {
            // Deeper: a list item holding the nested list.
            let (nested, n) = list_node(&blocks[i..], depth + 1);
            let mut item = element("listitem", vec![nested]);
            item["indent"] = json!(depth);
            item["value"] = json!(value);
            item["checked"] = Value::Null;
            items.push(item);
            value += 1;
            i += n;
            continue;
        }
        let mut item = inline_element("listitem", b);
        item["indent"] = json!(depth);
        item["value"] = json!(value);
        if let Kind::Checklist(c) = b.kind {
            item["checked"] = json!(c);
        }
        items.push(item);
        value += 1;
        i += 1;
    }
    let mut l = element("list", items);
    l["listType"] = json!(lt);
    l["start"] = json!(1);
    l["tag"] = json!(if lt == "number" { "ol" } else { "ul" });
    if depth > 0 {
        l["indent"] = json!(0);
    }
    (l, i)
}

/// A table cell's lines, each as a paragraph block of its own.
fn cell_lines(b: &Block) -> Vec<Block> {
    let mut lines = vec![Block::new(Kind::Body, "")];
    for (c, s) in b.text.chars().zip(&b.styles) {
        if c == '\n' {
            lines.push(Block::new(Kind::Body, ""));
        } else {
            let l = lines.last_mut().unwrap();
            l.text.push(c);
            l.styles.push(s.clone());
        }
    }
    lines
}

/// The table node for a run of cells.
fn table_node(cells: &[Block], cols: usize, header_row: bool, header_col: bool) -> Value {
    let rows: Vec<Value> = cells
        .chunks(cols.max(1))
        .enumerate()
        .map(|(r, row)| {
            let cells: Vec<Value> = row
                .iter()
                .enumerate()
                .map(|(c, b)| {
                    let paragraphs = cell_lines(b).iter().map(|l| inline_element("paragraph", l)).collect();
                    let mut cell = element("tablecell", paragraphs);
                    cell["backgroundColor"] = Value::Null;
                    cell["colSpan"] = json!(1);
                    cell["headerState"] =
                        json!(if r == 0 && header_row { HEADER_ROW } else { 0 } | if c == 0 && header_col { HEADER_COLUMN } else { 0 });
                    cell["rowSpan"] = json!(1);
                    cell
                })
                .collect();
            element("tablerow", cells)
        })
        .collect();
    element("table", rows)
}

/// A [`Doc`] as a Super note's text.
pub fn from_doc(doc: &Doc) -> String {
    let blocks = &doc.blocks;
    let mut nodes = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        let b = &blocks[i];
        match b.kind {
            Kind::Cell { cols, header_row, header_col } => {
                let end = blocks[i..].iter().position(|x| !x.kind.is_cell()).map_or(blocks.len(), |n| i + n);
                nodes.push(table_node(&blocks[i..end], cols as usize, header_row, header_col));
                i = end;
            }
            Kind::Title | Kind::Heading | Kind::Subheading => {
                let mut h = inline_element("heading", b);
                h["tag"] = json!(match b.kind {
                    Kind::Title => "h1",
                    Kind::Heading => "h2",
                    _ => "h3",
                });
                nodes.push(h);
                i += 1;
            }
            Kind::Body => {
                nodes.push(inline_element("paragraph", b));
                i += 1;
            }
            Kind::Quote => {
                let mut q = inline_element("quote", b);
                q["indent"] = json!(b.indent);
                nodes.push(q);
                i += 1;
            }
            Kind::Monospaced => {
                let mut kids = Vec::new();
                while i < blocks.len() && blocks[i].kind == Kind::Monospaced {
                    if !kids.is_empty() {
                        kids.push(json!({ "type": "linebreak", "version": 1 }));
                    }
                    let line = &blocks[i].text;
                    let mut parts = line.split('\t').peekable();
                    while let Some(p) = parts.next() {
                        if !p.is_empty() {
                            kids.push(text_node("code-highlight", p, &Style::default()));
                        }
                        if parts.peek().is_some() {
                            kids.push(json!({ "detail": 2, "format": 0, "mode": "normal", "style": "", "text": "\t", "type": "tab", "version": 1 }));
                        }
                    }
                    i += 1;
                }
                let mut c = element("code", kids);
                c["language"] = Value::Null;
                nodes.push(c);
            }
            _ => {
                // A list (items at indent 0 and their nested ones). Items
                // starting deeper are nested under an empty level.
                let (l, n) = list_node(&blocks[i..], 0);
                nodes.push(l);
                i += n.max(1);
            }
        }
    }
    let mut root = element("root", nodes);
    if doc.blocks.iter().all(|b| b.text.is_empty()) {
        root["direction"] = Value::Null;
    }
    json!({ "root": root }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(f: impl Fn(&mut Style)) -> Style {
        let mut s = Style::default();
        f(&mut s);
        s
    }

    /// As Super writes it (Lexical 0.49).
    const SAMPLE: &str = r#"{"root":{"children":[
        {"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Trip","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"heading","version":1,"tag":"h1"},
        {"children":[{"detail":0,"format":1,"mode":"normal","style":"","text":"Pack","type":"text","version":1},{"detail":0,"format":0,"mode":"normal","style":"","text":" light, see ","type":"text","version":1},{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"the list","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"link","version":1,"rel":"noreferrer","target":null,"title":null,"url":"https://example.com"},{"detail":0,"format":0,"mode":"normal","style":"","text":" #travel","type":"hashtag","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":1,"textStyle":""},
        {"children":[
            {"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Passport","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"listitem","version":1,"checked":true,"value":1},
            {"children":[{"children":[{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Visa","type":"text","version":1}],"direction":"ltr","format":"","indent":1,"type":"listitem","version":1,"checked":false,"value":1}],"direction":"ltr","format":"","indent":0,"type":"list","version":1,"listType":"check","start":1,"tag":"ul"}],"direction":"ltr","format":"","indent":0,"type":"listitem","version":1,"value":2},
            {"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Tickets","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"listitem","version":1,"checked":false,"value":3}
        ],"direction":"ltr","format":"","indent":0,"type":"list","version":1,"listType":"check","start":1,"tag":"ul"},
        {"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Go","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"quote","version":1},
        {"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"let x = 1;","type":"code-highlight","version":1},{"type":"linebreak","version":1},{"detail":0,"format":0,"mode":"normal","style":"","text":"x","type":"code-highlight","version":1,"highlightType":"variable"}],"direction":"ltr","format":"","indent":0,"type":"code","version":1,"language":null},
        {"children":[],"direction":null,"format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}
    ],"direction":"ltr","format":"","indent":0,"type":"root","version":1}}"#;

    #[test]
    fn a_super_note_becomes_a_doc() {
        let d = to_doc(SAMPLE).unwrap();
        let kinds: Vec<(Kind, u8, &str)> = d.blocks.iter().map(|b| (b.kind, b.indent, b.text.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (Kind::Title, 0, "Trip"),
                (Kind::Body, 0, "Pack light, see the list #travel"),
                (Kind::Checklist(true), 0, "Passport"),
                (Kind::Checklist(false), 1, "Visa"),
                (Kind::Checklist(false), 0, "Tickets"),
                (Kind::Quote, 0, "Go"),
                (Kind::Monospaced, 0, "let x = 1;"),
                (Kind::Monospaced, 0, "x"),
                (Kind::Body, 0, ""),
            ]
        );
        let body = &d.blocks[1];
        assert!(body.styles[0].bold && !body.styles[4].bold);
        // "Pack light, see " is 16 characters, then the link's text.
        assert_eq!(body.styles[15].link, None);
        assert_eq!(body.styles[16].link.as_deref(), Some("https://example.com"));
        assert_eq!(body.styles[23].link.as_deref(), Some("https://example.com"));
        assert_eq!(body.styles[24].link, None);
    }

    #[test]
    fn doc_to_super_and_back_is_the_same() {
        let d = to_doc(SAMPLE).unwrap();
        assert_eq!(to_doc(&from_doc(&d)).unwrap(), d);
        // And through the app's Markdown, as edited here.
        let md = crate::markdown::to_markdown(&d);
        let again = crate::markdown::from_markdown(&md);
        assert_eq!(to_doc(&from_doc(&again)).unwrap().blocks[..8], d.blocks[..8]);
    }

    #[test]
    fn written_super_looks_like_supers_own() {
        let d = Doc::new(vec![
            Block::styled(Kind::Body, &[("a ", Style::default()), ("b", st(|s| s.bold = true))]),
            Block::new(Kind::Numbered, "one"),
            Block::new(Kind::Numbered, "two"),
        ]);
        let v: Value = serde_json::from_str(&from_doc(&d)).unwrap();
        let root = &v["root"];
        assert_eq!(root["type"], "root");
        let p = &root["children"][0];
        assert_eq!((p["type"].as_str(), p["textFormat"].as_u64()), (Some("paragraph"), Some(0)));
        assert_eq!(p["children"][1]["format"], 1);
        let l = &root["children"][1];
        assert_eq!((l["listType"].as_str(), l["tag"].as_str()), (Some("number"), Some("ol")));
        assert_eq!(l["children"][1]["value"], 2);
    }

    /// A 2×2 table as Super makes it (a header row and column), a line
    /// break in one cell, then a paragraph.
    const TABLE: &str = r#"{"root":{"children":[
        {"children":[
            {"children":[
                {"children":[{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Item","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}],"direction":"ltr","format":"","indent":0,"type":"tablecell","version":1,"backgroundColor":null,"colSpan":1,"headerState":3,"rowSpan":1},
                {"children":[{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Price","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}],"direction":"ltr","format":"","indent":0,"type":"tablecell","version":1,"backgroundColor":null,"colSpan":1,"headerState":1,"rowSpan":1}
            ],"direction":null,"format":"","indent":0,"type":"tablerow","version":1},
            {"children":[
                {"children":[{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Tea","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}],"direction":"ltr","format":"","indent":0,"type":"tablecell","version":1,"backgroundColor":null,"colSpan":1,"headerState":2,"rowSpan":1},
                {"children":[{"children":[{"detail":0,"format":1,"mode":"normal","style":"","text":"3","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":1,"textStyle":""},{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"each","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}],"direction":"ltr","format":"","indent":0,"type":"tablecell","version":1,"backgroundColor":null,"colSpan":1,"headerState":0,"rowSpan":1}
            ],"direction":null,"format":"","indent":0,"type":"tablerow","version":1}
        ],"direction":null,"format":"","indent":0,"type":"table","version":1,"colWidths":[92,92]},
        {"children":[],"direction":null,"format":"","indent":0,"type":"paragraph","version":1,"textFormat":0,"textStyle":""}
    ],"direction":"ltr","format":"","indent":0,"type":"root","version":1}}"#;

    #[test]
    fn super_tables_become_tables_and_back() {
        let d = to_doc(TABLE).unwrap();
        let kind = Kind::Cell { cols: 2, header_row: true, header_col: true };
        let cells: Vec<(Kind, &str)> = d.blocks.iter().filter(|b| b.kind.is_cell()).map(|b| (b.kind, b.text.as_str())).collect();
        assert_eq!(cells, [(kind, "Item"), (kind, "Price"), (kind, "Tea"), (kind, "3\neach")]);
        assert!(d.blocks[3].styles[0].bold);
        // Back to Super: the same table (header states, one paragraph per
        // line).
        let v: Value = serde_json::from_str(&from_doc(&d)).unwrap();
        let t = &v["root"]["children"][0];
        assert_eq!(t["type"], "table");
        let states: Vec<u64> = t["children"].as_array().unwrap().iter().flat_map(|r| r["children"].as_array().unwrap().iter().map(|c| c["headerState"].as_u64().unwrap())).collect();
        assert_eq!(states, [3, 1, 2, 0]);
        assert_eq!(t["children"][1]["children"][1]["children"].as_array().unwrap().len(), 2);
        assert_eq!(to_doc(&from_doc(&d)).unwrap(), d);
        // And editable: exact through the app's Markdown too.
        let md = crate::sync::standardnotes::items::super_markdown(TABLE).unwrap();
        assert_eq!(md, "| Item | Price |\n| :--- | --- |\n| Tea | **3**<br>each |\n");
    }

    #[test]
    fn what_the_app_cant_show_keeps_the_note_read_only() {
        let wrap = |node: &str| format!(r#"{{"root":{{"children":[{node}],"direction":"ltr","format":"","indent":0,"type":"root","version":1}}}}"#);
        let text = |extra: &str| format!(r#"{{"children":[{{"detail":0,"format":0,"mode":"normal","style":"","text":"x","type":"text","version":1{extra}}}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1}}"#);
        for (json, why) in [
            (wrap(r#"{"type":"table","children":[{"type":"tablerow","children":[{"type":"tablecell","colSpan":2,"rowSpan":1,"headerState":0,"backgroundColor":null,"children":[]}]}],"version":1}"#), "merged"),
            (wrap(r#"{"type":"horizontalrule","version":1}"#), "divider"),
            (wrap(r#"{"type":"snfile","fileUuid":"x","version":1}"#), "file"),
            (wrap(&text(r#","style":"color: red""#).replace(r#""style":"","#, "")), "coloured"),
            (wrap(&text("").replace(r#""format":0,"mode""#, r#""format":128,"mode""#)), "highlighted"),
            (wrap(&text("").replace(r#""format":"","indent":0,"type":"paragraph""#, r#""format":"center","indent":0,"type":"paragraph""#)), "aligned"),
            (wrap(r#"{"children":[],"direction":null,"format":"","indent":0,"type":"heading","version":1,"tag":"h4"}"#), "level h4"),
            (wrap(r#"{"children":[],"direction":null,"format":"","indent":0,"type":"code","version":1,"language":"rust"}"#), "language"),
        ] {
            let err = to_doc(&json).expect_err(why);
            assert!(err.contains(why), "{why}: {err}");
        }
        // Empty notes are fine.
        assert_eq!(to_doc("").unwrap(), Doc::default());
    }
}
