//! YYC-265: tests for `Config` extracted from the parent module.
//!
//! `super::*` here pulls in everything `pub` from `config/mod.rs`,
//! plus the private items the tests use via the parent's module
//! visibility.

use super::*;

// ── YYC-239 (YYC-238 PR-1): active_profile resolution ──────────

#[test]
fn active_profile_unset_falls_back_to_legacy_provider() {
    let cfg: Config = toml::from_str(
        r#"
[provider]
type = "openai-compat"
base_url = "https://default.example.com"
model = "default-model"
"#,
    )
    .expect("parses");
    assert_eq!(
        cfg.active_provider_config().base_url,
        "https://default.example.com"
    );
    assert_eq!(cfg.active_provider_config().model, "default-model");
}

#[test]
fn active_profile_set_resolves_to_named_provider() {
    let cfg: Config = toml::from_str(
        r#"
active_profile = "fast-model"

[provider]
type = "openai-compat"
base_url = "https://default.example.com"
model = "default-model"

[providers.fast-model]
type = "openai-compat"
base_url = "https://fast.example.com"
model = "speedy"
"#,
    )
    .expect("parses");
    let active = cfg.active_provider_config();
    assert_eq!(active.base_url, "https://fast.example.com");
    assert_eq!(active.model, "speedy");
}

#[test]
fn active_profile_pointing_at_missing_falls_back_to_legacy() {
    let cfg: Config = toml::from_str(
        r#"
active_profile = "ghost"

[provider]
type = "openai-compat"
base_url = "https://default.example.com"
model = "default-model"
"#,
    )
    .expect("parses");
    // Falls back rather than panicking — verifies the
    // graceful-degradation contract from the issue.
    assert_eq!(
        cfg.active_provider_config().base_url,
        "https://default.example.com"
    );
}

// ── YYC-226 (YYC-165 PR-3): [extensions] gating ────────────────

fn seed_registry(ids: &[&str]) -> crate::extensions::ExtensionRegistry {
    let reg = crate::extensions::ExtensionRegistry::new();
    for id in ids {
        let mut m = crate::extensions::ExtensionMetadata::new(
            *id,
            *id,
            "0.1.0",
            crate::extensions::ExtensionSource::Builtin,
        );
        m.status = crate::extensions::ExtensionStatus::Active;
        reg.upsert(m);
    }
    reg
}

#[test]
fn extensions_disabled_array_forces_inactive() {
    let cfg: Config = toml::from_str(
        r#"
[extensions]
disabled = ["legacy-foo"]
"#,
    )
    .expect("parses");
    let reg = seed_registry(&["legacy-foo", "lint-helper"]);
    let flips = cfg.extensions.apply_to_registry(&reg);
    assert_eq!(flips, 1);
    assert_eq!(
        reg.get("legacy-foo").unwrap().status,
        crate::extensions::ExtensionStatus::Inactive
    );
    assert_eq!(
        reg.get("lint-helper").unwrap().status,
        crate::extensions::ExtensionStatus::Active
    );
}

#[test]
fn per_extension_enabled_false_overrides_active_default() {
    let cfg: Config = toml::from_str(
        r#"
[extensions.lint-helper]
enabled = false
"#,
    )
    .expect("parses");
    let reg = seed_registry(&["lint-helper"]);
    cfg.extensions.apply_to_registry(&reg);
    assert_eq!(
        reg.get("lint-helper").unwrap().status,
        crate::extensions::ExtensionStatus::Inactive
    );
}

#[test]
fn per_extension_enabled_true_promotes_inactive_to_active() {
    let cfg: Config = toml::from_str(
        r#"
[extensions.preview-tool]
enabled = true
"#,
    )
    .expect("parses");
    // Seed as inactive (the default) — the explicit enable
    // should promote it.
    let reg = crate::extensions::ExtensionRegistry::new();
    reg.upsert(crate::extensions::ExtensionMetadata::new(
        "preview-tool",
        "preview-tool",
        "0.1.0",
        crate::extensions::ExtensionSource::Builtin,
    ));
    cfg.extensions.apply_to_registry(&reg);
    assert_eq!(
        reg.get("preview-tool").unwrap().status,
        crate::extensions::ExtensionStatus::Active
    );
}

