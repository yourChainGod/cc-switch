use cc_switch_lib::{AppState, AppType, Database, Provider, ProviderKeyInput, ProviderService};
use serde_json::json;
use std::sync::Arc;

#[test]
fn provider_key_pool_crud_and_cascade_delete() {
    let db = Database::memory().expect("create memory database");
    let app_type = AppType::Claude.as_str();

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type, &provider)
        .expect("save provider fixture");

    let later = db
        .add_provider_key(
            app_type,
            "provider-a",
            &ProviderKeyInput {
                name: "later".to_string(),
                key_value: "sk-later".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 20,
                weight: 1,
            },
        )
        .expect("insert later key");
    let earlier = db
        .add_provider_key(
            app_type,
            "provider-a",
            &ProviderKeyInput {
                name: "earlier".to_string(),
                key_value: "sk-earlier".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert earlier key");

    let keys = db
        .get_enabled_provider_keys(app_type, "provider-a")
        .expect("list enabled keys");
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].id, earlier.id);
    assert_eq!(keys[1].id, later.id);

    assert!(db
        .record_provider_key_failure(app_type, "provider-a", &earlier.id, 60, 60)
        .expect("record key failure"));
    let enabled = db
        .get_enabled_provider_keys(app_type, "provider-a")
        .expect("list enabled keys after cooldown");
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, later.id);

    assert!(db
        .reset_provider_key_health(app_type, "provider-a", &earlier.id)
        .expect("reset key health"));
    assert!(db
        .update_provider_key(
            app_type,
            "provider-a",
            &earlier.id,
            &ProviderKeyInput {
                name: "earlier renamed".to_string(),
                key_value: "sk-earlier-2".to_string(),
                auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                enabled: true,
                priority: 5,
                weight: 2,
            },
        )
        .expect("update key")
        .is_some());

    let keys = db
        .get_provider_keys(app_type, "provider-a")
        .expect("list all keys");
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].name, "earlier renamed");
    assert_eq!(keys[0].key_value, "sk-earlier-2");
    assert_eq!(keys[0].auth_field.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));

    db.delete_provider(app_type, "provider-a")
        .expect("delete provider");
    let keys = db
        .get_provider_keys(app_type, "provider-a")
        .expect("list keys after provider delete");
    assert!(keys.is_empty(), "provider_keys should cascade delete");
}

#[test]
fn provider_key_summaries_return_aggregate_health_without_key_values() {
    let db = Database::memory().expect("create memory database");
    let app_type = AppType::Claude.as_str();

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type, &provider)
        .expect("save provider fixture");

    db.add_provider_key(
        app_type,
        "provider-a",
        &ProviderKeyInput {
            name: "active".to_string(),
            key_value: "sk-active".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 5,
            weight: 1,
        },
    )
    .expect("insert active key");
    let cooldown = db
        .add_provider_key(
            app_type,
            "provider-a",
            &ProviderKeyInput {
                name: "cooldown".to_string(),
                key_value: "sk-cooldown".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 20,
                weight: 1,
            },
        )
        .expect("insert cooldown key");
    let degraded = db
        .add_provider_key(
            app_type,
            "provider-a",
            &ProviderKeyInput {
                name: "degraded".to_string(),
                key_value: "sk-degraded".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 30,
                weight: 1,
            },
        )
        .expect("insert degraded key");
    db.add_provider_key(
        app_type,
        "provider-a",
        &ProviderKeyInput {
            name: "disabled".to_string(),
            key_value: "sk-disabled".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: false,
            priority: 0,
            weight: 1,
        },
    )
    .expect("insert disabled key");

    db.record_provider_key_failure(app_type, "provider-a", &cooldown.id, 60, 60)
        .expect("mark cooldown key");
    db.record_provider_key_failure(app_type, "provider-a", &degraded.id, 0, 0)
        .expect("mark degraded key");

    let summaries = db
        .get_provider_key_summaries(app_type)
        .expect("load key summaries");
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.app_type, app_type);
    assert_eq!(summary.provider_id, "provider-a");
    assert_eq!(summary.total, 4);
    assert_eq!(summary.available, 2);
    assert_eq!(summary.degraded, 1);
    assert_eq!(summary.cooldown, 1);
    assert_eq!(summary.disabled, 1);
    assert_eq!(summary.min_priority, Some(5));

    let serialized = serde_json::to_string(summary).expect("serialize summary");
    assert!(!serialized.contains("keyValue"));
    assert!(!serialized.contains("sk-"));
}

