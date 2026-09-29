//! A small hand-rolled markdown reader — PLAN §6's "rendered markdown
//! (replaces glow)".
//!
//! Two halves, both pure and both tested without a window: [`parse`] turns
//! source into a list of [`Block`]s, and [`inline`] turns one block's text into
//! styled [`Span`]s. Painting them is [`super::paint`]'s job, so nothing here
//! knows about egui, fonts or colours.
//!
//! ## What it understands
//!
//! ATX headings (`#`…`######`), fenced code blocks with an info string,
//! bullet and ordered lists with nesting, blockquotes, horizontal rules,
//! paragraphs, and inline `**bold**`, `*italic*`, `` `code` ``, `[text](url)`
//! and `<https://autolinks>`. That is the vocabulary of every README this pane
//! will ever be pointed at.
//!
//! ## What it does not, and why that is a choice
//!
//! - **No images.** `![alt](src)` renders as its alt text with a marker. A
//!   preview pane that fetched and decoded images out of a document would be a
//!   second decode pipeline running on a file the user only glanced at, and it
//!   is the one markdown feature that can go and read the network.
//! - **No tables, footnotes, setext headings or HTML passthrough.** Each of
//!   them is either rare in a README or needs a layout engine (tables) rather
//!   than a parser.
//! - **No reference links** (`[a][b]` with a definition elsewhere): resolving
//!   them means two passes and a map, for a form almost nobody writes any more.
//!
//! None of these fail loudly — an unhandled construct renders as the plain
//! text it is written as, which is exactly what a terminal reader shows too.

/// How one run of text is drawn. A set of flags rather than an enum because
/// they compose: `**a *b* c**` is bold, then bold+italic, then bold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    /// Inline `code`, drawn monospace on its own plate.
    pub code: bool,
    /// Inside a link's text. Not clickable yet (PLAN §6 has openers in Phase
    /// 2); coloured so it reads as one.
    pub link: bool,
}

/// One styled run of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    /// A list item. `marker` is what to draw in the gutter — a bullet, or the
    /// number the document wrote, so `3.` stays `3.` rather than being
    /// renumbered from one.
    Item {
        depth: usize,
        marker: String,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    /// A fenced code block, with the language name the highlighter wants.
    Code {
        syntax: Option<&'static str>,
        lines: Vec<String>,
    },
    Rule,
    /// A blank line between blocks. Carried rather than dropped so the painter
    /// can space paragraphs the way the author wrote them.
    Gap,
}

/// How many spaces of indentation make one list level.
///
/// Two, because that is what every markdown formatter in use emits (prettier,
/// deno fmt, rustfmt's doc comments). A four-space list still nests, one level
/// per two spaces, which over-indents nothing anybody will notice at a glance.
const INDENT: usize = 2;

/// The most a list may nest in the gutter. Past this the text column would be
/// narrower than the indent that pushed it there, so deeper items simply stop
/// moving right.
pub const MAX_DEPTH: usize = 6;

