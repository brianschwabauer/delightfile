//! A small hand-rolled syntax highlighter.
//!
//! PLAN §6 asks for "text with syntax highlighting (tmTheme from the vendored
//! catppuccin flavor)". There is no tmTheme engine in this tree and PLAN §1
//! forbids adding one (`syntect` pulls in `onig`/`fancy-regex`, `regex`,
//! `plist`, `bincode` and a 2 MB dump file; the egui syntax crates pull the
//! same). So this is the deliberate smaller answer: a **lexical** highlighter
//! that knows five kinds of token — comment, string, number, keyword, type —
//! and a table of per-language rules for finding them.
//!
//! ## The tradeoff, stated plainly
//!
//! A real grammar knows *structure*: that `impl` introduces a block, that a
//! name after `struct` is a type, that a Ruby `%w[]` is a word array. This
//! knows none of that. What it gets right is the 95% that a reader of a
//! **preview pane** is actually using colour for — telling code from prose,
//! finding the string literals, skimming past the comments — and it gets it in
//! a few microseconds per line with no dependencies and no theme files.
//!
//! It is structured so a better engine can replace it without touching the
//! painter: everything outside this file speaks only [`Tok`] and [`Span`], so
//! a future tree-sitter or tmTheme backend has to produce spans and nothing
//! else. [`profile_for`] is the only place language names are known.
//!
//! ## Where the cost is
//!
//! [`scan_line`] is O(line) with no allocation when `out` is `None`, which is
//! the mode [`block_states`] uses to walk a whole file once on arrival and
//! record where each line *starts* (inside a block comment? inside a multi-line
//! string?). The painter then highlights only the lines it can see, starting
//! from the state that pass recorded — so scrolling a 20,000-line file costs
//! one screenful of tokenising per frame, not a file's worth.

/// What a run of characters is. Deliberately few: these are the distinctions a
/// person skims a preview for, and every extra one is a colour competing for
/// the same attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    /// Ordinary code — identifiers, whitespace, anything unclassified.
    Text,
    Comment,
    /// A string or character literal, quotes included.
    Str,
    Number,
    /// A language keyword.
    Keyword,
    /// A builtin type or constant (`u32`, `true`, `nil`).
    Type,
    /// An identifier immediately followed by `(` — read as a call.
    Func,
    /// Preprocessor lines, attributes, decorators, YAML/TOML keys, HTML tag
    /// names: the "this line is about the structure" colour.
    Meta,
    /// Operators and separators.
    Punct,
    /// A diff line that adds.
    Added,
    /// A diff line that removes.
    Removed,
}

/// A classified byte range of one line. Ranges are non-overlapping, in order,
/// and always land on character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub tok: Tok,
}

/// What a line *starts* inside, which is the only thing one line needs to know
/// about the lines before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Block {
    #[default]
    None,
    /// Inside a block comment, at this nesting depth (1 for languages whose
    /// block comments do not nest).
    Comment(u8),
    /// Inside a multi-line string — the index into [`Profile::strings`].
    Str(u8),
}

/// One kind of string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// The delimiter, opening and closing (`"`, `'''`, `` ` ``).
    pub delim: &'static str,
    /// Whether `\` escapes the delimiter.
    pub escape: bool,
    /// Whether it may run past the end of a line.
    pub multiline: bool,
}

const DQ: Quote = Quote { delim: "\"", escape: true, multiline: false };
const SQ: Quote = Quote { delim: "'", escape: true, multiline: false };
/// Shell and TOML single quotes take no escapes at all — `'\'` is a backslash.
const SQ_RAW: Quote = Quote { delim: "'", escape: false, multiline: false };
const BACKTICK: Quote = Quote { delim: "`", escape: true, multiline: true };
const TRIPLE_D: Quote = Quote { delim: "\"\"\"", escape: true, multiline: true };
const TRIPLE_S: Quote = Quote { delim: "'''", escape: true, multiline: true };

/// How a whole file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Comments, strings, numbers, keywords — the great majority.
    Code,
    /// `<tag attr="value">`: the tag is the structure and the text between
    /// tags is prose.
    Markup,
    /// The prefix of the line decides the whole line.
    Diff,
    /// No rules at all. Prose, CSV, anything with no grammar worth guessing.
    Plain,
}

/// Everything the scanner needs to know about one language.
pub struct Profile {
    pub mode: Mode,
    /// Sequences that comment out the rest of the line.
    pub line_comment: &'static [&'static str],
    /// `(open, close)`, if the language has block comments.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// Whether those block comments nest (Rust, Zig-style). When false a
    /// second `/*` inside one changes nothing.
    pub nest_block: bool,
    pub strings: &'static [Quote],
    /// Keywords, **sorted is not required** — the lists are short enough that a
    /// linear scan beats anything cleverer, and they are compared once per
    /// identifier on the ~40 lines that are on screen.
    pub keywords: &'static [&'static str],
    /// Builtin types and constants.
    pub types: &'static [&'static str],
    /// Extra characters that are part of an identifier here — `-` in CSS and
    /// Lisp, `$` in shell and PHP, `!`/`?` in Ruby.
    pub ident_extra: &'static str,
    /// A line whose first non-space character is this is structural: a C
    /// preprocessor line, a Python decorator.
    pub meta_prefix: &'static [char],
    /// Numbers are worth colouring. Off for prose-ish formats where every
    /// number is content rather than a literal.
    pub numbers: bool,
}

