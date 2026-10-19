//! Local checkout provenance. GitSnapshot records an observation, not a guarantee
//! that source contents stayed unchanged throughout compilation.

use crate::util::now_ms;
use serde::{Deserialize, Serialize};
use std::{path::Path, process::Command};

/// Saved Git state, independent of project and compilation configuration identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSnapshot {
    /// Outcome of discovery; older manifests default to NotRecorded.
    pub status: GitStatus,
    /// Unix milliseconds when discovery began, or the earliest known time in a batch.
    /// None means no observation time was recorded.
    pub observed_ms: Option<u64>,
    /// Local branch name; None for detached HEAD, mixed batches, or unavailable data.
    pub branch: Option<String>,
    /// Full HEAD object ID; None before the first commit or without a single known revision.
    pub commit: Option<String>,
    /// Repository-wide staged, unstaged, conflicted, or untracked changes. None means
    /// unknown; a mixed batch is true if any invocation was dirty and false only if all were clean.
    pub dirty: Option<bool>,
}

/// Distinguishes missing historical metadata from a failed query or a mixed batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    /// Git successfully described the source checkout.
    Available,
    /// The source directory is outside a Git working tree.
    NotRepository,
    /// Git could not be executed, the source root was unknown, or discovery failed.
    Unavailable,
    /// Launcher observations disagree about branch, commit, or discovery outcome.
    Mixed,
    /// Metadata was absent in an older capture or launcher observation.
    #[default]
    NotRecorded,
}

impl std::fmt::Display for GitSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            GitStatus::Available => {
                write!(
                    f,
                    "{} · {}",
                    self.branch.as_deref().unwrap_or("Detached HEAD"),
                    self.commit
                        .as_deref()
                        .map(|id| id.chars().take(12).collect::<String>())
                        .unwrap_or_else(|| "no commits yet".into())
                )?;
            }
            GitStatus::Mixed => write!(f, "Multiple Git revisions or unavailable observations")?,
            GitStatus::NotRepository => write!(f, "No Git repository")?,
            GitStatus::Unavailable => write!(f, "Git information unavailable")?,
            GitStatus::NotRecorded => write!(f, "Git information not recorded")?,
        }
        if self.dirty == Some(true) {
            write!(f, " · uncommitted changes")?;
        }
        Ok(())
    }
}

/// Observe the containing repository from the configured source root. Discovery
/// never prevents a build and never fetches remote data or refreshes Git's index.
pub fn observe(source_dir: &Path) -> GitSnapshot {
    let observed_ms = Some(now_ms());
    let mut command = Command::new("git");
    command
        .args(["--no-optional-locks", "-C"])
        .arg(source_dir)
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
            "--ignore-submodules=none",
            "--no-ahead-behind",
            "-z",
        ])
        .env("LC_ALL", "C");
    // A caller's explicit Git context must not redirect discovery away from source_dir.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    let mut snapshot = match command.output() {
        Ok(output) if output.status.success() => {
            parse_status(&output.stdout).unwrap_or_else(|| GitSnapshot {
                status: GitStatus::Unavailable,
                ..Default::default()
            })
        }
        Ok(output) if String::from_utf8_lossy(&output.stderr).contains("not a git repository") => {
            GitSnapshot {
                status: GitStatus::NotRepository,
                ..Default::default()
            }
        }
        _ => GitSnapshot {
            status: GitStatus::Unavailable,
            ..Default::default()
        },
    };
    snapshot.observed_ms = observed_ms;
    snapshot
}

/// Summarize saved invocation snapshots, ignoring observation time and combining
/// dirtiness without discarding individual evidence. No current checkout is queried.
pub fn summarize<'a>(snapshots: impl IntoIterator<Item = &'a GitSnapshot>) -> GitSnapshot {
    let mut snapshots = snapshots.into_iter();
    let Some(first) = snapshots.next() else {
        return GitSnapshot::default();
    };
    let mut summary = first.clone();
    for snapshot in snapshots {
        if summary.status != snapshot.status
            || summary.branch != snapshot.branch
            || summary.commit != snapshot.commit
        {
            summary.status = GitStatus::Mixed;
            summary.branch = None;
            summary.commit = None;
        }
        summary.observed_ms = summary
            .observed_ms
            .into_iter()
            .chain(snapshot.observed_ms)
            .min();
        summary.dirty = match (summary.dirty, snapshot.dirty) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        };
    }
    summary
}

fn parse_status(output: &[u8]) -> Option<GitSnapshot> {
    let mut branch = None;
    let mut commit = None;
    let mut dirty = false;
    let mut records = output.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if let Some(value) = record.strip_prefix(b"# branch.head ") {
            branch = Some(std::str::from_utf8(value).ok()?.to_owned());
        } else if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            commit = Some(std::str::from_utf8(value).ok()?.to_owned());
        } else if record.starts_with(b"2 ") {
            dirty = true;
            // Renames have a second NUL-delimited path, which can resemble a header.
            records.next()?;
        } else if record.starts_with(b"1 ")
            || record.starts_with(b"u ")
            || record.starts_with(b"? ")
        {
            dirty = true;
        }
    }
    let branch = branch?;
    let commit = commit?;
    Some(GitSnapshot {
        status: GitStatus::Available,
        observed_ms: None,
        branch: (branch != "(detached)").then_some(branch),
        commit: (commit != "(initial)").then_some(commit),
        dirty: Some(dirty),
    })
}
