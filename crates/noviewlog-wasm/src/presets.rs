//! Bundled filter presets for JS hosts.
//!
//! The desktop parses `presets/defaults.yaml` (single source of truth) via
//! `noviewlog-core`; the wasm facade cannot depend on core, so it mirrors
//! only the `presets.*.filters` shape with `serde_yaml_ng` and embeds the
//! same file. Formats are ignored — the extension does not parse lines.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use noviewlog_terminal::types::FilterRule;

const BUNDLED_PRESET_YAML: &str = include_str!("../../../presets/defaults.yaml");

#[derive(Deserialize)]
struct BundledConfig {
    #[serde(default)]
    presets: BTreeMap<String, PresetYaml>,
}

#[derive(Deserialize)]
struct PresetYaml {
    #[serde(default)]
    filters: Vec<FilterRule>,
}

/// One panel preset: preset id plus its raw filter rules.
#[derive(Serialize)]
pub struct PresetDto {
    pub id: String,
    pub filters: Vec<FilterRule>,
}

/// Parse the bundled `presets/defaults.yaml` into panel DTOs (sorted by id).
pub fn builtin_presets() -> Result<Vec<PresetDto>, String> {
    let config: BundledConfig = serde_yaml::from_str(BUNDLED_PRESET_YAML)
        .map_err(|e| format!("bundled presets YAML failed to parse: {e}"))?;
    Ok(config
        .presets
        .into_iter()
        .map(|(id, preset)| PresetDto {
            id,
            filters: preset.filters,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_yaml_parses_with_known_preset_ids() {
        let presets = builtin_presets().expect("bundled YAML parses");
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        for expected in [
            "docker-compose",
            "go-errors",
            "nginx-access",
            "node-dev",
            "node-errors",
            "php-dev",
            "php-errors",
            "python-dev",
            "python-errors",
        ] {
            assert!(
                ids.contains(&expected),
                "preset '{expected}' missing from {ids:?}"
            );
        }
    }

    #[test]
    fn preset_filters_carry_the_panel_serde_shape() {
        let presets = builtin_presets().expect("bundled YAML parses");
        let node_errors = presets
            .iter()
            .find(|p| p.id == "node-errors")
            .expect("node-errors preset");
        let json = serde_json::to_value(node_errors).expect("serialize");
        assert_eq!(json["id"], "node-errors");
        let filters = json["filters"].as_array().expect("filters array");
        assert!(!filters.is_empty(), "node-errors ships rules");
        let rule = &filters[0];
        for key in ["id", "type", "pattern", "enabled", "use_regex"] {
            assert!(
                rule.get(key).is_some(),
                "rule must serialize '{key}' for the panel: {rule}"
            );
        }
        assert_eq!(rule["type"], "include");
        assert_eq!(rule["use_regex"], true);
        // The compiled regex never crosses to JS.
        assert!(rule.get("regex").is_none(), "compiled regex is skipped");
    }

    #[test]
    fn every_preset_rule_compiles_without_a_fallback_notice() {
        for preset in builtin_presets().expect("bundled YAML parses") {
            for rule in &preset.filters {
                let (_, notice) = noviewlog_terminal::types::compile_filter_checked(rule.clone());
                assert!(
                    notice.is_none(),
                    "{} rule '{}' fell back: {}",
                    preset.id,
                    rule.pattern,
                    notice.unwrap_or_default()
                );
            }
        }
    }
}