#[test]
fn disabled_array_wins_over_per_extension_enable() {
    let cfg: Config = toml::from_str(
        r#"
[extensions]
disabled = ["dangerous"]

[extensions.dangerous]
enabled = true
"#,
    )
    .expect("parses");
    let reg = seed_registry(&["dangerous"]);
    cfg.extensions.apply_to_registry(&reg);
    // `disabled` is the security-side block-list — even an
    // explicit `enabled = true` does not override it.
    assert_eq!(
        reg.get("dangerous").unwrap().status,
        crate::extensions::ExtensionStatus::Inactive
    );
}

#[test]
fn per_extension_auto_approve_input_disables_rewrite_approval() {
    let cfg: Config = toml::from_str(
        r#"
[extensions.input-demo]
auto_approve_input = true
"#,
    )
    .expect("parses");
    let reg = seed_registry(&["input-demo"]);
    reg.set_requires_user_approval("input-demo", true);
    let flips = cfg.extensions.apply_to_registry(&reg);
    assert_eq!(flips, 0);
    assert!(!reg.get("input-demo").unwrap().requires_user_approval);
}

#[test]
fn empty_extensions_table_is_a_noop() {
    let cfg: Config = toml::from_str("").expect("parses");
    let reg = seed_registry(&["a", "b"]);
    let flips = cfg.extensions.apply_to_registry(&reg);
    assert_eq!(flips, 0);
    assert_eq!(
        reg.get("a").unwrap().status,
        crate::extensions::ExtensionStatus::Active
    );
}

#[test]
fn provider_debug_mode_parses_from_toml() {
    let config: Config = toml::from_str(
        r#"
[provider]
debug = "wire"
"#,
    )
    .expect("config should parse");

    assert!(matches!(config.provider.debug, ProviderDebugMode::Wire));
}

// ── YYC-181: tool capability profile config + resolution ────────

#[test]
fn tools_profile_default_is_none() {
    let cfg: Config = toml::from_str("").expect("empty parses");
    assert!(cfg.tools.profile.is_none());
    assert!(cfg.tools.profiles.is_empty());
}

#[test]
fn tools_profile_parses_default_name() {
    let cfg: Config = toml::from_str("[tools]\nprofile = \"readonly\"\n").expect("should parse");
    assert_eq!(cfg.tools.profile.as_deref(), Some("readonly"));
}

#[test]
fn user_defined_profile_shadows_builtin() {
    let cfg: Config = toml::from_str(
        r#"
[tools.profiles.readonly]
description = "Override"
allowed = ["read_file"]
"#,
    )
    .expect("should parse");
    let resolved = cfg.tools.resolve_profile("readonly").expect("resolves");
    assert_eq!(resolved.allowed.len(), 1);
    assert!(resolved.allows("read_file"));
    assert!(!resolved.allows("git_status"));
}

#[test]
fn unknown_profile_resolves_to_none() {
    let cfg: Config = toml::from_str("").expect("empty parses");
    assert!(cfg.tools.resolve_profile("does-not-exist").is_none());
}

#[test]
fn builtin_profile_is_resolved_when_no_user_override() {
    let cfg: Config = toml::from_str("").expect("empty parses");
    let resolved = cfg
        .tools
        .resolve_profile("coding")
        .expect("built-in resolves");
    assert!(resolved.allows("write_file"));
    assert!(resolved.allows("bash"));
}

#[test]
fn native_enforcement_round_trips_each_mode() {
    for (raw, expected) in [
        ("off", NativeEnforcement::Off),
        ("warn", NativeEnforcement::Warn),
        ("block", NativeEnforcement::Block),
    ] {
        let toml = format!("[tools]\nnative_enforcement = \"{raw}\"\n");
        let cfg: Config = toml::from_str(&toml).expect("should parse");
        assert_eq!(cfg.tools.native_enforcement, expected);
    }
}

