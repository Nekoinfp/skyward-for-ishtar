//! A web page as text a reader can be shown: its title, and blocks of words
//! with the links among them.
//!
//! There is no engine here and there is not meant to be one: no scripts, no
//! stylesheets, no layout tables, no images beyond their names. What a page
//! says is read out of its markup, which is what a reader in a recovery can
//! use, and the rest of a browser's work is left to a browser.

/// One piece of a page, laid out top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading, 1 the largest.
    Heading { level: u8, text: String },
    /// A paragraph, its spans carrying the links inside it.
    Para(Vec<Span>),
    /// A list item: `depth` how far it is nested, `marker` its bullet or
    /// number.
    Item { depth: u8, marker: String, spans: Vec<Span> },
    /// A quotation.
    Quote(Vec<Span>),
    /// Preformatted text, kept as it was written.
    Code(String),
    /// A horizontal rule.
    Rule,
    /// An image, named but not shown.
    Image { alt: String, src: String },
}

/// A run of words, and where following it leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    /// The address this run opens, worked out against the page's own.
    pub link: Option<String>,
    pub bold: bool,
    pub italic: bool,
}


/// A page read out: what it calls itself, and what it says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub title: String,
    pub blocks: Vec<Block>,
}

impl Document {
    /// Whether there is nothing to show.
    pub fn empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The words of the page as one string, for a reader that wants them
    /// flat: a search, or a page saved to be read later.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            match block {
                Block::Heading { text, .. } => {
                    out.push_str(text);
                    out.push('\n');
                }
                Block::Para(spans) | Block::Quote(spans) => {
                    out.push_str(&spans_text(spans));
                    out.push('\n');
                }
                Block::Item { marker, spans, .. } => {
                    out.push_str(marker);
                    out.push(' ');
                    out.push_str(&spans_text(spans));
                    out.push('\n');
                }
                Block::Code(text) => {
                    out.push_str(text);
                    out.push('\n');
                }
                Block::Rule => out.push('\n'),
                Block::Image { alt, .. } => {
                    out.push('[');
                    out.push_str(alt);
                    out.push_str("]\n");
                }
            }
        }
        out
    }
}

fn spans_text(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        out.push_str(&span.text);
    }
    out
}

/// What the reader is in the middle of.
#[derive(Default)]
struct Building {
    title: Option<String>,
    blocks: Vec<Block>,
    /// The spans of the block being gathered.
    spans: Vec<Span>,
    text: String,
    link: Option<String>,
    bold: u8,
    italic: u8,
    /// The heading being gathered, 0 for none.
    heading: u8,
    /// The quotation being gathered.
    quote: bool,
    /// The preformatted text being gathered.
    code: Option<String>,
    /// How deep in lists we are, and how many items each has had.
    lists: Vec<ListState>,
    /// Skipping the words of a script or a style.
    skipping: Option<String>,
}

#[derive(Default)]
struct ListState {
    ordered: bool,
    count: u64,
}

/// Read `html` as a page, its relative links worked out against `base`.
pub fn read(html: &str, base: &str) -> Document {
    let mut b = Building::default();
    let bytes: Vec<char> = html.chars().collect();
    let mut at = 0usize;
    while at < bytes.len() {
        let ch = bytes[at];
        if ch == '<' {
            // A comment runs to its own end, whatever it holds.
            if starts_with(&bytes, at, "<!--") {
                at = find(&bytes, at, "-->").map_or(bytes.len(), |i| i + 3);
                continue;
            }
            match tag_at(&bytes, at) {
                Some((tag, closing, rest)) => {
                    let name = tag_name(&tag);
                    match name.as_str() {
                        "title" if !closing => {
                            // Everything up to </title> is the page's name.
                            if let Some(end) = find_ci(&bytes, rest, "</title>") {
                                let name: String = bytes[rest..end].iter().collect();
                                b.title = Some(collapse(&decode(&name)));
                                at = end + "</title>".len();
                                continue;
                            }
                        }
                        "script" | "style" | "noscript" | "template" => {
                            if !closing {
                                b.skipping = Some(name.clone());
                            } else if b.skipping.as_deref() == Some(name.as_str()) {
                                b.skipping = None;
                            }
                            at = rest;
                            continue;
                        }
                        _ => {}
                    }
                    if b.skipping.is_none() {
                        element(&mut b, &name, closing, &tag, base);
                    }
                    at = rest;
                }
                None => {
                    at += 1;
                }
            }
            continue;
        }
        // Words, up to the next tag.
        let mut end = at;
        while end < bytes.len() && bytes[end] != '<' {
            end += 1;
        }
        if b.skipping.is_none() {
            let raw: String = bytes[at..end].iter().collect();
            let decoded = decode(&raw);
            b.text.push_str(&decoded);
            b.flush_text();
        }
        at = end;
    }
    b.finish();
    Document {
        title: b.title.unwrap_or_default(),
        blocks: b.blocks,
    }
}

