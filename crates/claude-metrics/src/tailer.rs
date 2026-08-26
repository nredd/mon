//! An inode + offset checkpointed tailer for append-only JSONL files.
//!
//! Transcripts grow continuously and can reach tens of megabytes. Re-reading one on every
//! refresh would be wasteful, so the tailer remembers where it stopped and reads only what
//! has been appended since.
//!
//! Two things force a full re-read:
//! - **The inode changed.** The file was replaced rather than appended to, so the old
//!   offset points into a different file.
//! - **The file shrank.** It was truncated or rotated, so the old offset is past the end.

use std::{
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Tracks a read position within one append-only file.
#[derive(Clone, Debug)]
pub struct Tailer {
    path: PathBuf,
    offset: u64,
    inode: Option<u64>,
}

/// What a read did, beyond the lines it produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    /// Continued from the checkpoint.
    Appended,
    /// Started over, because the file was replaced or truncated.
    Restarted,
}

impl Tailer {
    /// Start tailing a path from the beginning.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            inode: None,
        }
    }

    /// Rebuild a tailer from a checkpoint taken in an earlier run.
    ///
    /// The checkpoint is not trusted: the next read re-stats the file and restarts from the
    /// top if the inode has changed or the file has shrunk below the offset, exactly as it
    /// would for a checkpoint taken a second ago. So a stale, corrupt, or hand-edited
    /// checkpoint costs a re-read rather than wrong data.
    #[must_use]
    pub fn restore(path: impl Into<PathBuf>, offset: u64, inode: Option<u64>) -> Self {
        Self {
            path: path.into(),
            offset,
            inode,
        }
    }

    /// The file identity this tailer last saw, for checkpointing.
    #[must_use]
    pub fn inode(&self) -> Option<u64> {
        self.inode
    }

    /// The file being tailed.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes consumed so far.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Bytes appended but not yet read, or zero if the file cannot be stat'd.
    ///
    /// Used to size a cold read before it starts, so progress can be reported as a
    /// fraction rather than as an open-ended "still working".
    #[must_use]
    pub fn outstanding(&self) -> u64 {
        std::fs::metadata(&self.path).map_or(0, |meta| meta.len().saturating_sub(self.offset))
    }

    /// Read whatever has been appended since the last call.
    ///
    /// Returns complete lines only. A partial trailing line is left unconsumed so the next
    /// call picks it up once the writer finishes it -- without that, a line split across
    /// two reads would be parsed as two broken halves.
    ///
    /// A missing or unreadable file yields no lines rather than an error: transcripts
    /// appear and disappear as sessions come and go, and that is not a failure.
    pub fn read_new(&mut self) -> (Vec<String>, ReadKind) {
        self.read_filtered(u64::MAX, |_| true)
    }

    /// Read appended lines, consuming at most `max_bytes` and keeping only what `keep`
    /// accepts.
    ///
    /// `keep` sees the raw bytes before any UTF-8 conversion, which is where the win is: a
    /// transcript is overwhelmingly lines with no usage object on them, and rejecting those
    /// on a substring test skips both the allocation and the JSON parse. On a real tree
    /// that is most of the bytes.
    ///
    /// `max_bytes` bounds one call without losing anything -- the offset advances over
    /// everything consumed, so the next call resumes where this one stopped. It is what
    /// lets a cold read of a very large tree be spread over several passes and report
    /// progress instead of blocking until it is done.
    pub fn read_filtered<F>(&mut self, max_bytes: u64, keep: F) -> (Vec<String>, ReadKind)
    where
        F: Fn(&[u8]) -> bool,
    {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return (Vec::new(), ReadKind::Appended);
        };

        let inode = file_id(&metadata);
        let len = metadata.len();

        // Replaced file, or truncated below our checkpoint: the offset is meaningless.
        let restarted = match self.inode {
            Some(previous) if Some(previous) != inode => true,
            _ => len < self.offset,
        };

        if restarted {
            self.offset = 0;
        }
        self.inode = inode;

        let kind = if restarted {
            ReadKind::Restarted
        } else {
            ReadKind::Appended
        };

        if len == self.offset {
            return (Vec::new(), kind);
        }

        let Ok(file) = File::open(&self.path) else {
            return (Vec::new(), kind);
        };

        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return (Vec::new(), kind);
        }

        let mut lines = Vec::new();
        let mut buffer = Vec::new();
        let mut consumed = 0u64;

        loop {
            if consumed >= max_bytes {
                break;
            }

            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                // EOF, or a read error partway through a live file. Either way there is
                // nothing more to hand over this round; the offset stays where it is.
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if buffer.last() != Some(&b'\n') {
                        // Partial trailing line: leave it for next time.
                        break;
                    }

                    self.offset = self.offset.saturating_add(read as u64);
                    consumed = consumed.saturating_add(read as u64);

                    if keep(&buffer) {
                        lines.push(String::from_utf8_lossy(&buffer).trim_end().to_owned());
                    }
                }
            }
        }

        (lines, kind)
    }
}

