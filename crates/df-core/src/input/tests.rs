//! The editor is a state machine with no I/O, so PLAN §9's "prove it without a
//! display" applies at its strongest: every binding in the prompt's map is
//! driven here as a script of keystrokes against a fixture line, and the
//! assertion is the text, the caret and the selection afterwards.
//!
//! Scripts are written the way a person would say them — `"cat"`,
//! `"<ctrl+w>"`, `"<shift+home>"` — because a test that reads like the
//! keystrokes it performs is a test whose failure names the binding that broke.

use super::*;
use crate::keymap::parse_chord;

// ── Driving ─────────────────────────────────────────────────────────────────

/// Turn a script into chords. Bare characters are themselves; anything in
/// angle brackets goes through the keymap's own parser, so `<ctrl+a>` and
/// `<esc>` are spelled exactly as `keymap.toml` would spell them.
fn chords(script: &str) -> Vec<Chord> {
    let mut out = Vec::new();
    let mut rest = script;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            let Some(end) = rest.find('>') else {
                panic!("unterminated `<` in {script:?}");
            };
            let name = &rest[1..end];
            out.push(parse_chord(name).unwrap_or_else(|e| panic!("{name:?}: {e}")));
            rest = &rest[end + 1..];
        } else {
            out.push(
                Chord::from_char(c).unwrap_or_else(|| panic!("{c:?} is not a chord in {script:?}")),
            );
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Feed a script, returning the last event.
fn run(buf: &mut InputBuffer, script: &str) -> InputEvent {
    let mut last = InputEvent::Consumed;
    for chord in chords(script) {
        last = buf.feed(chord);
    }
    last
}

/// The fixture line, deliberately mixed: ASCII words, punctuation, a run of
/// spaces, and multibyte characters that are one *word* but several bytes.
const FIXTURE: &str = "héllo wörld.foo  日本語 bar";

/// Char indices into [`FIXTURE`], so the expectations below can be read
/// against the string rather than counted by hand:
///
/// ```text
/// h é l l o _ w ö r l d .  f  o  o  _  _  日  本  語  _  b  a  r
/// 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23
/// ```
const FIXTURE_LEN: usize = 24;

/// A fixture buffer with the caret at char index `at`.
fn at(pos: usize) -> InputBuffer {
    InputBuffer::new(FIXTURE, pos)
}

#[test]
fn fixture_is_the_length_the_tables_assume() {
    assert_eq!(FIXTURE.chars().count(), FIXTURE_LEN);
    // …and it really is multibyte, or the Unicode tests prove nothing.
    assert!(FIXTURE.len() > FIXTURE_LEN);
}

// ── There is no mode ────────────────────────────────────────────────────────

/// The point of the whole rewrite: every letter that used to be a command is
/// text, on the first keystroke, with nothing to enter or leave first.
#[test]
fn every_letter_types_itself() {
    for script in ["i", "a", "v", "r", "d", "x", "V", "A", "0", "u", "p"] {
        let mut buf = InputBuffer::new("", 0);
        run(&mut buf, script);
        assert_eq!(buf.text(), script, "{script:?} must type itself");
        assert_eq!(buf.cursor(), 1);
    }
}

/// `Esc` cancels on the first press, from any state — including one where a
/// selection is live, which used to be a rung on the ladder.
#[test]
fn escape_always_cancels() {
    let mut buf = at(6);
    assert_eq!(run(&mut buf, "<esc>"), InputEvent::Cancel);

    let mut buf = at(6);
    run(&mut buf, "<shift+end>");
    assert!(buf.selection().is_some(), "a selection is live");
    assert_eq!(run(&mut buf, "<esc>"), InputEvent::Cancel);

    // `<C-[>` is the same keystroke on a terminal, and the same command here.
    let mut buf = at(6);
    assert_eq!(run(&mut buf, "<ctrl+[>"), InputEvent::Cancel);
    // …as is `Ctrl+c`.
    let mut buf = at(6);
    assert_eq!(run(&mut buf, "<ctrl+c>"), InputEvent::Cancel);
}

/// A prompt swallows what it does not understand, so a stray key never reaches
/// the file list behind it.
#[test]
fn an_unbound_chord_is_consumed_and_changes_nothing() {
    let mut buf = at(6);
    assert_eq!(run(&mut buf, "<ctrl+q>"), InputEvent::Consumed);
    assert_eq!(buf.text(), FIXTURE);
    assert_eq!(buf.cursor(), 6);
}

#[test]
fn enter_submits_the_line_as_it_stands() {
    let mut buf = InputBuffer::new("", 0);
    assert_eq!(
        run(&mut buf, "cat.png<enter>"),
        InputEvent::Submit("cat.png".to_string())
    );
}

// ── Typing ──────────────────────────────────────────────────────────────────

#[test]
fn typing_inserts_at_the_caret() {
    let mut buf = InputBuffer::new("photo.jpg", 5);
    run(&mut buf, "-2024");
    assert_eq!(buf.text(), "photo-2024.jpg");
    assert_eq!(buf.cursor(), 10);
}

/// Shift produces the shifted glyph, because a key is stored unshifted and the
/// buffer is the thing that knows what `Shift+-` looks like.
#[test]
fn shift_types_the_shifted_glyph() {
    let mut buf = InputBuffer::new("", 0);
    run(&mut buf, "<shift+->a<shift+1>");
    assert_eq!(buf.text(), "_a!");
}

#[test]
fn space_is_a_character_like_any_other() {
    let mut buf = InputBuffer::new("ab", 1);
    run(&mut buf, "<space>");
    assert_eq!(buf.text(), "a b");
    assert_eq!(buf.cursor(), 2);
}

// ── Motion ──────────────────────────────────────────────────────────────────

#[test]
fn character_motions_step_one_scalar_at_a_time() {
    // Char indices, not bytes: stepping over `é` and `日` must land between
    // characters and never split one.
    let cases: &[(&str, usize, usize)] = &[
        ("<left>", 6, 5),
        ("<right>", 6, 7),
        ("<ctrl+b>", 18, 17),
        ("<ctrl+f>", 18, 19),
        // The ends clamp rather than wrapping or panicking.
        ("<left>", 0, 0),
        ("<right>", FIXTURE_LEN, FIXTURE_LEN),
    ];
    for (script, from, want) in cases {
        let mut buf = at(*from);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), *want, "{script:?} from {from}");
    }
}