impl Building {
    /// Turn whatever words have been gathered into a span of the block being
    /// built, so that a link's start and end land where they should.
    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        if let Some(code) = &mut self.code {
            code.push_str(&text);
            return;
        }
        // A span keeps the spaces at its edges: markup splits words across
        // spans (was <b>not</b> rendered), and the block trims its own ends
        // once it is whole.
        let trimmed = collapse_edges(&text);
        if trimmed.is_empty() {
            return;
        }
        self.spans.push(Span {
            text: trimmed,
            link: self.link.clone(),
            bold: self.bold > 0,
            italic: self.italic > 0,
        });
    }

    /// Close the block being built, if it holds anything.
    fn close_block(&mut self) {
        self.flush_text();
        let mut spans = std::mem::take(&mut self.spans);
        trim_edges(&mut spans);
        if self.heading > 0 {
            let text = spans_text(&spans).trim().to_string();
            if !text.is_empty() {
                self.blocks.push(Block::Heading { level: self.heading, text });
            }
            self.heading = 0;
            return;
        }
        if self.quote {
            if !spans.iter().all(|s| s.text.trim().is_empty()) {
                self.blocks.push(Block::Quote(spans));
            }
            self.quote = false;
            return;
        }
        if let Some(depth) = self.lists.len().checked_sub(1) {
            if !spans.iter().all(|s| s.text.trim().is_empty()) {
                let state = &mut self.lists[depth];
                state.count += 1;
                let marker = if state.ordered { format!("{}.", state.count) } else { "-".to_string() };
                self.blocks.push(Block::Item { depth: depth as u8, marker, spans });
            }
            return;
        }
        if !spans.iter().all(|s| s.text.trim().is_empty()) {
            self.blocks.push(Block::Para(spans));
        }
    }

    fn finish(&mut self) {
        self.close_block();
        if let Some(code) = self.code.take() {
            let trimmed = trim_blank_edges(&code);
            if !trimmed.is_empty() {
                self.blocks.push(Block::Code(trimmed));
            }
        }
    }
}

