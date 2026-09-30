//! An opener's command as an argument list, for the platform with no shell
//! to hand it to (`plans/other-platforms/04-windows.md`, "Openers are argv
//! lists, no shell").
//!
//! On Linux and macOS an opener is a shell snippet and `$SHELL -c` reads it.
//! Windows has no shell that reads `"$@"` — `cmd` knows `%1`, PowerShell its
//! own syntax, and neither is a program an opener table should be written
//! in — so there the command is split here, into the program and its
//! arguments, and started without a shell at all:
//!
//! - **Split on whitespace, with double quotes grouping.** `"C:\Program
//!   Files\Zed\zed.exe" "$@"` is two words; the quotes go, and a word may be
//!   part quoted, part not (`--dir="$1"`). A backslash is a backslash, since
//!   it is what a Windows path is made of, so there is no escape and no way
//!   to put a quote inside a word — which no program on the opener tables
//!   needs.
//! - **`$1`, `$@` and `$dir` are the paths.** `$1` is the first path, `$dir`
//!   the folder it is in, and `$@`, as a word of its own, is one argument
//!   per path, so a path with spaces in it is one argument however it is
//!   spelled; inside a longer word `$@` is the paths joined by spaces. With
//!   no paths each is empty (and a word that was only `$@` is no word at
//!   all).
//! - **Nothing else is expanded.** No `%VAR%`, no `$VAR`, no `~`: a table
//!   that wants `%EDITOR%` writes the editor's path into `delightfile.toml`.
//!
//! The paths never pass through a parser: each is substituted into the word
//! that names it after the split, so a file called `a" & del *` is one
//! argument with odd characters in it, as it is in `"$@"` under a shell.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `command` split into a program and its arguments, with `paths`
/// substituted (the module's rules). Empty when the command is.
pub fn argv_from(command: &str, paths: &[PathBuf]) -> Vec<OsString> {
    let first = paths.first();
    let dir = first
        .and_then(|path| path.parent())
        .map(Path::as_os_str)
        .unwrap_or_default();
    let mut argv = Vec::new();
    for word in words(command) {
        if word == "$@" {
            argv.extend(paths.iter().map(|path| path.as_os_str().to_os_string()));
            continue;
        }
        argv.push(substitute(&word, first.map(|p| p.as_os_str()), dir, paths));
    }
    argv
}

/// The words of `command`: whitespace-separated, a double-quoted run kept
/// whole and its quotes dropped.
fn words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// One word with `$1`, `$dir` and an embedded `$@` put in.
fn substitute(
    word: &str,
    first: Option<&std::ffi::OsStr>,
    dir: &std::ffi::OsStr,
    paths: &[PathBuf],
) -> OsString {
    let mut out = OsString::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$1") {
            out.push(first.unwrap_or_default());
            rest = after;
        } else if let Some(after) = tail.strip_prefix("$dir") {
            out.push(dir);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("$@") {
            for (i, path) in paths.iter().enumerate() {
                if i > 0 {
                    out.push(" ");
                }
                out.push(path.as_os_str());
            }
            rest = after;
        } else {
            out.push("$");
            rest = &tail[1..];
        }
    }
    out.push(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// `$@` as a word is one argument per path, however many there are and
    /// whatever is in them.
    #[test]
    fn every_path_is_one_argument() {
        let two = paths(&[r"C:\Users\a b\one.txt", r#"C:\x\a" & del *.txt"#]);
        assert_eq!(
            strings(argv_from(r#"zed "$@""#, &two)),
            ["zed", r"C:\Users\a b\one.txt", r#"C:\x\a" & del *.txt"#]
        );
        assert_eq!(strings(argv_from("zed $@", &two)).len(), 3, "quoted or not");
        assert_eq!(strings(argv_from(r#"zed "$@""#, &[])), ["zed"], "none");
    }

    /// `$dir` is the first path's folder, for the terminal openers. The
    /// folder is the platform's `parent`, so the paths are built with its
    /// separator.
    #[test]
    fn dir_is_the_first_paths_folder() {
        let notes = std::env::temp_dir().join("brian").join("notes");
        let file = vec![notes.join("today.md")];
        let notes = notes.to_string_lossy().into_owned();
        let today = file[0].to_string_lossy().into_owned();
        assert_eq!(
            strings(argv_from(r#"wt -d "$dir""#, &file)),
            ["wt", "-d", notes.as_str()]
        );
        assert_eq!(
            strings(argv_from(r#"cmd /K cd /d "$1""#, &file)),
            ["cmd", "/K", "cd", "/d", today.as_str()]
        );
    }

    /// A program in a folder with a space in its name is one word, quoted;
    /// a word may be part quoted; backslashes are kept.
    #[test]
    fn quotes_group_and_backslashes_stay() {
        let folder = std::env::temp_dir().join("a b");
        let file = vec![folder.join("x.txt")];
        assert_eq!(
            strings(argv_from(
                r#""C:\Program Files\Zed\zed.exe" --dir="$dir" --wait"#,
                &file
            )),
            [
                r"C:\Program Files\Zed\zed.exe".to_string(),
                format!("--dir={}", folder.display()),
                "--wait".to_string()
            ]
        );
    }

    /// Only the three names are the paths; anything else with a `$` in it,
    /// and every `%VAR%`, is left as written.
    #[test]
    fn nothing_else_is_expanded() {
        let file = paths(&[r"C:\a.txt"]);
        assert_eq!(
            strings(argv_from(
                r#"notepad %EDITOR% $HOME $2 cost$ "$@-x""#,
                &file
            )),
            ["notepad", "%EDITOR%", "$HOME", "$2", "cost$", r"C:\a.txt-x"]
        );
        assert!(argv_from("   ", &file).is_empty());
    }
}
