//! The editor is a state machine with no I/O, so PLAN §9's "prove it without a
//! display" applies at its strongest: every binding in yazi's `[input]` map is
//! driven here as a script of keystrokes against a fixture line, and the
//! assertion is the text, the caret and the selection afterwards.
//!
//! Scripts are written the way a person would say them — `"dw"`, `"vlly"`,
//! `"<esc>ciw"` — because a test that reads like the keystrokes it performs is
//! a test whose failure names the binding that broke.

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

/// A buffer holding `text`, in Normal mode, caret at char index `at`.
///
/// Normal is the interesting starting mode for command tests, and getting
/// there through `<esc>` would move the caret, so it is set directly the way a
/// user who pressed `Esc` at position 0 would find it.
fn normal(text: &str, at: usize) -> InputBuffer {
    let mut buf = InputBuffer::new(text, at);
    buf.feed(parse_chord("esc").expect("esc"));
    buf.cursor = at.min(buf.limit());
    buf
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

#[test]
fn fixture_is_the_length_the_tables_assume() {
    assert_eq!(FIXTURE.chars().count(), FIXTURE_LEN);
    // …and it really is multibyte, or the Unicode tests prove nothing.
    assert!(FIXTURE.len() > FIXTURE_LEN);
}

// ── Modes ───────────────────────────────────────────────────────────────────

#[test]
fn every_mode_door_lands_where_yazi_says() {
    // (script from Normal@6, expected mode, expected caret)
    let cases: &[(&str, InputMode, usize)] = &[
        ("i", InputMode::Insert, 6),
        // `I` is `move first-char` then insert; the fixture has no leading
        // whitespace, so first-char is BOL.
        ("I", InputMode::Insert, 0),
        ("a", InputMode::Insert, 7),
        // `A` is `move eol` (clamped to the last char) then append: past it.
        ("A", InputMode::Insert, FIXTURE_LEN),
        ("v", InputMode::Visual, 6),
        ("r", InputMode::Replace, 6),
        // Nothing at all: Normal stays Normal.
        ("", InputMode::Normal, 6),
    ];
    for (script, mode, cursor) in cases {
        let mut buf = normal(FIXTURE, 6);
        run(&mut buf, script);
        assert_eq!(buf.mode(), *mode, "{script:?}");
        assert_eq!(buf.cursor(), *cursor, "{script:?}");
        assert_eq!(buf.text(), FIXTURE, "{script:?} must not edit");
    }
}

#[test]
fn i_on_a_leading_indent_finds_the_first_real_character() {
    let mut buf = normal("   spaced", 8);
    run(&mut buf, "I");
    assert_eq!(buf.cursor(), 3);
}

#[test]
fn insert_mode_types_the_letters_that_are_commands_in_normal() {
    let mut buf = InputBuffer::new("", 0);
    run(&mut buf, "idwvxAB");
    assert_eq!(buf.text(), "idwvxAB");
    assert_eq!(buf.cursor(), 7);
    assert_eq!(buf.mode(), InputMode::Insert);
}

#[test]
fn shifted_and_multibyte_keys_type_their_glyph() {
    let mut buf = InputBuffer::new("", 0);
    for c in "A_$<".chars() {
        buf.feed(Chord::from_char(c).unwrap_or_else(|| panic!("{c}")));
    }
    // `keymap::Key::from_char` only knows the US layout, so a composed or
    // dead-key character arrives from the compositor as a bare `Key::Char`
    // with no Shift claim. It still has to type itself.
    buf.feed(Chord::plain(Key::Char('ñ')));
    buf.feed(Chord::plain(Key::Char('語')));
    assert_eq!(buf.text(), "A_$<ñ語");
}

#[test]
fn replace_swaps_one_character_and_returns_to_normal() {
    let mut buf = normal("abc", 1);
    run(&mut buf, "rZ");
    assert_eq!(buf.text(), "aZc");
    assert_eq!(buf.cursor(), 1);
    assert_eq!(buf.mode(), InputMode::Normal);
    // The next key is a command again, not text.
    run(&mut buf, "0");
    assert_eq!(buf.cursor(), 0);
}

#[test]
fn replace_over_a_multibyte_character_keeps_the_rest_intact() {
    let mut buf = normal("aébc", 1);
    run(&mut buf, "rx");
    assert_eq!(buf.text(), "axbc");
}

#[test]
fn replace_at_the_end_of_an_empty_line_changes_nothing() {
    let mut buf = normal("", 0);
    run(&mut buf, "rx");
    assert_eq!(buf.text(), "");
    assert_eq!(buf.mode(), InputMode::Normal);
}

// ── Motions ─────────────────────────────────────────────────────────────────

/// Every motion in the `[input]` map, against the fixture, from a caret that
/// makes the motion interesting.
#[test]
fn motions_against_the_fixture() {
    // (start, script, expected caret)
    let cases: &[(usize, &str, usize)] = &[
        // Character-wise: h/l, arrows, Ctrl+b/Ctrl+f — three spellings, one
        // behavior each way.
        (6, "h", 5),
        (6, "l", 7),
        (6, "<left>", 5),
        (6, "<right>", 7),
        (6, "<ctrl+b>", 5),
        (6, "<ctrl+f>", 7),
        (0, "h", 0),
        (FIXTURE_LEN - 1, "l", FIXTURE_LEN - 1), // Normal clamps to the last char
        // Word-wise, forward. `.` is punctuation, so `w` stops on it and `W`
        // does not — the whole point of the WORD distinction.
        (0, "w", 6),
        (0, "W", 6),
        (6, "w", 11),
        (6, "W", 17),
        (11, "w", 12),
        (12, "w", 17),
        (17, "w", 21), // out of the multibyte run, into `bar`
        // In Normal mode a motion past the end clamps onto the last
        // character; the raw target still reaches past it for operators,
        // which `d$` below proves.
        (21, "w", FIXTURE_LEN - 1),
        // Word-wise, backward.
        (6, "b", 0),
        (11, "b", 6),
        (11, "B", 6),
        (12, "b", 11),
        (12, "B", 6),
        (17, "b", 12),
        (21, "b", 17),
        (0, "b", 0),
        // End-of-word.
        (0, "e", 4),
        (4, "e", 10),
        (6, "e", 10),
        (10, "e", 11),
        (11, "e", 14),
        (14, "e", 19),
        (0, "E", 4),
        (4, "E", 14), // `wörld.foo` is one WORD
        // Alt+b / Alt+f, the readline spellings of `b` and `e`.
        (11, "<alt+b>", 6),
        (0, "<alt+f>", 4),
        // Line-wise. `$` clamps to the last character in Normal mode.
        (6, "0", 0),
        (6, "$", FIXTURE_LEN - 1),
        (6, "<ctrl+a>", 0),
        (6, "<ctrl+e>", FIXTURE_LEN - 1),
        (6, "<home>", 0),
        (6, "<end>", FIXTURE_LEN - 1),
        // Chained, because motions have to compose.
        (0, "wwe", 14),
        (FIXTURE_LEN - 1, "bbb", 12),
    ];
    for (start, script, want) in cases {
        let mut buf = normal(FIXTURE, *start);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), *want, "from {start} via {script:?}");
        assert_eq!(buf.text(), FIXTURE, "{script:?} must not edit");
    }
}

