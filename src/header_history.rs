//! Explicit Git-history collection and pure, capture-anchored header prioritization.
//! Reports consume saved history and never inspect the current checkout.
use crate::{model::Capture, util::now_ms};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

/// One content-changing commit on the selected first-parent development line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditCommit {
    /// Full Git object ID; independent of current branch names.
    pub commit: String,
    /// Committer timestamp in milliseconds, used for the saved window.
    pub committed_ms: u64,
}

/// History for one header present at the anchor commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileHistory {
    /// Capture-portable header key, relative to the source root.
    pub key: String,
    /// Distinct content-changing commits; pure renames are excluded.
    pub edits: Vec<EditCommit>,
    /// Detected older-to-newer repository paths, documenting rename following.
    pub renames: Vec<(String, String)>,
}

/// Saved optional analysis, portable with the capture's other evidence artifacts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistorySnapshot {
    /// Analysis format/algorithm version; unknown versions are rejected.
    pub version: u32,
    /// Commit observed by the capture, never today's HEAD.
    pub anchor: String,
    /// Time the local history analysis ran, separate from the measured build.
    pub analyzed_ms: u64,
    /// End of the history window, taken from capture observation time.
    pub until_ms: u64,
    /// Oldest collected timestamp, inclusive.
    pub since_ms: u64,
    /// Whether the available local history covers the requested scan.
    pub complete: bool,
    /// Per-header committed history, including zero edits for tracked headers.
    pub files: Vec<FileHistory>,
    /// Missing history, dirty checkout, and counting-policy disclosures.
    pub notes: Vec<String>,
}