/// One element, opening or closing.
fn element(b: &mut Building, name: &str, closing: bool, tag: &str, base: &str) {
    match name {
        // A paragraph, in whichever of these the page wrote it, ends the one
        // before it whether it opens or closes.
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside" | "form" | "fieldset" => {
            b.close_block();
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            if closing {
                b.close_block();
            } else {
                b.close_block();
                b.heading = name[1..].parse().unwrap_or(3);
            }
        }
        "blockquote" => {
            b.close_block();
            b.quote = !closing;
        }
        "br" => {
            // A line break inside a block ends the run of words, not the block.
            b.flush_text();
            b.text.push(' ');
        }
        "hr" => {
            b.close_block();
            b.blocks.push(Block::Rule);
        }
        "ul" | "ol" => {
            b.close_block();
            if closing {
                b.lists.pop();
            } else {
                b.lists.push(ListState { ordered: name == "ol", count: 0 });
            }

        }
        // An item's words end the item before them, and the item itself is
        // made of them.
        "li" => b.close_block(),
        "pre" => {
            if closing {
                if let Some(code) = b.code.take() {
                    let trimmed = trim_blank_edges(&code);
                    if !trimmed.is_empty() {
                        b.blocks.push(Block::Code(trimmed));
                    }
                }
            } else {
                b.close_block();
                b.code = Some(String::new());
            }
        }
        "a" => {
            b.flush_text();
            b.link = if closing { None } else { attribute(tag, "href").map(|href| resolve(base, &href)) };
        }
        "b" | "strong" => {
            b.flush_text();
            b.bold = if closing { b.bold.saturating_sub(1) } else { b.bold + 1 };
        }
        "i" | "em" => {
            b.flush_text();
            b.italic = if closing { b.italic.saturating_sub(1) } else { b.italic + 1 };
        }
        "img" => {
            b.close_block();
            let alt = attribute(tag, "alt").unwrap_or_default();
            let src = attribute(tag, "src").map(|s| resolve(base, &s)).unwrap_or_default();
            b.blocks.push(Block::Image { alt: collapse(&decode(&alt)), src });
        }
        "td" | "th" => {
            // Cells read one after another, with room between them.
            b.flush_text();
            b.text.push_str("  ");
        }
        "tr" => {
            b.close_block();
        }
        "body" | "html" | "head" | "table" | "tbody" | "thead" | "span" | "font" | "small" | "sup" | "sub" => {}
        _ => {}
    }
}

fn starts_with(chars: &[char], at: usize, what: &str) -> bool {
    what.chars().enumerate().all(|(k, c)| chars.get(at + k) == Some(&c))
}

fn find(chars: &[char], from: usize, what: &str) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if starts_with(chars, i, what) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_ci(chars: &[char], from: usize, what: &str) -> Option<usize> {
    let lower = what.to_ascii_lowercase();
    let mut i = from;
    while i < chars.len() {
        let mut matched = true;
        for (k, c) in lower.chars().enumerate() {
            match chars.get(i + k) {
                Some(got) if got.to_ascii_lowercase() == c => {}
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The tag at `at`, which points at `<`: its whole text, whether it closes
/// something, and where it ends.
fn tag_at(chars: &[char], at: usize) -> Option<(String, bool, usize)> {
    let mut i = at + 1;
    let closing = chars.get(i) == Some(&'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    while i < chars.len() && (chars[i].is_alphanumeric()) {
        i += 1;
    }
    if i == name_start {
        // `<` that names nothing, or `<!doctype>`: no element in it.
        let end = find(chars, at, ">")?;
        return Some((String::new(), false, end + 1));
    }
    // Attributes may hold `>` inside quotes.
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    let tag: String = chars[at + 1..i].iter().collect();
                    return Some((tag, closing, i + 1));
                }
            }
        }
        i += 1;
    }
    // A tag that never ends: the rest of the page is not markup.
    None
}

/// The element a tag names, in lower case, without a closing slash.
fn tag_name(tag: &str) -> String {
    let t = tag.trim_start_matches('/').trim();
    let end = t.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(t.len());
    t[..end].to_ascii_lowercase()
}

/// What attribute `name` is given in `tag`, its quotes taken off.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        let before = lower[..start].chars().next_back();
        let after = lower[start + name.len()..].chars().next();
        let stands_alone = before.is_none_or(|c| c.is_whitespace());
        let followed = after.is_some_and(|c| c == '=' || c.is_whitespace());
        if stands_alone && followed {
            let rest = &tag[start + name.len()..];
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                let mut chars = rest.chars();
                if let Some(q) = chars.next().filter(|c| *c == '"' || *c == '\'') {
                    let body: String = chars.take_while(|c| *c != q).collect();
                    return Some(body);
                }
                let body: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '>').collect();
                return (!body.is_empty()).then_some(body);
            }
        }
        from = start + name.len();
    }
    None
}

