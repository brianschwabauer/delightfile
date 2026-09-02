//! The keymap engine: one registry of `(context, chord sequence, when) →
//! Command`, and the state machine that feeds keystrokes into it.
//!
//! PLAN §4 asks for one table behind four surfaces — dispatch, the which-key
//! card, the `?` help browser, and the command palette — so that the keymap
//! documents itself instead of drifting from three hand-written lists. That is
//! the whole design: [`Registry`] holds rows, and every surface is a different
//! read of the same rows. A binding that exists is a binding the help sheet
//! knows about, because there is nowhere else for it to have come from.
//!
//! Ported from delightviewer's `keymap.rs`, with three additions the file
//! manager needs and the viewer did not:
//!
//! 1. **`when` predicates.** PLAN §2.1: the keyboard is always in the list, so
//!    a binding's meaning turns on what the *cursor is standing on* rather than
//!    on which pane has focus — `.` is the hidden-files toggle on a text file
//!    and the frame step is somewhere else entirely. df-core is headless, so a
//!    predicate cannot be a closure over the app; it is a [`When`] tag, and
//!    dispatch is handed the [`WhenFlags`] that are true right now.
//! 2. **Descriptions on every row**, because which-key and the palette are not
//!    optional extras here.
//! 3. **Continuations.** An unfinished chord returns the candidates that could
//!    finish it, *in declaration order* — PLAN §4 says explicitly that
//!    which-key ordering is editorial, not alphabetical.
//!
//! ## Context precedence
//!
//! Contexts live on a stack whose bottom is always [`Context::Global`] (never
//! popped) and whose top is the most recently pushed overlay. Lookup walks the
//! stack from the top down and **the first context with anything to say wins**:
//! if it has an exact match, that is the command; if it has only a longer
//! binding this could be the start of, the chord is pending and no lower
//! context gets a turn. Rows whose `when` is false are invisible to that walk,
//! which is what lets Global read `Ctrl+→` as the frame step on a hovered clip
//! and as the document's next page on anything else — the second row is simply
//! not there while the first one is.
//!
//! ## `keymap.toml`
//!
//! User overrides amend the defaults (delightviewer's model: the table is in
//! code, the file edits it). One table per context, keys are chord sequences,
//! values are command ids:
//!
//! ```toml
//! # ~/.config/delightfile/keymap.toml
//! [files]
//! "g m" = "goto-3"          # bind a chord to a command id
//! "ctrl+p" = "command-palette"
//! "Q" = ""                  # an empty value unbinds
//!
//! [global]
//! "f5" = "help"
//! ```
//!
//! - Table names are context names, lowercase: `global`, `files`, `input`,
//!   `confirm`, `pick`, `tasks`, `spot`, `help`, `palette`.
//! - A key is a whitespace-separated chord sequence (`"g g"`, `", m"`,
//!   `"ctrl+shift+z"`). Shifted glyphs are written as themselves: `"<"`, not
//!   `"shift+,"`. The Space key is written `space`, since a literal space is
//!   the separator.
//! - A command id is what [`Command::id`] returns; the `?` help browser and the
//!   palette both show it.
//! - Rebinding a sequence **replaces** any default bound to that same sequence
//!   in that context, and the new row's `when` is [`When::Always`]: a user who
//!   says "this key does this" gets exactly that, not a predicate they cannot
//!   see. The default row it replaced is gone, so a predicate-guarded pair
//!   (`Ctrl+→` on a clip vs. on a document) needs both halves rebound to keep
//!   both.
//! - Anything unparseable — an unknown context, an unknown command, a bad
//!   chord, or one of the reserved transport keys — is a warning naming the
//!   file and line, and every other line in the file still applies (PLAN §3).

mod command;
mod defaults;
mod key;

use std::path::Path;
use std::time::{Duration, Instant};

pub use command::Command;
pub use key::{label_sequence, parse_chord, parse_sequence, Chord, Key, KeymapError, Mods};