/// Parse a whole document.
pub fn parse(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = source
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l));
    let mut pending: Vec<&str> = Vec::new();
    // `split` cannot be peeked and re-consumed cleanly across the fence case,
    // so the loop collects into a vector first — a preview is already capped at
    // `TEXT_BYTES` by df-core, so this is bounded.
    let mut all: Vec<&str> = lines.by_ref().collect();
    // A file ending in a newline splits into a trailing empty string, which is
    // not a line anybody wrote — the same trim `df_core::preview` applies to
    // its line list.
    if source.ends_with('\n') {
        all.pop();
    }

    let mut i = 0usize;
    while i < all.len() {
        let line = all[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // ── Fenced code ─────────────────────────────────────────────────────
        if let Some(fence) = fence_open(trimmed) {
            flush(&mut pending, &mut blocks);
            let info = trimmed[fence.len()..].trim();
            let syntax = fence_syntax(info);
            let mut body = Vec::new();
            i += 1;
            while i < all.len() {
                let candidate = all[i].trim_start();
                if candidate.starts_with(fence)
                    && candidate
                        .trim_end()
                        .chars()
                        .all(|c| c == fence.as_bytes()[0] as char)
                {
                    i += 1;
                    break;
                }
                // The opening fence's indent is stripped from the body, which
                // is what keeps a code block inside a list item from drawing
                // with four leading spaces on every line.
                let raw = all[i];
                let strip = raw.len() - raw.trim_start().len();
                body.push(raw[strip.min(indent)..].to_string());
                i += 1;
            }
            blocks.push(Block::Code {
                syntax,
                lines: body,
            });
            continue;
        }

        // ── Horizontal rule ─────────────────────────────────────────────────
        if is_rule(trimmed) {
            flush(&mut pending, &mut blocks);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }

        // ── Heading ─────────────────────────────────────────────────────────
        if let Some((level, text)) = heading(trimmed) {
            flush(&mut pending, &mut blocks);
            blocks.push(Block::Heading {
                level,
                spans: inline(text),
            });
            i += 1;
            continue;
        }

        // ── Blockquote ──────────────────────────────────────────────────────
        if let Some(text) = trimmed.strip_prefix('>') {
            flush(&mut pending, &mut blocks);
            blocks.push(Block::Quote(inline(text.trim_start())));
            i += 1;
            continue;
        }

        // ── List item ───────────────────────────────────────────────────────
        if let Some((marker, text)) = list_item(trimmed) {
            flush(&mut pending, &mut blocks);
            blocks.push(Block::Item {
                depth: (indent / INDENT).min(MAX_DEPTH),
                marker,
                spans: inline(text),
            });
            i += 1;
            continue;
        }

        // ── Blank ───────────────────────────────────────────────────────────
        if trimmed.is_empty() {
            flush(&mut pending, &mut blocks);
            // One gap, however many blank lines: a document with six blank
            // lines in it did not mean six.
            if !matches!(blocks.last(), Some(Block::Gap) | None) {
                blocks.push(Block::Gap);
            }
            i += 1;
            continue;
        }

        // ── Paragraph, accumulating until something else interrupts ─────────
        pending.push(trimmed);
        i += 1;
    }
    flush(&mut pending, &mut blocks);
    // A trailing gap is the file's final newline, not a blank line the author
    // wrote.
    if matches!(blocks.last(), Some(Block::Gap)) {
        blocks.pop();
    }
    blocks
}

/// Turn the accumulated paragraph lines, if any, into a block.
///
/// The lines are joined with a space, which is markdown's own rule: a hard
/// wrap in the source is not a line break in the output.
fn flush(pending: &mut Vec<&str>, blocks: &mut Vec<Block>) {
    if pending.is_empty() {
        return;
    }
    let text = pending.join(" ");
    pending.clear();
    blocks.push(Block::Paragraph(inline(&text)));
}

/// The fence that opens here — ``` or ~~~ — if one does.
fn fence_open(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// `---`, `***`, `___`: three or more of one character and nothing else.
fn is_rule(trimmed: &str) -> bool {
    let t = trimmed.trim_end();
    let Some(first) = t.chars().next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_') && t.len() >= 3 && t.chars().all(|c| c == first)
}

/// `## Heading` → `(2, "Heading")`. The space is required, so a `#tag` at the
/// start of a line stays a tag.
fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    let text = rest.strip_prefix(' ')?;
    // Closing hashes (`## Title ##`) are decoration, not content.
    Some((hashes as u8, text.trim().trim_end_matches('#').trim_end()))
}

/// `- text`, `* text`, `+ text`, `1. text`, `2) text`.
fn list_item(trimmed: &str) -> Option<(String, &str)> {
    for bullet in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            if let Some(text) = rest.strip_prefix(' ') {
                // `- [ ]` and `- [x]` are task list items; the box is the
                // marker, because that is the part being read.
                if let Some(task) = text.strip_prefix("[ ] ") {
                    return Some(("☐".to_string(), task));
                }
                if let Some(task) = text
                    .strip_prefix("[x] ")
                    .or_else(|| text.strip_prefix("[X] "))
                {
                    // `✓` rather than `☑`: the stock faces draw the empty
                    // box but not the ticked one, and a `✓` no face draws
                    // is drawn by `crate::glyphs`.
                    return Some(("✓".to_string(), task));
                }
                return Some(("•".to_string(), text));
            }
        }
    }
    let digits = trimmed.len()
        - trimmed
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = &trimmed[digits..];
    for sep in ['.', ')'] {
        if let Some(text) = rest.strip_prefix(sep).and_then(|r| r.strip_prefix(' ')) {
            return Some((format!("{}{sep}", &trimmed[..digits]), text));
        }
    }
    None
}

