//! Configuration schema (`NexusConfig`) and host CLI toolchain probing (`probe_environment`).

use crate::adapters::feishu::FeishuAdapterConfig;
use crate::adapters::github::GithubAdapterConfig;
use crate::core::gate::SourceAdmissionPolicy;
use crate::core::model::{AcceptMode, AdmissionStrength, ProactiveRule};
use crate::router::jev_http::HttpJevConfig;
use crate::router::orchestrator::RouterLlmConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Top-level configuration for a Nexus daemon instance (`<state_dir>/config.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NexusConfig {
    pub state_dir: PathBuf,
    pub worktree_root: PathBuf,
    pub http_addr: String,
    pub accept_mode: AcceptMode,
    pub max_concurrent_workers: usize,
    pub feishu: FeishuAdapterConfig,
    pub github: GithubAdapterConfig,
    #[serde(default)]
    pub default_repo: Option<String>,
    pub repositories: BTreeMap<String, PathBuf>,
    pub allowed_actors: HashSet<String>,
    pub allowed_bots: HashSet<String>,
    pub proactive_rules: Vec<ProactiveRule>,
    pub router_llm: RouterLlmConfig,
    pub jev: Option<HttpJevConfig>,
}

impl NexusConfig {
    pub fn default_with_state_dir(state_dir: PathBuf) -> Self {
        let worktree_root = state_dir.join("worktrees");
        Self {
            state_dir,
            worktree_root,
            http_addr: "127.0.0.1:3082".to_owned(),
            accept_mode: AcceptMode::Auto,
            max_concurrent_workers: 4,
            feishu: FeishuAdapterConfig::default(),
            github: GithubAdapterConfig::default(),
            default_repo: Some("xdlkc/nexus".to_owned()),
            repositories: BTreeMap::new(),
            allowed_actors: HashSet::new(),
            allowed_bots: HashSet::from(["ci-bot".to_owned(), "github-actions[bot]".to_owned()]),
            proactive_rules: vec![ProactiveRule {
                id: "bug_repro_fix".to_owned(),
                name: "Proactive Bug Reproduction & Fix".to_owned(),
                description: "Reproduce reported bugs or panics and open a fix PR".to_owned(),
                match_criteria: "Message reports a concrete bug, stack trace, or failing test"
                    .to_owned(),
                enabled: true,
                default_target: None,
            }],
            router_llm: RouterLlmConfig::default(),
            jev: None,
        }
    }

    pub fn config_file_path(state_dir: &Path) -> PathBuf {
        state_dir.join("config.json")
    }

    pub fn load_or_default(state_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::config_file_path(state_dir);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&raw)?)
        } else {
            Ok(Self::default_with_state_dir(state_dir.to_path_buf()))
        }
    }

    pub fn save(&self) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(&self.state_dir)?;
        let path = Self::config_file_path(&self.state_dir);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    /// Synchronizes `self.repositories` local checkout paths from `self.github.repos`.
    pub fn sync_local_repositories(&mut self) {
        let cwd = std::env::current_dir().ok();
        let parent_dir = cwd.as_ref().and_then(|d| d.parent().map(|p| p.to_path_buf()));
        for r in &self.github.repos {
            let short = r.rsplit('/').next().unwrap_or(r);
            if let Some(base) = &parent_dir {
                let candidate = base.join(short);
                if candidate.exists() {
                    self.repositories.insert(r.clone(), candidate.clone());
                    self.repositories.insert(short.to_owned(), candidate);
                    continue;
                }
            }
            if let Some(cur) = &cwd
                && cur.file_name().and_then(|n| n.to_str()) == Some(short)
            {
                self.repositories.insert(r.clone(), cur.clone());
                self.repositories.insert(short.to_owned(), cur.clone());
            }
        }
    }

    /// Builds the Tier-0 `SourceAdmissionPolicy` from configured allowlists.
    /// If `allowed_actors` is empty, defaults to allowing addressed and inferred messages during onboarding.
    pub fn admission_policy(&self) -> SourceAdmissionPolicy {
        if self.allowed_actors.is_empty() {
            SourceAdmissionPolicy {
                allowed_actors: HashSet::new(),
                allowed_bots: self.allowed_bots.clone(),
                unlisted_addressed: AdmissionStrength::Explicit,
                listed_unaddressed: AdmissionStrength::Inferred,
                unlisted_unaddressed: AdmissionStrength::Inferred,
            }
        } else {
            SourceAdmissionPolicy {
                allowed_actors: self.allowed_actors.clone(),
                allowed_bots: self.allowed_bots.clone(),
                unlisted_addressed: AdmissionStrength::Explicit,
                listed_unaddressed: AdmissionStrength::Inferred,
                unlisted_unaddressed: AdmissionStrength::NotAdmitted,
            }
        }
    }
}

/// Probe result for a single external CLI tool (`lark-cli`, `gh`, `claude`, `codex`, `pi`, `opencode`, `gws`, `dws`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProbe {
    pub binary: String,
    pub installed: bool,
    pub version_or_note: String,
}

/// Probes the host environment for peripheral and worker harness CLIs (`nexus setup`).
pub fn probe_environment() -> Vec<ToolProbe> {
    let home = std::env::var("HOME").unwrap_or_default();
    let current_path = std::env::var("PATH").unwrap_or_default();
    let augmented_path = format!(
        "{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{current_path}"
    );

    let tools = [
        ("lark-cli", &["--version"][..]),
        ("gh", &["--version"][..]),
        ("git", &["--version"][..]),
        ("pi", &["--version"][..]),
        ("claude", &["--version"][..]),
        ("codex", &["--version"][..]),
        ("opencode", &["--version"][..]),
        ("gws", &["--version"][..]),
        ("dws", &["--version"][..]),
    ];

    tools
        .into_iter()
        .map(|(bin, args)| {
            match Command::new(bin)
                .env("PATH", &augmented_path)
                .args(args)
                .output()
            {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                    let first_line = text
                        .lines()
                        .next()
                        .unwrap_or("installed")
                        .chars()
                        .take(80)
                        .collect();
                    ToolProbe {
                        binary: bin.to_owned(),
                        installed: true,
                        version_or_note: first_line,
                    }
                }
                Err(_) => ToolProbe {
                    binary: bin.to_owned(),
                    installed: false,
                    version_or_note: "not found in PATH".to_owned(),
                },
            }
        })
        .collect()
}