#[test]
fn native_enforcement_defaults_to_block_when_missing() {
    let cfg: Config = toml::from_str("").expect("empty parses");
    assert_eq!(cfg.tools.native_enforcement, NativeEnforcement::Block);
    let cfg: Config = toml::from_str("[tools]\n").expect("empty tools parses");
    assert_eq!(cfg.tools.native_enforcement, NativeEnforcement::Block);
}

#[test]
fn keybinds_block_parses_with_overrides() {
    let config: Config = toml::from_str(
        r#"
[keybinds]
toggle_tools = "F2"
"#,
    )
    .expect("config should parse");

    assert_eq!(config.keybinds.toggle_tools, "F2");
    assert_eq!(config.keybinds.toggle_sessions, "Ctrl+K");
    assert_eq!(config.keybinds.cancel, "Ctrl+C");
}

#[test]
fn keybinds_default_when_section_missing() {
    let config: Config = toml::from_str("").expect("empty toml is valid");
    let defaults = KeybindsConfig::default();
    assert_eq!(config.keybinds.toggle_tools, defaults.toggle_tools);
    assert_eq!(config.keybinds.toggle_sessions, defaults.toggle_sessions);
}

#[test]
fn provider_debug_mode_helpers_match_expected_scopes() {
    assert!(!ProviderDebugMode::Off.logs_wire());
    assert!(!ProviderDebugMode::Off.logs_tool_fallback());

    assert!(!ProviderDebugMode::ToolFallback.logs_wire());
    assert!(ProviderDebugMode::ToolFallback.logs_tool_fallback());

    assert!(ProviderDebugMode::Wire.logs_wire());
    assert!(ProviderDebugMode::Wire.logs_tool_fallback());
}

fn sample_job(id: &str, cron: &str) -> SchedulerJobConfig {
    SchedulerJobConfig {
        id: id.into(),
        name: "test".into(),
        enabled: true,
        cron: cron.into(),
        timezone: "UTC".into(),
        platform: "loopback".into(),
        lane: "c1".into(),
        prompt: "do thing".into(),
        max_runtime_secs: None,
        overlap_policy: OverlapPolicy::Skip,
    }
}

// YYC-17: well-formed jobs validate cleanly.
#[test]
fn scheduler_job_validate_accepts_minimal() {
    sample_job("daily", "0 8 * * * *").validate().expect("ok");
}

// YYC-17: bad cron expression bubbles up with the offending input.
#[test]
fn scheduler_job_validate_rejects_bad_cron() {
    let mut job = sample_job("bad", "obviously not a cron");
    let err = job.validate().expect_err("bad cron must error");
    assert!(format!("{err:#}").contains("invalid cron expression"));
    // Whitespace cron also rejected (parser treats it as empty).
    job.cron = "   ".into();
    assert!(job.validate().is_err());
}

// YYC-17: unknown timezone surfaces a typed error.
#[test]
fn scheduler_job_validate_rejects_unknown_timezone() {
    let mut job = sample_job("tz", "0 8 * * * *");
    job.timezone = "Mars/Olympus_Mons".into();
    let err = job.validate().expect_err("bad tz");
    assert!(format!("{err:#}").contains("invalid timezone"));
}

// YYC-17: required fields cannot be empty.
#[test]
fn scheduler_job_validate_requires_non_empty_fields() {
    let mut job = sample_job("ok", "0 8 * * * *");
    job.id = "".into();
    assert!(job.validate().is_err());
    let mut job = sample_job("ok", "0 8 * * * *");
    job.platform = "".into();
    assert!(job.validate().is_err());
    let mut job = sample_job("ok", "0 8 * * * *");
    job.lane = "".into();
    assert!(job.validate().is_err());
    let mut job = sample_job("ok", "0 8 * * * *");
    job.prompt = "".into();
    assert!(job.validate().is_err());
}

// YYC-17: max_runtime_secs = 0 is meaningless.
#[test]
fn scheduler_job_validate_rejects_zero_runtime_cap() {
    let mut job = sample_job("ok", "0 8 * * * *");
    job.max_runtime_secs = Some(0);
    assert!(job.validate().is_err());
}

