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
use crate::util::on_path;

/// Untracked files larger than this are listed without their content.
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

/// How many commits the commit list offers.
const COMMITS: &str = "200";

/// git's empty tree, the parent side of a repository's first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What a review compares.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// Uncommitted changes (staged, unstaged and untracked) against HEAD.
    WorkingTree,
    /// This branch against the main branch, from where they split.
    Branch,
    /// Commits from `oldest` to `newest` (full hashes), against the parent of
    /// `oldest`. One commit when they are the same.
    Commits { oldest: String, newest: String },
    /// A GitHub pull request, through `gh`.
    PullRequest { number: u64 },
}

impl Source {
    /// The name a review of this source is kept under.
    pub fn key(&self) -> String {
        match self {
            Self::WorkingTree => "working-tree".into(),
            Self::Branch => "branch".into(),
            Self::Commits { oldest, newest } => format!("commits:{oldest}..{newest}"),
            Self::PullRequest { number } => format!("pr:{number}"),
        }
    }
}

/// What choosing a picker entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Review this source.
    Load(Source),
    /// Pick commits from a list first.
    Commits,
    /// Pick a pull request from a list first.
    Prs,
}

/// A commit in the commit list.
#[derive(Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    /// "3 days ago".
    pub when: String,
    pub author: String,
    pub subject: String,
}

/// An open pull request in the PR list.
#[derive(Clone, Debug, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    #[serde(rename = "headRefName")]
    pub branch: String,
    pub author: Author,
    #[serde(rename = "isDraft", default)]
    pub draft: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Author {
    pub login: String,
}

/// A list to pick from, as fetched.
pub enum Choices {
    Commits(Vec<Commit>),
    Prs(Vec<PullRequest>),
}

/// A source as the picker shows it.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub entry: Entry,
    /// "Your changes" or "This branch".
    pub title: &'static str,
    /// "working tree vs HEAD", "feature/cart vs main", or what the list offers.
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
    let has_github = on_path("gh");
    vec![
        SourceInfo {
            entry: Entry::Load(Source::WorkingTree),
            title: "Your changes",
            detail: "working tree vs HEAD".to_string(),
            files: working,
            unavailable: None,
        },
        SourceInfo {
            entry: Entry::Load(Source::Branch),
            title: "This branch",
            detail,
            files,
            unavailable,
        },
        SourceInfo {
            entry: Entry::Commits,
            title: "Commits...",
            detail: "pick one or more from the history".to_string(),
            files: None,
            unavailable: None,
        },
        SourceInfo {
            entry: Entry::Prs,
            title: "Pull request...",
            detail: "pick an open one from GitHub".to_string(),
            files: None,
            unavailable: (!has_github).then(|| "needs the GitHub CLI, gh".to_string()),
        },
    ]
}

/// Run the GitHub CLI in `root` and return what it printed.
fn github(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|err| format!("gh: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "gh: {}",
            message.lines().next().unwrap_or("failed")
        ))
    }
}

/// The recent commits of HEAD, newest first.
pub fn commits(root: &Path) -> Result<Choices, String> {
    let out = git(
        root,
        &[
            "log",
            "-n",
            COMMITS,
            "--format=%H%x1f%h%x1f%ar%x1f%an%x1f%s",
        ],
    )?;
    let list = out
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            Some(Commit {
                hash: parts.next()?.to_string(),
                short: parts.next()?.to_string(),
                when: parts.next()?.to_string(),
                author: parts.next()?.to_string(),
                subject: parts.next().unwrap_or("").to_string(),
            })
        })
        .collect();
    Ok(Choices::Commits(list))
}

/// The repository's open pull requests, the current branch's first.
pub fn prs(root: &Path) -> Result<Choices, String> {
    let out = github(
        root,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,title,headRefName,author,isDraft",
        ],
    )?;
    let mut list: Vec<PullRequest> =
        serde_json::from_str(&out).map_err(|err| format!("gh pr list: {err}"))?;
    // The current branch's PR first; on the main branch itself there is none
    // (a fork's PR from its own `main` is not ours).
    let branch = current_branch(root);
    if base_branch(root).as_deref() != Some(branch.as_str()) {
        list.sort_by_key(|request| request.branch != branch);
    }
    Ok(Choices::Prs(list))
}

