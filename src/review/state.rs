//! Review comments and viewed marks: what they are, where they are kept
//! between runs, and the Markdown a review is copied as.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;

use karet_diff::DiffLine;
use karet_diff::FileDiff;
use karet_diff::LineKind;
use serde::Deserialize;
use serde::Serialize;

use super::git::Source;

/// What a comment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Issue,
    Suggestion,
    Note,
    Praise,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Suggestion => "suggestion",
            Self::Note => "note",
            Self::Praise => "praise",
        }
    }

    /// The next kind, for Tab in the comment box.
    pub fn next(self) -> Self {
        match self {
            Self::Issue => Self::Suggestion,
            Self::Suggestion => Self::Note,
            Self::Note => Self::Praise,
            Self::Praise => Self::Issue,
        }
    }
}

/// Which side of the diff a line number counts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// The old text (a removed line).
    Old,
    /// The new text (an added or unchanged line).
    New,
}

/// A comment on a line or a range of lines of one side of a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub path: String,
    pub side: Side,
    /// First and last line (1-based, inclusive).
    pub start: u32,
    pub end: u32,
    /// The text of the first line, to find it again when the diff moves.
    pub anchor: String,
    pub kind: Kind,
    pub body: String,
}

impl Comment {
    pub fn lines_label(&self) -> String {
        if self.start == self.end {
            format!("line {}", self.start)
        } else {
            format!("lines {}-{}", self.start, self.end)
        }
    }
}

/// The review of one source.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceState {
    pub comments: Vec<Comment>,
    /// Files marked viewed, with a fingerprint of their diff at the time: a
    /// file that changed since is no longer viewed.
    pub viewed: HashMap<String, u64>,
}

/// Everything kept for one repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Saved {
    pub last_source: Option<Source>,
    pub working_tree: SourceState,
    pub branch: SourceState,
    /// Reviews of commits and pull requests, by [`Source::key`].
    #[serde(default)]
    pub others: HashMap<String, SourceState>,
}

impl Saved {
    pub fn source(&self, source: &Source) -> Option<&SourceState> {
        match source {
            Source::WorkingTree => Some(&self.working_tree),
            Source::Branch => Some(&self.branch),
            other => self.others.get(&other.key()),
        }
    }

    pub fn source_mut(&mut self, source: &Source) -> &mut SourceState {
        match source {
            Source::WorkingTree => &mut self.working_tree,
            Source::Branch => &mut self.branch,
            other => self.others.entry(other.key()).or_default(),
        }
    }
}

