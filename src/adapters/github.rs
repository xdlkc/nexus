//! GitHub Multi-Repo (`gh` / `gh-agent`) Source and Channel Adapter.
//!
//! Supports:
//! - Issue & PR comment polling (`repos/<owner>/<repo>/issues/comments`)
//! - PR inline review comment polling (`repos/<owner>/<repo>/pulls/comments`)
//! - Followed PR state polling (`gh pr view <num> --json state,mergedAt`) for auto-completing Tickets
//! - Delivering Issue/PR comments and Review thread replies via `gh` (`gh-agent`)

use crate::adapters::traits::{ChannelAdapter, SourceAdapter, SourcePollOutput};
use crate::core::gate::parse_inline_target_directive;
use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::process::Command;

/// Configuration for the GitHub (`gh` / `gh-agent`) multi-repo adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubAdapterConfig {
    pub source_id: String,
    /// List of `owner/repo` repositories monitored by this adapter.
    pub repos: Vec<String>,
    pub bot_login: String,
    pub gh_bin: PathBuf,
    pub followed_pr_threads: Vec<String>,
}

impl Default for GithubAdapterConfig {
    fn default() -> Self {
        Self {
            source_id: "github-main".to_owned(),
            repos: Vec::new(),
            bot_login: "nexus-bot".to_owned(),
            gh_bin: PathBuf::from("gh"),
            followed_pr_threads: Vec::new(),
        }
    }
}

/// Multi-repo GitHub adapter implementing `SourceAdapter` and `ChannelAdapter`.
#[derive(Debug, Clone)]
pub struct GithubAdapter {
    pub config: GithubAdapterConfig,
}

impl GithubAdapter {
    pub fn new(config: GithubAdapterConfig) -> Self {
        Self { config }
    }

    fn build_command(&self) -> Command {
        Command::new(&self.config.gh_bin)
    }

    /// Checks a followed PR thread (`github:pr:<owner>/<repo>#<number>`) via `gh pr view`
    /// and returns `Some("merged")` or `Some("closed")` if the PR has ended.
    pub async fn check_pr_closed(&self, pr_thread_key: &str) -> anyhow::Result<Option<String>> {
        let Some((repo, number)) = parse_pr_thread_key(pr_thread_key) else {
            return Ok(None);
        };
        let output = self
            .build_command()
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                &repo,
                "--json",
                "state,mergedAt",
            ])
            .output()
            .await?;

        if !output.status.success() {
            return Ok(None);
        }
        let raw: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(parse_github_pr_state_json(&raw))
    }
}

/// Parses `github:pr:<owner>/<repo>#<number>` into `("<owner>/<repo>", <number>)`.
pub fn parse_pr_thread_key(thread_key: &str) -> Option<(String, u64)> {
    let rest = thread_key.strip_prefix("github:pr:")?;
    let (repo, num_str) = rest.split_once('#')?;
    let num = num_str.parse::<u64>().ok()?;
    Some((repo.to_owned(), num))
}

/// Parses `gh pr view --json state,mergedAt` output into `Some("merged")`, `Some("closed")`, or `None` (still open).
pub fn parse_github_pr_state_json(raw: &serde_json::Value) -> Option<String> {
    let state = raw
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_uppercase();
    let merged_at = raw
        .get("mergedAt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    if merged_at.is_some() || state == "MERGED" {
        Some("merged".to_owned())
    } else if state == "CLOSED" {
        Some("closed".to_owned())
    } else {
        None
    }
}

