//! Syntax colours for the review: each side of a file highlighted as a whole
//! (so a string or comment spanning lines is coloured right), then cut into
//! lines for the diff rows.

use std::path::Path;

use karet_core::TokenId;

/// The coloured runs of one line: (start byte, end byte, token), relative to
/// the line.
pub type LineTokens = Vec<(usize, usize, TokenId)>;

/// Both sides of a file: `old[n]` / `new[n]` are line `n + 1`.
#[derive(Clone, Debug, Default)]
pub struct FileColours {
    pub old: Vec<LineTokens>,
    pub new: Vec<LineTokens>,
}

impl FileColours {
    /// The runs of 1-based `lineno` on one side.
    pub fn line(&self, new_side: bool, lineno: u32) -> &[(usize, usize, TokenId)] {
        let lines = if new_side { &self.new } else { &self.old };
        usize::try_from(lineno)
            .ok()
            .and_then(|lineno| lineno.checked_sub(1))
            .and_then(|idx| lines.get(idx))
            .map_or(&[], Vec::as_slice)
    }
}

/// Highlight `text` as the file `path` and cut the result into lines.
pub fn colour(path: &str, text: &str) -> Vec<LineTokens> {
    let Some(highlights) = crate::doc::highlight_text(Path::new(path), text) else {
        return Vec::new();
    };
    // Where each line starts, in bytes.
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(idx, _)| idx + 1));
    let mut lines: Vec<LineTokens> = vec![Vec::new(); starts.len()];
    for span in highlights.all() {
        let (from, until) = (span.span.start.0, span.span.end.0);
        // A span may run over several lines (a docstring): give each its part.
        let first = starts
            .partition_point(|start| *start <= from)
            .saturating_sub(1);
        for (line, start) in starts.iter().enumerate().skip(first) {
            if *start >= until {
                break;
            }
            let next = starts.get(line + 1).copied().unwrap_or(text.len() + 1);
            let line_end = next.saturating_sub(1);
            let begin = from.max(*start) - start;
            let end = until.min(line_end).saturating_sub(*start);
            if end > begin
                && let Some(runs) = lines.get_mut(line)
            {
                runs.push((begin, end, span.token));
            }
        }
    }
    lines
}