/// Where a repository's review is kept: outside the project, under the
/// user's state directory, named after the repository path.
pub fn state_file(root: &Path) -> Option<PathBuf> {
    let dir = dirs::state_dir().or_else(dirs::data_local_dir)?;
    let name = root.file_name().map_or_else(
        || "root".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    Some(
        dir.join("sanitty")
            .join(format!("review-{name}-{:016x}.json", hasher.finish())),
    )
}

/// Where the last copied review is also written, next to the state.
pub fn export_file(root: &Path) -> Option<PathBuf> {
    let state = state_file(root)?;
    if let Some(dir) = state.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    Some(state.with_extension("md"))
}

pub fn load(root: &Path) -> Saved {
    state_file(root)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Write the state; a failure is reported but loses nothing in memory.
pub fn save(root: &Path, saved: &Saved) -> Result<(), String> {
    let path = state_file(root).ok_or("no state directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    let json = serde_json::to_string_pretty(saved).map_err(|err| err.to_string())?;
    std::fs::write(&path, json).map_err(|err| err.to_string())
}

/// A fingerprint of a file's diff, so a viewed mark lapses when it changes.
pub fn fingerprint(file: &FileDiff) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file.path.hash(&mut hasher);
    for hunk in &file.hunks {
        for line in &hunk.lines {
            line.content.hash(&mut hasher);
            matches!(line.kind, LineKind::Add).hash(&mut hasher);
            matches!(line.kind, LineKind::Remove).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// The line number of `line` on `side`, if it is on that side.
pub fn line_on(line: &DiffLine, side: Side) -> Option<u32> {
    match side {
        Side::Old => line.old_lineno.filter(|_| line.kind != LineKind::Add),
        Side::New => line.new_lineno.filter(|_| line.kind != LineKind::Remove),
    }
}

/// Move a comment to where its first line is now, nearest to where it was.
/// `None` when that line is no longer in the diff.
pub fn reanchor(comment: &Comment, file: &FileDiff) -> Option<Comment> {
    let found = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.content == comment.anchor)
        .filter_map(|line| line_on(line, comment.side))
        .min_by_key(|lineno| lineno.abs_diff(comment.start))?;
    let span = comment.end.saturating_sub(comment.start);
    let mut moved = comment.clone();
    moved.start = found;
    moved.end = found.saturating_add(span);
    Some(moved)
}

/// The diff lines a comment is about: its range on its side, plus the lines
/// it replaced (a comment on added lines also shows the removed lines right
/// before them, and the other way round).
pub fn commented_lines<'a>(comment: &Comment, file: &'a FileDiff) -> Vec<&'a DiffLine> {
    for hunk in &file.hunks {
        let inside = |line: &DiffLine| {
            line_on(line, comment.side)
                .is_some_and(|lineno| lineno >= comment.start && lineno <= comment.end)
        };
        let Some(first) = hunk.lines.iter().position(inside) else {
            continue;
        };
        let last = hunk.lines.iter().rposition(inside).unwrap_or(first);
        let (other, ours) = match comment.side {
            Side::New => (LineKind::Remove, LineKind::Add),
            Side::Old => (LineKind::Add, LineKind::Remove),
        };
        let mut from = first;
        if hunk.lines.get(first).is_some_and(|line| line.kind == ours) {
            while from > 0
                && hunk
                    .lines
                    .get(from - 1)
                    .is_some_and(|line| line.kind == other)
            {
                from -= 1;
            }
        }
        let mut until = last;
        if hunk.lines.get(last).is_some_and(|line| line.kind == ours) {
            while hunk
                .lines
                .get(until + 1)
                .is_some_and(|line| line.kind == other)
            {
                until += 1;
            }
        }
        return hunk
            .lines
            .get(from..=until)
            .map_or_else(Vec::new, |lines| lines.iter().collect());
    }
    Vec::new()
}

/// The review as Markdown: per file (in diff order), each comment with the
/// diff lines it is about.
pub fn export(detail: &str, files: &[FileDiff], comments: &[Comment]) -> String {
    let mut out = format!("# Review: {detail}\n");
    let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    for comment in comments {
        if !paths.contains(&comment.path.as_str()) {
            paths.push(&comment.path);
        }
    }
    for path in paths {
        let mut mine: Vec<&Comment> = comments
            .iter()
            .filter(|comment| comment.path == path)
            .collect();
        if mine.is_empty() {
            continue;
        }
        mine.sort_by_key(|comment| (comment.start, comment.side == Side::New));
        let _ = write!(out, "\n## {path}\n");
        let file = files.iter().find(|file| file.path == path);
        for comment in mine {
            let _ = write!(
                out,
                "\n**{}** ({}): {}\n",
                comment.kind.word(),
                comment.lines_label(),
                comment.body
            );
            let lines = file.map_or_else(Vec::new, |file| commented_lines(comment, file));
            if !lines.is_empty() {
                out.push_str("\n```diff\n");
                for line in lines {
                    let mark = match line.kind {
                        LineKind::Add => '+',
                        LineKind::Remove => '-',
                        LineKind::Context => ' ',
                    };
                    let _ = writeln!(out, "{mark}{}", line.content);
                }
                out.push_str("```\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "diff --git a/shop.py b/shop.py
--- a/shop.py
+++ b/shop.py
@@ -39,4 +39,5 @@ class Order:
 class Order:
     def total(self):
-        return sum(...)
+        return sum(
+            l.price for l in self.lines)

";

    fn file() -> Result<FileDiff, Box<dyn std::error::Error>> {
        karet_diff::parse(RAW)?
            .files
            .into_iter()
            .next()
            .ok_or_else(|| "no file".into())
    }

    fn comment(start: u32, end: u32, anchor: &str) -> Comment {
        Comment {
            path: "shop.py".into(),
            side: Side::New,
            start,
            end,
            anchor: anchor.into(),
            kind: Kind::Issue,
            body: "use Decimal".into(),
        }
    }

    #[test]
    fn export_shows_the_lines_a_comment_replaced() -> Result<(), Box<dyn std::error::Error>> {
        let file = file()?;
        let text = export(
            "working tree vs HEAD",
            &[file],
            &[comment(41, 42, "        return sum(")],
        );
        assert!(text.contains("## shop.py"));
        assert!(text.contains("**issue** (lines 41-42): use Decimal"));
        assert!(text.contains(
            "```diff\n-        return sum(...)\n+        return sum(\n+            l.price for l in self.lines)\n```"
        ));
        Ok(())
    }

    #[test]
    fn a_comment_follows_its_line() -> Result<(), Box<dyn std::error::Error>> {
        let file = file()?;
        // Saved when the line was further down; it is now line 41.
        let moved = reanchor(&comment(50, 51, "        return sum("), &file).ok_or("lost")?;
        assert_eq!((moved.start, moved.end), (41, 42));
        assert!(reanchor(&comment(41, 41, "gone"), &file).is_none());
        Ok(())
    }
}