#[test]
fn first_char_motions_skip_leading_whitespace() {
    for script in ["^", "_"] {
        let mut buf = normal("   indented text", 12);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), 3, "{script:?}");
    }
    // A line of nothing but whitespace has no first character; BOL is the
    // honest answer rather than a caret past the end.
    let mut buf = normal("    ", 3);
    run(&mut buf, "^");
    assert_eq!(buf.cursor(), 0);
}

#[test]
fn motions_on_an_empty_line_stay_at_zero() {
    for script in ["h", "l", "w", "b", "e", "W", "B", "E", "0", "$", "^"] {
        let mut buf = normal("", 0);
        run(&mut buf, script);
        assert_eq!(buf.cursor(), 0, "{script:?}");
    }
}

#[test]
fn insert_mode_arrows_may_sit_past_the_last_character() {
    let mut buf = InputBuffer::new("ab", 0);
    run(&mut buf, "<right><right><right>");
    assert_eq!(buf.cursor(), 2, "insert caret may rest after the last char");
}

// ── Visual selection ────────────────────────────────────────────────────────

#[test]
fn visual_selection_is_inclusive_of_the_character_under_the_caret() {
    let mut buf = normal("abcdef", 1);
    run(&mut buf, "v");
    assert_eq!(buf.selection(), Some(1..2), "one char selected immediately");
    run(&mut buf, "ll");
    assert_eq!(buf.selection(), Some(1..4));
    assert_eq!(buf.mode(), InputMode::Visual);
    // Backwards past the anchor: the span flips, the anchor stays put.
    run(&mut buf, "hhhh");
    assert_eq!(buf.selection(), Some(0..2));
}