#[test]
fn config_key_binding_writes_provider_config_and_repairs_when_disabled() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let first = db
        .add_provider_key(
            app_type.as_str(),
            "provider-a",
            &ProviderKeyInput {
                name: "first".to_string(),
                key_value: "sk-first".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert first key");
    let second = db
        .add_provider_key(
            app_type.as_str(),
            "provider-a",
            &ProviderKeyInput {
                name: "second".to_string(),
                key_value: "sk-second".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 20,
                weight: 1,
            },
        )
        .expect("insert second key");

    let updated =
        ProviderService::set_config_key(&state, app_type.clone(), "provider-a", &first.id)
            .expect("set config key");
    assert_eq!(
        updated
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-first")
    );
    assert_eq!(
        updated
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(first.id.as_str())
    );
    assert_eq!(
        updated
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("manual")
    );

    db.update_provider_key(
        app_type.as_str(),
        "provider-a",
        &first.id,
        &ProviderKeyInput {
            name: "first".to_string(),
            key_value: "sk-first".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: false,
            priority: 10,
            weight: 1,
        },
    )
    .expect("disable first key");
    ProviderService::repair_config_key_binding(&state, app_type, "provider-a", &first.id)
        .expect("repair config key");

    let repaired = db
        .get_provider_by_id("provider-a", AppType::Claude.as_str())
        .expect("read repaired provider")
        .expect("provider exists");
    assert_eq!(
        repaired
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-second")
    );
    assert_eq!(
        repaired
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(second.id.as_str())
    );
    assert_eq!(
        repaired
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("auto")
    );
}

#[test]
fn add_key_auto_config_follows_highest_priority_available_key() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let lower_priority = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "lower priority".to_string(),
            key_value: "sk-lower".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 20,
            weight: 1,
        },
    )
    .expect("add lower priority key");
    let first_bound = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read provider after first key")
        .expect("provider exists");
    assert_eq!(
        first_bound
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-lower")
    );
    assert_eq!(
        first_bound
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(lower_priority.id.as_str())
    );
    assert_eq!(
        first_bound
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("auto")
    );

    let higher_priority = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "higher priority".to_string(),
            key_value: "sk-higher".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 5,
            weight: 1,
        },
    )
    .expect("add higher priority key");

    let stored = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read provider after higher priority key")
        .expect("provider exists");
    assert_eq!(
        stored
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-higher")
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(higher_priority.id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("auto")
    );

    db.update_provider_key(
        app_type.as_str(),
        "provider-a",
        &higher_priority.id,
        &ProviderKeyInput {
            name: "higher priority updated".to_string(),
            key_value: "sk-higher-updated".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 5,
            weight: 1,
        },
    )
    .expect("update higher priority key");
    ProviderService::repair_config_key_binding(
        &state,
        app_type.clone(),
        "provider-a",
        &higher_priority.id,
    )
    .expect("repair after selected key update");
    let stored = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read provider after selected key update")
        .expect("provider exists");
    assert_eq!(
        stored
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-higher-updated")
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(higher_priority.id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("auto")
    );
}

#[test]
fn manual_config_key_does_not_follow_higher_priority_key() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let manual_key = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "manual".to_string(),
            key_value: "sk-manual".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 20,
            weight: 1,
        },
    )
    .expect("add manual key");
    ProviderService::set_config_key(&state, app_type.clone(), "provider-a", &manual_key.id)
        .expect("set manual config key");

    let higher_priority = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "higher priority".to_string(),
            key_value: "sk-higher".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 5,
            weight: 1,
        },
    )
    .expect("add higher priority key");

    db.update_provider_key(
        app_type.as_str(),
        "provider-a",
        &higher_priority.id,
        &ProviderKeyInput {
            name: "higher priority updated".to_string(),
            key_value: "sk-higher-updated".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 1,
            weight: 1,
        },
    )
    .expect("update higher priority key");
    ProviderService::repair_config_key_binding(
        &state,
        app_type.clone(),
        "provider-a",
        &higher_priority.id,
    )
    .expect("repair after higher priority update");

    let stored = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read provider after higher priority key")
        .expect("provider exists");
    assert_eq!(
        stored
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-manual")
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(manual_key.id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("manual")
    );
}