/// Parses `/repos/<owner>/<repo>/issues` JSON array (newly opened/updated Issues and PRs) into normalized `Trigger`s.
pub fn parse_github_issues_json(
    cfg: &GithubAdapterConfig,
    repo: &str,
    raw: &serde_json::Value,
    since_iso: Option<&str>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut latest_iso: Option<String> = since_iso.map(str::to_owned);

    let Some(arr) = raw.as_array() else {
        return (triggers, latest_iso);
    };

    for item in arr {
        let number = item.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
        if number == 0 {
            continue;
        }
        let created_at = item
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if let Some(since) = since_iso
            && !created_at.is_empty()
            && created_at.as_str() <= since
        {
            continue;
        }
        if !created_at.is_empty() {
            latest_iso = Some(match latest_iso {
                Some(cur) if cur >= created_at => cur,
                _ => created_at.clone(),
            });
        }

        let login = item
            .pointer("/user/login")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let body = item.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if body.contains("<!-- nexus-agent -->") {
            continue;
        }
        let is_pr = item.get("pull_request").is_some();
        // Skip PRs opened by our own bot_login unless they explicitly mention `@nexus`
        if is_pr
            && login.eq_ignore_ascii_case(&cfg.bot_login)
            && !body.to_lowercase().contains("@nexus")
        {
            continue;
        }

        let thread_key = if is_pr {
            format!("github:pr:{repo}#{number}")
        } else {
            format!("github:issue:{repo}#{number}")
        };
        let combined = if body.trim().is_empty() {
            format!("[GitHub #{number}] {title}")
        } else {
            format!("[GitHub #{number}] {title}\n{body}")
        };
        let lower = combined.to_lowercase();
        let addressed = !is_pr
            || lower.contains(&format!("@{}", cfg.bot_login.to_lowercase()))
            || lower.contains("@nexus")
            || lower.contains("奈克斯");
        let (clean_text, inline_target) = parse_inline_target_directive(&combined);

        triggers.push(Trigger {
            id: format!("gh-item-{repo}-{number}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Github,
            external_id: format!("item-{number}"),
            thread_key,
            actor_id: login,
            actor_is_bot: false,
            addressed_agent: addressed,
            text: clean_text,
            reply_to_trigger_id: None,
            inline_target,
            reply_channel: Some(ChannelAddress::GithubIssueComment {
                source_id: cfg.source_id.clone(),
                repo: repo.to_owned(),
                number,
            }),
            at: created_at,
            meta: BTreeMap::new(),
        });
    }

    (triggers, latest_iso)
}

/// Parses `/repos/<owner>/<repo>/issues/comments` JSON array into normalized `Trigger`s.
pub fn parse_github_issue_comments_json(
    cfg: &GithubAdapterConfig,
    repo: &str,
    raw: &serde_json::Value,
    since_iso: Option<&str>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut latest_iso: Option<String> = since_iso.map(str::to_owned);

    let Some(arr) = raw.as_array() else {
        return (triggers, latest_iso);
    };

    for item in arr {
        let comment_id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        if comment_id == 0 {
            continue;
        }

        let updated_at = item
            .get("updated_at")
            .or_else(|| item.get("created_at"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if let Some(since) = since_iso
            && !updated_at.is_empty() && updated_at.as_str() <= since {
                continue;
            }
        if !updated_at.is_empty() {
            latest_iso = Some(match latest_iso {
                Some(cur) if cur >= updated_at => cur,
                _ => updated_at.clone(),
            });
        }

        let login = item
            .pointer("/user/login")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let body = item.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if body.contains("<!-- nexus-agent -->") {
            continue;
        }
        let user_type = item
            .pointer("/user/type")
            .and_then(|v| v.as_str())
            .unwrap_or("User");
        let actor_is_bot = user_type.eq_ignore_ascii_case("Bot") || login.ends_with("[bot]");
        if actor_is_bot && login.eq_ignore_ascii_case(&cfg.bot_login) {
            continue;
        }

        let issue_url = item
            .get("issue_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let number = issue_url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);

        let is_pr = item
            .get("html_url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| u.contains("/pull/"));
        let thread_key = if is_pr {
            format!("github:pr:{repo}#{number}")
        } else {
            format!("github:issue:{repo}#{number}")
        };

        let addressed = body
            .to_lowercase()
            .contains(&format!("@{}", cfg.bot_login.to_lowercase()))
            || body.to_lowercase().contains("@nexus");
        let (clean_text, inline_target) = parse_inline_target_directive(body);

        triggers.push(Trigger {
            id: format!("gh-ic-{comment_id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Github,
            external_id: format!("ic-{comment_id}"),
            thread_key,
            actor_id: login,
            actor_is_bot,
            addressed_agent: addressed,
            text: clean_text,
            reply_to_trigger_id: None,
            inline_target,
            reply_channel: Some(ChannelAddress::GithubIssueComment {
                source_id: cfg.source_id.clone(),
                repo: repo.to_owned(),
                number,
            }),
            at: updated_at,
            meta: BTreeMap::new(),
        });
    }

    (triggers, latest_iso)
}

/// Parses `/repos/<owner>/<repo>/pulls/comments` JSON array into normalized `Trigger`s.
pub fn parse_github_review_comments_json(
    cfg: &GithubAdapterConfig,
    repo: &str,
    raw: &serde_json::Value,
    since_iso: Option<&str>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut latest_iso: Option<String> = since_iso.map(str::to_owned);

    let Some(arr) = raw.as_array() else {
        return (triggers, latest_iso);
    };

    for item in arr {
        let comment_id = item.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        if comment_id == 0 {
            continue;
        }

        let updated_at = item
            .get("updated_at")
            .or_else(|| item.get("created_at"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if let Some(since) = since_iso
            && !updated_at.is_empty() && updated_at.as_str() <= since {
                continue;
            }
        if !updated_at.is_empty() {
            latest_iso = Some(match latest_iso {
                Some(cur) if cur >= updated_at => cur,
                _ => updated_at.clone(),
            });
        }

        let login = item
            .pointer("/user/login")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let body = item.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if body.contains("<!-- nexus-agent -->") {
            continue;
        }
        let user_type = item
            .pointer("/user/type")
            .and_then(|v| v.as_str())
            .unwrap_or("User");
        let actor_is_bot = user_type.eq_ignore_ascii_case("Bot") || login.ends_with("[bot]");
        if actor_is_bot && login.eq_ignore_ascii_case(&cfg.bot_login) {
            continue;
        }

        let pr_url = item
            .get("pull_request_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pull_number = pr_url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let addressed = body
            .to_lowercase()
            .contains(&format!("@{}", cfg.bot_login.to_lowercase()))
            || body.to_lowercase().contains("@nexus");
        let (clean_text, inline_target) = parse_inline_target_directive(body);
        let full_text = if path.is_empty() {
            clean_text
        } else {
            format!("[File `{path}`] {clean_text}")
        };

        triggers.push(Trigger {
            id: format!("gh-rc-{comment_id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Github,
            external_id: format!("rc-{comment_id}"),
            thread_key: format!("github:pr:{repo}#{pull_number}"),
            actor_id: login,
            actor_is_bot,
            addressed_agent: addressed,
            text: full_text,
            reply_to_trigger_id: item
                .get("in_reply_to_id")
                .and_then(|v| v.as_u64())
                .map(|id| format!("gh-rc-{id}")),
            inline_target,
            reply_channel: Some(ChannelAddress::GithubReviewReply {
                source_id: cfg.source_id.clone(),
                repo: repo.to_owned(),
                pull_number,
                comment_id,
            }),
            at: updated_at,
            meta: BTreeMap::new(),
        });
    }

    (triggers, latest_iso)
}

impl SourceAdapter for GithubAdapter {
    fn source_id(&self) -> &str {
        &self.config.source_id
    }

    fn source_kind(&self) -> SourceKindId {
        SourceKindId::Github
    }

    async fn poll(&self, cursor: Option<&str>) -> anyhow::Result<SourcePollOutput> {
        let mut out = SourcePollOutput::default();
        let mut latest_cursor: Option<String> = cursor.map(str::to_owned);

        for repo in &self.config.repos {
            let issues_endpoint = format!("repos/{repo}/issues?state=open&per_page=20");
            if let Ok(iss_out) = self
                .build_command()
                .args(["api", &issues_endpoint])
                .output()
                .await
                && iss_out.status.success()
                && let Ok(raw) = serde_json::from_slice::<serde_json::Value>(&iss_out.stdout)
            {
                let (mut trigs, _) =
                    parse_github_issues_json(&self.config, repo, &raw, None);
                out.triggers.append(&mut trigs);
            }

            let ic_endpoint = match cursor {
                Some(since) => {
                    format!("repos/{repo}/issues/comments?since={since}&per_page=30")
                }
                None => format!("repos/{repo}/issues/comments?per_page=30"),
            };
            let ic_out = self
                .build_command()
                .args(["api", &ic_endpoint])
                .output()
                .await?;
            if ic_out.status.success() {
                let raw: serde_json::Value = serde_json::from_slice(&ic_out.stdout)?;
                let (mut trigs, max_iso) =
                    parse_github_issue_comments_json(&self.config, repo, &raw, cursor);
                out.triggers.append(&mut trigs);
                if let Some(iso) = max_iso {
                    latest_cursor = Some(match latest_cursor {
                        Some(cur) if cur >= iso => cur,
                        _ => iso,
                    });
                }
            }

            let rc_endpoint = match cursor {
                Some(since) => {
                    format!("repos/{repo}/pulls/comments?since={since}&per_page=30")
                }
                None => format!("repos/{repo}/pulls/comments?per_page=30"),
            };
            let rc_out = self
                .build_command()
                .args(["api", &rc_endpoint])
                .output()
                .await?;
            if rc_out.status.success() {
                let raw: serde_json::Value = serde_json::from_slice(&rc_out.stdout)?;
                let (mut trigs, max_iso) =
                    parse_github_review_comments_json(&self.config, repo, &raw, cursor);
                out.triggers.append(&mut trigs);
                if let Some(iso) = max_iso {
                    latest_cursor = Some(match latest_cursor {
                        Some(cur) if cur >= iso => cur,
                        _ => iso,
                    });
                }
            }
        }

        for pr_thread in &self.config.followed_pr_threads {
            if let Ok(Some(outcome)) = self.check_pr_closed(pr_thread).await {
                out.closed_threads.push((pr_thread.clone(), outcome));
            }
        }

        if let Some(iso) = latest_cursor {
            out.updated_cursors.push(("since_iso".to_owned(), iso));
        }

        Ok(out)
    }
}

impl ChannelAdapter for GithubAdapter {
    fn can_deliver(&self, address: &ChannelAddress) -> bool {
        match address {
            ChannelAddress::GithubIssueComment { source_id, .. }
            | ChannelAddress::GithubReviewReply { source_id, .. } => {
                source_id == &self.config.source_id
            }
            _ => false,
        }
    }

    async fn send_message(&self, message: &OutboxMessage) -> anyhow::Result<()> {
        match &message.channel {
            ChannelAddress::GithubIssueComment { repo, number, .. } => {
                let endpoint = format!("repos/{repo}/issues/{number}/comments");
                let body_arg = format!("body={}\n\n<!-- nexus-agent -->", message.body);
                let out = self
                    .build_command()
                    .args(["api", &endpoint, "-f", &body_arg])
                    .output()
                    .await?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("gh issue comment failed: {stderr}");
                }
                Ok(())
            }
            ChannelAddress::GithubReviewReply {
                repo,
                pull_number,
                comment_id,
                ..
            } => {
                let endpoint =
                    format!("repos/{repo}/pulls/{pull_number}/comments/{comment_id}/replies");
                let body_arg = format!("body={}\n\n<!-- nexus-agent -->", message.body);
                let out = self
                    .build_command()
                    .args(["api", &endpoint, "-f", &body_arg])
                    .output()
                    .await?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("gh review reply failed: {stderr}");
                }
                Ok(())
            }
            _ => anyhow::bail!("GithubAdapter cannot deliver non-GitHub channel address"),
        }
    }
}
