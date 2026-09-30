//! Waiting on the child's pipes with a deadline, on Windows, where a pipe
//! cannot be polled: a thread per pipe does the blocking, and the connection
//! thread waits on channels with the same deadlines as Unix's `poll`
//! (`04-windows.md`, "thread-per-pipe").
//!
//! `df-sftp-out` and `df-sftp-err` each loop a blocking `read` on their pipe
//! and send what arrives over a bounded channel (64 chunks; stdout's of up to
//! the transport's read size), so [`Pipes::read`] is a `select` over the two
//! with the time that is left. `df-sftp-in` takes one buffer at a time and
//! writes it whole, answering when it is done, so [`Pipes::write`] waits for
//! that answer with the time that is left and a write the child has stopped
//! draining times out like a read — the blocked thread is freed when the
//! transport kills the child, which closes the pipe under it.
//!
//! None of the threads is joined. Each ends on its own: a reader at the end
//! of its pipe or when nobody is listening, the writer when [`Pipes`] drops
//! its channel or its pipe breaks. Waiting for them could hang on a grandchild
//! that inherited a pipe, which is exactly the hang this design exists to
//! avoid. There is no `unsafe` here: the threads use `std`'s own blocking I/O.

use std::io::{self, Write as _};
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, never, select, Receiver, RecvTimeoutError, Sender};

/// How many chunks a reader thread may be ahead of the connection.
const CHUNKS: usize = 64;

/// Time left until `deadline`, or `None` if it has passed.
pub fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

/// What one wait on the child's stdout came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// Bytes were appended.
    Data,
    /// Stdout is closed: the child is gone.
    Eof,
    /// Nothing yet — the wait ran out, or only stderr spoke. The caller
    /// checks its deadline and asks again.
    Nothing,
}

/// What one wait on the child's stdin came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    /// This many bytes went — here always the whole buffer.
    Bytes(usize),
    /// Not yet written when the wait ran out.
    Nothing,
    /// The pipe is broken: the child exited, and its stderr says why.
    Broken,
}

/// The child's three pipes, each behind its thread, and what it has said on
/// stderr.
pub struct Pipes {
    to_stdin: Sender<Vec<u8>>,
    written: Receiver<io::Result<()>>,
    out: Receiver<io::Result<Vec<u8>>>,
    err: Receiver<io::Result<Vec<u8>>>,
    errbuf: Vec<u8>,
    stderr_cap: usize,
    stderr_done: bool,
    /// The length of a buffer handed to the writer and not yet answered for.
    in_flight: Option<usize>,
}

impl Pipes {
    /// Take the child's pipes and start their threads: stdout read
    /// `read_size` bytes at a time, stderr kept up to `stderr_cap` bytes.
    /// Fails when a thread cannot be started. `label` is Unix's, for its log
    /// line; nothing is logged here.
    pub fn open(
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: ChildStderr,
        read_size: usize,
        stderr_cap: usize,
        _label: &str,
    ) -> io::Result<Pipes> {
        let (to_stdin, from_pipes) = bounded::<Vec<u8>>(1);
        let (answer, written) = bounded(1);
        let (out_tx, out) = bounded(CHUNKS);
        let (err_tx, err) = bounded(CHUNKS);
        std::thread::Builder::new()
            .name("df-sftp-in".to_string())
            .spawn(move || feed(stdin, &from_pipes, &answer))?;
        std::thread::Builder::new()
            .name("df-sftp-out".to_string())
            .spawn(move || pump(stdout, &out_tx, read_size))?;
        std::thread::Builder::new()
            .name("df-sftp-err".to_string())
            .spawn(move || pump(stderr, &err_tx, 4096))?;
        Ok(Pipes {
            to_stdin,
            written,
            out,
            err,
            errbuf: Vec::new(),
            stderr_cap,
            stderr_done: false,
            in_flight: None,
        })
    }

    /// Everything the child has said on stderr so far, up to the cap.
    pub fn stderr(&self) -> &[u8] {
        &self.errbuf
    }

    /// Wait up to `left` for stdout, collecting stderr as it arrives, and
    /// append what came to `into`.
    pub fn read(&mut self, into: &mut Vec<u8>, left: Duration) -> io::Result<Read> {
        let err = if self.stderr_done {
            never()
        } else {
            self.err.clone()
        };
        select! {
            recv(self.out) -> got => match got {
                Ok(Ok(chunk)) => {
                    into.extend_from_slice(&chunk);
                    Ok(Read::Data)
                }
                Ok(Err(e)) => Err(e),
                // The reader ended: the end of the pipe.
                Err(_) => Ok(Read::Eof),
            },
            recv(err) -> got => {
                self.heard(got.ok());
                Ok(Read::Nothing)
            },
            default(left) => Ok(Read::Nothing),
        }
    }

    /// Hand `bytes` to the writer — once; a call that follows a wait that ran
    /// out waits for the same buffer — and wait up to `left` for it to be
    /// written.
    pub fn write(&mut self, bytes: &[u8], left: Duration) -> io::Result<Wrote> {
        if self.in_flight.is_none() {
            if self.to_stdin.send(bytes.to_vec()).is_err() {
                // The writer has stopped: its pipe broke on an earlier write.
                return Ok(Wrote::Broken);
            }
            self.in_flight = Some(bytes.len());
        }
        match self.written.recv_timeout(left) {
            Ok(Ok(())) => Ok(Wrote::Bytes(self.in_flight.take().unwrap_or(0))),
            Ok(Err(e)) => {
                self.in_flight = None;
                if e.kind() == io::ErrorKind::BrokenPipe {
                    Ok(Wrote::Broken)
                } else {
                    Err(e)
                }
            }
            Err(RecvTimeoutError::Timeout) => Ok(Wrote::Nothing),
            Err(RecvTimeoutError::Disconnected) => {
                self.in_flight = None;
                Ok(Wrote::Broken)
            }
        }
    }

    /// Give stderr until `deadline` to deliver the child's parting words after
    /// the connection has died, then stop caring.
    pub fn drain_stderr(&mut self, deadline: Instant) {
        while !self.stderr_done {
            let Some(left) = remaining(deadline) else {
                return;
            };
            match self.err.recv_timeout(left) {
                Ok(got) => self.heard(Some(got)),
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => self.stderr_done = true,
            }
        }
    }

    /// Keep what stderr said, up to the cap; its end, or an error reading it,
    /// is its last word.
    fn heard(&mut self, got: Option<io::Result<Vec<u8>>>) {
        match got {
            Some(Ok(chunk)) => {
                let room = self.stderr_cap.saturating_sub(self.errbuf.len());
                self.errbuf
                    .extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Some(Err(_)) | None => self.stderr_done = true,
        }
    }
}

/// A reader thread: read `from` in chunks of `size` and send each, until its
/// end (the channel then disconnects), an error (sent, then the end), or
/// nobody listening.
fn pump(mut from: impl io::Read, to: &Sender<io::Result<Vec<u8>>>, size: usize) {
    let mut buffer = vec![0u8; size];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                if to.send(Ok(buffer[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let _ = to.send(Err(e));
                return;
            }
        }
    }
}

/// The writer thread: write each buffer whole and say how it went, until
/// [`Pipes`] drops its end or a write fails.
fn feed(mut to: ChildStdin, buffers: &Receiver<Vec<u8>>, answer: &Sender<io::Result<()>>) {
    for buffer in buffers.iter() {
        let result = to.write_all(&buffer).and_then(|()| to.flush());
        let failed = result.is_err();
        if answer.send(result).is_err() || failed {
            return;
        }
    }
}
