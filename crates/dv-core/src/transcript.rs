//! Transcript model (§18) and its `transcript.json` cache asset (§8.3).
//!
//! A transcript is a flat, time-ordered list of [`Word`]s with word-level
//! source-time stamps, produced by the whisper.cpp job in `dv-media` and
//! cached under the media content hash. Sentences are *derived*, not stored:
//! [`Transcript::sentences`] groups words at trailing sentence punctuation,
//! so the grouping can improve without a cache-format bump.
//!
//! # `transcript.json` format
//!
//! ```json
//! {"version":1,"language":"en","model":"ggml-base.en.bin",
//!  "words":[{"start_us":0,"end_us":420000,"text":"Hello,","prob":0.97}]}
//! ```
//!
//! Like every cache asset it is regenerable and safe to delete; readers return
//! `None` on missing/garbage/newer-version files and callers re-transcribe.

use std::fs;
use std::io;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The persisted cache format version. Readers refuse anything newer.
pub const FORMAT_VERSION: u32 = 1;

/// One recognized word, in **source time** (µs), text as whisper emitted it
/// (leading/trailing punctuation intact — display trims, matching strips).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
    /// Mean token probability, 0..=1 — the UI dims low-confidence words.
    pub prob: f32,
}

/// A whole media file's recognized speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Detected (or forced) language, e.g. `"en"`.
    pub language: String,
    /// Model file name the words came from (provenance, shown in inspector).
    pub model: String,
    /// Time-ordered, non-overlapping-ish words in source time.
    pub words: Vec<Word>,
}

/// On-disk wrapper adding the format version.
#[derive(Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    #[serde(flatten)]
    transcript: Transcript,
}

impl Transcript {
    /// Group words into sentences: each range ends after a word whose trimmed
    /// text ends in `.`, `!`, `?`, or `…` (ellipsis included — whisper emits
    /// it for trailing-off speech). A final unterminated run is its own
    /// sentence. Ranges are word-index ranges into `self.words`, contiguous
    /// and covering.
    pub fn sentences(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        for (i, w) in self.words.iter().enumerate() {
            let t = w.text.trim_end();
            if t.ends_with(['.', '!', '?', '…']) {
                out.push(start..i + 1);
                start = i + 1;
            }
        }
        if start < self.words.len() {
            out.push(start..self.words.len());
        }
        out
    }

    /// Index of the word whose span contains `t_us`, else the nearest word
    /// *starting at or before* `t_us` (so a playhead in a pause highlights the
    /// word just spoken). `None` only before the first word or when empty.
    pub fn word_at(&self, t_us: i64) -> Option<usize> {
        let i = self.words.partition_point(|w| w.start_us <= t_us);
        i.checked_sub(1)
    }

    /// Sentence range (from [`Self::sentences`]' grouping) containing word `i`.
    pub fn sentence_of(&self, i: usize) -> Range<usize> {
        self.sentences()
            .into_iter()
            .find(|r| r.contains(&i))
            .unwrap_or(i..i.min(self.words.len()))
    }
}

/// Is `text` (one word, as stored) a filler per `fillers`? Comparison strips
/// surrounding punctuation and lowercases, so `"Um,"` matches filler `"um"`.
pub fn is_filler(text: &str, fillers: &[String]) -> bool {
    let bare = text
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    !bare.is_empty() && fillers.iter().any(|f| f.eq_ignore_ascii_case(&bare))
}

/// Persist `t` at `path` atomically (temp sibling + rename, like every cache
/// asset writer).
pub fn write_transcript(path: &Path, t: &Transcript) -> io::Result<()> {
    let on_disk = OnDisk {
        version: FORMAT_VERSION,
        transcript: t.clone(),
    };
    let json = serde_json::to_string(&on_disk).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.part");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Load a transcript; `None` on missing, unreadable, garbage, or
/// newer-than-us files (callers treat all four as "not transcribed yet").
pub fn read_transcript(path: &Path) -> Option<Transcript> {
    let text = fs::read_to_string(path).ok()?;
    let on_disk: OnDisk = serde_json::from_str(&text).ok()?;
    (on_disk.version <= FORMAT_VERSION).then_some(on_disk.transcript)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    fn w(start_us: i64, end_us: i64, text: &str) -> Word {
        Word {
            start_us,
            end_us,
            text: text.into(),
            prob: 0.9,
        }
    }

    fn sample() -> Transcript {
        Transcript {
            language: "en".into(),
            model: "test".into(),
            words: vec![
                w(0, 300_000, "Hello,"),
                w(300_000, 600_000, "world."),
                w(1_000_000, 1_200_000, "Um,"),
                w(1_200_000, 1_500_000, "next"),
                w(1_500_000, 1_900_000, "sentence!"),
                w(2_000_000, 2_300_000, "Trailing"),
            ],
        }
    }

    #[test]
    fn sentence_grouping() {
        let t = sample();
        assert_eq!(t.sentences(), vec![0..2, 2..5, 5..6]);
        assert_eq!(t.sentence_of(3), 2..5);
        assert_eq!(t.sentence_of(5), 5..6);
    }

    #[test]
    fn sentences_empty_and_unterminated() {
        let mut t = sample();
        t.words.clear();
        assert!(t.sentences().is_empty());
        t.words = vec![w(0, 1, "no"), w(1, 2, "period")];
        assert_eq!(t.sentences(), vec![0..2]);
    }

    #[test]
    fn word_lookup() {
        let t = sample();
        assert_eq!(t.word_at(-1), None);
        assert_eq!(t.word_at(0), Some(0));
        assert_eq!(t.word_at(450_000), Some(1));
        // In the pause after "world." the last-spoken word stays current.
        assert_eq!(t.word_at(800_000), Some(1));
        assert_eq!(t.word_at(10_000_000), Some(5));
    }

    #[test]
    fn filler_matching() {
        let fillers = vec!["um".to_string(), "uh".to_string()];
        assert!(is_filler("Um,", &fillers));
        assert!(is_filler(" uh…", &fillers));
        assert!(!is_filler("umbrella", &fillers));
        assert!(!is_filler(",", &fillers));
    }

    #[test]
    fn roundtrip_and_version_gate() {
        let dir = std::env::temp_dir().join(format!("dv-transcript-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.json");
        let t = sample();
        write_transcript(&path, &t).unwrap();
        assert_eq!(read_transcript(&path).unwrap(), t);

        // Newer version refused → None (caller re-transcribes).
        let newer = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"version\":1", "\"version\":99");
        std::fs::write(&path, newer).unwrap();
        assert!(read_transcript(&path).is_none());

        std::fs::write(&path, "garbage").unwrap();
        assert!(read_transcript(&path).is_none());
        std::fs::remove_file(&path).unwrap();
        assert!(read_transcript(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
