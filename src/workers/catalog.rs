//! Read the installed Pi model catalog without exposing credentials.
use serde::Serialize;
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CatalogModel {
    pub provider: String,
    pub model: String,
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
    let mut command = tokio::process::Command::new("pi");
    command
        .arg("--list-models")
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