#[test]
fn line_motions_reach_both_ends() {
    for script in ["<home>", "<ctrl+a>"] {
        let mut buf = at(12);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), 0, "{script:?}");
    }
    for script in ["<end>", "<ctrl+e>"] {
        let mut buf = at(12);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), FIXTURE_LEN, "{script:?}");
    }
}

/// Word motions are class-based, so `héllo` is one word and `日本語` is one
/// word — the multibyte case is handled by asking each character what it is.
#[test]
fn word_motions_walk_by_character_class() {
    let back: &[(usize, usize)] = &[
        (5, 0),   // inside `héllo`, back to its start
        (6, 0),   // on `w`, back over the space to `héllo`
        (11, 6),  // on the `.`, back to `wörld`
        (17, 12), // on `日`, back over the spaces to `foo`
        (21, 17), // on `bar`, back to `日本語`
        (0, 0),   // the start clamps
    ];
    for (from, want) in back {
        for script in ["<alt+b>", "<ctrl+left>"] {
            let mut buf = at(*from);
            run(&mut buf, script);
            assert_eq!(buf.cursor(), *want, "{script:?} from {from}");
        }
    }

    let forward: &[(usize, usize)] = &[
        (0, 5),   // over `héllo`
        (5, 11),  // over the space and `wörld`, stopping at the `.`
        (11, 12), // the `.` is a word of its own
        (15, 20), // over the spaces and `日本語`
        (21, FIXTURE_LEN),
        (FIXTURE_LEN, FIXTURE_LEN),
    ];
    for (from, want) in forward {
        for script in ["<alt+f>", "<ctrl+right>"] {
            let mut buf = at(*from);
            run(&mut buf, script);
            assert_eq!(buf.cursor(), *want, "{script:?} from {from}");
        }
    }
}

// ── Selection ───────────────────────────────────────────────────────────────

#[test]
fn shift_extends_a_selection_and_a_bare_motion_drops_it() {
    let mut buf = at(6);
    run(&mut buf, "<shift+right><shift+right>");
    assert_eq!(buf.selection(), Some(6..8));
    run(&mut buf, "<shift+left>");
    assert_eq!(buf.selection(), Some(6..7));
    // Back onto the anchor is *no* selection, not an empty one.
    run(&mut buf, "<shift+left>");
    assert_eq!(buf.selection(), None);

    let mut buf = at(6);
    run(&mut buf, "<shift+end>");
    assert_eq!(buf.selection(), Some(6..FIXTURE_LEN));
    run(&mut buf, "<left>");
    assert_eq!(buf.selection(), None, "a bare motion drops the selection");
}

