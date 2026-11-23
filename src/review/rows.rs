//! A file's diff laid out as the rows the view shows: hunk headers, lines
//! (paired side by side, or one column), and comments under their lines.

use karet_diff::Cell;
use karet_diff::DiffLine;
use karet_diff::FileDiff;
use karet_diff::LineKind;

use super::state::Comment;
use super::state::Side;
use super::state::line_on;

pub enum Row {
    /// `@@ -a,b +c,d @@ scope`.
    Hunk(String),
    /// Side by side: the old line and the new line.
    Pair {
        left: Option<Cell>,
        right: Option<Cell>,
    },
    /// One column.
    Line(DiffLine),
    /// A comment, by its index in the source's comments.
    Comment(usize),
    /// A remark about the file (binary, nothing to show, lost comments).
    Note(String),
}

impl Row {
    /// The line a comment on this row is about: its side, number and text.
    pub fn anchor(&self) -> Option<(Side, u32, &str)> {
        match self {
            Self::Pair { left, right } => {
                if let Some(cell) = right.as_ref().filter(|cell| cell.kind != LineKind::Remove) {
                    Some((Side::New, cell.lineno, cell.content.as_str()))
                } else {
                    left.as_ref()
                        .filter(|cell| cell.kind == LineKind::Remove)
                        .map(|cell| (Side::Old, cell.lineno, cell.content.as_str()))
                }
            }
            Self::Line(line) => line_on(line, Side::New)
                .map(|lineno| (Side::New, lineno))
                .or_else(|| line_on(line, Side::Old).map(|lineno| (Side::Old, lineno)))
                .map(|(side, lineno)| (side, lineno, line.content.as_str())),
            _ => None,
        }
    }

    /// Every (side, line) this row shows, for placing comments under it.
    fn lines(&self) -> Vec<(Side, u32)> {
        match self {
            Self::Pair { left, right } => {
                let mut found = Vec::new();
                if let Some(cell) = left.as_ref().filter(|cell| cell.kind == LineKind::Remove) {
                    found.push((Side::Old, cell.lineno));
                }
                if let Some(cell) = right.as_ref().filter(|cell| cell.kind != LineKind::Remove) {
                    found.push((Side::New, cell.lineno));
                }
                found
            }
            Self::Line(line) => [Side::New, Side::Old]
                .into_iter()
                .filter_map(|side| line_on(line, side).map(|lineno| (side, lineno)))
                .take(1)
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// The rows for `file`, with each of `comments` (index, comment) for this
/// file under the last line it covers. Comments whose line is not in the diff
/// are listed first, under a note.
pub fn build(file: &FileDiff, comments: &[(usize, &Comment)], split: bool) -> Vec<Row> {
    let mut lines: Vec<Row> = Vec::new();
    if file.is_binary {
        lines.push(Row::Note("Binary file: no lines to show".into()));
    } else if file.hunks.is_empty() {
        lines.push(Row::Note(
            "No line changes (a rename or a mode change)".into(),
        ));
    }
    for hunk in &file.hunks {
        lines.push(Row::Hunk(hunk.header()));
        if split {
            lines.extend(
                karet_diff::align_hunk(&hunk.lines)
                    .into_iter()
                    .map(|row| Row::Pair {
                        left: row.left,
                        right: row.right,
                    }),
            );
        } else {
            lines.extend(hunk.lines.iter().cloned().map(Row::Line));
        }
    }
    let mut placed = vec![false; comments.len()];
    let mut rows = Vec::with_capacity(lines.len() + comments.len());
    for row in lines {
        let here = row.lines();
        rows.push(row);
        for ((idx, comment), done) in comments.iter().zip(placed.iter_mut()) {
            if !*done && here.contains(&(comment.side, comment.end)) {
                *done = true;
                rows.push(Row::Comment(*idx));
            }
        }
    }
    let lost: Vec<usize> = comments
        .iter()
        .zip(&placed)
        .filter(|(_, done)| !**done)
        .map(|((idx, _), _)| *idx)
        .collect();
    if !lost.is_empty() {
        let mut top = vec![Row::Note(
            "Comments whose line is no longer in the diff:".into(),
        )];
        top.extend(lost.into_iter().map(Row::Comment));
        top.append(&mut rows);
        rows = top;
    }
    rows
}