/// The three selection commands. All three select the whole line; `<C-A>`
/// leaves the caret at BOL while `V` and `<C-e>` leave it at EOL. That is the
/// yazi quirk, and it is a behavior, not an accident.
#[test]
fn the_three_line_selection_commands() {
    let cases: &[(&str, usize)] = &[
        ("V", FIXTURE_LEN - 1),
        ("<ctrl+e>", FIXTURE_LEN - 1),
        ("<ctrl+shift+a>", 0),
    ];
    for (script, caret) in cases {
        let mut buf = normal(FIXTURE, 8);
        run(&mut buf, script);
        assert_eq!(buf.mode(), InputMode::Visual, "{script:?}");
        assert_eq!(buf.selection(), Some(0..FIXTURE_LEN), "{script:?}");
        assert_eq!(buf.cursor(), *caret, "{script:?}");
    }
}

#[test]
fn ctrl_a_and_ctrl_shift_a_are_different_commands() {
    let mut plain = normal(FIXTURE, 8);
    run(&mut plain, "<ctrl+a>");
    assert_eq!(plain.selection(), None, "Ctrl+a only moves");
    assert_eq!(plain.cursor(), 0);

    let mut shifted = normal(FIXTURE, 8);
    run(&mut shifted, "<ctrl+shift+a>");
    assert_eq!(shifted.selection(), Some(0..FIXTURE_LEN));
}

#[test]
fn selection_bytes_track_multibyte_text() {
    let mut buf = normal("héllo", 0);
    run(&mut buf, "vl");
    assert_eq!(buf.selection(), Some(0..2));
    // `h` is one byte, `é` is two.
    assert_eq!(buf.selection_bytes(), Some(0..3));
    let r = buf.selection_bytes().expect("selection");
    assert_eq!(&buf.text()[r], "hé");
}

#[test]
fn escape_drops_the_selection_before_it_closes_the_prompt() {
    let mut buf = normal("abc", 0);
    run(&mut buf, "vll");
    assert_eq!(run(&mut buf, "<esc>"), InputEvent::Consumed);
    assert_eq!(buf.selection(), None);
    assert_eq!(buf.mode(), InputMode::Normal);
    assert_eq!(run(&mut buf, "<esc>"), InputEvent::Cancel);
}

// ── Operators ───────────────────────────────────────────────────────────────

