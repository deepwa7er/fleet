//! Git-format diff parsing at the domain boundary.
//!
//! Clients receive files, hunks, and numbered lines. They never parse patch
//! text or try to re-fit annotations in the browser.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub files: Vec<DiffFile>,
}

impl Diff {
    pub fn contains_anchor(&self, path: &str, side: crate::AnnotationSide, line: u32) -> bool {
        self.files.iter().any(|file| {
            let path_matches = match side {
                crate::AnnotationSide::Old => file.old_path.as_deref() == Some(path),
                crate::AnnotationSide::New => file.new_path.as_deref() == Some(path),
            };
            path_matches
                && file
                    .hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .any(|candidate| match side {
                        crate::AnnotationSide::Old => candidate.old_line == Some(line),
                        crate::AnnotationSide::New => candidate.new_line == Some(line),
                    })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub heading: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    pub no_newline: bool,
}

pub fn parse_diff(text: &str) -> Diff {
    let mut files = Vec::new();
    let mut file = None;
    let mut hunk = None;
    let mut old_line = 0;
    let mut new_line = 0;

    for raw in text.lines() {
        if raw.starts_with("diff --git ") {
            finish_hunk(&mut file, &mut hunk);
            finish_file(&mut files, &mut file);
            let (old_path, new_path) = diff_header_paths(raw).unwrap_or((None, None));
            file = Some(DiffFile {
                old_path,
                new_path,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(current_file) = file.as_mut() else {
            continue;
        };
        if raw.starts_with("Binary files ") || raw == "GIT binary patch" {
            current_file.binary = true;
            continue;
        }
        if let Some(path) = raw.strip_prefix("--- ") {
            current_file.old_path = patch_path(path);
            continue;
        }
        if let Some(path) = raw.strip_prefix("+++ ") {
            current_file.new_path = patch_path(path);
            continue;
        }
        if raw.starts_with("@@ ") {
            finish_hunk(&mut file, &mut hunk);
            if let Some(parsed) = parse_hunk_header(raw) {
                old_line = parsed.old_start;
                new_line = parsed.new_start;
                hunk = Some(parsed);
            }
            continue;
        }
        let Some(current_hunk) = hunk.as_mut() else {
            continue;
        };
        if raw == "\\ No newline at end of file" {
            if let Some(previous) = current_hunk.lines.last_mut() {
                previous.no_newline = true;
            }
            continue;
        }
        let Some((prefix, content)) = raw.split_at_checked(1) else {
            continue;
        };
        let line = match prefix {
            " " => {
                let line = DiffLine {
                    kind: DiffKind::Context,
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    text: content.to_owned(),
                    no_newline: false,
                };
                old_line += 1;
                new_line += 1;
                line
            }
            "-" => {
                let line = DiffLine {
                    kind: DiffKind::Deletion,
                    old_line: Some(old_line),
                    new_line: None,
                    text: content.to_owned(),
                    no_newline: false,
                };
                old_line += 1;
                line
            }
            "+" => {
                let line = DiffLine {
                    kind: DiffKind::Addition,
                    old_line: None,
                    new_line: Some(new_line),
                    text: content.to_owned(),
                    no_newline: false,
                };
                new_line += 1;
                line
            }
            _ => continue,
        };
        current_hunk.lines.push(line);
    }
    finish_hunk(&mut file, &mut hunk);
    finish_file(&mut files, &mut file);
    Diff { files }
}

fn finish_hunk(file: &mut Option<DiffFile>, hunk: &mut Option<DiffHunk>) {
    if let (Some(file), Some(hunk)) = (file.as_mut(), hunk.take()) {
        file.hunks.push(hunk);
    }
}

fn finish_file(files: &mut Vec<DiffFile>, file: &mut Option<DiffFile>) {
    if let Some(file) = file.take() {
        files.push(file);
    }
}

fn diff_header_paths(line: &str) -> Option<(Option<String>, Option<String>)> {
    let rest = line.strip_prefix("diff --git ")?;
    if let Some(rest) = rest.strip_prefix("a/") {
        let boundary = rest.find(" b/")?;
        return Some((
            Some(rest[..boundary].to_owned()),
            Some(rest[boundary + 3..].to_owned()),
        ));
    }
    let (old, consumed) = quoted_token(rest)?;
    let (new, _) = quoted_token(rest[consumed..].trim_start())?;
    Some((patch_path(&old), patch_path(&new)))
}

fn patch_path(raw: &str) -> Option<String> {
    let decoded;
    let path = if raw.starts_with('"') {
        decoded = quoted_token(raw)?.0;
        decoded.as_str()
    } else {
        raw.split('\t').next().unwrap_or(raw)
    };
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_owned(),
    )
}

fn quoted_token(raw: &str) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut output = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                return Some((String::from_utf8_lossy(&output).into_owned(), index + 1));
            }
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                match escaped {
                    b'0'..=b'7' => {
                        let mut value = 0u16;
                        let mut digits = 0;
                        while digits < 3
                            && index < bytes.len()
                            && matches!(bytes[index], b'0'..=b'7')
                        {
                            value = value * 8 + u16::from(bytes[index] - b'0');
                            digits += 1;
                            index += 1;
                        }
                        output.push(value as u8);
                        continue;
                    }
                    b't' => output.push(b'\t'),
                    b'n' => output.push(b'\n'),
                    b'r' => output.push(b'\r'),
                    b'b' => output.push(8),
                    b'f' => output.push(12),
                    b'v' => output.push(11),
                    other => output.push(other),
                }
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    None
}

fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, rest) = rest.split_once(" @@")?;
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    let heading = rest
        .strip_prefix(' ')
        .filter(|heading| !heading.is_empty())
        .map(str::to_owned);
    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        heading,
        lines: Vec::new(),
    })
}

