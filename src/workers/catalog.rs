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

fn push_unique(list: &mut Vec<String>, item: impl Into<String>) {
    let s = item.into();
    if !s.is_empty() && !list.contains(&s) {
        list.push(s);
    }
}

/// Loads Pi's full provider -> models map and defaults by combining:
/// 1. `~/.pi/agent/settings.json` (`defaultProvider`, `defaultModel`, `defaultThinkingLevel`, `enabledModels`)
/// 2. `~/.pi/agent/models.json` (`providers.<p>.models[].id`)
/// 3. `pi --list-models` CLI output
/// 4. `~/.pi/agent/models-store.json` (`<p>.models[].id`)
fn build_pi_provider_catalog(
    home: &str,
    cli_rows: &[CatalogModel],
) -> (BTreeMap<String, Vec<String>>, String, String, String) {
    let mut by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut default_provider = "hub".to_owned();
    let mut default_model = "glm/glm-5.3".to_owned();
    let mut default_think = "high".to_owned();

    // 1. Read `~/.pi/agent/models.json` first (highest priority: user-configured custom providers like `hub`, `kimi-coding`)
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.pi/agent/models.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(providers) = val.get("providers").and_then(|v| v.as_object())
    {
        for (prov, p_obj) in providers {
            if let Some(models) = p_obj.get("models").and_then(|v| v.as_array()) {
                let list = by_provider.entry(prov.clone()).or_default();
                for m in models {
                    if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                        push_unique(list, id);
                    }
                }
            }
        }
    }

    // 2. Read `~/.pi/agent/settings.json` for defaults and `enabledModels` (prioritize enabled models at top)
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.pi/agent/settings.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        if let Some(dp) = val.get("defaultProvider").and_then(|v| v.as_str())
            && !dp.is_empty()
        {
            default_provider = dp.to_owned();
        }
        if let Some(dm) = val.get("defaultModel").and_then(|v| v.as_str())
            && !dm.is_empty()
        {
            default_model = dm.to_owned();
        }
        if let Some(dt) = val.get("defaultThinkingLevel").and_then(|v| v.as_str())
            && !dt.is_empty()
        {
            default_think = dt.to_owned();
        }
        if let Some(enabled) = val.get("enabledModels").and_then(|v| v.as_array()) {
            for item in enabled {
                if let Some(full) = item.as_str()
                    && let Some((prov, model)) = full.split_once('/')
                {
                    let list = by_provider.entry(prov.to_owned()).or_default();
                    push_unique(list, model);
                }
            }
        }
    }

    // 3. Merge `pi --list-models` rows
    for row in cli_rows {
        let list = by_provider.entry(row.provider.clone()).or_default();
        push_unique(list, &row.model);
    }

    // 4. Merge `~/.pi/agent/models-store.json`
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.pi/agent/models-store.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(obj) = val.as_object()
    {
        for (prov, p_obj) in obj {
            if let Some(models) = p_obj.get("models").and_then(|v| v.as_array()) {
                let list = by_provider.entry(prov.clone()).or_default();
                for m in models {
                    if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                        push_unique(list, id);
                    }
                }
            }
        }
    }

    // Ensure fallback if empty
    if by_provider.is_empty() {
        by_provider.insert(
            "anthropic".to_owned(),
            vec![
                "claude-sonnet-4-6".to_owned(),
                "claude-opus-4-6".to_owned(),
                "claude-haiku-4-5".to_owned(),
            ],
        );
        by_provider.insert(
            "openai".to_owned(),
            vec![
                "gpt-6-astra".to_owned(),
                "gpt-6.1-sol".to_owned(),
                "gpt-5.6-sol".to_owned(),
            ],
        );
    }

    if !by_provider.contains_key(&default_provider) {
        default_provider = if by_provider.contains_key("hub") {
            "hub".to_owned()
        } else {
            by_provider
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "openai".to_owned())
        };
    }

    if let Some(models) = by_provider.get_mut(&default_provider) {
        if !models.contains(&default_model) {
            if let Some(first) = models.first().cloned() {
                default_model = first;
            }
        }
    }

    (by_provider, default_provider, default_model, default_think)
}