// YYC-17: parse a [[scheduler.jobs]] block from TOML and round-
// trip through validate.
#[test]
fn scheduler_section_parses_from_toml() {
    let toml = r#"
            [[scheduler.jobs]]
            id = "daily-summary"
            cron = "0 8 * * * *"
            platform = "telegram"
            lane = "personal"
            prompt = "Summarize yesterday's work."
        "#;
    let cfg: Config = toml::from_str(toml).expect("parse");
    assert_eq!(cfg.scheduler.jobs.len(), 1);
    let job = &cfg.scheduler.jobs[0];
    assert_eq!(job.id, "daily-summary");
    assert!(job.enabled);
    assert_eq!(job.timezone, "UTC");
    assert_eq!(job.overlap_policy, OverlapPolicy::Skip);
    cfg.scheduler.validate().expect("validate ok");
}

// YYC-17: overlap_policy parses kebab-case variants.
#[test]
fn scheduler_overlap_policy_parses_kebab_case() {
    let toml = r#"
            [[scheduler.jobs]]
            id = "j"
            cron = "0 * * * * *"
            platform = "p"
            lane = "l"
            prompt = "x"
            overlap_policy = "replace"
        "#;
    let cfg: Config = toml::from_str(toml).expect("parse");
    assert_eq!(cfg.scheduler.jobs[0].overlap_policy, OverlapPolicy::Replace);
}

// YYC-156: stale backup files are pruned by retention age.
#[test]
fn prune_stale_bak_files_removes_aged_backups() {
    let dir = tempfile::tempdir().unwrap();
    let bak = dir.path().join("config.toml.bak");
    std::fs::write(&bak, "old contents").unwrap();
    // Backdate the mtime by 40 days, past the 30-day default.
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 60 * 60);
    let f = std::fs::File::options().write(true).open(&bak).unwrap();
    f.set_modified(aged).unwrap();
    drop(f);

    let removed = prune_stale_bak_files(
        dir.path(),
        std::time::Duration::from_secs(BAK_RETENTION_SECS),
    );
    assert_eq!(removed, 1);
    assert!(!bak.exists(), "stale .bak should have been removed");
}

// YYC-156: fresh backups (within the retention window) survive.
#[test]
fn prune_stale_bak_files_keeps_fresh_backups() {
    let dir = tempfile::tempdir().unwrap();
    let bak = dir.path().join("config.toml.bak");
    std::fs::write(&bak, "fresh contents").unwrap();

    let removed = prune_stale_bak_files(
        dir.path(),
        std::time::Duration::from_secs(BAK_RETENTION_SECS),
    );
    assert_eq!(removed, 0);
    assert!(bak.exists(), "fresh .bak must be kept");
}

// YYC-156: an unknown .bak filename is left alone — only the
// known config-backup names are eligible for cleanup so a
// user's hand-staged `mybackup.bak` stays put.
#[test]
fn prune_stale_bak_files_ignores_unknown_filenames() {
    let dir = tempfile::tempdir().unwrap();
    let bak = dir.path().join("mybackup.bak");
    std::fs::write(&bak, "user backup").unwrap();
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(365 * 24 * 60 * 60);
    let f = std::fs::File::options().write(true).open(&bak).unwrap();
    f.set_modified(aged).unwrap();
    drop(f);

    let removed = prune_stale_bak_files(
        dir.path(),
        std::time::Duration::from_secs(BAK_RETENTION_SECS),
    );
    assert_eq!(removed, 0);
    assert!(bak.exists(), "unknown .bak files must not be pruned");
}

// YYC-147: stream_channel_capacity defaults to 1024 when not
// configured, preserving prior behavior.
#[test]
fn provider_stream_channel_capacity_defaults_to_legacy_value() {
    let cfg = ProviderConfig::default();
    assert_eq!(cfg.stream_channel_capacity, STREAM_CHANNEL_CAPACITY_DEFAULT);
    assert_eq!(
        cfg.effective_stream_channel_capacity(),
        STREAM_CHANNEL_CAPACITY_DEFAULT,
    );
}