// The `Option` is load-bearing despite this arm always returning `Some`: the non-unix arm
// below has no inode to report and returns `None`.
#[allow(clippy::unnecessary_wraps)]
#[cfg(unix)]
fn file_id(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn file_id(_metadata: &std::fs::Metadata) -> Option<u64> {
    // Without an inode there is no way to notice a replacement, so every read is treated
    // as an append and only truncation triggers a restart.
    None
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::PathBuf,
    };

    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("claude-metrics-tailer-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn append(path: &Path, text: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn only_new_lines_come_back_on_each_read() {
        let dir = tempdir("append");
        let path = dir.join("t.jsonl");

        append(&path, "one\ntwo\n");
        let mut tailer = Tailer::new(&path);

        let (lines, kind) = tailer.read_new();
        assert_eq!(lines, vec!["one", "two"]);
        assert_eq!(kind, ReadKind::Appended);

        let (lines, _) = tailer.read_new();
        assert!(lines.is_empty(), "nothing new means nothing returned");

        append(&path, "three\n");
        let (lines, _) = tailer.read_new();
        assert_eq!(lines, vec!["three"], "only the appended line");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_partial_trailing_line_is_held_until_it_is_complete() {
        let dir = tempdir("partial");
        let path = dir.join("t.jsonl");

        append(&path, "complete\npar");
        let mut tailer = Tailer::new(&path);

        let (lines, _) = tailer.read_new();
        assert_eq!(
            lines,
            vec!["complete"],
            "a half-written line must not be handed over"
        );

        append(&path, "tial\n");
        let (lines, _) = tailer.read_new();
        assert_eq!(lines, vec!["partial"], "and must arrive whole afterwards");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_truncated_file_is_re_read_from_the_start() {
        let dir = tempdir("truncate");
        let path = dir.join("t.jsonl");

        append(&path, "a\nb\nc\n");
        let mut tailer = Tailer::new(&path);
        assert_eq!(tailer.read_new().0.len(), 3);

        fs::write(&path, "x\n").unwrap();
        let (lines, kind) = tailer.read_new();
        assert_eq!(kind, ReadKind::Restarted);
        assert_eq!(lines, vec!["x"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_file_of_the_same_length_is_still_noticed() {
        // The case a length check alone cannot catch: same size, different inode.
        let dir = tempdir("replace");
        let path = dir.join("t.jsonl");

        append(&path, "aaa\n");
        let mut tailer = Tailer::new(&path);
        assert_eq!(tailer.read_new().0, vec!["aaa"]);

        let replacement = dir.join("other.jsonl");
        append(&replacement, "bbb\n");
        fs::rename(&replacement, &path).unwrap();

        let (lines, kind) = tailer.read_new();
        assert_eq!(
            kind,
            ReadKind::Restarted,
            "the inode changed, so the offset is meaningless"
        );
        assert_eq!(lines, vec!["bbb"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let mut tailer = Tailer::new("/definitely/not/a/real/path.jsonl");
        let (lines, _) = tailer.read_new();
        assert!(lines.is_empty());
    }
}