#[test]
fn set_config_key_auto_switches_manual_binding_back_to_highest_priority_key() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let manual_key = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "manual".to_string(),
            key_value: "sk-manual".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 20,
            weight: 1,
        },
    )
    .expect("add manual key");
    ProviderService::set_config_key(&state, app_type.clone(), "provider-a", &manual_key.id)
        .expect("set manual config key");

    let auto_key = ProviderService::add_key(
        &state,
        app_type.clone(),
        "provider-a",
        &ProviderKeyInput {
            name: "auto".to_string(),
            key_value: "sk-auto".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 1,
            weight: 1,
        },
    )
    .expect("add auto key");

    let updated = ProviderService::set_config_key_auto(&state, app_type, "provider-a")
        .expect("switch config key back to auto mode");
    assert_eq!(
        updated
            .settings_config
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-auto")
    );
    assert_eq!(
        updated
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(auto_key.id.as_str())
    );
    assert_eq!(
        updated
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("auto")
    );
}

#[test]
fn config_key_binding_clears_direct_config_when_last_key_disabled() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let only_key = db
        .add_provider_key(
            app_type.as_str(),
            "provider-a",
            &ProviderKeyInput {
                name: "only".to_string(),
                key_value: "sk-only".to_string(),
                auth_field: Some("ANTHROPIC_API_KEY".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert only key");
    ProviderService::set_config_key(&state, app_type.clone(), "provider-a", &only_key.id)
        .expect("set config key");

    db.update_provider_key(
        app_type.as_str(),
        "provider-a",
        &only_key.id,
        &ProviderKeyInput {
            name: "only".to_string(),
            key_value: "sk-only".to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: false,
            priority: 10,
            weight: 1,
        },
    )
    .expect("disable only key");
    ProviderService::repair_config_key_binding(&state, app_type, "provider-a", &only_key.id)
        .expect("repair config key");

    let stored = db
        .get_provider_by_id("provider-a", AppType::Claude.as_str())
        .expect("read repaired provider")
        .expect("provider exists");
    assert!(stored
        .settings_config
        .pointer("/env/ANTHROPIC_API_KEY")
        .is_none());
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        None
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        None
    );
}

#[test]
fn config_key_binding_clears_direct_config_when_last_key_deleted() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_AUTH_TOKEN": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let only_key = db
        .add_provider_key(
            app_type.as_str(),
            "provider-a",
            &ProviderKeyInput {
                name: "only".to_string(),
                key_value: "sk-only".to_string(),
                auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert only key");
    ProviderService::set_config_key(&state, app_type.clone(), "provider-a", &only_key.id)
        .expect("set config key");

    db.delete_provider_key(app_type.as_str(), "provider-a", &only_key.id)
        .expect("delete only key");
    ProviderService::repair_config_key_binding(&state, app_type, "provider-a", &only_key.id)
        .expect("repair config key");

    let stored = db
        .get_provider_by_id("provider-a", AppType::Claude.as_str())
        .expect("read repaired provider")
        .expect("provider exists");
    assert!(stored
        .settings_config
        .pointer("/env/ANTHROPIC_AUTH_TOKEN")
        .is_none());
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        None
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        None
    );
}

#[test]
fn provider_update_imports_embedded_config_key_into_pool() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let updated = Provider {
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "sk-manual"}}),
        ..provider
    };
    ProviderService::update(&state, app_type.clone(), Some("provider-a"), updated)
        .expect("update provider");

    let keys = db
        .get_provider_keys(app_type.as_str(), "provider-a")
        .expect("read provider keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key_value, "sk-manual");
    assert_eq!(keys[0].auth_field.as_deref(), Some("ANTHROPIC_API_KEY"));

    let stored = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read stored provider")
        .expect("provider exists");
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(keys[0].id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("manual")
    );
}