use crate::toml::{ConfigWarning, Value};

/// How long an unfinished chord waits before the which-key card appears
/// (PLAN §4).
///
/// 175 ms is under the ~200 ms it takes to type the second key of a chord you
/// already know, so a fast typist never sees the card at all, and over the
/// ~100 ms at which a panel appearing reads as a flicker rather than a
/// deliberate arrival. The card is a hint for hesitation, and hesitation is
/// what this number measures.
pub const WHICH_KEY_DELAY: Duration = Duration::from_millis(175);

/// The keys that belong to media transport in every context (PLAN §4.3).
///
/// delightfile's one deliberate break from yazi is that `hjkl` navigation is
/// gone and `j` `k` `l` are the shuttle keys *globally* — arrow onto a video
/// and press `l` and it is shuttling, without leaving the list. That only holds if
/// nothing else can ever claim them, so a non-Global binding of one is a
/// registration error rather than a preference, and a `keymap.toml` that tries
/// gets a warning.
///
/// **The reservation is on the unshifted press only.** `J`/`K` are the preview
/// seek and `{`/`}` are the tab swaps in PLAN §4.1's own table, and Shift is
/// what makes them a different keystroke; `L` (loop) and `<`/`>` (skip) stay
/// transport by being bound in Global, where every one of these is legal.
pub const RESERVED_TRANSPORT_KEYS: &[Key] = &[
    Key::Char('j'),
    Key::Char('k'),
    Key::Char('l'),
    Key::Char('['),
    Key::Char(']'),
];

/// Binding contexts, stacked most-specific-wins (PLAN §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// Never popped. Transport lives here, so it is reachable from everywhere.
    Global,
    /// The browser: the three panes and everything you do to files.
    Files,
    /// The shared vi line editor (PLAN §4.2) — rename, filter, create, cd,
    /// search, shell.
    Input,
    Confirm,
    Pick,
    Tasks,
    Spot,
    Help,
    Palette,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Global => "global",
            Context::Files => "files",
            Context::Input => "input",
            Context::Confirm => "confirm",
            Context::Pick => "pick",
            Context::Tasks => "tasks",
            Context::Spot => "spot",
            Context::Help => "help",
            Context::Palette => "palette",
        }
    }

    pub fn from_name(name: &str) -> Option<Context> {
        Some(match name.to_ascii_lowercase().as_str() {
            "global" => Context::Global,
            "files" | "mgr" => Context::Files,
            "input" => Context::Input,
            "confirm" => Context::Confirm,
            "pick" => Context::Pick,
            "tasks" => Context::Tasks,
            "spot" => Context::Spot,
            "help" => Context::Help,
            "palette" => Context::Palette,
            _ => return None,
        })
    }
}

/// The active context stack. Global is at the bottom and cannot be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextStack {
    stack: Vec<Context>,
}

impl Default for ContextStack {
    fn default() -> Self {
        ContextStack::new()
    }
}

impl ContextStack {
    pub fn new() -> ContextStack {
        ContextStack {
            stack: vec![Context::Global],
        }
    }

    /// The browser's resting state: Global with Files on top.
    pub fn browser() -> ContextStack {
        ContextStack {
            stack: vec![Context::Global, Context::Files],
        }
    }

    /// Global plus these, bottom-up — the shape most tests want.
    pub fn with(extra: &[Context]) -> ContextStack {
        let mut stack = ContextStack::new();
        for &c in extra {
            stack.push(c);
        }
        stack
    }

    pub fn push(&mut self, context: Context) {
        if context == Context::Global {
            return; // already at the bottom; pushing it again would be a lie
        }
        self.stack.push(context);
    }

