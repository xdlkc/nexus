//! Multi-repo `git worktree` lifecycle manager for isolated Worker assignments.

use std::path::Path;
use tokio::process::Command;

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