/// A fence's info string → the highlighter's language name.
///
/// Info strings are written as language *names* (` ```rust `, ` ```bash `)
/// while df-core's table is keyed by file extension, so the common names are
/// listed here and anything else is tried as an extension — which catches
/// ` ```rs `, ` ```py ` and every other abbreviation for free.
fn fence_syntax(info: &str) -> Option<&'static str> {
    // Only the first word: ` ```rust,ignore ` and ` ```js title="x" ` are both
    // real in the wild.
    let word = info
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if word.is_empty() {
        return None;
    }
    let named = match word.as_str() {
        "rust" => "Rust",
        "c" => "C",
        "cpp" | "c++" => "C++",
        "csharp" => "C#",
        "java" => "Java",
        "kotlin" => "Kotlin",
        "go" | "golang" => "Go",
        "zig" => "Zig",
        "swift" => "Swift",
        "javascript" | "node" => "JavaScript",
        "typescript" => "TypeScript",
        "python" => "Python",
        "ruby" => "Ruby",
        "php" => "PHP",
        "lua" => "Lua",
        "bash" | "sh" | "shell" | "zsh" | "console" | "terminal" => "Shell",
        "fish" => "Fish",
        "powershell" | "pwsh" => "PowerShell",
        "toml" => "TOML",
        "ini" => "INI",
        "yaml" => "YAML",
        "json" | "json5" => "JSON",
        "html" => "HTML",
        "xml" => "XML",
        "svelte" => "Svelte",
        "vue" => "Vue",
        "css" => "CSS",
        "sql" => "SQL",
        "diff" | "patch" => "Diff",
        "make" | "makefile" => "Makefile",
        "docker" | "dockerfile" => "Dockerfile",
        "cmake" => "CMake",
        "graphql" => "GraphQL",
        "proto" | "protobuf" => "Protocol Buffer",
        "haskell" => "Haskell",
        "ocaml" => "OCaml",
        "clojure" => "Clojure",
        "nix" => "Nix",
        "elixir" => "Elixir",
        "erlang" => "Erlang",
        "julia" => "Julia",
        "perl" => "Perl",
        "r" => "R",
        "latex" | "tex" => "LaTeX",
        "text" | "txt" | "plain" | "output" => return None,
        // Not a name we know: try it as an extension, which is how `rs`, `py`,
        // `ts` and `yml` all resolve.
        other => return df_core::preview::syntax_for_name(&format!("fence.{other}")),
    };
    Some(named)
}