fn git(source: &str, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-optional-locks", "-C", source]);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        cmd.env_remove(key);
    }
    let output = cmd
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .context("run local Git history query")?;
    ensure!(
        output.status.success(),
        "Git history query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Read committed history explicitly; this never fetches, changes refs, or checks out files.
pub fn collect(capture: &Capture, days: u32) -> Result<HistorySnapshot> {
    ensure!(
        (1..=3660).contains(&days),
        "history window must be between 1 and 3660 days"
    );
    let anchor = capture
        .git
        .commit
        .as_deref()
        .context("capture has no single saved Git commit")?;
    ensure!(
        anchor.len() >= 40 && anchor.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid saved Git object ID"
    );
    let source = &capture.project.source_dir;
    let prefix = String::from_utf8(git(source, &["rev-parse", "--show-prefix"])?)
        .context("non-UTF-8 repository prefix")?;
    let prefix = prefix.trim_end_matches('\n');
    let tracked = git(source, &["ls-tree", "-r", "--name-only", "-z", anchor])?;
    let tracked: BTreeSet<_> = tracked
        .split(|b| *b == 0)
        .filter_map(|p| std::str::from_utf8(p).ok())
        .collect();
    // ls-tree from a subdirectory emits source-relative names; raw log paths are root-relative.
    let mut files = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for h in &capture.headers {
        if Path::new(&h.path).strip_prefix(source).is_ok()
            && !h.key.starts_with('<')
            && tracked.contains(h.key.as_str())
        {
            aliases.insert(format!("{prefix}{}", h.key), h.key.clone());
            files.insert(
                h.key.clone(),
                FileHistory {
                    key: h.key.clone(),
                    edits: Vec::new(),
                    renames: Vec::new(),
                },
            );
        }
    }
    let until_ms = capture.git.observed_ms.unwrap_or(capture.started_ms);
    let since_ms = until_ms.saturating_sub(u64::from(days) * 86_400_000);
    let since = format!("--since-as-filter=@{}", since_ms / 1000);
    let until = format!("--until=@{}", until_ms / 1000);
    let raw = git(
        source,
        &[
            "log",
            "--first-parent",
            "--diff-merges=first-parent",
            "--root",
            "--raw",
            "--no-abbrev",
            "-M",
            "-z",
            "--format=%x00BUILDSCOPE%x00%H%x00%ct",
            "--max-count=10001",
            &since,
            &until,
            anchor,
            "--",
        ],
    )?;
    let shallow = git(source, &["rev-parse", "--is-shallow-repository"])?;
    let mut snapshot = HistorySnapshot { version: 1, anchor: anchor.into(), analyzed_ms: now_ms(), until_ms, since_ms,
        complete: shallow != b"true\n", files: Vec::new(), notes: vec![
            "Counts are content-changing first-parent commits, with merges compared against their first parent. Rename following uses Git's similarity detection; pure renames are not edits.".into(),
            "Priority is edit count multiplied by associated compilation work, not predicted savings. Current dirty edits are excluded from committed frequency.".into(),
        ] };
    parse_log(&raw, &mut files, &mut aliases, &mut snapshot)?;
    if !snapshot.complete {
        snapshot.notes.push("Local history is shallow or the 10,000-commit scan budget was reached; counts are a lower bound. No remote fetch was attempted.".into());
    }
    snapshot.files = files.into_values().collect();
    Ok(snapshot)
}

fn parse_log(
    raw: &[u8],
    files: &mut BTreeMap<String, FileHistory>,
    aliases: &mut BTreeMap<String, String>,
    snapshot: &mut HistorySnapshot,
) -> Result<()> {
    let tokens: Vec<_> = raw.split(|b| *b == 0).collect();
    let mut index = 0;
    let mut commit = String::new();
    let mut committed_ms = 0;
    let mut commits = 0;
    while index < tokens.len() {
        let token = tokens[index];
        index += 1;
        if token == b"BUILDSCOPE" {
            ensure!(index + 1 < tokens.len(), "truncated Git commit record");
            commit = std::str::from_utf8(tokens[index])?.into();
            committed_ms = std::str::from_utf8(tokens[index + 1])?
                .parse::<u64>()?
                .saturating_mul(1000);
            index += 2;
            commits += 1;
            if commits > 10000 {
                snapshot.complete = false;
                break;
            }
        } else if token.strip_prefix(b"\n").unwrap_or(token).starts_with(b":") {
            let header = std::str::from_utf8(token)?.trim_start_matches('\n');
            let fields: Vec<_> = header.split_whitespace().collect();
            ensure!(
                fields.len() == 5 && index < tokens.len(),
                "invalid Git raw diff record"
            );
            let old = tokens[index];
            index += 1;
            let status = fields[4];
            let new = if status.starts_with('R') || status.starts_with('C') {
                ensure!(index < tokens.len(), "truncated Git rename record");
                let path = tokens[index];
                index += 1;
                path
            } else {
                old
            };
            let (Ok(old), Ok(new)) = (std::str::from_utf8(old), std::str::from_utf8(new)) else {
                snapshot.complete = false;
                continue;
            };
            let Some(key) = aliases.get(new).cloned() else {
                continue;
            };
            let file = files
                .get_mut(&key)
                .context("history path mapping missing")?;
            if fields[2] != fields[3]
                && committed_ms >= snapshot.since_ms
                && committed_ms <= snapshot.until_ms
                && file.edits.last().is_none_or(|edit| edit.commit != commit)
            {
                file.edits.push(EditCommit {
                    commit: commit.clone(),
                    committed_ms,
                });
            }
            if status.starts_with('R') {
                file.renames.push((old.into(), new.into()));
                aliases.remove(new);
                aliases.insert(old.into(), key);
            } else if status == "A" {
                aliases.remove(new);
            }
        }
    }
    Ok(())
}

/// Rank source headers using only saved history and the selected capture's observations.
pub fn hotspots(
    capture: &Capture,
    history: &HistorySnapshot,
    days: u32,
    target: &str,
    text: &str,
    path: &str,
) -> Value {
    if history.version != 1 || capture.git.commit.as_deref() != Some(history.anchor.as_str()) {
        return json!({"available":false,"rows":[],"notes":["Saved header history does not match this capture or uses an unsupported version. Run analyze-history again."]});
    }
    let since = history
        .until_ms
        .saturating_sub(u64::from(days) * 86_400_000);
    let mut work = BTreeMap::<&str, f64>::new();
    for a in &capture.actions {
        if a.success
            && let Some(id) = a.unit_id.as_deref()
        {
            *work.entry(id).or_default() += (a.end_ms - a.start_ms).max(0.0);
        }
    }
    let selected: BTreeSet<_> = capture
        .project
        .units
        .iter()
        .filter(|u| target.is_empty() || crate::store::target_name(&u.output_key) == target)
        .map(|u| u.id.as_str())
        .collect();
    let headers: BTreeMap<_, _> = capture
        .headers
        .iter()
        .map(|h| (h.key.as_str(), h))
        .collect();
    let mut rows = Vec::new();
    for file in &history.files {
        let Some(header) = headers.get(file.key.as_str()) else {
            continue;
        };
        if (!path.is_empty() && header.path != path)
            || !header.key.to_lowercase().contains(&text.to_lowercase())
        {
            continue;
        }
        let units: Vec<_> = header
            .units
            .iter()
            .filter(|u| selected.contains(u.as_str()))
            .collect();
        if units.is_empty() {
            continue;
        }
        let measured = units
            .iter()
            .filter(|u| work.contains_key(u.as_str()))
            .count();
        let cost: f64 = units.iter().filter_map(|u| work.get(u.as_str())).sum();
        let edits: Vec<_> = file
            .edits
            .iter()
            .filter(|e| e.committed_ms >= since && e.committed_ms <= history.until_ms)
            .collect();
        let complete = capture.kind == "profile"
            && capture.profile_origin.is_some()
            && capture.status == "success"
            && measured == units.len()
            && history.complete
            && since >= history.since_ms;
        let processing: Vec<_> = capture
            .profiles
            .iter()
            .filter(|p| units.iter().any(|u| **u == p.unit_id))
            .flat_map(|p| p.header_costs.iter())
            .filter(|c| c.path == header.path)
            .collect();
        rows.push(json!({"path":header.path,"key":header.key,"editing_commits":edits.len(),
            "last_edited_ms":edits.iter().map(|e|e.committed_ms).max(),"dependent_units":units.len(),"measured_units":measured,
            "work_ms":if measured>0 {Some(cost)} else {None},"processing_ms":if processing.is_empty(){None}else{Some(processing.iter().map(|c|c.inclusive_ms).sum::<f64>())},
            "priority":complete.then_some(cost*edits.len() as f64),"complete":complete,
            "edits":if path.is_empty(){json!([])}else{json!(edits)},"renames":if path.is_empty(){json!([])}else{json!(file.renames)}}));
    }
    rows.sort_by(|a, b| {
        b["priority"]
            .as_f64()
            .unwrap_or(-1.0)
            .total_cmp(&a["priority"].as_f64().unwrap_or(-1.0))
            .then(
                b["editing_commits"]
                    .as_u64()
                    .cmp(&a["editing_commits"].as_u64()),
            )
            .then(a["key"].as_str().cmp(&b["key"].as_str()))
    });
    json!({"available":true,"anchor":history.anchor,"since_ms":since.max(history.since_ms),"until_ms":history.until_ms,
        "complete":history.complete && since>=history.since_ms,"requested_days":days,
        "saved_since_ms":history.since_ms,"window_truncated":since<history.since_ms,"notes":history.notes,"rows":rows})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_budget_never_claims_complete_history() {
        let mut snapshot = HistorySnapshot {
            version: 1,
            anchor: "anchor".into(),
            analyzed_ms: 0,
            until_ms: 1_000,
            since_ms: 0,
            complete: true,
            files: vec![],
            notes: vec![],
        };
        let mut raw = Vec::new();
        for index in 0..10_001 {
            raw.extend_from_slice(format!("\0BUILDSCOPE\0{index:040x}\0{index}\0").as_bytes());
        }
        parse_log(
            &raw,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            &mut snapshot,
        )
        .unwrap();
        assert!(!snapshot.complete);
    }
}
