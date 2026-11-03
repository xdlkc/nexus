//! Read the installed model catalogs per Harness (`codex`, `claude-code`, `pi-rpc`, `pi-native`, `opencode`)
//! without exposing credentials.
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CatalogModel {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessCatalog {
    pub default_provider: String,
    pub default_model: String,
    pub default_think_effort: String,
    pub think_efforts: Vec<String>,
    pub providers: BTreeMap<String, Vec<String>>,
}

pub fn parse_pi_models(output: &str) -> Vec<CatalogModel> {
    let mut rows = BTreeSet::new();
    let mut in_table = false;
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.starts_with(&["provider", "model", "context"]) {
            in_table = true;
            continue;
        }
        if in_table
            && fields.len() == 6
            && matches!(fields[4], "yes" | "no")
            && matches!(fields[5], "yes" | "no")
        {
            rows.insert(CatalogModel {
                provider: fields[0].into(),
                model: fields[1].into(),
            });
        }
    }
    rows.into_iter().collect()
}

pub async fn load_pi_models() -> anyhow::Result<Vec<CatalogModel>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let cur_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!(
        "{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{cur_path}"
    );
    let mut command = tokio::process::Command::new("pi");
    command
        .arg("--list-models")
        .env("PATH", path_env)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| anyhow::anyhow!("读取 Pi 模型目录超时"))?
        .map_err(|_| anyhow::anyhow!("无法运行 pi --list-models，请检查 Pi 是否安装"))?;
    anyhow::ensure!(output.status.success(), "Pi 模型目录读取失败");
    let models = parse_pi_models(&String::from_utf8_lossy(&output.stdout));
    anyhow::ensure!(!models.is_empty(), "Pi 未返回可用模型，请检查 Pi 配置");
    Ok(models)
}