/// The base every profile is edited from, so a new language names only what is
/// unusual about it.
const BASE: Profile = Profile {
    mode: Mode::Code,
    line_comment: &[],
    block_comment: None,
    nest_block: false,
    strings: &[DQ, SQ],
    keywords: &[],
    types: &[],
    ident_extra: "",
    meta_prefix: &[],
    numbers: true,
};

// ── The language table ──────────────────────────────────────────────────────
//
// One profile per *family*, not per name: `.cc` and `.cpp` are the same
// language and `.kt` and `.java` differ in ways this highlighter cannot see
// anyway. `profile_for` below maps every name in df-core's table onto one of
// these, and the mapping is the honest statement of what is shared.

const C_LIKE: Profile = Profile {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    keywords: &[
        "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern",
        "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static",
        "struct", "switch", "typedef", "union", "volatile", "while", "class", "namespace",
        "template", "typename", "public", "private", "protected", "virtual", "override", "new",
        "delete", "using", "try", "catch", "throw", "operator", "constexpr", "explicit", "friend",
        "mutable", "noexcept", "nullptr", "this", "@interface", "@implementation", "@end",
    ],
    types: &[
        "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void",
        "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t", "int16_t",
        "int32_t", "int64_t", "true", "false", "NULL", "nil", "id", "auto_ptr", "wchar_t",
    ],
    meta_prefix: &['#'],
    ident_extra: "@",
    ..BASE
};

const RUST: Profile = Profile {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    // Rust's block comments nest, and a preview that painted the tail of a
    // commented-out block as live code would be actively misleading.
    nest_block: true,
    strings: &[DQ, SQ],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "union", "unsafe", "use", "where", "while", "macro_rules",
    ],
    types: &[
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8",
        "u16", "u32", "u64", "u128", "usize", "String", "Vec", "Option", "Result", "Box", "Some",
        "None", "Ok", "Err", "true", "false",
    ],
    meta_prefix: &['#'],
    ..BASE
};

const JVM: Profile = Profile {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    keywords: &[
        "abstract", "as", "break", "case", "catch", "class", "companion", "const", "continue",
        "data", "def", "default", "do", "else", "enum", "extends", "extension", "final", "finally",
        "for", "fun", "func", "guard", "if", "implements", "import", "in", "init", "interface",
        "internal", "is", "lateinit", "let", "match", "new", "object", "open", "operator",
        "override", "package", "private", "protected", "public", "return", "sealed", "static",
        "struct", "super", "suspend", "switch", "synchronized", "this", "throw", "throws", "trait",
        "try", "typealias", "val", "var", "when", "where", "while", "yield", "async", "await",
    ],
    types: &[
        "Any", "Boolean", "Byte", "Char", "Double", "Float", "Int", "Integer", "Long", "Short",
        "String", "Unit", "Void", "boolean", "byte", "char", "double", "float", "int", "long",
        "short", "void", "true", "false", "null", "nil", "None", "Some",
    ],
    meta_prefix: &['@'],
    ident_extra: "@",
    ..BASE
};

const GO_ZIG: Profile = Profile {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &[DQ, SQ, BACKTICK],
    keywords: &[
        "break", "case", "chan", "comptime", "const", "continue", "default", "defer", "else",
        "errdefer", "fallthrough", "fn", "for", "func", "go", "goto", "if", "import", "inline",
        "interface", "map", "orelse", "package", "pub", "range", "return", "select", "struct",
        "switch", "test", "try", "type", "union", "unreachable", "var", "while",
    ],
    types: &[
        "any", "anytype", "bool", "byte", "c_int", "comptime_int", "error", "f32", "f64", "float32",
        "float64", "i8", "i16", "i32", "i64", "int", "int8", "int16", "int32", "int64", "rune",
        "string", "u8", "u16", "u32", "u64", "uint", "uintptr", "usize", "void", "true", "false",
        "nil", "null", "undefined",
    ],
    ..BASE
};

const JS: Profile = Profile {
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &[DQ, SQ, BACKTICK],
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
        "debugger", "declare", "default", "delete", "do", "else", "enum", "export", "extends",
        "finally", "for", "from", "function", "get", "if", "implements", "import", "in",
        "instanceof", "interface", "keyof", "let", "namespace", "new", "of", "private",
        "protected", "public", "readonly", "return", "satisfies", "set", "static", "super",
        "switch", "this", "throw", "try", "type", "typeof", "var", "void", "while", "with", "yield",
    ],
    types: &[
        "any", "bigint", "boolean", "never", "null", "number", "object", "string", "symbol",
        "undefined", "unknown", "true", "false", "Array", "Map", "Promise", "Set", "Object",
    ],
    ident_extra: "$",
    ..BASE
};