// YYC-147: explicit user override flows through.
#[test]
fn provider_stream_channel_capacity_honors_user_override() {
    let toml = r#"
            [provider]
            stream_channel_capacity = 4096
        "#;
    let cfg: Config = toml::from_str(toml).expect("parse");
    assert_eq!(cfg.provider.effective_stream_channel_capacity(), 4096,);
}

// YYC-147: nonsensical values clamp into the documented bounds
// so a typo can't OOM the host or starve the renderer.
#[test]
fn provider_stream_channel_capacity_clamps_to_bounds() {
    let mut cfg = ProviderConfig::default();
    cfg.stream_channel_capacity = 1;
    assert_eq!(
        cfg.effective_stream_channel_capacity(),
        STREAM_CHANNEL_CAPACITY_MIN,
    );
    cfg.stream_channel_capacity = 10_000_000;
    assert_eq!(
        cfg.effective_stream_channel_capacity(),
        STREAM_CHANNEL_CAPACITY_MAX,
    );
}

// YYC-161: a clean canonical config should produce no
// unknown-key warnings.
#[test]
fn detect_unknown_keys_returns_empty_for_canonical_config() {
    let toml = r#"
            [provider]
            api_key = "k"
            [tools]
            [recall]
            enabled = false
            [cortex]
            enabled = true
            [keybinds]
            [tui]
            theme = "system"
        "#;
    assert!(Config::detect_unknown_top_level_keys(toml).is_empty());
}

// YYC-161: a typo at the top level must be reported so the user
// can fix it instead of silently getting defaults.
#[test]
fn detect_unknown_keys_flags_top_level_typo() {
    let toml = r#"
            [recal]
            enabled = true
        "#;
    let unknown = Config::detect_unknown_top_level_keys(toml);
    assert_eq!(unknown, vec!["recal".to_string()]);
}

// YYC-161: more than one unknown key should be returned sorted
// and deduplicated for stable warning output.
#[test]
fn detect_unknown_keys_returns_sorted_unique_list() {
    let toml = r#"
            [zeta]
            x = 1
            [alpha]
            y = 2
            [recall]
            enabled = false
        "#;
    let unknown = Config::detect_unknown_top_level_keys(toml);
    assert_eq!(unknown, vec!["alpha".to_string(), "zeta".to_string()],);
}

// YYC-161: malformed TOML should not panic the detector — that
// path is taken before the strongly-typed parse, which will
// surface a proper parse error to the user.
#[test]
fn detect_unknown_keys_returns_empty_for_invalid_toml() {
    let raw = "this is not = valid =[ toml";
    assert!(Config::detect_unknown_top_level_keys(raw).is_empty());
}

#[test]
fn gateway_section_parses_with_defaults() {
    let toml = r#"
            [gateway]
            api_token = "test-token"
        "#;
    let cfg: Config = toml::from_str(toml).expect("parse");
    let g = cfg.gateway.expect("gateway present");
    assert_eq!(g.bind, "127.0.0.1:7777");
    assert_eq!(g.api_token, "test-token");
    assert_eq!(g.idle_ttl_secs, 1800);
    assert_eq!(g.max_concurrent_lanes, 64);
    assert_eq!(g.outbound_max_attempts, 5);
}

#[test]
fn gateway_discord_section_parses_with_defaults() {
    let toml = r#"
            [gateway]
            api_token = "test-token"

            [gateway.discord]
            enabled = true
            bot_token = "discord-token"
        "#;
    let cfg: Config = toml::from_str(toml).expect("parse");
    let discord = cfg.gateway.expect("gateway present").discord;
    assert!(discord.enabled);
    assert_eq!(discord.bot_token, "discord-token");
    assert!(!discord.allow_bots);
}

fn gateway_with_token(token: &str) -> GatewayConfig {
    GatewayConfig {
        bind: "127.0.0.1:7777".into(),
        api_token: token.into(),
        idle_ttl_secs: 1800,
        max_concurrent_lanes: 64,
        outbound_max_attempts: 5,
        discord: DiscordConfig::default(),
        telegram: TelegramConfig::default(),
        commands: HashMap::new(),
    }
}