/// Selecting backwards is the same range read the other way round.
#[test]
fn a_backwards_selection_is_the_same_span() {
    let mut buf = at(6);
    run(&mut buf, "<shift+home>");
    assert_eq!(buf.selection(), Some(0..6));
    assert_eq!(buf.cursor(), 0);
}

/// Collapsing with an arrow lands on the edge the arrow points at, which is
/// what every other text field does.
#[test]
fn an_arrow_collapses_a_selection_to_its_edge() {
    let mut buf = at(6);
    run(&mut buf, "<shift+right><shift+right><left>");
    assert_eq!(buf.cursor(), 6);

    let mut buf = at(6);
    run(&mut buf, "<shift+right><shift+right><right>");
    assert_eq!(buf.cursor(), 8);
}

#[test]
fn selection_bytes_slice_the_string_directly() {
    let mut buf = at(0);
    run(&mut buf, "<shift+right><shift+right>");
    let range = buf.selection_bytes().expect("a selection");
    assert_eq!(&buf.text()[range], "hé");
}

#[test]
fn typing_over_a_selection_replaces_it() {
    let mut buf = InputBuffer::new("photo.jpg", 0);
    run(
        &mut buf,
        "<shift+right><shift+right><shift+right><shift+right><shift+right>cat",
    );
    assert_eq!(buf.text(), "cat.jpg");
    assert_eq!(buf.cursor(), 3);
    assert_eq!(buf.selection(), None);
}

#[test]
fn deleting_a_selection_takes_the_whole_span() {
    for script in ["<backspace>", "<delete>", "<ctrl+w>", "<ctrl+k>"] {
        let mut buf = InputBuffer::new("photo.jpg", 5);
        run(&mut buf, "<shift+end>");
        run(&mut buf, script);
        assert_eq!(buf.text(), "photo", "{script:?}");
        assert_eq!(buf.cursor(), 5, "{script:?}");
        assert_eq!(buf.selection(), None, "{script:?}");
    }
}

// ── Delete and kill ─────────────────────────────────────────────────────────

#[test]
fn backspace_and_delete_take_one_character() {
    for script in ["<backspace>", "<ctrl+h>"] {
        let mut buf = InputBuffer::new("héllo", 2);
        run(&mut buf, script);
        assert_eq!(buf.text(), "hllo", "{script:?}");
        assert_eq!(buf.cursor(), 1, "{script:?}");
    }
    for script in ["<delete>", "<ctrl+d>"] {
        let mut buf = InputBuffer::new("héllo", 1);
        run(&mut buf, script);
        assert_eq!(buf.text(), "hllo", "{script:?}");
        assert_eq!(buf.cursor(), 1, "{script:?}");
    }
    // …and both are no-ops at the end they cannot cross.
    let mut buf = InputBuffer::new("hi", 0);
    run(&mut buf, "<backspace>");
    assert_eq!(buf.text(), "hi");
    let mut buf = InputBuffer::new("hi", 2);
    run(&mut buf, "<delete>");
    assert_eq!(buf.text(), "hi");
}

#[test]
fn the_kill_family_cuts_to_where_it_says() {
    let cases: &[(&str, usize, &str, usize)] = &[
        ("<ctrl+u>", 6, "wörld.foo  日本語 bar", 0),
        ("<ctrl+k>", 6, "héllo ", 6),
        ("<ctrl+w>", 11, "héllo .foo  日本語 bar", 6),
        ("<alt+d>", 6, "héllo .foo  日本語 bar", 6),
        // Nothing to kill is not an edit: the line is untouched.
        ("<ctrl+u>", 0, FIXTURE, 0),
        ("<ctrl+k>", FIXTURE_LEN, FIXTURE, FIXTURE_LEN),
    ];
    for (script, from, want, caret) in cases {
        let mut buf = at(*from);
        run(&mut buf, script);
        assert_eq!(buf.text(), *want, "{script:?} from {from}");
        assert_eq!(buf.cursor(), *caret, "{script:?} from {from}");
    }
}

// ── Undo ────────────────────────────────────────────────────────────────────

/// A run of typing is one undo step: twenty presses to type a name and one to
/// take it back.
#[test]
fn a_run_of_typing_is_one_undo_step() {
    let mut buf = InputBuffer::new("", 0);
    run(&mut buf, "cat.png");
    assert_eq!(buf.text(), "cat.png");
    run(&mut buf, "<ctrl+z>");
    assert_eq!(buf.text(), "");
    for script in ["<ctrl+y>", "<ctrl+shift+z>"] {
        let mut buf = InputBuffer::new("", 0);
        run(&mut buf, "cat.png<ctrl+z>");
        run(&mut buf, script);
        assert_eq!(buf.text(), "cat.png", "{script:?}");
    }
}