const PYTHON: Profile = Profile {
    line_comment: &["#"],
    // Triple quotes first: the scanner takes the first delimiter that matches
    // at the cursor, so `"""` has to be tried before `"`.
    strings: &[TRIPLE_D, TRIPLE_S, DQ, SQ],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
        "with", "yield",
    ],
    types: &[
        "None", "True", "False", "bool", "bytes", "dict", "float", "frozenset", "int", "list",
        "object", "set", "str", "tuple", "self", "cls",
    ],
    meta_prefix: &['@'],
    ..BASE
};

const RUBY: Profile = Profile {
    line_comment: &["#"],
    keywords: &[
        "alias", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
        "end", "ensure", "for", "if", "in", "module", "next", "nil", "not", "or", "and", "redo",
        "rescue", "retry", "return", "self", "super", "then", "undef", "unless", "until", "when",
        "while", "yield", "require", "require_relative", "attr_accessor", "attr_reader",
    ],
    types: &["true", "false", "nil", "Array", "Hash", "String", "Symbol", "Integer", "Float"],
    ident_extra: "?!@$",
    ..BASE
};

/// Perl, R, Julia, Elixir, Nix and the other hash-comment scripting languages
/// that are not worth a table of their own in a preview pane. The keywords are
/// the union of the small set each of them shares, which colours the control
/// flow of all of them and over-colours none of them badly.
const HASH_SCRIPT: Profile = Profile {
    line_comment: &["#"],
    block_comment: Some(("=begin", "=end")),
    keywords: &[
        "and", "begin", "break", "case", "catch", "cond", "const", "continue", "def", "defmodule",
        "defp", "do", "else", "elseif", "elsif", "end", "export", "for", "function", "global", "if",
        "import", "in", "inherit", "let", "local", "macro", "module", "mutable", "my", "next",
        "not", "or", "package", "quote", "raise", "rec", "repeat", "require", "rescue", "return",
        "struct", "sub", "then", "try", "type", "unless", "until", "use", "using", "when", "while",
        "with",
    ],
    types: &["true", "false", "nil", "null", "NULL", "NA", "Inf", "NaN", "TRUE", "FALSE", "Nothing"],
    ident_extra: "$@%!?",
    ..BASE
};

const SHELL: Profile = Profile {
    line_comment: &["#"],
    strings: &[DQ, SQ_RAW, BACKTICK],
    keywords: &[
        "alias", "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac",
        "eval", "exec", "exit", "export", "fi", "for", "function", "if", "in", "local", "read",
        "readonly", "return", "select", "set", "shift", "source", "then", "trap", "unset", "until",
        "while", "end", "and", "or", "not", "begin", "switch",
    ],
    types: &["true", "false"],
    ident_extra: "$-",
    ..BASE
};

const POWERSHELL: Profile = Profile {
    line_comment: &["#"],
    block_comment: Some(("<#", "#>")),
    keywords: &[
        "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else",
        "elseif", "end", "exit", "filter", "finally", "for", "foreach", "function", "if", "in",
        "param", "process", "return", "switch", "throw", "trap", "try", "until", "using", "while",
    ],
    types: &["$true", "$false", "$null"],
    ident_extra: "$-",
    ..BASE
};

const LUA: Profile = Profile {
    line_comment: &["--"],
    block_comment: Some(("--[[", "]]")),
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    types: &["true", "false", "nil", "self"],
    ..BASE
};

/// Haskell, OCaml, Clojure and the other functional families. Their comment
/// syntaxes differ (`--`, `(* *)`, `;`) and all three are listed, which costs
/// nothing: none of the three sequences is legal code in the others.
const FUNCTIONAL: Profile = Profile {
    line_comment: &["--", ";;", ";", "#"],
    block_comment: Some(("{-", "-}")),
    keywords: &[
        "and", "as", "begin", "case", "class", "data", "def", "defn", "deriving", "do", "else",
        "end", "exception", "external", "fn", "for", "fun", "function", "functor", "if", "import",
        "in", "infix", "instance", "let", "loop", "match", "module", "mutable", "newtype", "ns",
        "of", "open", "or", "rec", "recur", "sig", "struct", "then", "type", "val", "when",
        "where", "with",
    ],
    types: &["Bool", "Char", "Double", "Float", "Int", "Integer", "String", "True", "False", "true", "false", "nil", "unit"],
    ident_extra: "-'*?!",
    ..BASE
};

const TOML_INI: Profile = Profile {
    line_comment: &["#", ";"],
    strings: &[TRIPLE_D, TRIPLE_S, DQ, SQ_RAW],
    types: &["true", "false"],
    ident_extra: "-.",
    // `[section]` headers, which is the structure a person scans a config for.
    meta_prefix: &['['],
    ..BASE
};

const YAML: Profile = Profile {
    line_comment: &["#"],
    strings: &[DQ, SQ_RAW],
    types: &["true", "false", "null", "yes", "no", "on", "off", "~"],
    ident_extra: "-.",
    ..BASE
};

const JSON: Profile = Profile {
    strings: &[DQ],
    types: &["true", "false", "null"],
    ..BASE
};