/// Turns in text, and the named characters HTML writes instead of them.
pub fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '&' {
            if let Some(end) = (i..chars.len()).take(12).find(|&k| chars[k] == ';') {
                let name: String = chars[i + 1..end].iter().collect();
                if let Some(c) = named(&name) {
                    out.push(c);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn named(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix('#') {
        let code = if let Some(hex) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            rest.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "laquo" => '«',
        "raquo" => '»',
        "ldquo" => '“',
        "rdquo" => '”',
        "lsquo" => '‘',
        "rsquo" => '’',
        "middot" => '·',
        "bull" => '•',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "sect" => '§',
        "para" => '¶',
        "shy" => '\u{ad}',
        _ => return None,
    })
}

/// Text with its runs of whitespace made one space each, keeping a single
/// space at either end where one was written.
///
/// A block's words are trimmed as a whole and not span by span: markup puts
/// a space before `<b>` and after `</b>`, and trimming each span would run
/// the words together.
fn collapse_edges(text: &str) -> String {
    let lead = text.starts_with(|c: char| c.is_whitespace());
    let trail = text.ends_with(|c: char| c.is_whitespace());
    let body = collapse(text);
    if body.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(body.len() + 2);
    if lead {
        out.push(' ');
    }
    out.push_str(&body);
    if trail {
        out.push(' ');
    }
    out
}

/// Take the space off a block's first and last words, and drop the spans that
/// were only space.
fn trim_edges(spans: &mut Vec<Span>) {
    if let Some(first) = spans.first_mut() {
        let cut = first.text.len() - first.text.trim_start().len();
        first.text.drain(..cut);
    }
    if let Some(last) = spans.last_mut() {
        let kept = last.text.trim_end().len();
        last.text.truncate(kept);
    }
    spans.retain(|span| !span.text.is_empty());
}

/// Text with its runs of whitespace made one space each, and its ends
/// trimmed: what markup means by whitespace.
pub fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

fn trim_blank_edges(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let last = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
    lines[first..last].join("\n")
}

/// `href` as an address, worked out against the page it was written in.
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.is_empty() {
        return base.to_string();
    }
    if href.contains("://") || href.starts_with("mailto:") || href.starts_with("data:") {
        return href.to_string();
    }
    let (scheme, rest) = match base.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("https", base),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if let Some(without) = href.strip_prefix("//") {
        return format!("{scheme}://{without}");
    }
    if href.starts_with('/') {
        return format!("{scheme}://{authority}{href}");
    }
    if href.starts_with('#') {
        let here = base.split('#').next().unwrap_or(base);
        return format!("{here}{href}");
    }
    // A path relative to the folder the page is in.
    let folder = match path.rfind('/') {
        Some(at) => &path[..=at],
        None => "/",
    };
    format!("{scheme}://{authority}{folder}{href}")
}

/// An address as a reader typed it, made into one a server is asked for.
///
/// `example.com` is a place to go, so it is asked for over https; anything
/// that already names its scheme is left as it was written.
pub fn normalise(input: &str) -> String {
    let typed = input.trim();
    if typed.is_empty() {
        return String::new();
    }
    if typed.contains("://") {
        return typed.to_string();
    }
    if typed.starts_with("//") {
        return format!("https:{typed}");
    }
    // Somebody who wrote about:blank meant it.
    if typed.split(':').next().is_some_and(|scheme| {
        !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    }) && typed.contains(':')
        && !typed.contains('.')
    {
        return typed.to_string();
    }
    format!("https://{typed}")
}

/// A page of plain text, as blocks a reader is shown: its lines, blank ones
/// kept as the paragraph breaks they are.
pub fn from_text(body: &str) -> Document {
    let mut blocks = Vec::new();
    let mut held: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            if !held.is_empty() {
                blocks.push(Block::Para(vec![Span { text: held.join("\n"), link: None, bold: false, italic: false }]));
                held.clear();
            }
            continue;
        }
        held.push(line.to_string());
    }
    if !held.is_empty() {
        blocks.push(Block::Para(vec![Span { text: held.join("\n"), link: None, bold: false, italic: false }]));
    }
    Document { title: String::new(), blocks }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_read_into_its_title_and_its_blocks() {
        let html = r#"<html><head><title>A page about &amp; things</title>
            <style>body { color: red }</style><script>var x = "<p>not text</p>";</script></head>
            <body><h1>Heading</h1><p>First paragraph.</p><p>Second one.</p></body></html>"#;
        let doc = read(html, "https://example.com/page");
        assert_eq!(doc.title, "A page about & things");
        assert_eq!(doc.blocks[0], Block::Heading { level: 1, text: "Heading".into() });
        assert!(matches!(&doc.blocks[1], Block::Para(s) if spans_text(s) == "First paragraph."), "{:?}", doc.blocks[1]);
        assert!(matches!(&doc.blocks[2], Block::Para(s) if spans_text(s) == "Second one."));
        let text = doc.text();
        assert!(!text.contains("color"), "a stylesheet is not what the page says: {text}");
        assert!(!text.contains("var x"), "a script is not what the page says: {text}");
    }

    #[test]
    fn a_link_keeps_where_it_leads_worked_out_against_the_page() {
        let html = r#"<p>See <a href="/about">about</a> and <a href="next">next</a>.</p>"#;
        let doc = read(html, "https://example.com/docs/page.html");
        let Block::Para(spans) = &doc.blocks[0] else { panic!("a paragraph") };
        let linked: Vec<(&str, Option<&str>)> = spans.iter().map(|s| (s.text.as_str(), s.link.as_deref())).collect();
        assert_eq!(
            linked,
            vec![
                ("See ", None),
                ("about", Some("https://example.com/about")),
                (" and ", None),
                ("next", Some("https://example.com/docs/next")),
                (".", None),
            ]
        );
    }

    #[test]
    fn a_list_is_numbered_or_bulleted_and_keeps_its_depth() {
        let html = "<ul><li>one</li><li>two<ul><li>deeper</li></ul></li></ul><ol><li>first</li><li>second</li></ol>";
        let doc = read(html, "https://example.com");
        let items: Vec<(u8, &str, String)> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Item { depth, marker, spans } => Some((*depth, marker.as_str(), spans_text(spans))),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![
                (0, "-", "one".to_string()),
                (0, "-", "two".to_string()),
                (1, "-", "deeper".to_string()),
                (0, "1.", "first".to_string()),
                (0, "2.", "second".to_string()),
            ]
        );
    }

    #[test]
    fn preformatted_text_keeps_its_lines_and_its_spaces() {
        let doc = read("<pre>fn main() {\n    one();\n}</pre>", "https://example.com");
        assert_eq!(doc.blocks, vec![Block::Code("fn main() {\n    one();\n}".into())]);
    }

    #[test]
    fn a_broken_page_is_read_as_far_as_it_goes() {
        for broken in [
            "<p>unclosed",
            "<p>a <b>bold never closed",
            "<<<>>>",
            "<h2>",
            "<a href=",
            "<div><span>text</div>",
            "&amp &notanentity; &#",
            "",
        ] {
            let doc = read(broken, "https://example.com");
            // Nothing about reading a page may end the recovery.
            let _ = doc.text();
        }
    }

    #[test]
    fn what_a_page_calls_characters_is_read_back() {
        assert_eq!(decode("a &amp; b &lt;c&gt; &quot;d&quot; &#65; &#x42; &copy;"), "a & b <c> \"d\" A B ©");
        assert_eq!(decode("&notanentity; &"), "&notanentity; &", "an unknown name is left as it was written");
        assert_eq!(collapse("  a \n\t b  "), "a b");
    }

    #[test]
    fn an_address_is_worked_out_the_way_a_browser_does() {
        let base = "https://example.com/dir/page.html";
        assert_eq!(resolve(base, "https://other.org/x"), "https://other.org/x");
        assert_eq!(resolve(base, "//cdn.org/x"), "https://cdn.org/x");
        assert_eq!(resolve(base, "/root"), "https://example.com/root");
        assert_eq!(resolve(base, "sibling.html"), "https://example.com/dir/sibling.html");
        assert_eq!(resolve(base, "sub/deep.html"), "https://example.com/dir/sub/deep.html");
        assert_eq!(resolve(base, "mailto:a@b.c"), "mailto:a@b.c");
        assert_eq!(resolve("https://example.com", "/x"), "https://example.com/x", "a page at the root");
    }

    #[test]
    fn an_image_is_named_but_not_shown() {
        let doc = read(r#"<img src="/a.png" alt="a picture">"#, "https://example.com");
        assert_eq!(doc.blocks, vec![Block::Image { alt: "a picture".into(), src: "https://example.com/a.png".into() }]);
    }
}