#[test]
fn undo_walks_back_one_edit_at_a_time() {
    let mut buf = InputBuffer::new("photo.jpg", 9);
    run(&mut buf, "<ctrl+w>"); // "photo."
    run(&mut buf, "<ctrl+u>"); // ""
    run(&mut buf, "<ctrl+z>");
    assert_eq!(buf.text(), "photo.");
    run(&mut buf, "<ctrl+z>");
    assert_eq!(buf.text(), "photo.jpg");
    // Past the bottom of the stack is a no-op, not a panic.
    run(&mut buf, "<ctrl+z><ctrl+z>");
    assert_eq!(buf.text(), "photo.jpg");
}

/// Typing after an undo starts a new run, so the redo stack is dropped rather
/// than left pointing at a line that no longer exists.
#[test]
fn typing_after_an_undo_drops_the_redo() {
    let mut buf = InputBuffer::new("", 0);
    run(&mut buf, "cat<ctrl+z>dog<ctrl+y>");
    assert_eq!(buf.text(), "dog");
}

// ── Composed text ───────────────────────────────────────────────────────────

#[test]
fn a_commit_lands_at_the_caret() {
    let mut buf = InputBuffer::new("ab", 1);
    assert_eq!(buf.insert_text("私は"), InputEvent::Consumed);
    assert_eq!(buf.text(), "a私はb");
    assert_eq!(buf.cursor(), 3);
    // An empty commit is not an edit.
    let before = buf.text().to_string();
    buf.insert_text("");
    assert_eq!(buf.text(), before);
}

#[test]
fn a_commit_replaces_a_live_selection() {
    let mut buf = InputBuffer::new("photo.jpg", 0);
    run(
        &mut buf,
        "<shift+right><shift+right><shift+right><shift+right><shift+right>",
    );
    buf.insert_text("cat");
    assert_eq!(buf.text(), "cat.jpg");
    assert_eq!(buf.selection(), None);
}

/// A commit coalesces with the typing around it: one `Ctrl+z` takes back the
/// whole word rather than splitting it around the composition.
#[test]
fn a_commit_joins_the_typing_run_it_arrives_in() {
    let mut buf = InputBuffer::new("", 0);
    run(&mut buf, "a");
    buf.insert_text("私");
    run(&mut buf, "b");
    assert_eq!(buf.text(), "a私b");
    run(&mut buf, "<ctrl+z>");
    assert_eq!(buf.text(), "");
}

// ── The rename presets (PLAN §4.1) ──────────────────────────────────────────

#[test]
fn rename_opens_with_the_caret_before_the_extension() {
    let cases: &[(&str, &str, usize)] = &[
        ("photo.jpg", "photo.jpg", 5),
        // No extension: the caret is at the end.
        ("README", "README", 6),
        // A dotfile's leading dot is part of the name, not an extension.
        (".bashrc", ".bashrc", 7),
        ("archive.tar.gz", "archive.tar.gz", 11),
        ("héllo.jpg", "héllo.jpg", 5),
    ];
    for (name, text, caret) in cases {
        let buf = InputBuffer::for_rename_stem(name);
        assert_eq!(buf.text(), *text, "{name}");
        assert_eq!(buf.cursor(), *caret, "{name}");
    }
}

#[test]
fn rename_with_an_empty_stem_keeps_the_extension() {
    let cases: &[(&str, &str)] = &[
        ("photo.jpg", ".jpg"),
        ("README", ""),
        (".bashrc", ""),
        ("archive.tar.gz", ".gz"),
    ];
    for (name, text) in cases {
        let buf = InputBuffer::for_rename_empty(name);
        assert_eq!(buf.text(), *text, "{name}");
        assert_eq!(buf.cursor(), 0, "{name}");
    }
}

/// The caret may sit past the last character — there is no block cursor to
/// keep on one, so `End` and the presets can both land at the line's length.
#[test]
fn the_caret_may_sit_after_the_last_character() {
    let mut buf = InputBuffer::new("ab", 99);
    assert_eq!(
        buf.cursor(),
        2,
        "an out-of-range start is clamped, not wrapped"
    );
    run(&mut buf, "c");
    assert_eq!(buf.text(), "abc");
}