const MARKUP: Profile = Profile {
    mode: Mode::Markup,
    block_comment: Some(("<!--", "-->")),
    strings: &[DQ, SQ],
    ident_extra: "-:.",
    numbers: false,
    ..BASE
};

const CSS: Profile = Profile {
    block_comment: Some(("/*", "*/")),
    keywords: &[
        "@media", "@supports", "@import", "@keyframes", "@font-face", "@layer", "@container",
        "@charset", "@page", "@property", "and", "not", "only", "from", "to",
    ],
    types: &["inherit", "initial", "unset", "revert", "none", "auto", "currentColor", "transparent"],
    ident_extra: "-@#.%",
    ..BASE
};

const SQL: Profile = Profile {
    line_comment: &["--", "#"],
    block_comment: Some(("/*", "*/")),
    strings: &[SQ, DQ],
    keywords: &[
        "ALTER", "AND", "AS", "ASC", "BEGIN", "BETWEEN", "BY", "CASE", "COMMIT", "CREATE", "CROSS",
        "DELETE", "DESC", "DISTINCT", "DROP", "ELSE", "END", "EXISTS", "FROM", "FULL", "GROUP",
        "HAVING", "IN", "INDEX", "INNER", "INSERT", "INTO", "IS", "JOIN", "LEFT", "LIKE", "LIMIT",
        "NOT", "OFFSET", "ON", "OR", "ORDER", "OUTER", "RIGHT", "ROLLBACK", "SELECT", "SET",
        "TABLE", "THEN", "UNION", "UPDATE", "VALUES", "VIEW", "WHEN", "WHERE", "WITH",
    ],
    types: &[
        "BIGINT", "BOOLEAN", "BLOB", "CHAR", "DATE", "DECIMAL", "DOUBLE", "FLOAT", "INT",
        "INTEGER", "JSON", "NULL", "NUMERIC", "REAL", "SERIAL", "SMALLINT", "TEXT", "TIMESTAMP",
        "TRUE", "FALSE", "UUID", "VARCHAR",
    ],
    ..BASE
};

/// Makefiles, Dockerfiles, CMake, protobuf, GraphQL, G-code — hash comments and
/// a set of leading verbs. The verbs are what a person looks for in all six.
const DIRECTIVE: Profile = Profile {
    line_comment: &["#", ";", "//"],
    block_comment: Some(("\"\"\"", "\"\"\"")),
    keywords: &[
        "ADD", "ARG", "CMD", "COPY", "ENTRYPOINT", "ENV", "EXPOSE", "FROM", "HEALTHCHECK", "LABEL",
        "RUN", "SHELL", "STOPSIGNAL", "USER", "VOLUME", "WORKDIR", "add_executable", "add_library",
        "else", "endif", "endforeach", "find_package", "foreach", "if", "include", "install",
        "message", "option", "project", "set", "target_link_libraries", "define", "ifeq", "ifdef",
        "ifneq", "endef", "export", "enum", "extend", "import", "message", "oneof", "optional",
        "package", "repeated", "required", "returns", "rpc", "service", "syntax", "fragment",
        "input", "interface", "mutation", "query", "scalar", "schema", "subscription", "type",
        "union", "on",
    ],
    types: &[
        "bool", "bytes", "double", "fixed32", "fixed64", "float", "int32", "int64", "sint32",
        "sint64", "string", "uint32", "uint64", "Boolean", "Float", "ID", "Int", "String", "true",
        "false", "ON", "OFF",
    ],
    ident_extra: "$@_.",
    ..BASE
};

const LATEX: Profile = Profile {
    line_comment: &["%"],
    strings: &[],
    keywords: &[],
    ident_extra: "\\@",
    numbers: false,
    ..BASE
};

const DIFF: Profile = Profile {
    mode: Mode::Diff,
    ..BASE
};

const PLAIN: Profile = Profile {
    mode: Mode::Plain,
    numbers: false,
    ..BASE
};

/// The language name df-core settled on → the rules for reading it.
///
/// `None`, and any name with no entry, is prose: [`Mode::Plain`], which paints
/// every line in one colour. That is the right answer for a `LICENSE` and it is
/// also the graceful degradation for a language added to df-core's table before
/// it is added here.
pub fn profile_for(syntax: Option<&str>) -> &'static Profile {
    let Some(name) = syntax else { return &PLAIN };
    match name {
        "Rust" => &RUST,
        "C" | "C++" | "Objective-C" | "Objective-C++" => &C_LIKE,
        "C#" | "Java" | "Kotlin" | "Scala" | "Dart" | "Groovy" | "Swift" => &JVM,
        "Go" | "Zig" => &GO_ZIG,
        "JavaScript" | "TypeScript" => &JS,
        "Python" => &PYTHON,
        "Ruby" => &RUBY,
        // PHP is closer to C than to the hash-comment scripts, and its `$` is
        // already an identifier character in the JS profile.
        "PHP" => &JS,
        "Perl" | "R" | "Julia" | "Elixir" | "Erlang" | "Nix" => &HASH_SCRIPT,
        "Shell" | "Fish" => &SHELL,
        "PowerShell" => &POWERSHELL,
        "Lua" => &LUA,
        "Haskell" | "OCaml" | "Clojure" => &FUNCTIONAL,
        "TOML" | "INI" => &TOML_INI,
        "YAML" => &YAML,
        "JSON" => &JSON,
        // Svelte and Vue files are markup with a `<script>` in them; the
        // markup half is the bulk and the half a glance is reading.
        "HTML" | "XML" | "Svelte" | "Vue" => &MARKUP,
        "CSS" => &CSS,
        "SQL" => &SQL,
        "Diff" => &DIFF,
        "Makefile" | "Dockerfile" | "CMake" | "Protocol Buffer" | "GraphQL" | "G-code" => {
            &DIRECTIVE
        }
        "LaTeX" | "Typst" => &LATEX,
        _ => &PLAIN,
    }
}