/// A run of words with one look, laid out as part of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaidPiece {
    /// Which of the block's spans this came from.
    pub span: usize,
    /// The words of that span on this line.
    pub text: String,
}

/// One line of a block, its pieces in the order they are read, and how wide
/// it came out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaidLine {
    pub pieces: Vec<LaidPiece>,
    pub width: f32,
}

impl LaidLine {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            out.push_str(&piece.text);
        }
        out
    }
}

/// Lay `spans` out into lines no wider than `room`, `measure` giving the
/// width of a run of words at the size it will be drawn.
///
/// A word too long for a line of its own is broken between characters, which
/// is what keeps a page of one long unbroken address or one Chinese sentence
/// from running off the side of the screen.
pub fn layout(spans: &[Span], room: f32, measure: &dyn Fn(&str) -> f32) -> Vec<LaidLine> {
    let mut lines: Vec<LaidLine> = vec![LaidLine::default()];
    // Whether the line being written would still hold `width`.
    let fits = |lines: &Vec<LaidLine>, width: f32| -> bool {
        match lines.last() {
            Some(line) => line.pieces.is_empty() || line.width + width <= room,
            None => true,
        }
    };
    let put = |lines: &mut Vec<LaidLine>, index: usize, text: String| {
        let line = lines.last_mut().expect("a line is always being written");
        line.pieces.push(LaidPiece { span: index, text });
        line.width = measure(&line.text());
    };
    for (index, span) in spans.iter().enumerate() {
        // Words keep their trailing space, so that joining two of them reads
        // as the page writes it.
        for word in words_of(&span.text) {
            let width = measure(&word);
            if !fits(&lines, width) {
                lines.push(LaidLine::default());
            }
            if width <= room {
                put(&mut lines, index, word);
                continue;
            }
            // Wider than a line on its own: break it between characters.
            for ch in word.chars() {
                let one = ch.to_string();
                let cw = measure(&one);
                if !fits(&lines, cw) {
                    lines.push(LaidLine::default());
                }
                put(&mut lines, index, one);
            }
        }
    }
    // Trailing blank lines come from trailing spaces.
    while lines.len() > 1 && lines.last().is_some_and(|l| l.pieces.is_empty()) {
        lines.pop();
    }
    lines
}

