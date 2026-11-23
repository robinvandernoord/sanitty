//! Getting a diff out of git: which sources there are, and loading one.
//!
//! Everything here runs `git` and blocks, so callers run it off the app loop.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use karet_diff::DiffOptions;
use karet_diff::FileDiff;
use karet_diff::FileStatus;
use serde::Deserialize;
use serde::Serialize;

use super::colour;
use super::colour::FileColours;

/// Untracked files larger than this are listed without their content.
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

/// What a review compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// Uncommitted changes (staged, unstaged and untracked) against HEAD.
    WorkingTree,
    /// This branch against the main branch, from where they split.
    Branch,
}

/// A source as the picker shows it.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub source: Source,
    /// "Your changes" or "This branch".
    pub title: &'static str,
    /// "working tree vs HEAD" or "feature/cart vs main".
    pub detail: String,
    /// Changed files, when they could be counted.
    pub files: Option<usize>,
    /// Why it cannot be used (no main branch, already on it).
    pub unavailable: Option<String>,
}

/// A loaded diff.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub source: Source,
    /// What is compared, for the title line.
    pub detail: String,
    pub files: Vec<FileDiff>,
    /// Syntax colours for both sides of each file, by path.
    pub colours: HashMap<String, FileColours>,
}

/// A file's text at a revision (`None`: the working tree), or `None` when it
/// is missing, binary or too large to colour.
fn text_at(root: &Path, rev: Option<&str>, path: &str) -> Option<String> {
    let text = if let Some(rev) = rev {
        git(root, &["show", &format!("{rev}:{path}")]).ok()?
    } else {
        let full = root.join(path);
        if std::fs::metadata(&full).ok()?.len() > MAX_UNTRACKED_BYTES {
            return None;
        }
        std::fs::read_to_string(full).ok()?
    };
    (text.len() as u64 <= MAX_UNTRACKED_BYTES && !text.contains('\0')).then_some(text)
}

/// Colour both sides of every file: the old side at `old_rev`, the new side
/// at `new_rev` (`None`: the working tree).
fn colours(
    root: &Path,
    files: &[FileDiff],
    old_rev: &str,
    new_rev: Option<&str>,
) -> HashMap<String, FileColours> {
    files
        .iter()
        .filter(|file| !file.is_binary && !file.hunks.is_empty())
        .map(|file| {
            let old_path = file.old_path.as_deref().unwrap_or(&file.path);
            let old = match file.status {
                FileStatus::Added => Vec::new(),
                _ => text_at(root, Some(old_rev), old_path)
                    .map_or_else(Vec::new, |text| colour::colour(old_path, &text)),
            };
            let new = match file.status {
                FileStatus::Removed => Vec::new(),
                _ => text_at(root, new_rev, &file.path)
                    .map_or_else(Vec::new, |text| colour::colour(&file.path, &text)),
            };
            (file.path.clone(), FileColours { old, new })
        })
        .collect()
}

/// Run git in `root` and return what it printed.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(message.lines().next().unwrap_or("git failed").to_string())
    }
}

/// The branch this one is compared with: the remote's default branch, else
/// `main`, else `master`.
fn base_branch(root: &Path) -> Option<String> {
    if let Ok(head) = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let name = head.trim().trim_start_matches("origin/").to_string();
        if !name.is_empty() && git(root, &["rev-parse", "--verify", "--quiet", &name]).is_ok() {
            return Some(name);
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|name| git(root, &["rev-parse", "--verify", "--quiet", name]).is_ok())
        .map(str::to_string)
}

fn current_branch(root: &Path) -> String {
    git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map_or_else(|_| "HEAD".to_string(), |name| name.trim().to_string())
}

fn untracked(root: &Path) -> Vec<String> {
    git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).map_or_else(
        |_| Vec::new(),
        |out| {
            out.split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .collect()
        },
    )
}

fn count_lines(out: &str) -> usize {
    out.lines().filter(|line| !line.is_empty()).count()
}

/// Both sources, with how many files each would show.
pub fn sources(root: &Path) -> Vec<SourceInfo> {
    let working = git(root, &["diff", "--name-only", "HEAD"])
        .map(|out| count_lines(&out) + untracked(root).len())
        .ok();
    let branch = current_branch(root);
    let base = base_branch(root);
    let (detail, files, unavailable) = match &base {
        None => (
            format!("{branch} vs main"),
            None,
            Some("no main or master branch".to_string()),
        ),
        Some(base) if *base == branch => (
            format!("{branch} vs {base}"),
            None,
            Some(format!("already on {base}")),
        ),
        Some(base) => (
            format!("{branch} vs {base}"),
            git(root, &["diff", "--name-only", &format!("{base}...HEAD")])
                .map(|out| count_lines(&out))
                .ok(),
            None,
        ),
    };
    vec![
        SourceInfo {
            source: Source::WorkingTree,
            title: "Your changes",
            detail: "working tree vs HEAD".to_string(),
            files: working,
            unavailable: None,
        },
        SourceInfo {
            source: Source::Branch,
            title: "This branch",
            detail,
            files,
            unavailable,
        },
    ]
}

/// Load the diff for `source`.
pub fn load(root: &Path, source: Source) -> Result<Loaded, String> {
    let diff_args = ["diff", "--no-color", "--no-ext-diff", "-M"];
    match source {
        Source::WorkingTree => {
            let raw = git(root, &[&diff_args[..], &["HEAD"]].concat())?;
            let mut files = karet_diff::parse(&raw)
                .map_err(|err| format!("cannot read the diff: {err}"))?
                .files;
            files.extend(
                untracked(root)
                    .into_iter()
                    .map(|path| untracked_diff(root, path)),
            );
            let colours = colours(root, &files, "HEAD", None);
            Ok(Loaded {
                source,
                detail: "working tree vs HEAD".to_string(),
                files,
                colours,
            })
        }
        Source::Branch => {
            let branch = current_branch(root);
            let base = base_branch(root).ok_or("No main or master branch to compare with")?;
            if base == branch {
                return Err(format!("Already on {base}: there is no branch to compare"));
            }
            let range = format!("{base}...HEAD");
            let raw = git(root, &[&diff_args[..], &[range.as_str()]].concat())?;
            let files = karet_diff::parse(&raw)
                .map_err(|err| format!("cannot read the diff: {err}"))?
                .files;
            // The old side is where the branch split off.
            let fork = git(root, &["merge-base", &base, "HEAD"])
                .map_or_else(|_| base.clone(), |rev| rev.trim().to_string());
            let colours = colours(root, &files, &fork, Some("HEAD"));
            Ok(Loaded {
                source,
                detail: format!("{branch} vs {base}"),
                files,
                colours,
            })
        }
    }
}

/// A new, untracked file as an addition of all its lines.
fn untracked_diff(root: &Path, path: String) -> FileDiff {
    let full = root.join(&path);
    let size = std::fs::metadata(&full).map_or(0, |meta| meta.len());
    let text = if size <= MAX_UNTRACKED_BYTES {
        std::fs::read(&full).ok()
    } else {
        None
    };
    let opts = DiffOptions {
        path_hint: Some(path.clone()),
        ..DiffOptions::default()
    };
    match text.filter(|bytes| !bytes.contains(&0)) {
        Some(bytes) => {
            let mut diff = karet_diff::diff_text("", &String::from_utf8_lossy(&bytes), &opts);
            diff.path = path;
            diff.status = FileStatus::Added;
            diff
        }
        None => FileDiff {
            path,
            old_path: None,
            status: FileStatus::Added,
            old_mode: None,
            new_mode: None,
            is_binary: true,
            hunks: Vec::new(),
        },
    }
}
