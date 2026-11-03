//! Credential-isolated wrapper shims (`gh-agent`, `lark-bot`, `dws-agent`, `gws-agent`, `nexus-worker`)
//! and `worker-gitconfig` generator.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Configuration for generating Worker credential shims.
#[derive(Debug, Clone, Default)]
pub struct ShimConfig {
    pub state_dir: PathBuf,
    pub github_token: Option<String>,
    pub github_config_dir: Option<PathBuf>,
    pub lark_profile: Option<String>,
    pub gws_account: Option<String>,
    pub nexus_bin_path: Option<PathBuf>,
}

/// Generated paths inside `<state_dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedShims {
    pub worker_bin_dir: PathBuf,
    pub gitconfig_path: PathBuf,
    pub gh_agent_path: PathBuf,
    pub lark_bot_path: PathBuf,
    pub gws_agent_path: PathBuf,
    pub dws_agent_path: PathBuf,
    pub nexus_worker_path: PathBuf,
}

/// Provisions `<state_dir>/worker-bin/{gh-agent,lark-bot,gws-agent,dws-agent,nexus-worker}` (`0755`)
/// and `<state_dir>/worker-gitconfig`.
pub fn provision_worker_shims(cfg: &ShimConfig) -> anyhow::Result<ProvisionedShims> {
    let worker_bin_dir = cfg.state_dir.join("worker-bin");
    std::fs::create_dir_all(&worker_bin_dir)?;

    let gh_agent_path = worker_bin_dir.join("gh-agent");
    let mut gh_env_lines = vec![
        "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\""
            .to_owned(),
    ];
    if let Some(tok) = &cfg.github_token {
        gh_env_lines.push(format!("export GH_TOKEN='{}'", tok.replace('\'', "'\\''")));
    }
    if let Some(dir) = &cfg.github_config_dir {
        gh_env_lines.push(format!("export GH_CONFIG_DIR='{}'", dir.display()));
    }
    let gh_script = format!(
        "#!/usr/bin/env zsh\nset -e\n{}\nexec gh \"$@\"\n",
        gh_env_lines.join("\n")
    );
    write_executable(&gh_agent_path, &gh_script)?;

    let lark_bot_path = worker_bin_dir.join("lark-bot");
    let lark_script = if let Some(profile) = &cfg.lark_profile {
        format!(
            "#!/usr/bin/env zsh\nset -e\nexport PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\"\nexec lark-cli --profile '{}' \"$@\"\n",
            profile.replace('\'', "'\\''")
        )
    } else {
        "#!/usr/bin/env zsh\nset -e\nexport PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\"\nexec lark-cli \"$@\"\n"
            .to_owned()
    };
    write_executable(&lark_bot_path, &lark_script)?;

    let gws_agent_path = worker_bin_dir.join("gws-agent");
    let gws_script = if let Some(account) = &cfg.gws_account {
        format!(
            "#!/usr/bin/env zsh\nset -e\nexport GWS_ACCOUNT='{}'\nexec gws \"$@\"\n",
            account.replace('\'', "'\\''")
        )
    } else {
        "#!/usr/bin/env zsh\nset -e\nexec gws \"$@\"\n".to_owned()
    };
    write_executable(&gws_agent_path, &gws_script)?;

    let dws_agent_path = worker_bin_dir.join("dws-agent");
    let dws_script = "#!/usr/bin/env zsh\nset -e\nexec dws \"$@\"\n";
    write_executable(&dws_agent_path, dws_script)?;

    let nexus_worker_path = worker_bin_dir.join("nexus-worker");
    let nexus_bin = cfg
        .nexus_bin_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "nexus".to_owned());
    let nexus_script = format!(
        "#!/usr/bin/env zsh\nset -e\nexec '{}' worker \"$@\"\n",
        nexus_bin.replace('\'', "'\\''")
    );
    write_executable(&nexus_worker_path, &nexus_script)?;

    let gitconfig_path = cfg.state_dir.join("worker-gitconfig");
    let gitconfig_content = format!(
        "[user]\n\
         \tname = Nexus Agent\n\
         \temail = nexus-agent@users.noreply.github.com\n\
         [url \"https://github.com/\"]\n\
         \tinsteadOf = git@github.com:\n\
         \tinsteadOf = ssh://git@github.com/\n\
         [credential \"https://github.com\"]\n\
         \thelper = \n\
         \thelper = !{} auth git-credential\n",
        gh_agent_path.display()
    );
    std::fs::write(&gitconfig_path, gitconfig_content)?;

    Ok(ProvisionedShims {
        worker_bin_dir,
        gitconfig_path,
        gh_agent_path,
        lark_bot_path,
        gws_agent_path,
        dws_agent_path,
        nexus_worker_path,
    })
}

fn write_executable(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::write(path, content)?;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}