/// The words of `text`, each with the space that follows it. The space stays
/// on the word it follows so that a line's pieces joined read as the page
/// writes them, including across the spans a line is made of.
fn words_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut held = String::new();
    for ch in text.chars() {
        held.push(ch);
        if ch.is_whitespace() {
            out.push(std::mem::take(&mut held));
        }
    }
    if !held.is_empty() {
        out.push(held);
    }
    out
}


    #[test]
    fn a_word_split_across_spans_keeps_the_spaces_around_it() {
        let doc = read("<p>This page was <b>not</b> rendered by an engine.</p>", "https://example.com");
        let Block::Para(spans) = &doc.blocks[0] else { panic!("a paragraph: {:?}", doc.blocks) };
        assert_eq!(spans_text(spans), "This page was not rendered by an engine.", "spans: {spans:?}");
    }

    #[test]
    fn a_quotation_is_collapsed_like_any_other_block() {
        let html = "<blockquote>An image is named\n            rather than fetched.</blockquote>";
        let doc = read(html, "https://example.com");
        let Block::Quote(spans) = &doc.blocks[0] else { panic!("a quotation: {:?}", doc.blocks) };
        assert_eq!(spans_text(spans), "An image is named rather than fetched.", "spans: {spans:?}");
    }

    #[test]
    fn a_block_does_not_begin_or_end_with_a_space() {
        for html in ["<p>  padded  </p>", "<p><b>bold</b> tail</p>", "<p>lead <i>it</i></p>"] {
            let doc = read(html, "https://example.com");
            let Block::Para(spans) = &doc.blocks[0] else { panic!("a paragraph") };
            let text = spans_text(spans);
            assert_eq!(text, text.trim(), "{html}: {text:?} keeps a space at an edge");
        }
    }
