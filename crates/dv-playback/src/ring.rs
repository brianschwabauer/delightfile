//! Lock-free SPSC audio ring buffer (§5): the audio render thread pushes,
//! the cpal callback pops — the callback never allocates or locks.
//!
//! Samples are stored as `AtomicU32` bit patterns with relaxed loads/stores
//! (cursor acquire/release ordering publishes them), which keeps the whole
//! thing safe Rust; at 48 kHz stereo the atomic traffic is negligible.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Interleaved stereo f32 ring. Capacity is rounded up to a power of two.
pub struct AudioRing {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// Write cursor in samples (monotonic; wraps via mask).
    head: AtomicUsize,
    /// Read cursor in samples (monotonic; wraps via mask).
    tail: AtomicUsize,
    /// Total sample **frames** popped since creation — the master clock's
    /// input (§4.4: playback position = samples consumed).
    consumed_frames: AtomicU64,
    /// Writer requests a flush; the reader drains and clears it (see
    /// [`AudioRing::flush`] for the handshake).
    flush: AtomicBool,
}

impl AudioRing {
    /// `capacity_samples`: interleaved sample capacity (frames × 2).
    pub fn new(capacity_samples: usize) -> AudioRing {
        let cap = capacity_samples.next_power_of_two();
        let buf = (0..cap).map(|_| AtomicU32::new(0)).collect::<Vec<_>>();
        AudioRing {
            buf: buf.into_boxed_slice(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            consumed_frames: AtomicU64::new(0),
            flush: AtomicBool::new(false),
        }
    }

    /// Samples currently readable.
    pub fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Free space in samples.
    pub fn free(&self) -> usize {
        self.buf.len() - self.len()
    }

    /// Writer: append samples; returns how many were accepted (the rest are
    /// dropped if the ring is full — callers wait on `free()` instead).
    pub fn push(&self, samples: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let n = samples.len().min(self.free());
        for (i, &s) in samples[..n].iter().enumerate() {
            self.buf[(head.wrapping_add(i)) & self.mask].store(s.to_bits(), Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Reader (cpal callback): pop up to `out.len()` samples; returns count.
    /// Also services a pending flush by draining everything first.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        if self.flush.swap(false, Ordering::AcqRel) {
            // Drop whatever is buffered; the writer is waiting on this.
            self.tail
                .store(self.head.load(Ordering::Acquire), Ordering::Release);
            return 0;
        }
        let tail = self.tail.load(Ordering::Relaxed);
        let n = out.len().min(self.len());
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(
                self.buf[(tail.wrapping_add(i)) & self.mask].load(Ordering::Relaxed),
            );
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        self.consumed_frames
            .fetch_add((n / 2) as u64, Ordering::Relaxed);
        n
    }

    /// Total sample frames ever consumed (monotonic).
    pub fn consumed(&self) -> u64 {
        self.consumed_frames.load(Ordering::Relaxed)
    }

    /// Writer: discard all buffered audio (seek/pause/source change). Sets the
    /// flush flag and waits briefly for the reader to service it; if the
    /// stream is stalled/absent, drains from this side instead — with no live
    /// reader there is no race to lose.
    pub fn flush(&self) {
        self.flush.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        while self.flush.load(Ordering::Acquire) {
            if std::time::Instant::now() > deadline {
                self.flush.store(false, Ordering::Release);
                self.tail
                    .store(self.head.load(Ordering::Acquire), Ordering::Release);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_roundtrip() {
        let ring = AudioRing::new(16);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0]), 4);
        let mut out = [0.0f32; 4];
        assert_eq!(ring.pop(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.consumed(), 2); // 2 stereo frames
        assert!(ring.is_empty());
    }

    #[test]
    fn wraps_and_respects_capacity() {
        let ring = AudioRing::new(8); // pow2 already
        let data: Vec<f32> = (0..10).map(|i| i as f32).collect();
        assert_eq!(ring.push(&data), 8); // full
        let mut out = [0.0f32; 6];
        assert_eq!(ring.pop(&mut out), 6);
        assert_eq!(ring.push(&data), 6); // wrapped write
        let mut rest = [0.0f32; 8];
        assert_eq!(ring.pop(&mut rest), 8);
        assert_eq!(&rest[..2], &[6.0, 7.0]);
        assert_eq!(&rest[2..], &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn flush_without_reader_drains() {
        let ring = AudioRing::new(16);
        ring.push(&[1.0; 8]);
        ring.flush();
        assert!(ring.is_empty());
    }

    #[test]
    fn reader_services_flush() {
        let ring = AudioRing::new(16);
        ring.push(&[1.0; 8]);
        ring.flush.store(true, Ordering::Release);
        let mut out = [0.0f32; 4];
        assert_eq!(ring.pop(&mut out), 0); // flush pass drains, returns nothing
        assert!(ring.is_empty());
        assert_eq!(ring.consumed(), 0);
    }
}
