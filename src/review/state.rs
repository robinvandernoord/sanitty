//! Review comments and viewed marks: what they are, where they are kept
//! between runs, and the Markdown a review is copied as.

use std::collections::HashMap;
use std::fmt::Write as _;
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceState {
    pub comments: Vec<Comment>,
    /// Files marked viewed, with a fingerprint of their diff at the time: a
    /// file that changed since is no longer viewed.
    pub viewed: HashMap<String, u64>,
}

/// Everything kept for one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    crate::statefile::path("review", root, ".json")
}

/// Where the last copied review is also written, next to the state.
pub fn export_file(root: &Path) -> Option<PathBuf> {
    let state = state_file(root)?;
    if let Some(dir) = state.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    Some(state.with_extension("md"))
}

/// Read the state. A file that is there but does not parse is set aside
/// (renamed `...broken-<seconds>.json`, so nothing overwrites it) and the
/// message says where; the review then starts empty.
pub fn load(root: &Path) -> (Saved, Option<String>) {
    let Some(path) = state_file(root) else {
        return (Saved::default(), None);
    };
    let Ok(json) = std::fs::read_to_string(&path) else {
        return (Saved::default(), None);
    };
    match serde_json::from_str(&json) {
        Ok(saved) => (saved, None),
        Err(err) => {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let aside = path.with_extension(format!("broken-{seconds}.json"));
            let kept = std::fs::rename(&path, &aside).is_ok();
            let message = if kept {
                format!(
                    "The saved review could not be read ({err}); it is kept as {}",
                    aside.display()
                )
            } else {
                format!("The saved review could not be read ({err})")
            };
            (Saved::default(), Some(message))
        }
    }
}

/// Write the state in one step (never half a file). Another sanitty on the
/// same repository may have written since `baseline` (the state as this one
/// last read or wrote it): then its changes are kept and this one's go on
/// top, deletions included. Gives back what was written, which this one
/// then has too.
pub fn save(root: &Path, saved: &Saved, baseline: &Saved) -> Result<Saved, String> {
    let path = state_file(root).ok_or("no state directory")?;
    let on_disk: Option<Saved> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok());
    let merged = match on_disk {
        Some(disk) if disk != *baseline => merge(&disk, baseline, saved),
        _ => saved.clone(),
    };
    let json = serde_json::to_string_pretty(&merged).map_err(|err| err.to_string())?;
    crate::statefile::write_atomic(&path, &json).map_err(|err| err.to_string())?;
    Ok(merged)
}

/// What another instance wrote (`disk`) with this one's changes since
/// `base` applied: comments it added or removed, viewed marks it set or
/// cleared.
fn merge(disk: &Saved, base: &Saved, ours: &Saved) -> Saved {
    let mut out = disk.clone();
    out.last_source.clone_from(&ours.last_source);
    merge_source(
        &mut out.working_tree,
        &base.working_tree,
        &ours.working_tree,
    );
    merge_source(&mut out.branch, &base.branch, &ours.branch);
    let empty = SourceState::default();
    let mut keys: Vec<&String> = base.others.keys().chain(ours.others.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let target = out.others.entry(key.clone()).or_default();
        merge_source(
            target,
            base.others.get(key).unwrap_or(&empty),
            ours.others.get(key).unwrap_or(&empty),
        );
    }
    out.others
        .retain(|_, state| *state != SourceState::default());
    out
}

fn merge_source(out: &mut SourceState, base: &SourceState, ours: &SourceState) {
    for gone in base
        .comments
        .iter()
        .filter(|comment| !ours.comments.contains(comment))
    {
        if let Some(pos) = out.comments.iter().position(|comment| comment == gone) {
            out.comments.remove(pos);
        }
    }
    for added in ours
        .comments
        .iter()
        .filter(|comment| !base.comments.contains(comment))
    {
        if !out.comments.contains(added) {
            out.comments.push(added.clone());
        }
    }
    for (path, print) in &ours.viewed {
        if base.viewed.get(path) != Some(print) {
            out.viewed.insert(path.clone(), *print);
        }
    }
    for path in base
        .viewed
        .keys()
        .filter(|path| !ours.viewed.contains_key(*path))
    {
        out.viewed.remove(path);
    }
}

/// A fingerprint of a file's diff, so a viewed mark lapses when it changes.
/// The same in every build (see [`crate::statefile::StableHash`]).
pub fn fingerprint(file: &FileDiff) -> u64 {
    let mut hash = crate::statefile::StableHash::new().add(file.path.as_bytes());
    for hunk in &file.hunks {
        for line in &hunk.lines {
            let kind: &[u8] = match line.kind {
                LineKind::Add => b"+",
                LineKind::Remove => b"-",
                LineKind::Context => b" ",
            };
            hash = hash.add(kind).add(line.content.as_bytes());
        }
    }
    hash.finish()
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

    fn note(body: &str) -> Comment {
        Comment {
            path: "a.rs".into(),
            side: Side::New,
            start: 1,
            end: 1,
            anchor: "fn main() {".into(),
            kind: Kind::Note,
            body: body.into(),
        }
    }

    /// Two sanitty instances on one repository: neither loses the other's
    /// comments, and a deletion stays deleted.
    #[test]
    fn saving_merges_with_what_another_instance_wrote() {
        let mut base = Saved::default();
        base.working_tree.comments.push(note("old"));
        base.working_tree.viewed.insert("a.rs".into(), 7);
        // The other instance added B and saved.
        let mut disk = base.clone();
        disk.working_tree.comments.push(note("B"));
        // This one added A, deleted "old" and cleared the viewed mark.
        let mut ours = base.clone();
        ours.working_tree.comments.retain(|kept| kept.body != "old");
        ours.working_tree.comments.push(note("A"));
        ours.working_tree.viewed.clear();
        let merged = merge(&disk, &base, &ours);
        let bodies: Vec<&str> = merged
            .working_tree
            .comments
            .iter()
            .map(|kept| kept.body.as_str())
            .collect();
        assert_eq!(bodies, ["B", "A"]);
        assert!(merged.working_tree.viewed.is_empty());
    }

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