/// Operator + motion, the vim way: half-open ranges.
#[test]
fn operators_driven_by_motions() {
    // (start, script, expected text, expected caret)
    let cases: &[(usize, &str, &str, usize)] = &[
        (0, "dw", "wörld.foo  日本語 bar", 0),
        (0, "dW", "wörld.foo  日本語 bar", 0),
        (6, "dw", "héllo .foo  日本語 bar", 6),
        (6, "dW", "héllo 日本語 bar", 6),
        (6, "db", "wörld.foo  日本語 bar", 0),
        (0, "de", "o wörld.foo  日本語 bar", 0),
        (6, "d$", "héllo ", 5),
        (6, "d0", "wörld.foo  日本語 bar", 0),
        (6, "dl", "héllo örld.foo  日本語 bar", 6),
        (6, "dh", "héllowörld.foo  日本語 bar", 5),
        // `D` and `C` are `delete` + `move eol`, and the raw EOL takes the
        // last character — unlike `$`, which stops on it.
        (6, "D", "héllo ", 5),
        (21, "D", "héllo wörld.foo  日本語 ", 20),
        // `x` deletes the character under the caret, including a multibyte one.
        (17, "x", "héllo wörld.foo  本語 bar", 17),
        // Deleting the last character pulls the block cursor back onto the
        // new last character, as vim does.
        (FIXTURE_LEN - 1, "x", "héllo wörld.foo  日本語 ba", 22),
        // `dd` — the operator armed twice — takes the line.
        (6, "dd", "", 0),
        (6, "cc", "", 0),
    ];
    for (start, script, want, caret) in cases {
        let mut buf = normal(FIXTURE, *start);
        run(&mut buf, script);
        assert_eq!(buf.text(), *want, "from {start} via {script:?}");
        assert_eq!(buf.cursor(), *caret, "caret after {script:?}");
    }
}

#[test]
fn change_operators_leave_you_in_insert_mode() {
    for script in ["cw", "C", "s", "S", "cc"] {
        let mut buf = normal("alpha beta", 0);
        run(&mut buf, script);
        assert_eq!(buf.mode(), InputMode::Insert, "{script:?}");
    }
}

#[test]
fn the_change_family_removes_what_yazi_says_it_does() {
    let cases: &[(usize, &str, &str, usize)] = &[
        (0, "cw", "beta", 0),
        (6, "C", "alpha ", 6),
        (0, "s", "lpha beta", 0),
        (4, "S", "", 0),
    ];
    for (start, script, want, caret) in cases {
        let mut buf = normal("alpha beta", *start);
        run(&mut buf, script);
        assert_eq!(buf.text(), *want, "{script:?}");
        assert_eq!(buf.cursor(), *caret, "{script:?}");
    }
}

#[test]
fn a_change_is_typed_straight_into() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "cwgamma ");
    assert_eq!(buf.text(), "gamma beta");
}

#[test]
fn operators_applied_to_a_visual_selection_are_inclusive() {
    let cases: &[(&str, &str)] = &[
        ("vlld", "def"),
        ("vllx", "def"),
        ("vllc", "def"),
        ("Vd", ""),
        ("<ctrl+shift+a>d", ""),
    ];
    for (script, want) in cases {
        let mut buf = normal("abcdef", 0);
        run(&mut buf, script);
        assert_eq!(buf.text(), *want, "{script:?}");
        assert_eq!(buf.selection(), None, "{script:?} consumes the selection");
    }
}

#[test]
fn an_armed_operator_is_visible_and_escapable() {
    let mut buf = normal("abcdef", 0);
    run(&mut buf, "d");
    assert!(buf.has_pending_operator());
    assert_eq!(run(&mut buf, "<esc>"), InputEvent::Consumed);
    assert!(!buf.has_pending_operator());
    assert_eq!(buf.text(), "abcdef", "an escaped operator edits nothing");
}

// ── Yank, paste, kill ───────────────────────────────────────────────────────

#[test]
fn yank_fills_the_register_without_editing() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "yw");
    assert_eq!(buf.yanked(), "alpha ");
    assert_eq!(buf.text(), "alpha beta");
    assert_eq!(buf.cursor(), 0);
}

#[test]
fn yank_twice_takes_the_line() {
    let mut buf = normal("alpha beta", 4);
    run(&mut buf, "yy");
    assert_eq!(buf.yanked(), "alpha beta");
    assert_eq!(buf.text(), "alpha beta");
}

#[test]
fn yank_over_a_visual_selection() {
    let mut buf = normal("abcdef", 1);
    run(&mut buf, "vly");
    assert_eq!(buf.yanked(), "bc");
    assert_eq!(buf.text(), "abcdef");
}

#[test]
fn cuts_fill_the_register_and_backspaces_do_not() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "dw");
    assert_eq!(buf.yanked(), "alpha ");
    // Backspace must not clobber it: it never has in any line editor.
    run(&mut buf, "i<backspace><backspace>");
    assert_eq!(buf.yanked(), "alpha ");
}