#[test]
fn gateway_validate_accepts_minimal_config() {
    gateway_with_token("token").validate().expect("ok");
}

#[test]
fn gateway_validate_rejects_empty_api_token() {
    let err = gateway_with_token("").validate().expect_err("empty token");
    assert!(err.to_string().contains("api_token"), "msg: {err}");
}

#[test]
fn gateway_validate_rejects_whitespace_api_token() {
    let err = gateway_with_token("  ").validate().expect_err("ws token");
    assert!(err.to_string().contains("api_token"), "msg: {err}");
}

#[test]
fn gateway_validate_rejects_zero_numeric_fields() {
    let mut g = gateway_with_token("token");
    g.idle_ttl_secs = 0;
    assert!(g.validate().is_err());
    let mut g = gateway_with_token("token");
    g.max_concurrent_lanes = 0;
    assert!(g.validate().is_err());
    let mut g = gateway_with_token("token");
    g.outbound_max_attempts = 0;
    assert!(g.validate().is_err());
}

#[test]
fn gateway_validate_rejects_discord_enabled_without_token() {
    let mut g = gateway_with_token("token");
    g.discord.enabled = true;
    let err = g.validate().expect_err("discord token missing");
    assert!(err.to_string().contains("bot_token"), "msg: {err}");
}

#[test]
fn gateway_validate_rejects_telegram_enabled_without_token() {
    let mut g = gateway_with_token("token");
    g.telegram.enabled = true;
    let err = g.validate().expect_err("telegram token missing");
    assert!(err.to_string().contains("bot_token"), "msg: {err}");
}

#[test]
fn gateway_validate_rejects_telegram_poll_interval_over_cap() {
    let mut g = gateway_with_token("token");
    g.telegram.enabled = true;
    g.telegram.bot_token = "tg".into();
    g.telegram.poll_interval_secs = 60;
    let err = g.validate().expect_err("poll interval cap");
    assert!(err.to_string().contains("poll_interval_secs"), "msg: {err}");
}

#[test]
fn named_provider_profiles_parse_without_breaking_legacy_provider() {
    let toml = r#"
            [provider]
            base_url = "https://openrouter.ai/api/v1"
            api_key = "openrouter-key"
            model = "deepseek/deepseek-v4-flash"

            [providers.local]
            base_url = "http://localhost:11434/v1"
            api_key = "ollama-key"
            model = "qwen2.5-coder:latest"
            disable_catalog = true
        "#;

    let cfg: Config = toml::from_str(toml).expect("config should parse");

    assert_eq!(cfg.provider.model, "deepseek/deepseek-v4-flash");
    assert_eq!(cfg.providers["local"].base_url, "http://localhost:11434/v1");
    assert_eq!(
        cfg.providers["local"].api_key.as_deref(),
        Some("ollama-key")
    );
    assert!(cfg.providers["local"].disable_catalog);
}

#[test]
fn load_from_dir_handles_missing_files_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::load_from_dir(dir.path()).expect("empty dir → defaults");
    assert!(cfg.providers.is_empty());
    assert_eq!(cfg.keybinds.toggle_tools, "Ctrl+T");
}

#[test]
fn load_from_dir_merges_three_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
[provider]
type = "openai-compat"
base_url = "https://main.example/v1"
model = "main-1"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("keybinds.toml"),
        r#"
toggle_tools = "F2"
toggle_sessions = "Ctrl+P"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("providers.toml"),
        r#"
[local]
type = "openai-compat"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:latest"
disable_catalog = true
"#,
    )
    .unwrap();

    let cfg = Config::load_from_dir(dir.path()).unwrap();
    assert_eq!(cfg.provider.base_url, "https://main.example/v1");
    assert_eq!(cfg.keybinds.toggle_tools, "F2");
    assert_eq!(cfg.keybinds.toggle_sessions, "Ctrl+P");
    assert_eq!(cfg.providers["local"].model, "qwen2.5-coder:latest");
    assert!(cfg.providers["local"].disable_catalog);
}