/// Where every line of `lines` starts, given `profile`.
///
/// One pass over the file when the preview arrives, so that scrolling later
/// costs only the visible lines. Allocates one byte per line and no tokens.
pub fn block_states(lines: &[String], profile: &Profile) -> Vec<Block> {
    let mut states = Vec::with_capacity(lines.len());
    let mut state = Block::None;
    for line in lines {
        states.push(state);
        state = scan_line(line, profile, state, None);
    }
    states
}

/// Tokenise one line.
///
/// Returns the [`Block`] the *next* line starts in. When `out` is `Some`, the
/// spans are appended to it (it is not cleared — the caller owns it, and reusing
/// one buffer per frame is why this takes a `&mut Vec` rather than returning
/// one).
pub fn scan_line(
    line: &str,
    p: &Profile,
    state: Block,
    mut out: Option<&mut Vec<Span>>,
) -> Block {
    match p.mode {
        Mode::Plain => Block::None,
        Mode::Diff => {
            if let Some(out) = out.as_deref_mut() {
                let tok = diff_tok(line);
                if tok != Tok::Text && !line.is_empty() {
                    out.push(Span { start: 0, end: line.len(), tok });
                }
            }
            Block::None
        }
        Mode::Code | Mode::Markup => scan_code(line, p, state, out),
    }
}