#[test]
fn paste_puts_it_after_or_before_the_caret() {
    let mut buf = normal("abcdef", 0);
    run(&mut buf, "vld"); // cut "ab"
    assert_eq!(buf.text(), "cdef");
    run(&mut buf, "p");
    assert_eq!(buf.text(), "cabdef");
    assert_eq!(buf.cursor(), 2, "caret rests on the last pasted char");

    let mut buf = normal("abcdef", 0);
    run(&mut buf, "vldP");
    assert_eq!(buf.text(), "abcdef");
    assert_eq!(buf.cursor(), 1);
}

#[test]
fn paste_over_a_selection_replaces_it() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "yw"); // register = "alpha "
    run(&mut buf, "$vp");
    assert_eq!(buf.text(), "alpha betalpha ");
}

#[test]
fn paste_with_an_empty_register_is_a_no_op() {
    let mut buf = normal("abc", 1);
    run(&mut buf, "p");
    assert_eq!(buf.text(), "abc");
    assert_eq!(buf.cursor(), 1);
}

#[test]
fn the_kill_family() {
    // (start, script, expected text, expected register, expected caret)
    let cases: &[(usize, &str, &str, &str, usize)] = &[
        (6, "<ctrl+u>", "wörld.foo  日本語 bar", "héllo ", 0),
        (6, "<ctrl+k>", "héllo ", "wörld.foo  日本語 bar", 5),
        (11, "<ctrl+w>", "héllo .foo  日本語 bar", "wörld", 6),
        (0, "<ctrl+u>", FIXTURE, "", 0),
        (
            FIXTURE_LEN - 1,
            "<ctrl+k>",
            "héllo wörld.foo  日本語 ba",
            "r",
            22,
        ),
        // Alt+d kills forward through the end of the word, inclusive.
        (6, "<alt+d>", "héllo .foo  日本語 bar", "wörld", 6),
        (0, "<alt+d>", " wörld.foo  日本語 bar", "héllo", 0),
    ];
    for (start, script, text, reg, caret) in cases {
        let mut buf = normal(FIXTURE, *start);
        run(&mut buf, script);
        assert_eq!(buf.text(), *text, "from {start} via {script:?}");
        assert_eq!(buf.yanked(), *reg, "register after {script:?}");
        assert_eq!(buf.cursor(), *caret, "caret after {script:?}");
    }
}

#[test]
fn kill_works_mid_word_in_insert_mode() {
    let mut buf = InputBuffer::new("alpha beta", 10);
    run(&mut buf, "<ctrl+w>");
    assert_eq!(buf.text(), "alpha ");
    assert_eq!(buf.mode(), InputMode::Insert, "kill does not leave insert");
}

// ── Delete keys ─────────────────────────────────────────────────────────────

#[test]
fn backspace_and_delete_and_their_control_spellings() {
    // (start, script, expected text, expected caret)
    let cases: &[(usize, &str, &str, usize)] = &[
        (2, "<backspace>", "ac", 1),
        (2, "<ctrl+h>", "ac", 1),
        (1, "<delete>", "ac", 1),
        (1, "<ctrl+d>", "ac", 1),
        (0, "<backspace>", "abc", 0),
        (3, "<delete>", "abc", 3),
    ];
    for (start, script, want, caret) in cases {
        let mut buf = InputBuffer::new("abc", *start);
        run(&mut buf, script);
        assert_eq!(buf.text(), *want, "{script:?} at {start}");
        assert_eq!(buf.cursor(), *caret, "{script:?} at {start}");
    }
}

#[test]
fn backspace_removes_a_whole_multibyte_character() {
    let mut buf = InputBuffer::new("a日b", 2);
    run(&mut buf, "<backspace>");
    assert_eq!(buf.text(), "ab");
    let mut buf = InputBuffer::new("a日b", 1);
    run(&mut buf, "<delete>");
    assert_eq!(buf.text(), "ab");
}

// ── Undo and redo ───────────────────────────────────────────────────────────