    /// Remove the most recent occurrence. Global is never removed.
    pub fn pop(&mut self, context: Context) -> bool {
        if context == Context::Global {
            return false;
        }
        match self.stack.iter().rposition(|c| *c == context) {
            Some(i) => {
                self.stack.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn contains(&self, context: Context) -> bool {
        self.stack.contains(&context)
    }

    pub fn top(&self) -> Context {
        // The stack always holds Global, so this cannot be empty.
        *self.stack.last().unwrap_or(&Context::Global)
    }

    /// Most specific first — the order lookup walks. Duplicates are dropped so
    /// a context pushed twice is not searched twice.
    pub fn active(&self) -> Vec<Context> {
        let mut out: Vec<Context> = Vec::with_capacity(self.stack.len());
        for &c in self.stack.iter().rev() {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }
}

/// A binding's precondition, as data instead of code — df-core has no app to
/// close over.
///
/// There used to be four of these, one per pane, because the keyboard could be
/// *in* a pane. It cannot any more (PLAN §2.1): the list is always where the
/// keys go, and the only thing left that a binding's meaning still turns on is
/// what the cursor is standing on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum When {
    #[default]
    Always,
    /// The cursor is on a file the media pipeline can play. PLAN §4.3: on a
    /// non-media file the transport keys are *inert* — no beep, no surprise —
    /// which also means they should not be advertised on the help sheet while
    /// a text file is hovered.
    MediaHovered,
}

/// Which of [`When`]'s conditions are true right now. The app fills this in
/// each dispatch; df-core never computes it.
///
/// One field, and still a struct: `allows` is the only thing that reads it, and
/// a bare `bool` at every call site is how the *next* predicate gets added as a
/// second positional argument nobody can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WhenFlags {
    pub media_hovered: bool,
}

impl WhenFlags {
    /// The resting state: nothing playable is under the cursor, so the
    /// transport rows are not there.
    pub const NONE: WhenFlags = WhenFlags {
        media_hovered: false,
    };

    /// The cursor is on something the transport can act on.
    pub const MEDIA: WhenFlags = WhenFlags {
        media_hovered: true,
    };

    pub fn allows(self, when: When) -> bool {
        match when {
            When::Always => true,
            When::MediaHovered => self.media_hovered,
        }
    }
}

/// One row of the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub context: Context,
    pub seq: Vec<Chord>,
    pub command: Command,
    pub description: String,
    pub when: When,
}

impl Binding {
    /// `g g`, `Ctrl+p`, `, m` — what the help sheet prints.
    pub fn label(&self) -> String {
        label_sequence(&self.seq)
    }
}

/// A candidate that could finish the pending chord — one row of the which-key
/// card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    /// The next keystroke.
    pub next: Chord,
    /// Everything after it, empty when `next` completes the binding.
    pub rest: Vec<Chord>,
    pub command: Command,
    pub description: String,
}

impl Continuation {
    pub fn label(&self) -> String {
        if self.rest.is_empty() {
            self.next.label()
        } else {
            format!("{} {}", self.next.label(), label_sequence(&self.rest))
        }
    }
}

/// What a keystroke turned into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// A binding completed. The pending chord is cleared.
    Match(Command),
    /// The keys so far start at least one longer binding. Hold them, and show
    /// `continuations` once [`WHICH_KEY_DELAY`] has passed.
    Pending {
        chord: Vec<Chord>,
        continuations: Vec<Continuation>,
    },
    /// Nothing matches. The pending chord is cleared — a wrong second key
    /// abandons the chord rather than waiting for a right one, which is what
    /// makes `g` `q` feel like a typo instead of a trap.
    NoMatch,
}

/// The keystroke-to-keystroke state: what is pending, and since when.
#[derive(Debug, Clone, Default)]
pub struct KeymapState {
    pending: Vec<Chord>,
    started: Option<Instant>,
}

impl KeymapState {
    pub fn new() -> KeymapState {
        KeymapState::default()
    }

    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Abandon the chord — Esc, a click anywhere, a focus change.
    pub fn cancel(&mut self) {
        self.pending.clear();
        self.started = None;
    }