#[test]
fn provider_update_reuses_existing_key_pool_value_as_config_key() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Claude;

    let provider = Provider {
        id: "provider-a".to_string(),
        name: "Provider A".to_string(),
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "legacy-key"}}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");
    let pooled = db
        .add_provider_key(
            app_type.as_str(),
            "provider-a",
            &ProviderKeyInput {
                name: "pooled".to_string(),
                key_value: "sk-pooled".to_string(),
                auth_field: None,
                enabled: false,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert pooled key");

    let updated = Provider {
        settings_config: json!({"env": {"ANTHROPIC_API_KEY": "sk-pooled"}}),
        ..provider
    };
    ProviderService::update(&state, app_type.clone(), Some("provider-a"), updated)
        .expect("update provider");

    let keys = db
        .get_provider_keys(app_type.as_str(), "provider-a")
        .expect("read provider keys");
    assert_eq!(keys.len(), 1, "matching key should not be duplicated");
    assert_eq!(keys[0].id, pooled.id);
    assert!(
        keys[0].enabled,
        "config key should be usable after direct edit"
    );
    assert_eq!(keys[0].auth_field.as_deref(), Some("ANTHROPIC_API_KEY"));

    let stored = db
        .get_provider_by_id("provider-a", app_type.as_str())
        .expect("read stored provider")
        .expect("provider exists");
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(pooled.id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("manual")
    );
}

#[test]
fn codex_update_imports_experimental_bearer_token_into_key_pool() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Codex;

    let config = r#"
model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "AnyRouter"
base_url = "https://anyrouter.top/v1"
wire_api = "responses"
experimental_bearer_token = "sk-codex-config"
"#;
    let provider = Provider {
        id: "codex-anyrouter".to_string(),
        name: "Codex AnyRouter".to_string(),
        settings_config: json!({"auth": {}, "config": config}),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    ProviderService::update(&state, app_type.clone(), Some("codex-anyrouter"), provider)
        .expect("update codex provider");

    let keys = db
        .get_provider_keys(app_type.as_str(), "codex-anyrouter")
        .expect("read provider keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key_value, "sk-codex-config");
    assert_eq!(keys[0].auth_field.as_deref(), Some("OPENAI_API_KEY"));

    let stored = db
        .get_provider_by_id("codex-anyrouter", app_type.as_str())
        .expect("read stored provider")
        .expect("provider exists");
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        Some(keys[0].id.as_str())
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        Some("manual")
    );
}

#[test]
fn codex_config_key_binding_syncs_and_clears_experimental_bearer_token() {
    let db = Arc::new(Database::memory().expect("create memory database"));
    let state = AppState::new(db.clone());
    let app_type = AppType::Codex;

    let config = r#"
model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "AnyRouter"
base_url = "https://anyrouter.top/v1"
wire_api = "responses"
experimental_bearer_token = "sk-old"
"#;
    let provider = Provider {
        id: "codex-anyrouter".to_string(),
        name: "Codex AnyRouter".to_string(),
        settings_config: json!({
            "auth": { "OPENAI_API_KEY": "sk-old" },
            "config": config,
        }),
        website_url: None,
        category: Some("third_party".to_string()),
        created_at: Some(1),
        sort_index: Some(1),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    };
    db.save_provider(app_type.as_str(), &provider)
        .expect("save provider fixture");

    let key = db
        .add_provider_key(
            app_type.as_str(),
            "codex-anyrouter",
            &ProviderKeyInput {
                name: "next".to_string(),
                key_value: "sk-next".to_string(),
                auth_field: Some("OPENAI_API_KEY".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
            },
        )
        .expect("insert codex key");

    let updated =
        ProviderService::set_config_key(&state, app_type.clone(), "codex-anyrouter", &key.id)
            .expect("set codex config key");
    assert_eq!(
        updated
            .settings_config
            .pointer("/auth/OPENAI_API_KEY")
            .and_then(|value| value.as_str()),
        Some("sk-next")
    );
    let updated_config = updated
        .settings_config
        .get("config")
        .and_then(|value| value.as_str())
        .expect("codex config text");
    assert!(updated_config.contains("sk-next"));
    assert!(!updated_config.contains("sk-old"));

    db.delete_provider_key(app_type.as_str(), "codex-anyrouter", &key.id)
        .expect("delete codex key");
    ProviderService::repair_config_key_binding(&state, app_type, "codex-anyrouter", &key.id)
        .expect("repair codex config key");

    let stored = db
        .get_provider_by_id("codex-anyrouter", AppType::Codex.as_str())
        .expect("read repaired codex provider")
        .expect("provider exists");
    assert!(stored
        .settings_config
        .pointer("/auth/OPENAI_API_KEY")
        .is_none());
    assert!(!stored
        .settings_config
        .get("config")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .contains("experimental_bearer_token"));
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_id.as_deref()),
        None
    );
    assert_eq!(
        stored
            .meta
            .as_ref()
            .and_then(|meta| meta.config_key_mode.as_deref()),
        None
    );
}