/// A diff line's colour is decided entirely by its first character — that is
/// what makes a diff readable, and any deeper reading of it would be wrong for
/// half the diffs in the world (a `-` in a shell script inside a `+` line).
fn diff_tok(line: &str) -> Tok {
    match line.as_bytes().first() {
        // A file header (`+++`/`---`) is structure, not a change.
        Some(b'+') if line.starts_with("+++") => Tok::Meta,
        Some(b'-') if line.starts_with("---") => Tok::Meta,
        Some(b'+') => Tok::Added,
        Some(b'-') => Tok::Removed,
        Some(b'@') => Tok::Meta,
        Some(b'd') if line.starts_with("diff ") => Tok::Meta,
        Some(b'i') if line.starts_with("index ") => Tok::Meta,
        _ => Tok::Text,
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Operators and separators, as one string so the check is a `contains`.
const PUNCT: &str = "+-*/%=<>!&|^~?:;,.(){}[]";

fn scan_code(line: &str, p: &Profile, state: Block, mut out: Option<&mut Vec<Span>>) -> Block {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let push = |out: &mut Option<&mut Vec<Span>>, start: usize, end: usize, tok: Tok| {
        if start >= end {
            return;
        }
        if let Some(spans) = out.as_deref_mut() {
            spans.push(Span { start, end, tok });
        }
    };

    // ── Finish whatever the previous line left open ─────────────────────────
    match state {
        Block::Comment(depth) => {
            let (open, close) = match p.block_comment {
                Some(pair) => pair,
                None => return Block::None,
            };
            let (end, depth) = close_block(line, 0, open, close, depth, p.nest_block);
            push(&mut out, 0, end.min(line.len()), Tok::Comment);
            if depth > 0 {
                return Block::Comment(depth);
            }
            i = end;
        }
        Block::Str(k) => {
            let Some(quote) = p.strings.get(k as usize) else {
                return Block::None;
            };
            match string_end(line, 0, quote) {
                Some(end) => {
                    push(&mut out, 0, end, Tok::Str);
                    i = end;
                }
                None => {
                    push(&mut out, 0, line.len(), Tok::Str);
                    return Block::Str(k);
                }
            }
        }
        Block::None => {}
    }

    // ── A whole-line structural prefix ──────────────────────────────────────
    // Checked once, at the start: a `#` that is the first thing on a C line is
    // a preprocessor directive, and a `#` in the middle of one is not.
    if i == 0 && !p.meta_prefix.is_empty() {
        let trimmed = line.trim_start();
        if let Some(first) = trimmed.chars().next() {
            if p.meta_prefix.contains(&first) {
                // Not for a line comment that happens to start with the same
                // character — `#` opens a Python comment *and* a decorator is
                // `@`, but shell has no meta prefix at all, so this only bites
                // where the two genuinely collide.
                let is_comment = p.line_comment.iter().any(|c| trimmed.starts_with(c));
                if !is_comment {
                    push(&mut out, 0, line.len(), Tok::Meta);
                    return Block::None;
                }
            }
        }
    }

    // ── The body ────────────────────────────────────────────────────────────
    let mut markup_in_tag = false;
    let mut markup_seen_name = false;
    while i < bytes.len() {
        let rest = &line[i..];

        // Comments first: everything else can appear inside one.
        if let Some(c) = p.line_comment.iter().find(|c| rest.starts_with(**c)) {
            let _ = c;
            push(&mut out, i, line.len(), Tok::Comment);
            return Block::None;
        }
        if let Some((open, close)) = p.block_comment {
            if rest.starts_with(open) {
                let (end, depth) =
                    close_block(line, i + open.len(), open, close, 1, p.nest_block);
                push(&mut out, i, end.min(line.len()), Tok::Comment);
                if depth > 0 {
                    return Block::Comment(depth);
                }
                i = end;
                continue;
            }
        }

        // Strings.
        if let Some((k, quote)) = p
            .strings
            .iter()
            .enumerate()
            .find(|(_, q)| rest.starts_with(q.delim))
        {
            let from = i + quote.delim.len();
            match string_end(line, from, quote) {
                Some(end) => {
                    push(&mut out, i, end, Tok::Str);
                    i = end;
                    continue;
                }
                None if quote.multiline => {
                    push(&mut out, i, line.len(), Tok::Str);
                    return Block::Str(k as u8);
                }
                // An unterminated single-line string: colour the rest and stop,
                // rather than reading the remainder of the line as code. A
                // half-typed line in an editor looks like this constantly.
                None => {
                    push(&mut out, i, line.len(), Tok::Str);
                    return Block::None;
                }
            }
        }

        let b = bytes[i];

        // Markup: `<`, the tag name, then attribute names until `>`.
        if p.mode == Mode::Markup {
            if b == b'<' {
                markup_in_tag = true;
                markup_seen_name = false;
                push(&mut out, i, i + 1, Tok::Punct);
                i += 1;
                // `</` and `<?` and `<!` are part of the opening.
                while i < bytes.len() && matches!(bytes[i], b'/' | b'?' | b'!') {
                    push(&mut out, i, i + 1, Tok::Punct);
                    i += 1;
                }
                continue;
            }
            if b == b'>' {
                markup_in_tag = false;
                push(&mut out, i, i + 1, Tok::Punct);
                i += 1;
                continue;
            }
            if !markup_in_tag {
                // Text between tags is prose; walk to the next `<`.
                let end = rest.find('<').map(|n| i + n).unwrap_or(bytes.len());
                i = end.max(i + 1);
                continue;
            }
        }

        // Numbers, but only when not in the middle of an identifier
        // (`utf8_len` must not colour its `8`).
        if p.numbers && b.is_ascii_digit() && (i == 0 || !is_ident(bytes[i - 1])) {
            let end = number_end(bytes, i);
            push(&mut out, i, end, Tok::Number);
            i = end;
            continue;
        }

        // Identifiers.
        if is_ident_start(b) || p.ident_extra.contains(b as char) {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (is_ident(bytes[i]) || p.ident_extra.contains(bytes[i] as char))
            {
                i += 1;
            }
            let word = &line[start..i];
            let tok = if p.mode == Mode::Markup {
                if markup_seen_name {
                    Tok::Type
                } else {
                    markup_seen_name = true;
                    Tok::Meta
                }
            } else if p.keywords.contains(&word) {
                Tok::Keyword
            } else if p.types.contains(&word) {
                Tok::Type
            } else if bytes.get(i) == Some(&b'(') {
                Tok::Func
            } else {
                Tok::Text
            };
            push(&mut out, start, i, tok);
            continue;
        }

        if PUNCT.contains(b as char) {
            push(&mut out, i, i + 1, Tok::Punct);
            i += 1;
            continue;
        }

        // Anything else — whitespace, an unhandled byte — is plain, and the
        // cursor has to advance by a whole character or a multi-byte one would
        // be split across two spans.
        i += char_len(bytes[i]);
    }

    Block::None
}

/// How many bytes the UTF-8 character starting with `b` occupies. Never zero,
/// so a malformed byte cannot stall the scan.
fn char_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Walk from `from` to the end of a block comment, tracking nesting.
/// Returns `(byte index just past the close, depth still open)`.
fn close_block(
    line: &str,
    from: usize,
    open: &str,
    close: &str,
    mut depth: u8,
    nest: bool,
) -> (usize, u8) {
    let mut i = from.min(line.len());
    while i < line.len() {
        let rest = &line[i..];
        if rest.starts_with(close) {
            depth = depth.saturating_sub(1);
            i += close.len();
            if depth == 0 {
                return (i, 0);
            }
            continue;
        }
        if nest && rest.starts_with(open) {
            depth = depth.saturating_add(1);
            i += open.len();
            continue;
        }
        i += char_len(line.as_bytes()[i]);
    }
    (line.len(), depth)
}

/// Where a string that opened at `from` ends, or `None` if it runs off the end
/// of the line. The returned index is just past the closing delimiter.
fn string_end(line: &str, from: usize, quote: &Quote) -> Option<usize> {
    let mut i = from.min(line.len());
    while i < line.len() {
        let rest = &line[i..];
        if quote.escape && rest.starts_with('\\') {
            // A trailing backslash escapes the newline; the string continues.
            i += 1 + line.get(i + 1..).and_then(|r| r.chars().next()).map_or(0, char::len_utf8);
            continue;
        }
        if rest.starts_with(quote.delim) {
            return Some(i + quote.delim.len());
        }
        i += char_len(line.as_bytes()[i]);
    }
    None
}

/// Where the number starting at `from` ends. Deliberately generous — `0xFF`,
/// `1_000`, `1.5e-3`, `10px` — because a preview wants the *extent* of the
/// literal, not a validator.
fn number_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        let exponent = matches!(b, b'+' | b'-')
            && i > from
            && matches!(bytes[i - 1] | 0x20, b'e' | b'p');
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || exponent {
            // A `.` that is not followed by a digit is a method call, not part
            // of the number: `1.max(2)`.
            if b == b'.' && !bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                break;
            }
            i += 1;
        } else {
            break;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(line: &str, syntax: Option<&str>) -> Vec<(String, Tok)> {
        let p = profile_for(syntax);
        let mut out = Vec::new();
        scan_line(line, p, Block::None, Some(&mut out));
        out.iter()
            .map(|s| (line[s.start..s.end].to_string(), s.tok))
            .collect()
    }

    /// Every span is inside the line, in order, non-overlapping and on a
    /// character boundary. This is the invariant the painter relies on, and it
    /// must hold for *every* profile against text that is not its language.
    #[test]
    fn spans_are_always_well_formed() {
        let lines = [
            "fn main() { println!(\"héllo, 世界\"); } // done",
            "#include <stdio.h> /* a /* nested */ comment */",
            "  - key: \"value\" # trailing",
            "SELECT * FROM t WHERE a = 'b' -- note",
            "<div class=\"x\">text &amp; more</div>",
            "@@ -1,4 +1,6 @@",
            "\"\"\"docstring",
            "x = 0xFF + 1_000.5e-3",
            "",
            "\\\\",
            "日本語のテキスト",
        ];
        let names = [
            None,
            Some("Rust"),
            Some("C"),
            Some("Python"),
            Some("YAML"),
            Some("SQL"),
            Some("HTML"),
            Some("Diff"),
            Some("Shell"),
            Some("JSON"),
            Some("Lua"),
            Some("Makefile"),
        ];
        for name in names {
            let p = profile_for(name);
            let mut state = Block::None;
            for line in lines {
                let mut out = Vec::new();
                state = scan_line(line, p, state, Some(&mut out));
                let mut last = 0;
                for s in &out {
                    assert!(s.start >= last, "{name:?} {line:?}: {s:?} overlaps");
                    assert!(s.start < s.end, "{name:?} {line:?}: empty {s:?}");
                    assert!(s.end <= line.len(), "{name:?} {line:?}: {s:?} past end");
                    assert!(line.is_char_boundary(s.start) && line.is_char_boundary(s.end));
                    last = s.end;
                }
            }
        }
    }

    #[test]
    fn rust_keywords_strings_numbers_and_comments() {
        let got = spans("let x = \"hi\"; // note", Some("Rust"));
        assert!(got.contains(&("let".into(), Tok::Keyword)));
        assert!(got.contains(&("\"hi\"".into(), Tok::Str)));
        assert!(got.contains(&("// note".into(), Tok::Comment)));

        let got = spans("const N: u32 = 1_000;", Some("Rust"));
        assert!(got.contains(&("u32".into(), Tok::Type)));
        assert!(got.contains(&("1_000".into(), Tok::Number)));
    }

    /// The `//` inside a string is not a comment — the single most visible bug
    /// a naive line-based highlighter has.
    #[test]
    fn a_comment_marker_inside_a_string_is_not_a_comment() {
        let got = spans("let url = \"https://example.com\"; // real", Some("Rust"));
        assert!(got.contains(&("\"https://example.com\"".into(), Tok::Str)));
        assert!(got.contains(&("// real".into(), Tok::Comment)));
        assert_eq!(
            got.iter().filter(|(_, t)| *t == Tok::Comment).count(),
            1,
            "the URL's slashes opened a comment"
        );
    }

    /// …and neither is a quote inside a comment the start of a string.
    #[test]
    fn a_quote_inside_a_comment_opens_nothing() {
        let p = profile_for(Some("Rust"));
        let state = scan_line("// it's fine", p, Block::None, None);
        assert_eq!(state, Block::None);
    }

    #[test]
    fn block_comments_carry_across_lines_and_rust_nests_them() {
        let p = profile_for(Some("Rust"));
        let lines: Vec<String> = ["/* one", "two /* deeper", "*/ still", "*/ code"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let states = block_states(&lines, p);
        assert_eq!(states[0], Block::None);
        assert_eq!(states[1], Block::Comment(1));
        assert_eq!(states[2], Block::Comment(2));
        assert_eq!(states[3], Block::Comment(1));
        // …and after the last line the comment is closed.
        assert_eq!(scan_line(&lines[3], p, states[3], None), Block::None);

        // C's do not nest: the first `*/` closes it.
        let c = profile_for(Some("C"));
        let states = block_states(&lines, c);
        assert_eq!(states[2], Block::Comment(1));
        assert_eq!(scan_line(&lines[2], c, states[2], None), Block::None);
    }

    #[test]
    fn python_triple_quoted_strings_span_lines() {
        let p = profile_for(Some("Python"));
        let lines: Vec<String> = ["x = \"\"\"start", "middle", "end\"\"\"", "y = 1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let states = block_states(&lines, p);
        assert_eq!(states[1], Block::Str(0));
        assert_eq!(states[2], Block::Str(0));
        assert_eq!(states[3], Block::None);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let got = spans(r#"s = "a\"b" + 1"#, Some("Rust"));
        assert!(got.contains(&(r#""a\"b""#.into(), Tok::Str)));
        assert!(got.contains(&("1".into(), Tok::Number)));
    }

    /// Shell single quotes take no escapes at all, so `'\'` ends the string.
    #[test]
    fn shell_single_quotes_are_raw() {
        let got = spans(r"echo 'a\' b", Some("Shell"));
        assert!(got.contains(&(r"'a\'".into(), Tok::Str)), "{got:?}");
    }

    #[test]
    fn a_diff_is_coloured_by_its_first_character() {
        assert_eq!(spans("+added", Some("Diff")), [("+added".into(), Tok::Added)]);
        assert_eq!(
            spans("-removed", Some("Diff")),
            [("-removed".into(), Tok::Removed)]
        );
        assert_eq!(spans("@@ -1 +1 @@", Some("Diff")), [("@@ -1 +1 @@".into(), Tok::Meta)]);
        assert_eq!(spans("+++ b/x", Some("Diff")), [("+++ b/x".into(), Tok::Meta)]);
        assert_eq!(spans(" context", Some("Diff")), []);
    }

    #[test]
    fn markup_separates_the_tag_from_its_attributes() {
        let got = spans("<a href=\"/x\">text</a>", Some("HTML"));
        assert!(got.contains(&("a".into(), Tok::Meta)), "{got:?}");
        assert!(got.contains(&("href".into(), Tok::Type)), "{got:?}");
        assert!(got.contains(&("\"/x\"".into(), Tok::Str)), "{got:?}");
        // The prose between the tags is not tokenised at all.
        assert!(!got.iter().any(|(text, _)| text == "text"));
    }

    #[test]
    fn a_c_preprocessor_line_is_structure() {
        assert_eq!(
            spans("#include <stdio.h>", Some("C")),
            [("#include <stdio.h>".into(), Tok::Meta)]
        );
        // …but a Python comment starting with the same character is a comment.
        assert_eq!(
            spans("# a note", Some("Python")),
            [("# a note".into(), Tok::Comment)]
        );
    }

    #[test]
    fn a_call_is_told_from_a_name() {
        let got = spans("value = compute(x)", Some("Python"));
        assert!(got.contains(&("compute".into(), Tok::Func)), "{got:?}");
        assert!(got.contains(&("value".into(), Tok::Text)), "{got:?}");
    }

    /// A number embedded in an identifier is part of the name.
    #[test]
    fn a_digit_inside_a_name_is_not_a_number() {
        let got = spans("utf8_len = 3", Some("Rust"));
        assert!(!got.iter().any(|(t, k)| t == "8" && *k == Tok::Number));
        assert!(got.contains(&("3".into(), Tok::Number)));
    }

    /// `1.max(2)` is a method call on `1`, not the number `1.max`.
    #[test]
    fn a_dot_that_is_not_a_decimal_point_ends_the_number() {
        let got = spans("1.max(2)", Some("Rust"));
        assert!(got.contains(&("1".into(), Tok::Number)), "{got:?}");
        assert!(got.contains(&("max".into(), Tok::Func)), "{got:?}");
        // …while a real decimal keeps its fraction.
        assert!(spans("1.5", Some("Rust")).contains(&("1.5".into(), Tok::Number)));
    }

    /// An unknown language, and no language at all, must produce no spans
    /// rather than guessing.
    #[test]
    fn prose_is_left_alone() {
        assert_eq!(spans("Copyright (c) 2026 nobody", None), []);
        assert_eq!(spans("fn main() {}", Some("Klingon")), []);
    }

    /// Every name in df-core's table resolves to a profile — this is the test
    /// that catches a language added there and forgotten here (it degrades to
    /// prose, which is fine, but the list below says which ones do).
    #[test]
    fn every_named_language_has_a_profile() {
        let highlighted = [
            "Rust", "C", "C++", "C#", "Java", "Kotlin", "Go", "Zig", "Swift", "JavaScript",
            "TypeScript", "Python", "Ruby", "PHP", "Shell", "Lua", "TOML", "YAML", "JSON", "HTML",
            "XML", "CSS", "SQL", "Diff", "Makefile", "Dockerfile",
        ];
        for name in highlighted {
            assert_ne!(
                profile_for(Some(name)).mode,
                Mode::Plain,
                "{name} lost its profile"
            );
        }
    }
}