/// Probes all 5 Harnesses (`codex`, `claude-code`, `pi-rpc`, `pi-native`, `opencode`) from host CLI configs.
pub async fn load_all_harness_catalogs() -> (Vec<CatalogModel>, BTreeMap<String, HarnessCatalog>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let cur_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!(
        "{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{cur_path}"
    );

    let pi_rows = load_pi_models().await.unwrap_or_default();
    let mut pi_by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in &pi_rows {
        pi_by_provider
            .entry(row.provider.clone())
            .or_default()
            .push(row.model.clone());
    }

    let openai_models = pi_by_provider.get("openai").cloned().unwrap_or_else(|| {
        vec![
            "gpt-6-astra".to_owned(),
            "gpt-6.1-sol".to_owned(),
            "gpt-5.6-sol".to_owned(),
            "gpt-5.3-codex".to_owned(),
            "o3-pro".to_owned(),
        ]
    });

    // 1. Codex (`~/.codex/config.toml`)
    let mut codex_default_model = "gpt-6-astra".to_owned();
    let mut codex_default_think = "high".to_owned();
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.codex/config.toml")) {
        for line in raw.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("model =") {
                let val = rest.trim().trim_matches('"');
                if !val.is_empty() {
                    codex_default_model = val.to_owned();
                }
            } else if let Some(rest) = trimmed.strip_prefix("model_reasoning_effort =") {
                let val = rest.trim().trim_matches('"');
                if !val.is_empty() {
                    codex_default_think = val.to_owned();
                }
            }
        }
    }
    let mut codex_models = openai_models.clone();
    if !codex_models.contains(&codex_default_model) {
        codex_models.insert(0, codex_default_model.clone());
    }
    let codex_catalog = HarnessCatalog {
        default_provider: "openai".to_owned(),
        default_model: codex_default_model,
        default_think_effort: codex_default_think,
        think_efforts: vec![
            "minimal".to_owned(),
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: BTreeMap::from([("openai".to_owned(), codex_models)]),
    };

    // 2. Claude Code (`~/.claude/settings.json`)
    let mut claude_default_provider = "anthropic".to_owned();
    let mut claude_default_model = "claude-sonnet-4-6".to_owned();
    let mut claude_providers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.claude/settings.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        if let Some(m) = val.get("model").and_then(|v| v.as_str()) {
            claude_default_model = m.to_owned();
            if m.contains('/') {
                claude_default_provider = "hub".to_owned();
            }
        }
        if let Some(opts) = val
            .pointer("/modelPicker/options")
            .and_then(|v| v.as_array())
        {
            for opt in opts {
                if let Some(m) = opt.get("model").and_then(|v| v.as_str()) {
                    let prov = opt
                        .get("label")
                        .and_then(|v| v.as_str())
                        .and_then(|l| l.split_once('/').map(|(p, _)| p.to_owned()))
                        .unwrap_or_else(|| "hub".to_owned());
                    let list = claude_providers.entry(prov).or_default();
                    if !list.contains(&m.to_owned()) {
                        list.push(m.to_owned());
                    }
                }
            }
        }
    }
    claude_providers.entry("anthropic".to_owned()).or_insert_with(|| {
        vec![
            "claude-sonnet-4-6".to_owned(),
            "claude-opus-4-6".to_owned(),
            "claude-haiku-4-5".to_owned(),
        ]
    });
    let claude_catalog = HarnessCatalog {
        default_provider: claude_default_provider,
        default_model: claude_default_model,
        default_think_effort: "high".to_owned(),
        think_efforts: vec![
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: claude_providers,
    };

    // 3. Pi RPC (`pi --list-models`)
    let mut pi_rpc_providers = pi_by_provider.clone();
    if pi_rpc_providers.is_empty() {
        pi_rpc_providers.insert("openai".to_owned(), openai_models.clone());
    }
    let pi_rpc_def_prov = if pi_rpc_providers.contains_key("hub") {
        "hub".to_owned()
    } else {
        pi_rpc_providers.keys().next().cloned().unwrap_or_else(|| "openai".to_owned())
    };
    let pi_rpc_def_model = pi_rpc_providers
        .get(&pi_rpc_def_prov)
        .and_then(|m| m.first().cloned())
        .unwrap_or_else(|| "gpt-6-astra".to_owned());
    let pi_rpc_catalog = HarnessCatalog {
        default_provider: pi_rpc_def_prov,
        default_model: pi_rpc_def_model,
        default_think_effort: "medium".to_owned(),
        think_efforts: vec![
            "off".to_owned(),
            "minimal".to_owned(),
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: pi_rpc_providers,
    };

    // 4. Pi Native (Direct Anthropic & OpenAI HTTP)
    let pi_native_providers = BTreeMap::from([
        (
            "anthropic".to_owned(),
            vec![
                "claude-sonnet-4-6".to_owned(),
                "claude-opus-4-6".to_owned(),
                "claude-haiku-4-5".to_owned(),
            ],
        ),
        ("openai".to_owned(), openai_models.clone()),
    ]);
    let pi_native_catalog = HarnessCatalog {
        default_provider: "anthropic".to_owned(),
        default_model: "claude-sonnet-4-6".to_owned(),
        default_think_effort: "medium".to_owned(),
        think_efforts: vec![
            "minimal".to_owned(),
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: pi_native_providers,
    };

    // 5. OpenCode (`opencode models`)
    let mut opencode_providers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut oc_cmd = tokio::process::Command::new("opencode");
    oc_cmd
        .arg("models")
        .env("PATH", path_env)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), oc_cmd.output()).await
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let trimmed = line.trim();
            if let Some((prov, model)) = trimmed.split_once('/')
                && !prov.is_empty()
                && !model.is_empty()
            {
                let list = opencode_providers.entry(prov.to_owned()).or_default();
                if !list.contains(&model.to_owned()) {
                    list.push(model.to_owned());
                }
            }
        }
    }
    if opencode_providers.is_empty() {
        opencode_providers = pi_rpc_catalog.providers.clone();
    }
    let oc_def_prov = if opencode_providers.contains_key("anthropic") {
        "anthropic".to_owned()
    } else {
        opencode_providers.keys().next().cloned().unwrap_or_else(|| "openai".to_owned())
    };
    let oc_def_model = opencode_providers
        .get(&oc_def_prov)
        .and_then(|m| m.first().cloned())
        .unwrap_or_else(|| "claude-sonnet-4-6".to_owned());
    let opencode_catalog = HarnessCatalog {
        default_provider: oc_def_prov,
        default_model: oc_def_model,
        default_think_effort: "medium".to_owned(),
        think_efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        providers: opencode_providers,
    };

    let harnesses = BTreeMap::from([
        ("codex".to_owned(), codex_catalog),
        ("claude-code".to_owned(), claude_catalog),
        ("pi-rpc".to_owned(), pi_rpc_catalog),
        ("pi-native".to_owned(), pi_native_catalog),
        ("opencode".to_owned(), opencode_catalog),
    ]);

    (pi_rows, harnesses)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_catalog_and_ignores_diagnostics_and_duplicates() {
        let rows = parse_pi_models(
            "Warning: no models match\nprovider model context max-out thinking images\nanthropic claude-test 200K 64K yes yes\nhub vendor/model 1M 8K no yes\nanthropic claude-test 200K 64K yes yes\ninvalid row\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].model, "vendor/model");
        assert!(parse_pi_models("No models available").is_empty());
    }
}
