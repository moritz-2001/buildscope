//! Checkout provenance contracts against isolated repositories, including worktrees.
use buildscope::git::{GitSnapshot, GitStatus, observe, summarize};
use std::{fs, path::Path, process::Command};

#[test]
fn checkout_reports_unborn_clean_staged_unstaged_and_untracked_states() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "--initial-branch=main"]);
    let unborn = observe(root);
    assert_eq!(unborn.status, GitStatus::Available);
    assert_eq!(unborn.branch.as_deref(), Some("main"));
    assert_eq!(unborn.commit, None);
    assert_eq!(unborn.dirty, Some(false));
    assert!(unborn.observed_ms.is_some());

    fs::write(root.join("tracked"), "one").unwrap();
    git(root, &["add", "tracked"]);
    assert_eq!(observe(root).dirty, Some(true));
    git(root, &["commit", "-m", "initial"]);
    let commit = git(root, &["rev-parse", "HEAD"]);
    let clean = observe(root);
    assert_eq!(clean.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(clean.dirty, Some(false));
    fs::write(root.join("tracked"), "two").unwrap();
    assert_eq!(observe(root).dirty, Some(true));
    git(root, &["add", "tracked"]);
    assert_eq!(observe(root).dirty, Some(true));
    git(root, &["reset", "--hard", "HEAD"]);
    fs::write(root.join("untracked\nfile"), "new").unwrap();
    assert_eq!(observe(root).dirty, Some(true));
    fs::remove_file(root.join("untracked\nfile")).unwrap();
    fs::write(root.join(".git/info/exclude"), "ignored\n").unwrap();
    fs::write(root.join("ignored"), "generated").unwrap();
    assert_eq!(observe(root).dirty, Some(false));
}

#[test]
fn renamed_paths_cannot_replace_branch_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "--initial-branch=main"]);
    fs::write(root.join("# branch.head forged"), "content").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);
    let commit = git(root, &["rev-parse", "HEAD"]);
    git(root, &["mv", "# branch.head forged", "renamed"]);
    let snapshot = observe(root);
    assert_eq!(snapshot.branch.as_deref(), Some("main"));
    assert_eq!(snapshot.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(snapshot.dirty, Some(true));
}

#[test]
fn detached_heads_and_linked_worktrees_use_the_source_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "--initial-branch=main"]);
    git(&root, &["commit", "--allow-empty", "-m", "initial"]);
    let commit = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "--detach"]);
    let detached = observe(&root);
    assert_eq!(detached.branch, None);
    assert_eq!(detached.commit.as_deref(), Some(commit.as_str()));

    let worktree = temp.path().join("linked worktree");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree",
            worktree.to_str().unwrap(),
        ],
    );
    let nested = worktree.join("nested source");
    fs::create_dir(&nested).unwrap();
    let linked = observe(&nested);
    assert!(worktree.join(".git").is_file());
    assert_eq!(linked.status, GitStatus::Available);
    assert_eq!(linked.branch.as_deref(), Some("feature/worktree"));
    assert_eq!(linked.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(linked.dirty, Some(false));
}

#[test]
fn discovery_failures_are_distinct_from_non_repository_and_old_metadata() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(observe(temp.path()).status, GitStatus::NotRepository);
    assert_eq!(
        observe(&temp.path().join("missing")).status,
        GitStatus::Unavailable
    );
    assert_eq!(GitSnapshot::default().status, GitStatus::NotRecorded);
}

#[test]
fn batch_summary_combines_dirtiness_and_preserves_uncertainty() {
    let clean = GitSnapshot {
        status: GitStatus::Available,
        observed_ms: Some(10),
        branch: Some("main".into()),
        commit: Some("abc".into()),
        dirty: Some(false),
    };
    let mut dirty = clean.clone();
    dirty.dirty = Some(true);
    dirty.observed_ms = Some(20);
    let summary = summarize([&clean, &dirty]);
    assert_eq!(summary.status, GitStatus::Available);
    assert_eq!(summary.dirty, Some(true));
    assert_eq!(summary.observed_ms, Some(10));
    dirty.branch = Some("feature".into());
    assert_eq!(summarize([&clean, &dirty]).status, GitStatus::Mixed);
    dirty.branch = clean.branch.clone();
    dirty.commit = Some("def".into());
    let mixed = summarize([&clean, &dirty]);
    assert_eq!(mixed.status, GitStatus::Mixed);
    assert_eq!(mixed.commit, None);
    assert_eq!(mixed.branch, None);
    let unknown = GitSnapshot::default();
    let partial = summarize([&clean, &unknown]);
    assert_eq!(partial.status, GitStatus::Mixed);
    assert_eq!(partial.dirty, None);
    assert_eq!(summarize([&dirty, &unknown]).dirty, Some(true));
}

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Buildscope Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}