#[cfg(test)]
mod layout_tests {
    use super::*;

    /// A stand-in for measuring text: a character is ten wide, 2 for a space.
    fn measure(text: &str) -> f32 {
        text.chars().map(|c| if c == ' ' { 2.0 } else { 10.0 }).sum()
    }

    fn span(text: &str, link: Option<&str>) -> Span {
        Span { text: text.into(), link: link.map(str::to_string), bold: false, italic: false }
    }

    #[test]
    fn words_are_laid_out_no_wider_than_the_room() {
        let spans = vec![span("one two three four five six seven", None)];
        let lines = layout(&spans, 60.0, &measure);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.width <= 60.0, "{:?} came to {}", line.text(), line.width);
        }
        // Every word is there, once.
        let all: String = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
        for word in ["one", "two", "three", "four", "five", "six", "seven"] {
            assert!(all.contains(word), "{word} went missing from {all:?}");
        }
    }

    #[test]
    fn a_word_longer_than_the_room_is_broken_rather_than_run_off() {
        let spans = vec![span("aaaaaaaaaaaaaaaaaaaaaaaa", None)];
        let lines = layout(&spans, 50.0, &measure);
        assert!(lines.len() >= 5, "{:?}", lines.iter().map(|l| l.text()).collect::<Vec<_>>());
        for line in &lines {
            assert!(line.width <= 50.0, "{:?} came to {}", line.text(), line.width);
        }
        let joined: String = lines.iter().map(|l| l.text()).collect();
        assert_eq!(joined, "aaaaaaaaaaaaaaaaaaaaaaaa", "nothing was lost in the breaking");
    }

    #[test]
    fn a_chinese_sentence_wraps_between_characters() {
        let spans = vec![span("这是一句没有任何空格的中文句子用来测试换行是否正常", None)];
        let lines = layout(&spans, 100.0, &measure);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.width <= 100.0, "{:?} came to {}", line.text(), line.width);
        }
        let joined: String = lines.iter().map(|l| l.text()).collect();
        assert_eq!(joined, "这是一句没有任何空格的中文句子用来测试换行是否正常");
    }

    #[test]
    fn each_piece_says_which_span_it_came_from_so_links_keep_where_they_lead() {
        let spans = vec![span("go ", None), span("here", Some("https://example.com"))];
        let lines = layout(&spans, 500.0, &measure);
        assert_eq!(lines.len(), 1);
        let pieces: Vec<(usize, &str)> = lines[0].pieces.iter().map(|p| (p.span, p.text.as_str())).collect();
        assert_eq!(pieces, vec![(0, "go "), (1, "here")]);
    }
}
    #[test]
    fn an_address_typed_without_a_scheme_is_asked_for_over_https() {
        assert_eq!(normalise("example.com"), "https://example.com");
        assert_eq!(normalise("  example.com/a?b=1  "), "https://example.com/a?b=1");
        assert_eq!(normalise("http://example.com"), "http://example.com", "a scheme written is kept");
        assert_eq!(normalise("//example.com/x"), "https://example.com/x");
        assert_eq!(normalise("about:blank"), "about:blank");
        assert_eq!(normalise(""), "");
    }

    #[test]
    fn plain_text_is_read_with_its_paragraphs_kept() {
        let doc = from_text("one\ntwo\n\nthree\n");
        assert_eq!(doc.blocks.len(), 2);
        assert!(matches!(&doc.blocks[0], Block::Para(s) if spans_text(s) == "one\ntwo"));
        assert!(matches!(&doc.blocks[1], Block::Para(s) if spans_text(s) == "three"));
    }