//! Credential-isolated wrapper shims (`gh-agent`, `lark-bot`, `dws-agent`, `gws-agent`, `nexus-worker`)
//! and isolated Agent workspace (`<state_dir>/agent-home/` + `<state_dir>/worker-gitconfig`) generator.

use crate::config::NexusConfig;
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

const CLEAN_PATH_EXPORT: &str = "export PATH=$(printf \"%s\" \"$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\" | tr ':' '\\n' | grep -vF '/worker-bin' | paste -sd ':' -)";

/// Provisions `<state_dir>/worker-bin/{gh-agent,lark-bot,gws-agent,dws-agent,nexus-worker}` (`0755`)
/// and `<state_dir>/worker-gitconfig`.
pub fn provision_worker_shims(cfg: &ShimConfig) -> anyhow::Result<ProvisionedShims> {
    let worker_bin_dir = cfg.state_dir.join("worker-bin");
    std::fs::create_dir_all(&worker_bin_dir)?;

    let gh_agent_path = worker_bin_dir.join("gh-agent");
    let mut gh_env_lines = vec![CLEAN_PATH_EXPORT.to_owned()];
    if let Some(tok) = &cfg.github_token
        && !tok.trim().is_empty()
    {
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
    // Also expose `gh` inside worker-bin so bare `gh` commands in Workers inherit the isolated identity
    write_executable(&worker_bin_dir.join("gh"), &gh_script)?;

    let lark_bot_path = worker_bin_dir.join("lark-bot");
    let lark_script = if let Some(profile) = &cfg.lark_profile
        && !profile.trim().is_empty()
    {
        let escaped = profile.replace('\'', "'\\''");
        let bare_lark_script = format!(
            "#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nif [[ \" $* \" == *\" --profile \"* ]]; then\n  exec lark-cli \"$@\"\nelse\n  exec lark-cli --profile '{escaped}' \"$@\"\nfi\n"
        );
        write_executable(&worker_bin_dir.join("lark-cli"), &bare_lark_script)?;
        format!(
            "#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nexec lark-cli --profile '{escaped}' \"$@\"\n"
        )
    } else {
        let bare_lark_script =
            format!("#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nexec lark-cli \"$@\"\n");
        write_executable(&worker_bin_dir.join("lark-cli"), &bare_lark_script)?;
        bare_lark_script
    };
    write_executable(&lark_bot_path, &lark_script)?;

    let gws_agent_path = worker_bin_dir.join("gws-agent");
    let gws_script = if let Some(account) = &cfg.gws_account
        && !account.trim().is_empty()
    {
        format!(
            "#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nexport GWS_ACCOUNT='{}'\nexec gws \"$@\"\n",
            account.replace('\'', "'\\''")
        )
    } else {
        format!("#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nexec gws \"$@\"\n")
    };
    write_executable(&gws_agent_path, &gws_script)?;
    write_executable(&worker_bin_dir.join("gws"), &gws_script)?;

    let dws_agent_path = worker_bin_dir.join("dws-agent");
    let dws_script = format!("#!/usr/bin/env zsh\nset -e\n{CLEAN_PATH_EXPORT}\nexec dws \"$@\"\n");
    write_executable(&dws_agent_path, &dws_script)?;
    write_executable(&worker_bin_dir.join("dws"), &dws_script)?;

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
    let gitconfig_content = render_worker_gitconfig(
        "Nexus Agent",
        "nexus-agent@users.noreply.github.com",
        &gh_agent_path,
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

/// Provisions the complete isolated Agent CLI workspace (`<state_dir>/agent-home/{gh,gws,dws}`),
/// `<state_dir>/worker-bin/*`, and `<state_dir>/worker-gitconfig` from `NexusConfig`.
pub fn provision_agent_workspace_from_config(cfg: &NexusConfig) -> anyhow::Result<ProvisionedShims> {
    let _ = std::fs::create_dir_all(&cfg.state_dir);
    let abs_state_dir =
        std::fs::canonicalize(&cfg.state_dir).unwrap_or_else(|_| cfg.state_dir.clone());

    let agent_home = abs_state_dir.join("agent-home");
    let gh_home = agent_home.join("gh");
    let gws_home = agent_home.join("gws");
    let dws_home = agent_home.join("dws");
    for dir in [&agent_home, &gh_home, &gws_home, &dws_home] {
        std::fs::create_dir_all(dir)?;
        let mut perms = std::fs::metadata(dir)?.permissions();
        perms.set_mode(0o700);
        let _ = std::fs::set_permissions(dir, perms);
    }

    let configured_gh_token = cfg
        .agent_identity
        .github_agent_token
        .clone()
        .filter(|s| !s.trim().is_empty());
    let github_token = configured_gh_token.clone().or_else(|| {
        std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .ok()
            .filter(|s| !s.trim().is_empty())
    });

    let use_isolated_gh = cfg.agent_identity.isolate_gh_config
        || configured_gh_token.is_some()
        || gh_home.join("hosts.yml").exists();
    let github_config_dir = if use_isolated_gh {
        Some(gh_home.clone())
    } else {
        None
    };

    let shims = provision_worker_shims(&ShimConfig {
        state_dir: abs_state_dir.clone(),
        github_token,
        github_config_dir,
        lark_profile: cfg.feishu.profile.clone(),
        gws_account: cfg.agent_identity.gws_account.clone(),
        nexus_bin_path: std::env::current_exe().ok(),
    })?;

    // Enhance gws-agent / gws and dws-agent / dws with isolated config dirs
    let mut gws_lines = vec![
        "#!/usr/bin/env zsh".to_owned(),
        "set -e".to_owned(),
        CLEAN_PATH_EXPORT.to_owned(),
        format!("export GWS_CONFIG_DIR='{}'", gws_home.display()),
    ];
    if let Some(acc) = &cfg.agent_identity.gws_account
        && !acc.trim().is_empty()
    {
        gws_lines.push(format!("export GWS_ACCOUNT='{}'", acc.replace('\'', "'\\''")));
    }
    gws_lines.push("exec gws \"$@\"".to_owned());
    let gws_script = format!("{}\n", gws_lines.join("\n"));
    write_executable(&shims.gws_agent_path, &gws_script)?;
    write_executable(&shims.worker_bin_dir.join("gws"), &gws_script)?;

    let mut dws_lines = vec![
        "#!/usr/bin/env zsh".to_owned(),
        "set -e".to_owned(),
        CLEAN_PATH_EXPORT.to_owned(),
        format!("export DWS_CONFIG_DIR='{}'", dws_home.display()),
    ];
    if let Some(prof) = &cfg.agent_identity.dws_profile
        && !prof.trim().is_empty()
    {
        dws_lines.push(format!(
            "export DWS_PROFILE='{}'",
            prof.replace('\'', "'\\''")
        ));
    }
    dws_lines.push("exec dws \"$@\"".to_owned());
    let dws_script = format!("{}\n", dws_lines.join("\n"));
    write_executable(&shims.dws_agent_path, &dws_script)?;
    write_executable(&shims.worker_bin_dir.join("dws"), &dws_script)?;

    // Write worker-gitconfig with configured git_user_name and git_user_email
    let git_name = if cfg.agent_identity.git_user_name.trim().is_empty() {
        "Nexus Agent (奈克斯)"
    } else {
        cfg.agent_identity.git_user_name.trim()
    };
    let git_email = if cfg.agent_identity.git_user_email.trim().is_empty() {
        "nexus-agent@users.noreply.github.com"
    } else {
        cfg.agent_identity.git_user_email.trim()
    };
    let gitconfig_content = render_worker_gitconfig(git_name, git_email, &shims.gh_agent_path);
    std::fs::write(&shims.gitconfig_path, gitconfig_content)?;

    Ok(shims)
}

fn render_worker_gitconfig(git_name: &str, git_email: &str, gh_agent_path: &Path) -> String {
    format!(
        "[user]\n\
         \tname = {git_name}\n\
         \temail = {git_email}\n\
         [url \"https://github.com/\"]\n\
         \tinsteadOf = git@github.com:\n\
         \tinsteadOf = ssh://git@github.com/\n\
         [credential \"https://github.com\"]\n\
         \thelper = \n\
         \thelper = !{} auth git-credential\n",
        gh_agent_path.display()
    )
}

fn write_executable(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::write(path, content)?;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}