fn range(raw: &str) -> Option<(u32, u32)> {
    let (start, count) = raw.split_once(',').unwrap_or((raw, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = concat!(
        "diff --git a/old name.txt b/new name.txt\n",
        "similarity index 80%\n",
        "rename from old name.txt\n",
        "rename to new name.txt\n",
        "--- a/old name.txt\n",
        "+++ b/new name.txt\n",
        "@@ -2,2 +2,3 @@ a heading\n",
        " same\n",
        "-gone\n",
        "+new\n",
        "+last\n",
        "\\ No newline at end of file\n",
    );

    #[test]
    fn parses_paths_hunks_numbers_and_missing_newline() {
        let diff = parse_diff(PATCH);
        let file = &diff.files[0];
        assert_eq!(file.old_path.as_deref(), Some("old name.txt"));
        assert_eq!(file.new_path.as_deref(), Some("new name.txt"));
        let hunk = &file.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (2, 2));
        assert_eq!((hunk.new_start, hunk.new_count), (2, 3));
        assert_eq!(hunk.heading.as_deref(), Some("a heading"));
        assert_eq!(hunk.lines[0].old_line, Some(2));
        assert_eq!(hunk.lines[0].new_line, Some(2));
        assert_eq!(hunk.lines[1].kind, DiffKind::Deletion);
        assert_eq!(hunk.lines[2].new_line, Some(3));
        assert!(hunk.lines[3].no_newline);
    }

    #[test]
    fn validates_anchors_on_the_correct_side_and_path() {
        let diff = parse_diff(PATCH);
        assert!(diff.contains_anchor("old name.txt", crate::AnnotationSide::Old, 3));
        assert!(diff.contains_anchor("new name.txt", crate::AnnotationSide::New, 4));
        assert!(!diff.contains_anchor("new name.txt", crate::AnnotationSide::Old, 3));
        assert!(!diff.contains_anchor("new name.txt", crate::AnnotationSide::New, 99));
    }

    #[test]
    fn recognizes_additions_deletions_and_binary_files() {
        let diff = parse_diff(concat!(
            "diff --git a/new.bin b/new.bin\n",
            "new file mode 100644\n",
            "Binary files /dev/null and b/new.bin differ\n",
            "diff --git a/gone b/gone\n",
            "--- a/gone\n",
            "+++ /dev/null\n",
            "@@ -1 +0,0 @@\n",
            "-bye\n",
        ));
        assert!(diff.files[0].binary);
        assert_eq!(diff.files[1].new_path, None);
        assert_eq!(diff.files[1].hunks[0].lines[0].new_line, None);
    }

    #[test]
    fn decodes_git_quoted_paths_including_octal_utf8() {
        let diff = parse_diff(concat!(
            "diff --git \"a/tab\\tand-\\303\\251.txt\" \"b/tab\\tand-\\303\\251.txt\"\n",
            "--- \"a/tab\\tand-\\303\\251.txt\"\n",
            "+++ \"b/tab\\tand-\\303\\251.txt\"\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
        ));
        assert_eq!(diff.files[0].old_path.as_deref(), Some("tab\tand-é.txt"));
        assert_eq!(diff.files[0].new_path.as_deref(), Some("tab\tand-é.txt"));
    }
}