/// Parse one line's inline markup.
///
/// A single left-to-right pass with a style that is pushed and popped by the
/// delimiters, rather than a tree: markdown's emphasis rules are famously
/// ambiguous and a preview does not need to win the edge cases. An unclosed
/// `**` simply styles the rest of the line, which is what the author will see
/// and immediately understand.
pub fn inline(text: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut style = Style::default();
    let bytes = text.as_bytes();
    let mut i = 0usize;

    let push = |buf: &mut String, spans: &mut Vec<Span>, style: Style| {
        if !buf.is_empty() {
            spans.push(Span {
                text: std::mem::take(buf),
                style,
            });
        }
    };

    while i < bytes.len() {
        let rest = &text[i..];

        // Backslash escapes: `\*` is an asterisk, not emphasis.
        if let Some(escaped) = rest.strip_prefix('\\') {
            if let Some(c) = escaped.chars().next() {
                if "\\`*_[]()#+-.!>".contains(c) {
                    buf.push(c);
                    i += 1 + c.len_utf8();
                    continue;
                }
            }
        }

        // Inline code wins over everything: `**` inside backticks is literal.
        if rest.starts_with('`') {
            let ticks = rest.len() - rest.trim_start_matches('`').len();
            let fence = &rest[..ticks];
            if let Some(end) = rest[ticks..].find(fence) {
                push(&mut buf, &mut spans, style);
                spans.push(Span {
                    text: rest[ticks..ticks + end].to_string(),
                    style: Style {
                        code: true,
                        ..style
                    },
                });
                i += ticks + end + ticks;
                continue;
            }
        }

        // Emphasis. `**` and `__` before `*` and `_`, or `**a**` would open
        // italic twice.
        if rest.starts_with("**") || rest.starts_with("__") {
            push(&mut buf, &mut spans, style);
            style.bold = !style.bold;
            i += 2;
            continue;
        }
        if (rest.starts_with('*') || rest.starts_with('_'))
            // `snake_case_names` are not three italic runs: an underscore
            // between two word characters is a letter.
            && !(rest.starts_with('_')
                && i > 0
                && is_word(bytes[i - 1])
                && bytes.get(i + 1).is_some_and(|b| is_word(*b)))
        {
            push(&mut buf, &mut spans, style);
            style.italic = !style.italic;
            i += 1;
            continue;
        }

        // Links, and images as their alt text.
        let image = rest.starts_with("![");
        if image || rest.starts_with('[') {
            let from = if image { 2 } else { 1 };
            if let Some((label, after)) = link(&rest[from..]) {
                push(&mut buf, &mut spans, style);
                if image {
                    // The marker says a picture is *here* without pretending to
                    // show it (see the module note on images).
                    spans.push(Span {
                        text: format!("🖼 {label}"),
                        style: Style {
                            code: true,
                            ..style
                        },
                    });
                } else {
                    for span in inline(label) {
                        spans.push(Span {
                            style: Style {
                                link: true,
                                bold: span.style.bold || style.bold,
                                italic: span.style.italic || style.italic,
                                code: span.style.code,
                            },
                            text: span.text,
                        });
                    }
                }
                i += from + after;
                continue;
            }
        }

        // `<https://example.com>` — an autolink is its own label.
        if rest.starts_with("<http") {
            if let Some(end) = rest.find('>') {
                push(&mut buf, &mut spans, style);
                spans.push(Span {
                    text: rest[1..end].to_string(),
                    style: Style {
                        link: true,
                        ..style
                    },
                });
                i += end + 1;
                continue;
            }
        }

        let c = rest.chars().next().unwrap_or(' ');
        buf.push(c);
        i += c.len_utf8();
    }
    push(&mut buf, &mut spans, style);
    spans
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Parse `label](target)` starting just after the opening bracket. Returns the
/// label and how many bytes the whole `label](target)` took.
///
/// Bracket depth is counted so `[a [b] c](x)` finds the right `]`.
fn link(rest: &str) -> Option<(&str, usize)> {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut close = None;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' if depth == 0 => {
                close = Some(i);
                break;
            }
            b']' => depth -= 1,
            _ => {}
        }
    }
    let close = close?;
    let after = rest.get(close + 1..)?;
    if !after.starts_with('(') {
        return None;
    }
    let end = after.find(')')?;
    Some((&rest[..close], close + 1 + end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Vec<Span> {
        vec![Span {
            text: text.to_string(),
            style: Style::default(),
        }]
    }

    #[test]
    fn headings_carry_their_level_and_lose_their_hashes() {
        let blocks = parse("# One\n## Two ##\n###### Six\n####### not a heading\n");
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 1,
                spans: plain("One")
            }
        );
        assert_eq!(
            blocks[1],
            Block::Heading {
                level: 2,
                spans: plain("Two")
            }
        );
        assert_eq!(
            blocks[2],
            Block::Heading {
                level: 6,
                spans: plain("Six")
            }
        );
        // Seven hashes is not a heading in any dialect.
        assert_eq!(blocks[3], Block::Paragraph(plain("####### not a heading")));
    }

    /// A `#` with no space is a tag, a colour, a comment — not a heading.
    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        assert_eq!(parse("#tag\n")[0], Block::Paragraph(plain("#tag")));
    }

    #[test]
    fn a_paragraph_joins_its_hard_wrapped_lines() {
        let blocks = parse("one\ntwo\n\nthree\n");
        assert_eq!(blocks[0], Block::Paragraph(plain("one two")));
        assert_eq!(blocks[1], Block::Gap);
        assert_eq!(blocks[2], Block::Paragraph(plain("three")));
        assert_eq!(blocks.len(), 3, "{blocks:?}");
    }

    #[test]
    fn fenced_code_keeps_its_lines_and_finds_its_language() {
        let blocks = parse("text\n\n```rust\nfn main() {}\n\n// blank line inside\n```\nafter\n");
        let code = blocks
            .iter()
            .find_map(|b| match b {
                Block::Code { syntax, lines } => Some((*syntax, lines.clone())),
                _ => None,
            })
            .expect("a code block");
        assert_eq!(code.0, Some("Rust"));
        assert_eq!(code.1, ["fn main() {}", "", "// blank line inside"]);
        assert!(blocks.contains(&Block::Paragraph(plain("after"))));
    }

    #[test]
    fn info_strings_resolve_by_name_and_by_extension() {
        assert_eq!(fence_syntax("bash"), Some("Shell"));
        assert_eq!(fence_syntax("rust,ignore"), Some("Rust"));
        assert_eq!(fence_syntax("ts"), Some("TypeScript"));
        assert_eq!(fence_syntax("yml"), Some("YAML"));
        assert_eq!(fence_syntax(""), None);
        assert_eq!(fence_syntax("text"), None);
        assert_eq!(fence_syntax("nonsense"), None);
    }

    /// An unclosed fence must not swallow the parser — the rest of the file is
    /// the code block, and nothing is lost.
    #[test]
    fn an_unclosed_fence_ends_at_the_file() {
        let blocks = parse("```\nstill code\n");
        assert_eq!(
            blocks,
            [Block::Code {
                syntax: None,
                lines: vec!["still code".to_string()]
            }]
        );
    }

    #[test]
    fn lists_nest_and_keep_the_number_the_document_wrote() {
        let blocks = parse("- one\n  - two\n3. three\n- [x] done\n");
        assert_eq!(
            blocks[0],
            Block::Item {
                depth: 0,
                marker: "•".into(),
                spans: plain("one")
            }
        );
        assert_eq!(
            blocks[1],
            Block::Item {
                depth: 1,
                marker: "•".into(),
                spans: plain("two")
            }
        );
        assert_eq!(
            blocks[2],
            Block::Item {
                depth: 0,
                marker: "3.".into(),
                spans: plain("three")
            }
        );
        assert_eq!(
            blocks[3],
            Block::Item {
                depth: 0,
                marker: "✓".into(),
                spans: plain("done")
            }
        );
    }

    #[test]
    fn rules_quotes_and_gaps() {
        let blocks = parse("---\n> quoted\n\n\n\nend\n");
        assert_eq!(blocks[0], Block::Rule);
        assert_eq!(blocks[1], Block::Quote(plain("quoted")));
        assert_eq!(blocks[2], Block::Gap, "runs of blank lines collapse");
        assert_eq!(blocks[3], Block::Paragraph(plain("end")));
        assert_eq!(blocks.len(), 4, "{blocks:?}");
    }

    #[test]
    fn emphasis_composes() {
        let spans = inline("a **b *c* d** e");
        let styled: Vec<_> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.style.bold, s.style.italic))
            .collect();
        assert_eq!(
            styled,
            [
                ("a ", false, false),
                ("b ", true, false),
                ("c", true, true),
                (" d", true, false),
                (" e", false, false),
            ]
        );
    }

    /// The bug every naive emphasis parser has.
    #[test]
    fn snake_case_is_not_italic() {
        let spans = inline("a snake_case_name here");
        assert_eq!(spans, plain("a snake_case_name here"));
    }

    #[test]
    fn inline_code_is_literal_inside() {
        let spans = inline("use `a **b** c` here");
        assert_eq!(spans[1].text, "a **b** c");
        assert!(spans[1].style.code);
        assert!(!spans[1].style.bold);
    }

    #[test]
    fn links_keep_their_label_and_lose_their_target() {
        let spans = inline("see [the docs](https://example.com/x) now");
        assert_eq!(spans[1].text, "the docs");
        assert!(spans[1].style.link);
        assert_eq!(spans[2].text, " now");
        assert!(!spans[2].style.link);
        // …and an autolink is its own label.
        let spans = inline("at <https://example.com>");
        assert_eq!(spans[1].text, "https://example.com");
        assert!(spans[1].style.link);
    }

    /// PLAN §6 v1: an image is announced, never fetched.
    #[test]
    fn an_image_becomes_its_alt_text() {
        let spans = inline("![a cat](cat.png)");
        assert_eq!(spans.len(), 1);
        assert!(spans[0].text.contains("a cat"));
        assert!(spans[0].style.code);
    }

    #[test]
    fn an_escaped_delimiter_is_a_character() {
        assert_eq!(inline(r"2 \* 3 \*\* 4"), plain("2 * 3 ** 4"));
    }

    /// Nothing may panic or lose bytes on text that is not markdown at all.
    #[test]
    fn hostile_input_round_trips_its_characters() {
        let cases = [
            "**",
            "`",
            "[unclosed",
            "![",
            "<http",
            "***",
            "a\\",
            "日本語 **太字** です",
            "____",
        ];
        for case in cases {
            let spans = inline(case);
            // Every byte that was not a delimiter survives.
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            for c in case.chars().filter(|c| !"*_`[]()\\<>!".contains(*c)) {
                assert!(joined.contains(c), "{case:?} lost {c:?}");
            }
        }
        // …and the same for the block parser.
        for case in ["```", "> ", "- ", "#", "---", ""] {
            let _ = parse(case);
        }
    }
}
