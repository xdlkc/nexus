//! Multi-repo `git worktree` lifecycle manager for isolated Worker assignments.

use std::path::Path;
use tokio::process::Command;

async fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    let result = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .await
        .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

pub async fn is_issue_worktree(dir: &Path, checkout: Option<&Path>, branch: &str) -> bool {
    let Some(checkout) = checkout else {
        return false;
    };
    let common_args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    match (
        git_text(dir, &common_args).await,
        git_text(checkout, &common_args).await,
    ) {
        (Some(worktree_repo), Some(configured_repo)) if worktree_repo == configured_repo => {
            git_text(dir, &["rev-parse", "--show-prefix"])
                .await
                .as_deref()
                == Some("")
                && git_text(dir, &["branch", "--show-current"])
                    .await
                    .as_deref()
                    == Some(branch)
        }
        _ => false,
    }
}

pub async fn check_issue_delivery(
    dir: &Path,
    gh_bin: &Path,
    repo: &str,
    branch: &str,
    reported_pr: bool,
) -> Result<Option<(String, String)>, &'static str> {
    if git_text(dir, &["branch", "--show-current"])
        .await
        .as_deref()
        != Some(branch)
    {
        return Err("the worker did not leave the worktree on the issue branch");
    }
    let status = git_text(dir, &["status", "--porcelain", "--untracked-files=normal"])
        .await
        .ok_or("could not inspect the Git worktree")?;
    if !status.is_empty() {
        return Err("the issue worktree still has uncommitted changes");
    }
    let head = git_text(dir, &["rev-parse", "HEAD"])
        .await
        .ok_or("could not read the branch HEAD")?;
    if !reported_pr {
        let base = git_text(dir, &["symbolic-ref", "refs/remotes/origin/HEAD"])
            .await
            .unwrap_or_else(|| "refs/remotes/origin/main".to_owned());
        if git_text(dir, &["rev-list", "--count", "HEAD", "--not", &base])
            .await
            .as_deref() == Some("0")
        {
            return Ok(None);
        }
    }

    let lookup = Command::new(gh_bin)
        .kill_on_drop(true)
        .current_dir(dir)
        .args([
            "pr",
            "list",
            "--repo",
            repo,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "url,state,headRefOid",
        ])
        .output();
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), lookup)
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|output| output.status.success())
        .ok_or("GitHub could not verify the issue branch's pull request")?;
    let prs: Vec<serde_json::Value> = serde_json::from_slice(&result.stdout)
        .map_err(|_| "GitHub returned an invalid pull request list")?;
    let prefix = format!("https://github.com/{repo}/pull/");
    for pr in prs {
        if matches!(pr["state"].as_str(), Some("OPEN" | "MERGED"))
            && pr["headRefOid"].as_str() == Some(head.as_str())
            && let Some(url) = pr["url"].as_str()
            && let Some(number) = url
                .strip_prefix(&prefix)
                .and_then(|part| part.parse::<u64>().ok())
        {
            return Ok(Some((format!("github:pr:{repo}#{number}"), url.to_owned())));
        }
    }
    Err("there is no open or merged pull request containing the issue branch's current commit")
}

/// Prepares a Ticket's dedicated `git worktree` at `worktree_dir` on branch `agent/<short_name>`.
/// If `worktree_dir` already exists (e.g. during a mid-ticket cross-harness handoff or wakeup),
/// preserves all existing files and uncommitted changes intact.
pub async fn prepare_worktree(
    repo_checkout: Option<&Path>,
    worktree_dir: &Path,
    short_name: &str,
) -> anyhow::Result<()> {
    let abs_worktree_dir = if worktree_dir.is_absolute() {
        worktree_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| Path::new(".").to_path_buf())
            .join(worktree_dir)
    };

    if abs_worktree_dir.exists() {
        return Ok(());
    }

    if let Some(parent) = abs_worktree_dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if let Some(repo_dir) = repo_checkout
        && repo_dir.join(".git").exists()
    {
        let _ = Command::new("git")
            .current_dir(repo_dir)
            .args(["fetch", "origin", "main", "--tags", "-q"])
            .output()
            .await;

        let branch = format!("agent/{short_name}");
        let from_origin_main = Command::new("git")
            .current_dir(repo_dir)
            .args([
                "worktree",
                "add",
                "-B",
                &branch,
                &abs_worktree_dir.to_string_lossy(),
                "origin/main",
            ])
            .output()
            .await;

        if let Ok(out) = from_origin_main
            && out.status.success()
            && abs_worktree_dir.exists()
        {
            return Ok(());
        }

        let output = Command::new("git")
            .current_dir(repo_dir)
            .args([
                "worktree",
                "add",
                "-B",
                &branch,
                &abs_worktree_dir.to_string_lossy(),
                "HEAD",
            ])
            .output()
            .await?;

        if output.status.success() && abs_worktree_dir.exists() {
            return Ok(());
        }
    }

    // Fallback when no git repository is configured or initialized yet.
    tokio::fs::create_dir_all(&abs_worktree_dir).await?;
    Ok(())
}

/// Captures `git status --short` and `git diff --stat` inside `worktree_dir` for cross-harness `HandoffCard`s.
pub async fn capture_git_status_diff(worktree_dir: &Path) -> String {
    if !worktree_dir.exists() {
        return "(worktree not created yet)".to_owned();
    }

    let status_out = Command::new("git")
        .current_dir(worktree_dir)
        .args(["status", "--short", "--branch"])
        .output()
        .await;

    let diff_out = Command::new("git")
        .current_dir(worktree_dir)
        .args(["diff", "--stat"])
        .output()
        .await;

    let mut parts = Vec::new();
    if let Ok(out) = status_out
        && out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !s.is_empty() {
                parts.push(s);
            }
        }
    if let Ok(out) = diff_out
        && out.status.success() {
            let d = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !d.is_empty() {
                parts.push(d);
            }
        }

    if parts.is_empty() {
        "(clean worktree)".to_owned()
    } else {
        parts.join("\n---\n")
    }
}