#[test]
fn a_run_of_typing_is_one_undo_step() {
    let mut buf = normal("start", 0);
    run(&mut buf, "Amore");
    assert_eq!(buf.text(), "startmore");
    run(&mut buf, "<esc>u");
    assert_eq!(buf.text(), "start", "the whole insert run undoes at once");
}

#[test]
fn undo_and_redo_walk_the_stack_both_ways() {
    let mut buf = normal("alpha beta gamma", 0);
    run(&mut buf, "dw");
    assert_eq!(buf.text(), "beta gamma");
    run(&mut buf, "dw");
    assert_eq!(buf.text(), "gamma");
    run(&mut buf, "u");
    assert_eq!(buf.text(), "beta gamma");
    run(&mut buf, "u");
    assert_eq!(buf.text(), "alpha beta gamma");
    run(&mut buf, "u");
    assert_eq!(
        buf.text(),
        "alpha beta gamma",
        "undo past the bottom is inert"
    );
    run(&mut buf, "<ctrl+r>");
    assert_eq!(buf.text(), "beta gamma");
    run(&mut buf, "<ctrl+r>");
    assert_eq!(buf.text(), "gamma");
    run(&mut buf, "<ctrl+r>");
    assert_eq!(buf.text(), "gamma", "redo past the top is inert");
}

#[test]
fn undo_leaves_you_in_normal_mode_with_nothing_armed() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "cwx");
    assert_eq!(buf.mode(), InputMode::Insert);
    run(&mut buf, "<esc>u");
    assert_eq!(buf.text(), "alpha beta");
    assert_eq!(buf.mode(), InputMode::Normal);
    assert_eq!(buf.selection(), None);
}

#[test]
fn a_new_edit_after_undo_drops_the_redo_stack() {
    let mut buf = normal("alpha beta", 0);
    run(&mut buf, "dw");
    run(&mut buf, "u");
    run(&mut buf, "x");
    assert_eq!(buf.text(), "lpha beta");
    run(&mut buf, "<ctrl+r>");
    assert_eq!(buf.text(), "lpha beta", "the old redo branch is gone");
}

#[test]
fn the_undo_stack_is_bounded() {
    let mut buf = normal("", 0);
    // Each `i…<esc>` is one tagged edit; do many more than the cap.
    for i in 0..UNDO_LIMIT + 50 {
        let c = char::from(b'a' + (i % 26) as u8);
        buf.feed(Chord::from_char('i').expect("i"));
        buf.feed(Chord::from_char(c).expect("char"));
        buf.feed(parse_chord("esc").expect("esc"));
    }
    assert_eq!(buf.undo.len(), UNDO_LIMIT, "capped, not unbounded");
    // The oldest states were dropped, so undoing all the way back does not
    // reach the empty line — that is the price of the bound, and it is paid
    // 256 edits into a filename prompt.
    for _ in 0..UNDO_LIMIT {
        buf.feed(Chord::from_char('u').expect("u"));
    }
    assert!(!buf.text().is_empty());
    assert!(buf.undo.is_empty());
}

// ── Submit, cancel, the Esc ladder ──────────────────────────────────────────

#[test]
fn enter_submits_the_text_from_any_mode() {
    for script in ["", "<esc>", "<esc>v"] {
        let mut buf = InputBuffer::new("name.txt", 0);
        run(&mut buf, script);
        assert_eq!(
            buf.feed(parse_chord("enter").expect("enter")),
            InputEvent::Submit("name.txt".to_string()),
            "{script:?}"
        );
    }
}

#[test]
fn ctrl_c_cancels_from_any_mode() {
    for script in ["", "<esc>", "<esc>v", "<esc>d", "<esc>r"] {
        let mut buf = InputBuffer::new("name.txt", 0);
        run(&mut buf, script);
        assert_eq!(
            buf.feed(parse_chord("ctrl+c").expect("ctrl+c")),
            InputEvent::Cancel,
            "{script:?}"
        );
    }
}

