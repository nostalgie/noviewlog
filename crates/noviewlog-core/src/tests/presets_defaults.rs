use crate::core::config::{load_bundled_config, load_config_from_yaml};
use crate::core::formats::builtin_format_presets;

#[test]
fn bundled_defaults_include_popular_presets() {
    let cfg = load_bundled_config();
    assert_eq!(cfg.default_preset, "node-dev");
    assert_eq!(cfg.default_format, "node-default");
    for id in [
        "node-dev",
        "node-errors",
        "php-dev",
        "php-errors",
        "python-dev",
        "python-errors",
        "go-errors",
        "nginx-access",
        "docker-compose",
    ] {
        assert!(
            cfg.presets.contains_key(id),
            "bundled defaults missing preset {id}"
        );
        assert!(
            !cfg.presets[id].filters.is_empty(),
            "preset {id} should have filters"
        );
    }
    let node = &cfg.presets["node-dev"];
    assert!(
        node.filters
            .iter()
            .any(|f| f.id == "hide-deprecation" && f.enabled),
        "node-dev should enable hide-deprecation"
    );
}

#[test]
fn user_config_overrides_and_adds_presets() {
    let yaml = r#"
default_format: node-default
default_preset: my-custom
presets:
  node-dev:
    filters:
      - id: hide-deprecation
        type: exclude
        pattern: 'CUSTOM_DEP'
        enabled: false
        use_regex: true
  my-custom:
    filters:
      - id: mine
        type: include
        pattern: 'boom'
        enabled: true
        use_regex: true
"#;
    let cfg = load_config_from_yaml(yaml);
    assert_eq!(cfg.default_preset, "my-custom");
    assert!(
        cfg.presets.contains_key("php-dev"),
        "bundled presets remain"
    );
    assert!(cfg.presets.contains_key("my-custom"), "user-added preset");
    let node = &cfg.presets["node-dev"];
    let hide = node
        .filters
        .iter()
        .find(|f| f.id == "hide-deprecation")
        .expect("hide-deprecation");
    assert_eq!(hide.pattern, "CUSTOM_DEP");
    assert!(!hide.enabled, "user override wins for same preset id");
    let custom = &cfg.presets["my-custom"];
    assert_eq!(custom.filters[0].id, "mine");
}

#[test]
fn builtin_formats_include_stack_languages() {
    let formats = builtin_format_presets();
    for id in [
        "node-default",
        "pino",
        "php-default",
        "python-default",
        "go-default",
        "raw",
    ] {
        assert!(formats.contains_key(id), "missing format {id}");
        let f = &formats[id];
        assert!(!f.start.is_empty(), "format {id} needs a start pattern");
    }
    assert!(
        !formats["python-default"].continuation.is_empty(),
        "python-default should group traceback frames"
    );
    assert!(
        !formats["php-default"].continuation.is_empty(),
        "php-default should group stack frames"
    );
    assert!(
        !formats["go-default"].continuation.is_empty(),
        "go-default should group panic stacks"
    );
}