    /// Whether the which-key card should be on screen. The app calls this from
    /// its paint, and schedules one wake-up at `started + WHICH_KEY_DELAY` so
    /// a pending chord costs exactly one extra frame, not a poll loop
    /// (PLAN §1).
    pub fn which_key_visible(&self, now: Instant) -> bool {
        match self.started {
            Some(started) => now.duration_since(started) >= WHICH_KEY_DELAY,
            None => false,
        }
    }

    /// When the card is due, for scheduling that wake-up.
    pub fn which_key_due(&self) -> Option<Instant> {
        self.started.map(|s| s + WHICH_KEY_DELAY)
    }
}

/// Every binding there is.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    bindings: Vec<Binding>,
}

impl Registry {
    /// An empty registry. [`Registry::defaults`] is what the app wants.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// The shipped keymap — PLAN §4.1–4.3, which is Brian's yazi keymap with
    /// `hjkl` removed and the transport moved in.
    pub fn defaults() -> Registry {
        defaults::build()
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Install a binding. Fails on a reserved transport key outside Global, or
    /// on a sequence already bound in that context with the same predicate.
    pub fn register(
        &mut self,
        context: Context,
        seq: Vec<Chord>,
        command: Command,
        description: impl Into<String>,
        when: When,
    ) -> Result<(), KeymapError> {
        if seq.is_empty() {
            return Err(KeymapError::BadChord(String::new()));
        }
        // **The reservation is on the key you press**, which is the first chord
        // of a sequence and only that one. With a prefix pending, the transport
        // never sees the key at all — `g l` is looked up as a chord and the
        // Global `l` is never consulted — so refusing continuations would
        // protect the shuttle from a press that cannot reach it.
        // (delightviewer keymap.rs:194-197 makes the same argument.)
        if context != Context::Global && is_reserved(seq[0]) {
            return Err(KeymapError::ReservedKey(label_sequence(&seq)));
        }
        if self
            .bindings
            .iter()
            .any(|b| b.context == context && b.seq == seq && b.when == when)
        {
            return Err(KeymapError::Conflict(label_sequence(&seq)));
        }
        self.bindings.push(Binding {
            context,
            seq,
            command,
            description: description.into(),
            when,
        });
        Ok(())
    }

    /// Drop every binding of `seq` in `context`, whatever its predicate.
    /// Returns how many went. Used by user overrides so a rebind replaces the
    /// default rather than conflicting with it.
    pub fn unbind(&mut self, context: Context, seq: &[Chord]) -> usize {
        let before = self.bindings.len();
        self.bindings
            .retain(|b| !(b.context == context && b.seq == seq));
        before - self.bindings.len()
    }

    /// Drop every binding of a command — how the `g` goto rows are rebuilt when
    /// the user's `[goto]` table differs from the default one.
    pub fn unbind_command(&mut self, predicate: impl Fn(Command) -> bool) -> usize {
        let before = self.bindings.len();
        self.bindings.retain(|b| !predicate(b.command));
        before - self.bindings.len()
    }

    /// Every binding reachable right now, most-specific context first and in
    /// declaration order within a context. This is the `?` help browser and the
    /// command palette, in one function.
    pub fn active_bindings(&self, stack: &ContextStack, flags: WhenFlags) -> Vec<&Binding> {
        let mut out = Vec::new();
        for context in stack.active() {
            out.extend(
                self.bindings
                    .iter()
                    .filter(|b| b.context == context && flags.allows(b.when)),
            );
        }
        out
    }

    /// The binding to *advertise* for a command: fewest keystrokes, then the
    /// cheapest modifiers, then a context-specific row over the Global one.
    /// (Ported from delightviewer, weights and all — see [`Mods::weight`].)
    pub fn binding_label(&self, command: Command) -> Option<String> {
        self.bindings
            .iter()
            .filter(|b| b.command == command)
            .min_by_key(|b| {
                (
                    b.seq.len(),
                    b.seq.iter().map(|c| c.mods.weight() as u32).sum::<u32>(),
                    u8::from(b.context == Context::Global),
                )
            })
            .map(Binding::label)
    }

    /// What one chord runs in `context` on its own — no pending sequence, no
    /// stack, no `when` flags.
    ///
    /// The prompt's door. A line editor has no chords to hold and no context
    /// under it: every key it takes is one key, and a two-key row in an
    /// `[input]` table would be a keystroke swallowed waiting for a second one
    /// that types itself instead. So this looks at single-chord rows only, and
    /// [`Registry::dispatch`]'s state machine stays out of the prompt.
    pub fn lookup(&self, context: Context, chord: Chord) -> Option<Command> {
        self.bindings
            .iter()
            .find(|b| b.context == context && b.seq.len() == 1 && b.seq[0] == chord)
            .map(|b| b.command)
    }

    /// Feed one keystroke in. See the module header for how the context stack
    /// decides.
    pub fn dispatch(
        &self,
        state: &mut KeymapState,
        stack: &ContextStack,
        flags: WhenFlags,
        chord: Chord,
        now: Instant,
    ) -> Dispatch {
        let mut seq = state.pending.clone();
        seq.push(chord);

        for context in stack.active() {
            let mut exact: Option<Command> = None;
            let mut has_prefix = false;
            for b in self
                .bindings
                .iter()
                .filter(|b| b.context == context && flags.allows(b.when))
            {
                if b.seq == seq {
                    // First declared wins, so the table reads top-down.
                    if exact.is_none() {
                        exact = Some(b.command);
                    }
                } else if b.seq.len() > seq.len() && b.seq.starts_with(&seq) {
                    has_prefix = true;
                }
            }
            if let Some(command) = exact {
                state.cancel();
                return Dispatch::Match(command);
            }
            if has_prefix {
                // Continuations are gathered from *every* active context, not
                // just this one: the card should show everything the next key
                // could do, and a Global row is as pressable as a Files one.
                let continuations = self.continuations(stack, flags, &seq);
                state.pending = seq.clone();
                if state.started.is_none() {
                    state.started = Some(now);
                }
                return Dispatch::Pending {
                    chord: seq,
                    continuations,
                };
            }
        }

        state.cancel();
        Dispatch::NoMatch
    }

    /// Every binding that `prefix` could still become, most-specific context
    /// first and in declaration order within a context — PLAN §4's "`[which]`
    /// ordering preserves declaration order, not alphabetical".
    pub fn continuations(
        &self,
        stack: &ContextStack,
        flags: WhenFlags,
        prefix: &[Chord],
    ) -> Vec<Continuation> {
        let mut out: Vec<Continuation> = Vec::new();
        for context in stack.active() {
            for b in self
                .bindings
                .iter()
                .filter(|b| b.context == context && flags.allows(b.when))
            {
                if b.seq.len() <= prefix.len() || !b.seq.starts_with(prefix) {
                    continue;
                }
                let next = b.seq[prefix.len()];
                // A key already claimed by a nearer context is not offered
                // twice: what the card promises has to be what pressing it
                // does, and dispatch stops at the first context with a row.
                if out.iter().any(|c| c.next == next) {
                    continue;
                }
                out.push(Continuation {
                    next,
                    rest: b.seq[prefix.len() + 1..].to_vec(),
                    command: b.command,
                    description: b.description.clone(),
                });
            }
        }
        out
    }

    /// Apply `keymap.toml`. Format is documented in this module's header.
    /// Returns a warning per rejected line; every other line still applies.
    pub fn apply_overrides(&mut self, text: &str, file: &Path) -> Vec<ConfigWarning> {
        let mut doc = crate::toml::parse(text, file);
        let mut warnings = std::mem::take(&mut doc.warnings);
        for table in &doc.tables {
            let Some(context) = Context::from_name(&table.name) else {
                warnings.push(ConfigWarning::new(
                    file,
                    table.line,
                    format!("unknown context `[{}]`", table.name),
                ));
                continue;
            };
            for entry in &table.entries {
                let Value::String(id) = &entry.value else {
                    warnings.push(ConfigWarning::new(
                        file,
                        entry.line,
                        format!(
                            "`{}`: expected a command id (a string), found {}",
                            entry.key,
                            entry.value.type_name()
                        ),
                    ));
                    continue;
                };
                let seq = match parse_sequence(&entry.key) {
                    Ok(seq) => seq,
                    Err(e) => {
                        warnings.push(ConfigWarning::new(file, entry.line, e.to_string()));
                        continue;
                    }
                };
                // An empty value unbinds — the only way to take a default away.
                if id.is_empty() {
                    if self.unbind(context, &seq) == 0 {
                        warnings.push(ConfigWarning::new(
                            file,
                            entry.line,
                            format!("`{}` was not bound in [{}]", entry.key, context.name()),
                        ));
                    }
                    continue;
                }
                let Some(command) = Command::from_id(id) else {
                    warnings.push(ConfigWarning::new(
                        file,
                        entry.line,
                        KeymapError::UnknownCommand(id.clone()).to_string(),
                    ));
                    continue;
                };
                let description = self.default_description(command);
                self.unbind(context, &seq);
                if let Err(e) = self.register(context, seq, command, description, When::Always) {
                    warnings.push(ConfigWarning::new(file, entry.line, e.to_string()));
                }
            }
        }
        warnings
    }

    /// Read `<dir>/keymap.toml` over the defaults. A missing file is the normal
    /// case and says nothing (PLAN §3).
    pub fn apply_overrides_from_dir(&mut self, dir: &Path) -> Vec<ConfigWarning> {
        let path = dir.join("keymap.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => self.apply_overrides(&text, &path),
            Err(_) => Vec::new(),
        }
    }

    /// Rebuild the `g <key>` rows from a bookmark table (PLAN §3's `[goto]`).
    /// The defaults register the shipped table; this is what a user's own
    /// `[goto]` runs through. Returns a warning per bookmark whose key will not
    /// parse.
    pub fn apply_bookmarks(
        &mut self,
        bookmarks: &[crate::config::Bookmark],
        file: &Path,
    ) -> Vec<ConfigWarning> {
        self.unbind_command(|c| matches!(c, Command::Goto(_)));
        let mut warnings = Vec::new();
        for (i, bookmark) in bookmarks.iter().enumerate() {
            if i > u8::MAX as usize {
                warnings.push(ConfigWarning::new(
                    file,
                    0,
                    "more than 256 goto bookmarks; the rest are ignored",
                ));
                break;
            }
            let text = format!("g {}", bookmark.key);
            match parse_sequence(&text) {
                Ok(seq) => {
                    self.unbind(Context::Files, &seq);
                    if let Err(e) = self.register(
                        Context::Files,
                        seq,
                        Command::Goto(i as u8),
                        bookmark.description.clone(),
                        When::Always,
                    ) {
                        warnings.push(ConfigWarning::new(file, 0, e.to_string()));
                    }
                }
                Err(e) => warnings.push(ConfigWarning::new(file, 0, e.to_string())),
            }
        }
        warnings
    }

    /// The description a command already carries somewhere in the table, so a
    /// user override inherits it instead of showing a bare id in which-key.
    fn default_description(&self, command: Command) -> String {
        self.bindings
            .iter()
            .find(|b| b.command == command)
            .map(|b| b.description.clone())
            .unwrap_or_else(|| command.id())
    }
}

/// Whether this keystroke is one of the hard-reserved transport presses.
/// Unshifted and unmodified only — see [`RESERVED_TRANSPORT_KEYS`].
fn is_reserved(chord: Chord) -> bool {
    chord.mods.is_none() && RESERVED_TRANSPORT_KEYS.contains(&chord.key)
}

#[cfg(test)]
mod tests;