#[test]
fn migrate_extracts_keybinds_and_providers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"# top comment
[provider]
type = "openai-compat"
base_url = "https://x.example/v1"
model = "x-1"

[keybinds]
toggle_tools = "F4"

[providers.local]
type = "openai-compat"
base_url = "http://localhost:11434/v1"
model = "qwen2.5"
"#,
    )
    .unwrap();

    let report = Config::migrate(dir.path(), false).unwrap();
    assert!(report.keybinds_written);
    assert!(report.providers_written);
    assert!(report.main_rewritten);

    // After split: original config.toml should no longer contain
    // [keybinds] or [providers.*], the fragment files should.
    let main_after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!main_after.contains("[keybinds]"));
    assert!(!main_after.contains("[providers"));

    let keybinds_raw = std::fs::read_to_string(dir.path().join("keybinds.toml")).unwrap();
    assert!(keybinds_raw.contains("toggle_tools = \"F4\""));

    let providers_raw = std::fs::read_to_string(dir.path().join("providers.toml")).unwrap();
    assert!(providers_raw.contains("[local]"));

    // Re-run is a no-op (idempotent).
    let report2 = Config::migrate(dir.path(), false).unwrap();
    assert!(!report2.keybinds_written);
    assert!(!report2.providers_written);

    // Round-trip: load the migrated layout and assert behavior matches
    // pre-migration.
    let cfg = Config::load_from_dir(dir.path()).unwrap();
    assert_eq!(cfg.keybinds.toggle_tools, "F4");
    assert_eq!(cfg.providers["local"].model, "qwen2.5");
}

// ── YYC-136: atomic write + rollback safety net ─────────────────────

#[test]
fn atomic_write_replaces_destination_atomically() {
    // YYC-136: after atomic_write, the destination contains the new
    // content and no .tmp file is left behind.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "old = true\n").unwrap();

    atomic_write(&path, "new = true\n").unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "new = true\n");
    assert!(!dir.path().join("config.toml.tmp").exists());
}

#[test]
fn migrate_writes_bak_snapshot_for_rollback() {
    // YYC-136: every migration run snapshots the pre-mutation
    // config.toml to config.toml.bak. After the run completes the
    // .bak still exists so the user has a manual undo path.
    let dir = tempfile::tempdir().unwrap();
    let original = "# original\n[keybinds]\ntoggle_tools = \"F4\"\n";
    std::fs::write(dir.path().join("config.toml"), original).unwrap();

    Config::migrate(dir.path(), false).unwrap();

    let bak = dir.path().join("config.toml.bak");
    assert!(bak.exists(), ".bak snapshot should survive migration");
    let bak_raw = std::fs::read_to_string(&bak).unwrap();
    assert_eq!(bak_raw, original);
}

#[test]
fn migrate_rolls_back_when_inner_step_fails() {
    // YYC-136: simulate a failure mid-migration by handing migrate
    // a pre-existing keybinds.toml that's a directory (so the write
    // attempt fails). Without rollback the user would be left with
    // a wedged config; with rollback the original config.toml is
    // restored.
    let dir = tempfile::tempdir().unwrap();
    let original = "[keybinds]\ntoggle_tools = \"F4\"\n";
    std::fs::write(dir.path().join("config.toml"), original).unwrap();

    // Create keybinds.toml as a *directory* — atomic_write will fail
    // when its rename target is a non-empty directory on Linux.
    std::fs::create_dir(dir.path().join("keybinds.toml")).unwrap();
    std::fs::write(
        dir.path().join("keybinds.toml").join("blocker"),
        "non-empty\n",
    )
    .unwrap();

    // force=true so migration tries to overwrite the (directory)
    // keybinds.toml — that's the step that errors.
    let result = Config::migrate(dir.path(), true);
    assert!(
        result.is_err(),
        "expected migration to fail when keybinds.toml is a non-empty dir"
    );

    // Rollback ran: config.toml still has its original content.
    let restored = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert_eq!(
        restored, original,
        "config.toml should be rolled back to the original snapshot"
    );

    // No .tmp leftover from the partial write.
    assert!(!dir.path().join("config.toml.tmp").exists());
}