/// The ladder, one rung at a time: insert → normal → (selection) → cancel.
#[test]
fn the_escape_ladder() {
    let mut buf = InputBuffer::new("abc", 3);
    assert_eq!(buf.feed(esc()), InputEvent::Consumed);
    assert_eq!(buf.mode(), InputMode::Normal);
    assert_eq!(
        buf.cursor(),
        2,
        "leaving insert steps back onto a character"
    );
    run(&mut buf, "v");
    assert_eq!(buf.feed(esc()), InputEvent::Consumed);
    assert_eq!(buf.selection(), None);
    assert_eq!(buf.feed(esc()), InputEvent::Cancel);
}

#[test]
fn ctrl_bracket_is_the_same_key_as_escape() {
    let mut buf = InputBuffer::new("abc", 3);
    assert_eq!(
        buf.feed(parse_chord("ctrl+[").expect("ctrl+[")),
        InputEvent::Consumed
    );
    assert_eq!(buf.mode(), InputMode::Normal);
    assert_eq!(
        buf.feed(parse_chord("ctrl+[").expect("ctrl+[")),
        InputEvent::Cancel
    );
}

fn esc() -> Chord {
    parse_chord("esc").expect("esc")
}

#[test]
fn unbound_normal_mode_keys_are_swallowed_not_leaked() {
    let mut buf = normal("abc", 0);
    for script in ["q", "Q", "z", "!", "<f1>", "~", "<tab>"] {
        assert_eq!(run(&mut buf, script), InputEvent::Consumed, "{script:?}");
        assert_eq!(buf.text(), "abc", "{script:?} must not type");
    }
}

// ── Rename presets ──────────────────────────────────────────────────────────

#[test]
fn rename_presets_match_the_yazi_cursor_flags() {
    // (name, caret for `r`, whole text for `R`)
    let cases: &[(&str, usize, &str)] = &[
        ("photo.jpg", 5, ".jpg"),
        ("archive.tar.gz", 11, ".gz"),
        ("Makefile", 8, ""),
        // A dotfile has no extension: the dot is the name.
        (".bashrc", 7, ""),
        // Multibyte stems count in characters, not bytes.
        ("日本語.txt", 3, ".txt"),
        ("", 0, ""),
    ];
    for (name, caret, empty) in cases {
        let stem = InputBuffer::for_rename_stem(name);
        assert_eq!(stem.text(), *name, "{name}");
        assert_eq!(stem.cursor(), *caret, "{name}");
        assert_eq!(stem.mode(), InputMode::Insert, "{name}");

        let cleared = InputBuffer::for_rename_empty(name);
        assert_eq!(cleared.text(), *empty, "{name}");
        assert_eq!(cleared.cursor(), 0, "{name}");
        assert_eq!(cleared.mode(), InputMode::Insert, "{name}");
    }
}

#[test]
fn renaming_from_the_stem_preset_is_the_motion_it_was_built_for() {
    // The caret lands before `.jpg`, so a `<ctrl+w>` clears the stem and
    // leaves the extension — the reason the preset exists.
    let mut buf = InputBuffer::for_rename_stem("DSC_0198.jpg");
    run(&mut buf, "<ctrl+w>");
    assert_eq!(buf.text(), ".jpg");
    run(&mut buf, "sunset");
    assert_eq!(buf.text(), "sunset.jpg");
    assert_eq!(
        buf.feed(parse_chord("enter").expect("enter")),
        InputEvent::Submit("sunset.jpg".to_string())
    );
}

// ── Construction ────────────────────────────────────────────────────────────

#[test]
fn new_clamps_an_out_of_range_caret() {
    let buf = InputBuffer::new("abc", 99);
    assert_eq!(buf.cursor(), 3);
    let buf = InputBuffer::new("", 5);
    assert_eq!(buf.cursor(), 0);
}

#[test]
fn cursor_byte_tracks_multibyte_text() {
    let mut buf = InputBuffer::new("日本語", 0);
    assert_eq!(buf.cursor_byte(), 0);
    run(&mut buf, "<right><right>");
    assert_eq!(buf.cursor(), 2);
    assert_eq!(buf.cursor_byte(), 6);
    run(&mut buf, "<right>");
    assert_eq!(
        buf.cursor_byte(),
        9,
        "past the end is the end of the string"
    );
}
