//! Which language a text file is written in — a name for the highlighter.
//!
//! PLAN §6 wants "text with syntax highlighting (tmTheme from the vendored
//! catppuccin flavor)". The highlighter itself lives in df-app, because it
//! owns fonts and colours; what df-core owes it is the one fact it cannot
//! guess, which is *which grammar to load*. That is a table from a file name
//! to a language name, and a table is the entire implementation.
//!
//! ## Why a name and not an enum
//!
//! The names here are the ones syntax definitions are actually filed under
//! ("Rust", "C++", "Shell") rather than an enum df-core would have to grow a
//! variant on every time a language exists. df-app matches on the string, and
//! a language with no grammar installed degrades to plain text — which is what
//! `None` already means, so the two failure paths are one path.
//!
//! ## Why by name and not by mime
//!
//! [`crate::fs::mime`] collapses families on purpose: `.c`, `.h`, `.cc` and
//! `.cpp` all end at `text/x-c` or `text/x-c++`, and `.yml`/`.yaml` are one
//! type. That is right for icons and openers and wrong here, where the whole
//! job is telling `.h` from `.hpp`. So this reads the name, like the
//! highlighter would.

/// extension → language, lowercase. Sorted the way the mime table is: by
/// family, so a missing entry is obvious from the neighbours.
const SYNTAX: &[(&str, &str)] = &[
    // Systems.
    ("c", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("cxx", "C++"),
    ("h", "C"),
    ("hh", "C++"),
    ("hpp", "C++"),
    ("rs", "Rust"),
    ("zig", "Zig"),
    ("go", "Go"),
    ("swift", "Swift"),
    ("m", "Objective-C"),
    ("mm", "Objective-C++"),
    // Managed.
    ("cs", "C#"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("scala", "Scala"),
    ("dart", "Dart"),
    // Scripting.
    ("js", "JavaScript"),
    ("cjs", "JavaScript"),
    ("mjs", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("py", "Python"),
    ("pyi", "Python"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("lua", "Lua"),
    ("pl", "Perl"),
    ("r", "R"),
    ("jl", "Julia"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("hs", "Haskell"),
    ("ml", "OCaml"),
    ("clj", "Clojure"),
    ("nix", "Nix"),
    // Shell and config. `.env`, `.conf` and friends are close enough to shell
    // to highlight as it and wrong in no way anybody notices.
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("zsh", "Shell"),
    ("fish", "Fish"),
    ("ps1", "PowerShell"),
    ("env", "Shell"),
    ("conf", "Shell"),
    ("cfg", "INI"),
    ("ini", "INI"),
    ("toml", "TOML"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("json", "JSON"),
    ("jsonc", "JSON"),
    ("jsonl", "JSON"),
    ("ndjson", "JSON"),
    // Markup and web.
    ("html", "HTML"),
    ("htm", "HTML"),
    ("xml", "XML"),
    ("svg", "XML"),
    ("svelte", "Svelte"),
    ("vue", "Vue"),
    ("css", "CSS"),
    ("md", "Markdown"),
    ("markdown", "Markdown"),
    ("mdx", "Markdown"),
    ("rst", "reStructuredText"),
    ("tex", "LaTeX"),
    ("typ", "Typst"),
    // Data and build.
    ("sql", "SQL"),
    ("csv", "CSV"),
    ("tsv", "CSV"),
    ("diff", "Diff"),
    ("patch", "Diff"),
    ("proto", "Protocol Buffer"),
    ("graphql", "GraphQL"),
    ("gql", "GraphQL"),
    ("cmake", "CMake"),
    ("gradle", "Groovy"),
    ("bzl", "Python"),
    ("gcode", "G-code"),
    ("gco", "G-code"),
];

/// Whole names with no extension, and the dotfiles whose "extension" is really
/// their whole name. `.gitignore` is not a file of type `gitignore`.
const SYNTAX_NAMES: &[(&str, &str)] = &[
    ("makefile", "Makefile"),
    ("gnumakefile", "Makefile"),
    ("dockerfile", "Dockerfile"),
    ("containerfile", "Dockerfile"),
    ("justfile", "Makefile"),
    ("cmakelists.txt", "CMake"),
    ("cargo.lock", "TOML"),
    (".gitignore", "Shell"),
    (".gitattributes", "Shell"),
    (".dockerignore", "Shell"),
    (".env", "Shell"),
    (".bashrc", "Shell"),
    (".zshrc", "Shell"),
    (".profile", "Shell"),
    (".editorconfig", "INI"),
];

/// Interpreters worth recognising from a shebang, for the scripts that carry
/// no extension at all — `deploy`, `configure`, `pre-commit`. Matched against
/// the last path component of the interpreter, so `/usr/bin/env python3` and
/// `/usr/local/bin/python3.12` both land on Python.
const SHEBANGS: &[(&str, &str)] = &[
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("zsh", "Shell"),
    ("dash", "Shell"),
    ("fish", "Fish"),
    ("python", "Python"),
    ("ruby", "Ruby"),
    ("perl", "Perl"),
    ("node", "JavaScript"),
    ("deno", "TypeScript"),
    ("lua", "Lua"),
];

/// The language `name` is written in, or `None` for "highlight it as prose".
pub fn syntax_for_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, lang)) = SYNTAX_NAMES.iter().find(|(n, _)| *n == lower) {
        return Some(lang);
    }
    // A leading dot marks a hidden file, it does not introduce an extension —
    // the same rule [`crate::fs::mime::hint_for_name`] follows, for the same
    // reason.
    let stem = lower.strip_prefix('.').unwrap_or(&lower);
    let (_, ext) = stem.rsplit_once('.')?;
    SYNTAX
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, lang)| *lang)
}

/// The language a `#!` line names, for files whose name says nothing.
///
/// Reads only the first line, and only when it starts with `#!` — a `#` alone
/// is a comment in half the languages here and means nothing about which.
pub fn syntax_for_shebang(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"#!") {
        return None;
    }
    let line = head.split(|b| *b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    // `#!/usr/bin/env -S python3 -u` — take the last word that looks like an
    // interpreter rather than the first, so `env` never wins.
    for word in line[2..].split_whitespace().rev() {
        let base = word.rsplit('/').next().unwrap_or(word);
        // Strip a trailing version: `python3.12` → `python`.
        let base = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        if let Some((_, lang)) = SHEBANGS.iter().find(|(n, _)| *n == base) {
            return Some(lang);
        }
    }
    None
}

/// The language for a file, name first and shebang second.
pub fn syntax_for(name: &str, head: &[u8]) -> Option<&'static str> {
    syntax_for_name(name).or_else(|| syntax_for_shebang(head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_name_their_language() {
        let cases = [
            ("main.rs", Some("Rust")),
            ("App.svelte", Some("Svelte")),
            ("index.TSX", Some("TypeScript")),
            ("styles.css", Some("CSS")),
            ("notes.md", Some("Markdown")),
            ("Cargo.toml", Some("TOML")),
            ("part.gcode", Some("G-code")),
            ("query.sql", Some("SQL")),
            ("change.patch", Some("Diff")),
            ("photo.jpg", None),
            ("noextension", None),
        ];
        for (name, expected) in cases {
            assert_eq!(syntax_for_name(name), expected, "for {name}");
        }
    }

    #[test]
    fn whole_names_and_dotfiles_are_not_extensions() {
        assert_eq!(syntax_for_name("Makefile"), Some("Makefile"));
        assert_eq!(syntax_for_name("Dockerfile"), Some("Dockerfile"));
        assert_eq!(syntax_for_name("CMakeLists.txt"), Some("CMake"));
        assert_eq!(syntax_for_name(".zshrc"), Some("Shell"));
        assert_eq!(syntax_for_name(".editorconfig"), Some("INI"));
        // `.rs` as a whole name is a hidden file called `rs`, not Rust — the
        // dot is the hidden marker and never an extension.
        assert_eq!(syntax_for_name(".rs"), None);
    }

    #[test]
    fn every_table_entry_is_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (ext, _) in SYNTAX {
            assert_eq!(*ext, ext.to_ascii_lowercase(), "{ext} is not lowercase");
            assert!(seen.insert(*ext), "{ext} appears twice");
        }
        let mut seen = std::collections::HashSet::new();
        for (name, _) in SYNTAX_NAMES {
            assert_eq!(*name, name.to_ascii_lowercase(), "{name} is not lowercase");
            assert!(seen.insert(*name), "{name} appears twice");
        }
    }

    #[test]
    fn a_shebang_speaks_for_a_file_with_no_name_to_go_on() {
        assert_eq!(syntax_for_shebang(b"#!/bin/sh\n"), Some("Shell"));
        assert_eq!(syntax_for_shebang(b"#!/usr/bin/env bash\n"), Some("Shell"));
        assert_eq!(
            syntax_for_shebang(b"#!/usr/bin/env -S python3 -u\nimport os\n"),
            Some("Python")
        );
        assert_eq!(
            syntax_for_shebang(b"#!/usr/local/bin/python3.12\n"),
            Some("Python")
        );
        assert_eq!(syntax_for_shebang(b"# not a shebang\n"), None);
        assert_eq!(syntax_for_shebang(b"#!/opt/weird/thing\n"), None);
    }

    #[test]
    fn the_name_wins_over_the_shebang() {
        // A `.py` file whose shebang says sh is still Python: the name is the
        // author's intent, the shebang is the kernel's.
        assert_eq!(syntax_for("script.py", b"#!/bin/sh\n"), Some("Python"));
        assert_eq!(syntax_for("deploy", b"#!/bin/bash\n"), Some("Shell"));
        assert_eq!(syntax_for("deploy", b"binary\n"), None);
    }
}