/// Probes all 5 Harnesses (`codex`, `claude-code`, `pi-rpc`, `pi-native`, `opencode`) from host CLI configs.
pub async fn load_all_harness_catalogs() -> (Vec<CatalogModel>, BTreeMap<String, HarnessCatalog>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let cur_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!(
        "{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{cur_path}"
    );

    let pi_cli_rows = load_pi_models().await.unwrap_or_default();
    let (pi_providers, pi_def_prov, pi_def_model, pi_def_think) =
        build_pi_provider_catalog(&home, &pi_cli_rows);

    let mut all_catalog_rows = BTreeSet::new();
    for (prov, models) in &pi_providers {
        for m in models {
            all_catalog_rows.insert(CatalogModel {
                provider: prov.clone(),
                model: m.clone(),
            });
        }
    }

    // 1. Codex (`~/.codex/config.toml` + `~/.codex/models_cache.json`)
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
    let mut codex_models = Vec::new();
    push_unique(&mut codex_models, &codex_default_model);
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.codex/models_cache.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(arr) = val.get("models").and_then(|v| v.as_array())
    {
        for m in arr {
            if let Some(slug) = m
                .get("slug")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
            {
                push_unique(&mut codex_models, slug);
            }
        }
    }
    if let Some(openai_list) = pi_providers.get("openai") {
        for m in openai_list {
            push_unique(&mut codex_models, m);
        }
    }
    if codex_models.is_empty() {
        codex_models = vec![
            "gpt-6-astra".to_owned(),
            "gpt-6.1-sol".to_owned(),
            "gpt-5.6-sol".to_owned(),
            "gpt-5.3-codex".to_owned(),
        ];
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
                    push_unique(list, m);
                }
            }
        }
    }
    let anthropic_list = claude_providers.entry("anthropic".to_owned()).or_default();
    for m in [
        "claude-sonnet-4-6",
        "claude-opus-4-6",
        "claude-haiku-4-5",
        "claude-sonnet-5-5",
        "claude-opus-5-5",
    ] {
        push_unique(anthropic_list, m);
    }
    if let Some(pi_anth) = pi_providers.get("anthropic") {
        for m in pi_anth {
            push_unique(anthropic_list, m);
        }
    }
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

    // 3. Pi RPC (`~/.pi/agent/models.json` + `~/.pi/agent/settings.json` + `pi --list-models`)
    let pi_rpc_catalog = HarnessCatalog {
        default_provider: pi_def_prov.clone(),
        default_model: pi_def_model.clone(),
        default_think_effort: pi_def_think.clone(),
        think_efforts: vec![
            "off".to_owned(),
            "minimal".to_owned(),
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: pi_providers.clone(),
    };

    // 4. Pi Native (Shares the full Pi provider & model ecosystem from `~/.pi/agent/models.json` + `settings.json`,
    //    including `hub`, `kimi-coding`, `anthropic`, `openai`, `xai`)
    let pi_native_catalog = HarnessCatalog {
        default_provider: pi_def_prov,
        default_model: pi_def_model,
        default_think_effort: pi_def_think,
        think_efforts: vec![
            "minimal".to_owned(),
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
            "xhigh".to_owned(),
        ],
        providers: pi_providers,
    };

    // 5. OpenCode (`~/.config/opencode/opencode.json` + `opencode models`)
    let mut opencode_providers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut oc_def_prov = String::new();
    let mut oc_def_model = String::new();
    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.config/opencode/opencode.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        if let Some(m) = val.get("model").and_then(|v| v.as_str())
            && let Some((prov, model)) = m.split_once('/')
        {
            oc_def_prov = prov.to_owned();
            oc_def_model = model.to_owned();
        }
        if let Some(provs) = val.get("providers").and_then(|v| v.as_object()) {
            for (prov, p_obj) in provs {
                if let Some(models_obj) = p_obj.get("models").and_then(|v| v.as_object()) {
                    let list = opencode_providers.entry(prov.clone()).or_default();
                    for model_key in models_obj.keys() {
                        push_unique(list, model_key);
                    }
                }
            }
        }
    }

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
                push_unique(list, model);
            }
        }
    }
    if opencode_providers.is_empty() {
        opencode_providers = pi_rpc_catalog.providers.clone();
    }
    if oc_def_prov.is_empty() || !opencode_providers.contains_key(&oc_def_prov) {
        oc_def_prov = if opencode_providers.contains_key("hub") {
            "hub".to_owned()
        } else if opencode_providers.contains_key("anthropic") {
            "anthropic".to_owned()
        } else {
            opencode_providers
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "openai".to_owned())
        };
    }
    if oc_def_model.is_empty() {
        oc_def_model = opencode_providers
            .get(&oc_def_prov)
            .and_then(|m| m.first().cloned())
            .unwrap_or_else(|| "glm/glm-5.3".to_owned());
    }
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

    (all_catalog_rows.into_iter().collect(), harnesses)
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
