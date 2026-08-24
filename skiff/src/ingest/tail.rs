//! Forward reads over an append-only file, resumable by byte watermark
//! (DW-004 §4).
//!
//! Two rules make the offset trustworthy:
//!
//! - **A file whose inode changed, or that is shorter than the watermark, is
//!   not the file the watermark describes.** It was rotated, replaced, or
//!   truncated, so the cursor is discarded and the read restarts from zero.
//! - **A partial trailing line is never consumed.** The offset advances only
//!   past complete, newline-terminated lines, so a read that lands mid-write
//!   re-reads that line next tick instead of parsing half of it.
//!
//! The second rule is why the offset can be trusted at all: without it, one
//! read racing a writer would permanently desynchronise the cursor.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};

use crate::store::Cursor;

/// The result of reading forward from a cursor.
pub struct Tail {
    /// Complete lines read, in file order, with line endings stripped.
    pub lines: Vec<String>,
    /// The index of `lines[0]` in the file. Line indices are how entries get a
    /// `seq` that is stable across reads.
    pub first_line: i64,
    /// Where the next read should resume.
    pub cursor: Cursor,
    /// The prior cursor did not describe this file, so the read started at
    /// zero and `lines` is the whole file. Callers holding derived state for
    /// this file must discard it before applying these lines.
    pub restarted: bool,
}

/// Read every complete line after `prior` in `path`.
pub fn read_forward(path: &Path, prior: Option<Cursor>) -> Result<Tail> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let meta = file.metadata().with_context(|| format!("stat {}", path.display()))?;
    let inode = inode_of(&meta);
    let len = meta.len();

    let resumable = prior.is_some_and(|c| c.inode == inode && c.offset <= len);
    let start = if resumable { prior.expect("checked").offset } else { 0 };
    let lines_before = if resumable { prior.expect("checked").lines } else { 0 };

    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(start))
        .with_context(|| format!("seeking {} to {start}", path.display()))?;

    let mut lines = Vec::new();
    let mut consumed = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        if !buf.ends_with(b"\n") {
            // A partial trailing line: the writer is mid-append. Leave the
            // offset before it so the next read sees the whole line.
            break;
        }
        consumed += read as u64;
        // LF, with an optional CR tolerated: pi writes LF, but the RPC framing
        // spec allows CRLF and a session file may be produced by either.
        let mut line = &buf[..buf.len() - 1];
        if line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        lines.push(String::from_utf8_lossy(line).into_owned());
    }

    let cursor =
        Cursor { inode, offset: start + consumed, lines: lines_before + lines.len() as i64 };
    Ok(Tail { lines, first_line: lines_before, cursor, restarted: !resumable })
}

#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode_of(_meta: &std::fs::Metadata) -> u64 {
    // Without an inode there is no way to tell a replaced file from an
    // appended one, so every read restarts. Correct, and slower; skiffd is a
    // Linux service (DW-004 §2) so this path is never the deployed one.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    fn append(path: &Path, contents: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn a_first_read_takes_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\ntwo\n");

        let tail = read_forward(&path, None).unwrap();
        assert_eq!(tail.lines, ["one", "two"]);
        assert!(tail.restarted);
        assert_eq!(tail.cursor.lines, 2);
        assert_eq!(tail.cursor.offset, 8);
    }

    #[test]
    fn a_resumed_read_takes_only_what_was_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\n");
        let first = read_forward(&path, None).unwrap();

        append(&path, "two\n");
        let second = read_forward(&path, Some(first.cursor)).unwrap();
        assert_eq!(second.lines, ["two"]);
        assert!(!second.restarted);
        assert_eq!(second.first_line, 1, "the appended line is line 1, not line 0");
        assert_eq!(second.cursor.lines, 2);
    }

    #[test]
    fn a_partial_trailing_line_is_left_for_the_next_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\n{\"hal");

        let first = read_forward(&path, None).unwrap();
        assert_eq!(first.lines, ["one"], "a half-written line must not be parsed");
        assert_eq!(first.cursor.offset, 4);

        append(&path, "f\":1}\n");
        let second = read_forward(&path, Some(first.cursor)).unwrap();
        assert_eq!(second.lines, [r#"{"half":1}"#], "and must arrive whole next time");
    }

    #[test]
    fn a_truncated_file_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\ntwo\nthree\n");
        let first = read_forward(&path, None).unwrap();

        write(&path, "fresh\n");
        let second = read_forward(&path, Some(first.cursor)).unwrap();
        assert!(second.restarted, "a file shorter than the watermark is a different file");
        assert_eq!(second.lines, ["fresh"]);
        assert_eq!(second.cursor.lines, 1);
    }

    #[test]
    fn a_replaced_file_restarts_even_at_the_same_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\n");
        let first = read_forward(&path, None).unwrap();

        // Same name, same length, different inode.
        std::fs::remove_file(&path).unwrap();
        write(&path, "two\n");
        let second = read_forward(&path, Some(first.cursor)).unwrap();
        assert!(second.restarted);
        assert_eq!(second.lines, ["two"]);
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "one\r\ntwo\r\n");
        assert_eq!(read_forward(&path, None).unwrap().lines, ["one", "two"]);
    }

    #[test]
    fn an_empty_file_reads_as_nothing_at_offset_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "");
        let tail = read_forward(&path, None).unwrap();
        assert!(tail.lines.is_empty());
        assert_eq!(tail.cursor.offset, 0);
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_read() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_forward(&dir.path().join("gone.jsonl"), None).is_err());
    }
}