#[test]
fn session_affinity_upsert_get_and_delete() {
    let db = Database::memory().expect("create memory database");

    db.upsert_session_affinity("claude", "session-1", "provider-a", Some("key-a"))
        .expect("upsert affinity");
    let affinity = db
        .get_session_affinity("claude", "session-1")
        .expect("get affinity")
        .expect("affinity should exist");
    assert_eq!(affinity.provider_id, "provider-a");
    assert_eq!(affinity.key_id.as_deref(), Some("key-a"));

    db.upsert_session_affinity("claude", "session-1", "provider-b", None)
        .expect("update affinity");
    let affinity = db
        .get_session_affinity("claude", "session-1")
        .expect("get updated affinity")
        .expect("affinity should exist");
    assert_eq!(affinity.provider_id, "provider-b");
    assert_eq!(affinity.key_id, None);

    assert!(db
        .delete_session_affinity("claude", "session-1")
        .expect("delete affinity"));
    assert!(db
        .get_session_affinity("claude", "session-1")
        .expect("get deleted affinity")
        .is_none());
}

#[test]
fn session_affinity_delete_matching_channel_only() {
    let db = Database::memory().expect("create memory database");

    db.upsert_session_affinity("claude", "session-1", "provider-a", Some("key-a"))
        .expect("upsert affinity");
    db.upsert_session_affinity("claude", "session-2", "provider-a", Some("key-b"))
        .expect("upsert second affinity");

    assert!(!db
        .delete_session_affinity_if_matches("claude", "provider-a", Some("key-c"))
        .expect("delete non-matching affinity"));
    assert!(db
        .get_session_affinity("claude", "session-1")
        .expect("get non-deleted affinity")
        .is_some());

    assert!(db
        .delete_session_affinity_if_matches("claude", "provider-a", Some("key-a"))
        .expect("delete matching affinity"));
    assert!(db
        .get_session_affinity("claude", "session-1")
        .expect("get deleted affinity")
        .is_none());
    assert!(db
        .get_session_affinity("claude", "session-2")
        .expect("get unrelated affinity")
        .is_some());
}

#[test]
fn working_channel_affinity_upsert_get_and_delete_matching_channel() {
    let db = Database::memory().expect("create memory database");

    db.upsert_working_channel_affinity("claude", "provider-a", Some("key-a"))
        .expect("upsert working channel");
    let affinity = db
        .get_working_channel_affinity("claude")
        .expect("get working channel")
        .expect("working channel should exist");
    assert_eq!(affinity.provider_id, "provider-a");
    assert_eq!(affinity.key_id.as_deref(), Some("key-a"));

    assert!(!db
        .delete_working_channel_affinity_if_matches("claude", "provider-a", Some("key-b"))
        .expect("delete non-matching channel"));
    assert!(db
        .get_working_channel_affinity("claude")
        .expect("get working channel after non-match")
        .is_some());

    assert!(db
        .delete_working_channel_affinity_if_matches("claude", "provider-a", Some("key-a"))
        .expect("delete matching channel"));
    assert!(db
        .get_working_channel_affinity("claude")
        .expect("get deleted working channel")
        .is_none());
}