/// Load the diff for `source`.
pub fn load(root: &Path, source: Source) -> Result<Loaded, String> {
    let diff_args = ["diff", "--no-color", "--no-ext-diff", "-M"];
    match &source {
        Source::Commits { oldest, newest } => {
            // The first commit of a repository has no parent: compare with
            // the empty tree.
            let parent = git(
                root,
                &["rev-parse", "--verify", "--quiet", &format!("{oldest}^")],
            )
            .map_or_else(|_| EMPTY_TREE.to_string(), |rev| rev.trim().to_string());
            let raw = git(
                root,
                &[&diff_args[..], &[parent.as_str(), newest.as_str()]].concat(),
            )?;
            let files = karet_diff::parse(&raw)
                .map_err(|err| format!("cannot read the diff: {err}"))?
                .files;
            let colours = colours(root, &files, &parent, Some(newest));
            let short = |hash: &str| hash.chars().take(7).collect::<String>();
            let detail = if oldest == newest {
                let subject = git(root, &["log", "-1", "--format=%s", newest]).unwrap_or_default();
                format!("commit {} {}", short(newest), subject.trim())
            } else {
                let count = git(
                    root,
                    &["rev-list", "--count", &format!("{parent}..{newest}")],
                )
                .map_or_else(|_| "?".to_string(), |out| out.trim().to_string());
                format!("{count} commits {}..{}", short(oldest), short(newest))
            };
            return Ok(Loaded {
                source,
                detail,
                files,
                colours,
            });
        }
        Source::PullRequest { number } => return load_pr(root, source.clone(), *number),
        Source::WorkingTree | Source::Branch => {}
    }
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
        Source::Commits { .. } | Source::PullRequest { .. } => Err("not a local source".into()),
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

/// A pull request's title and the commits at its two ends.
#[derive(Deserialize)]
struct Heads {
    title: String,
    #[serde(rename = "headRefOid")]
    head: String,
    #[serde(rename = "baseRefOid")]
    base: String,
}

/// A pull request: its diff from GitHub, its two sides from git for the
/// colours (its commit is fetched when it is not here yet).
fn load_pr(root: &Path, source: Source, number: u64) -> Result<Loaded, String> {
    let id = number.to_string();
    let raw = github(root, &["pr", "diff", &id, "--color", "never"])?;
    let files = karet_diff::parse(&raw)
        .map_err(|err| format!("cannot read the diff: {err}"))?
        .files;
    let heads: Option<Heads> = github(
        root,
        &["pr", "view", &id, "--json", "title,headRefOid,baseRefOid"],
    )
    .ok()
    .and_then(|out| serde_json::from_str(&out).ok());
    let detail = heads.as_ref().map_or_else(
        || format!("PR #{number}"),
        |heads| format!("PR #{number} {}", heads.title),
    );
    // Colours need both sides here; without them the diff is still shown.
    let colours = heads
        .filter(|heads| have_commit(root, &heads.head) && have_commit(root, &heads.base))
        .and_then(|heads| {
            let fork = git(root, &["merge-base", &heads.base, &heads.head]).ok()?;
            Some(colours(root, &files, fork.trim(), Some(&heads.head)))
        })
        .unwrap_or_default();
    Ok(Loaded {
        source,
        detail,
        files,
        colours,
    })
}

/// Whether `oid` is in the repository, fetching it from `origin` if not.
fn have_commit(root: &Path, oid: &str) -> bool {
    let present =
        |root: &Path| git(root, &["cat-file", "-e", &format!("{oid}^{{commit}}")]).is_ok();
    present(root) || (git(root, &["fetch", "--quiet", "origin", oid]).is_ok() && present(root))
}

/// A new, untracked file as an addition of all its lines.
fn untracked_diff(root: &Path, path: String) -> FileDiff {
    let full = root.join(&path);
    // A symlink shows where it points, as git does, never what it points
    // to (which may be outside the repository); only regular files are read.
    let meta = std::fs::symlink_metadata(&full).ok();
    let text = match meta {
        Some(meta) if meta.file_type().is_symlink() => std::fs::read_link(&full)
            .ok()
            .map(|target| target.as_os_str().as_encoded_bytes().to_vec()),
        Some(meta) if meta.is_file() && meta.len() <= MAX_UNTRACKED_BYTES => {
            std::fs::read(&full).ok()
        }
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An untracked symlink shows where it points, never the contents of
    /// what it points to (here a file outside the repository).
    #[test]
    fn untracked_symlinks_show_their_target_not_its_contents() -> Result<(), String> {
        let base = std::env::temp_dir().join(format!("sanitty-link-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).map_err(|err| err.to_string())?;
        let secret = base.join("secret.txt");
        std::fs::write(&secret, "TOKEN=hunter2\n").map_err(|err| err.to_string())?;
        let run = |args: &[&str]| git(&repo, args).map(|_| ());
        run(&["init", "--quiet"])?;
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "start",
        ])?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, repo.join("link")).map_err(|err| err.to_string())?;
        let loaded = load(&repo, Source::WorkingTree)?;
        let shown: String = loaded
            .files
            .iter()
            .flat_map(|file| &file.hunks)
            .flat_map(|hunk| &hunk.lines)
            .map(|line| line.content.clone())
            .collect();
        let _ = std::fs::remove_dir_all(&base);
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("secret.txt"), "{shown}");
        Ok(())
    }
}
