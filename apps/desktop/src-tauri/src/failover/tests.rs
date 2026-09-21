use super::*;
use std::fs;
use std::sync::atomic::AtomicU64;

struct Fixture {
    dir: PathBuf,
    providers: Vec<SavedProvider>,
    original: String,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let tag = format!("failover-test-{}-{number}", std::process::id());
        let dir = std::env::temp_dir().join(&tag);
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut providers = Vec::new();
        for index in 0..3 {
            let provider = SavedProvider {
                id: format!("{tag}-{index}"),
                provider_name: format!("Provider {index}"),
                base_url: format!("https://{tag}-{index}.example.test/v1"),
                model: "same-model".into(),
                api_key: Some(format!("fixture-provider-secret-{index}")),
                toml_config: None,
                wire_api: "responses".into(),
                requires_openai_auth: false,
                model_mappings: vec![],
            };
            providers.push(crate::providers::save_provider_inner(provider).unwrap());
        }
        let primary = &providers[0];
        let original = format!("# preserve user config\nmodel_provider = \"custom\"\nmodel = \"same-model\"\nmodel_reasoning_effort = \"high\"\n\n[model_providers.custom]\nname = \"Provider 0\"\nbase_url = {:?}\nwire_api = \"responses\"\nrequires_openai_auth = false\nsupports_websockets = true\nexperimental_bearer_token = {:?}\nrequest_max_retries = 4\n\n[model_providers.custom.http_headers]\nx-fixture = \"primary-header\"\n\n[mcp_servers.fixture]\ncommand = \"fixture-tool\"\n", primary.base_url, primary.api_key.as_deref().unwrap());
        fs::write(crate::config_path(&dir), &original).unwrap();
        fs::write(
            dir.join("auth.json"),
            "{\"OPENAI_API_KEY\":\"untouched-official-auth-fixture\"}",
        )
        .unwrap();
        crate::providers::remember_active_provider_on_connection(
            &crate::app_db::open().unwrap(),
            &dir,
            &primary.id,
        )
        .unwrap();
        Self {
            dir,
            providers,
            original,
            port: free_port(),
        }
    }
    fn scope(&self) -> Option<String> {
        Some(self.dir.display().to_string())
    }
    fn settings(&self) -> FailoverSettings {
        FailoverSettings {
            router_enabled: true,
            takeover_enabled: true,
            auto_failover_enabled: true,
            listen_port: self.port,
            provider_ids: vec![self.providers[0].id.clone(), self.providers[1].id.clone()],
            ..Default::default()
        }
    }
    fn text(&self) -> String {
        fs::read_to_string(crate::config_path(&self.dir)).unwrap()
    }
    fn enable(&self) -> FailoverStatus {
        save_settings(self.scope(), self.settings()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        SHUTTING_DOWN.store(false, Ordering::Release);
        // If a test deliberately left invalid TOML, repair only its private fixture.
        if let Ok(record) = load_record(&self.dir) {
            if let Ok(mut runtimes) = lock_manager() {
                if let Some(runtime) = runtimes.remove(&self.dir) {
                    runtime.proxy.shutdown();
                }
            }
            let _ = record;
        }
        if let Ok(conn) = crate::app_db::open() {
            let _ = conn.execute(
                "DELETE FROM provider_failover WHERE codex_dir = ?1",
                [normalized_path_scope(&self.dir)],
            );
            let _ = conn.execute(
                "DELETE FROM active_provider_selections WHERE codex_dir = ?1",
                [normalized_path_scope(&self.dir)],
            );
            for provider in &self.providers {
                let _ = conn.execute("DELETE FROM providers WHERE id = ?1", [&provider.id]);
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn stop(fixture: &Fixture) {
    let mut settings = load_record(&fixture.dir).unwrap().settings;
    settings.router_enabled = false;
    save_settings(fixture.scope(), settings).unwrap();
}
fn parsed(fixture: &Fixture) -> DocumentMut {
    fixture.text().parse().unwrap()
}

#[test]
fn listener_takeover_and_auto_switches_are_independent() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let auth = fs::read(fixture.dir.join("auth.json")).unwrap();
    let mut settings = fixture.settings();
    settings.takeover_enabled = false;
    settings.auto_failover_enabled = false;
    let listening = save_settings(fixture.scope(), settings.clone()).unwrap();
    assert!(listening.running);
    assert!(!listening.takeover_active);
    assert!(!listening.auto_failover_active);
    assert_eq!(fixture.text(), fixture.original);
    settings.takeover_enabled = true;
    let single = save_settings(fixture.scope(), settings.clone()).unwrap();
    assert!(single.takeover_active);
    assert!(!single.auto_failover_active);
    assert_eq!(single.runtime.providers.len(), 1);
    assert_eq!(single.primary.as_ref().unwrap().id, fixture.providers[0].id);
    settings.auto_failover_enabled = true;
    let automatic = save_settings(fixture.scope(), settings).unwrap();
    assert!(automatic.auto_failover_active);
    assert_eq!(automatic.runtime.providers.len(), 2);
    assert_eq!(fs::read(fixture.dir.join("auth.json")).unwrap(), auth);
    stop(&fixture);
    let stopped = get_status(fixture.scope()).unwrap();
    assert!(!stopped.running && !stopped.takeover_active);
    assert!(stopped.settings.auto_failover_enabled);
    assert_eq!(stopped.settings.provider_ids.len(), 2);
    assert_eq!(
        parsed(&fixture)["mcp_servers"]["fixture"]["command"].as_str(),
        Some("fixture-tool")
    );
    assert_eq!(
        parsed(&fixture)["model_providers"]["custom"]["base_url"].as_str(),
        Some(fixture.providers[0].base_url.as_str())
    );
}

#[test]
fn enabling_auto_switches_to_p1_even_if_current_is_elsewhere() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let mut settings = fixture.settings();
    settings.provider_ids = vec![
        fixture.providers[2].id.clone(),
        fixture.providers[0].id.clone(),
    ];
    let status = save_settings(fixture.scope(), settings).unwrap();
    assert!(status.auto_failover_active);
    assert_eq!(status.primary.as_ref().unwrap().id, fixture.providers[2].id);
    assert_eq!(status.runtime.providers[0].id, fixture.providers[2].id);
    assert_eq!(status.settings.provider_ids[0], fixture.providers[2].id);
    stop(&fixture);
    assert_eq!(
        parsed(&fixture)["model_providers"]["custom"]["base_url"].as_str(),
        Some(fixture.providers[2].base_url.as_str())
    );
}

#[test]
fn a_single_provider_queue_and_an_empty_running_queue_are_valid() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let mut settings = fixture.settings();
    settings.provider_ids.clear();
    let single = save_settings(fixture.scope(), settings).unwrap();
    assert_eq!(
        single.settings.provider_ids,
        vec![fixture.providers[0].id.clone()]
    );
    assert!(single.auto_failover_active);
    let mut settings = single.settings;
    settings.provider_ids.clear();
    let empty = save_settings(fixture.scope(), settings).unwrap();
    assert!(empty.running && empty.auto_failover_active);
    assert!(empty.runtime.providers.is_empty());
}

#[test]
fn queue_accepts_different_models_and_can_be_prepared_while_stopped() {
    let _guard = crate::app_db::test_db_guard();
    let mut fixture = Fixture::new();
    fixture.providers[1].model = "another-model".into();
    crate::providers::save_provider_inner(fixture.providers[1].clone()).unwrap();
    let status = get_status(fixture.scope()).unwrap();
    assert!(
        status
            .providers
            .iter()
            .find(|p| p.id == fixture.providers[1].id)
            .unwrap()
            .eligible
    );
    let mut settings = fixture.settings();
    settings.router_enabled = false;
    settings.takeover_enabled = false;
    settings.auto_failover_enabled = false;
    let saved = save_settings(fixture.scope(), settings).unwrap();
    assert!(!saved.running);
    assert_eq!(fixture.text(), fixture.original);
    assert_eq!(saved.settings.provider_ids.len(), 2);
    fixture.enable();
}

#[test]
fn settings_and_scope_validation_have_no_live_side_effects() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let mut invalid = fixture.settings();
    invalid.listen_port = 1000;
    assert!(save_settings(fixture.scope(), invalid).is_err());
    let mut invalid = fixture.settings();
    invalid.provider_ids.push(invalid.provider_ids[0].clone());
    assert!(save_settings(fixture.scope(), invalid).is_err());
    let mut invalid = fixture.settings();
    invalid.provider_ids = vec!["official:openai-official".into()];
    assert!(save_settings(fixture.scope(), invalid).is_err());
    let mut invalid = fixture.settings();
    invalid.takeover_enabled = false;
    assert!(save_settings(fixture.scope(), invalid).is_err());
    assert_eq!(fixture.text(), fixture.original);
}

#[test]
fn failed_bind_and_missing_p1_leave_configuration_and_settings_unchanged() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let blocker =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, fixture.port)).unwrap();
    assert!(save_settings(fixture.scope(), fixture.settings()).is_err());
    assert_eq!(fixture.text(), fixture.original);
    assert!(!get_status(fixture.scope()).unwrap().running);
    drop(blocker);
    let mut settings = fixture.settings();
    settings.provider_ids = vec!["missing-p1".into()];
    assert!(save_settings(fixture.scope(), settings).is_err());
    assert_eq!(fixture.text(), fixture.original);
    assert!(!get_status(fixture.scope()).unwrap().settings.router_enabled);
}

#[test]
fn queue_and_tuning_updates_keep_the_endpoint_and_update_all_parameters() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let before = fixture.text();
    let mut settings = fixture.settings();
    settings.provider_ids.reverse();
    settings.tuning = RoutingTuning {
        max_retries: 8,
        streaming_first_byte_timeout: 10,
        streaming_idle_timeout: 0,
        non_streaming_timeout: 900,
        circuit_failure_threshold: 7,
        circuit_success_threshold: 4,
        circuit_timeout_seconds: 45,
        circuit_error_rate_threshold: 0.75,
        circuit_min_requests: 15,
    };
    let status = save_settings(fixture.scope(), settings.clone()).unwrap();
    assert_eq!(status.settings, settings);
    assert_eq!(fixture.text(), before);
    assert_eq!(status.runtime.providers[0].id, fixture.providers[1].id);
    let mut bad = status.settings;
    bad.listen_port = free_port();
    assert!(save_settings(fixture.scope(), bad).is_err());
    assert_eq!(fixture.text(), before);
}

#[test]
fn manual_switch_keeps_routing_and_auto_but_official_never_joins_api_queue() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    with_provider_change(fixture.scope(), || {
        crate::providers::activate_saved_provider_inner(
            fixture.scope(),
            fixture.providers[2].id.clone(),
        )
    })
    .unwrap();
    let switched = get_status(fixture.scope()).unwrap();
    assert!(switched.running && switched.auto_failover_active);
    assert_eq!(switched.primary.unwrap().id, fixture.providers[2].id);
    with_provider_change(fixture.scope(), || {
        crate::providers::official_profiles::switch_official_profile_inner(
            fixture.scope(),
            crate::providers::official_profiles::DEFAULT_OFFICIAL_PROFILE_ID.into(),
        )
    })
    .unwrap();
    let official = get_status(fixture.scope()).unwrap();
    assert!(official.running && official.takeover_active);
    assert!(!official.auto_failover_active);
    assert!(official.settings.auto_failover_enabled);
    assert!(official.primary.unwrap().official);
    assert!(official
        .runtime
        .providers
        .iter()
        .all(|p| p.id.starts_with("official:")));
    assert!(official.providers.iter().all(|p| !p.official));
}

#[test]
fn official_takeover_preserves_auth_and_can_restore_a_builtin_route() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let original = "model='official-model'\n[mcp_servers.test]\ncommand='tool'\n";
    fs::write(crate::config_path(&fixture.dir), original).unwrap();
    let auth = fs::read(fixture.dir.join("auth.json")).unwrap();
    let mut settings = fixture.settings();
    settings.auto_failover_enabled = false;
    settings.provider_ids.clear();
    let status = save_settings(fixture.scope(), settings).unwrap();
    assert!(status.primary.unwrap().official);
    assert!(status.takeover_active && !status.auto_failover_active);
    let doc = parsed(&fixture);
    assert_eq!(
        doc["model_providers"]["openai"]["requires_openai_auth"].as_bool(),
        Some(true)
    );
    assert!(doc["model_providers"]["openai"]
        .get("experimental_bearer_token")
        .is_none());
    assert!(doc["model_providers"]["openai"]["http_headers"]
        .get(ROUTE_TOKEN_HEADER)
        .is_some());
    assert_eq!(fs::read(fixture.dir.join("auth.json")).unwrap(), auth);
    stop(&fixture);
    assert!(parsed(&fixture).get("model_providers").is_none());
    assert_eq!(
        parsed(&fixture)["mcp_servers"]["test"]["command"].as_str(),
        Some("tool")
    );
    assert_eq!(fs::read(fixture.dir.join("auth.json")).unwrap(), auth);
}

#[test]
fn failed_manual_switch_reattaches_original_route() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let error = with_provider_change::<()>(fixture.scope(), || {
        Err(CodexxError::Config("expected switch failure".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("expected switch failure"));
    let status = get_status(fixture.scope()).unwrap();
    assert!(status.running && status.takeover_active);
    assert_eq!(status.primary.unwrap().id, fixture.providers[0].id);
}

#[test]
fn restore_keeps_external_edits_and_does_not_leave_local_headers() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let mut doc = parsed(&fixture);
    doc["model_reasoning_effort"] = value("max");
    doc["model_providers"]["custom"]["request_max_retries"] = value(9);
    doc["model_providers"]["custom"]["external_new_setting"] = value("preserved");
    fs::write(crate::config_path(&fixture.dir), doc.to_string()).unwrap();
    stop(&fixture);
    let restored = parsed(&fixture);
    assert_eq!(restored["model_reasoning_effort"].as_str(), Some("max"));
    assert_eq!(
        restored["model_providers"]["custom"]["request_max_retries"].as_integer(),
        Some(9)
    );
    assert_eq!(
        restored["model_providers"]["custom"]["external_new_setting"].as_str(),
        Some("preserved")
    );
    assert_eq!(
        restored["model_providers"]["custom"]["http_headers"]["x-fixture"].as_str(),
        Some("primary-header")
    );
    assert!(!fixture.text().contains(ROUTE_GENERATION_HEADER));
}

#[test]
fn external_provider_change_detaches_without_stopping_the_listener() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let external = fixture
        .original
        .replace(
            &fixture.providers[0].base_url,
            &fixture.providers[2].base_url,
        )
        .replace("fixture-provider-secret-0", "fixture-provider-secret-2");
    fs::write(crate::config_path(&fixture.dir), &external).unwrap();
    let status = get_status(fixture.scope()).unwrap();
    assert!(status.running && !status.takeover_active);
    assert!(!status.settings.takeover_enabled);
    assert_eq!(fixture.text(), external);
}

#[test]
fn failed_restore_keeps_the_listener_alive() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let managed = fixture.text();
    fs::write(crate::config_path(&fixture.dir), "invalid = [").unwrap();
    let mut off = fixture.settings();
    off.router_enabled = false;
    assert!(save_settings(fixture.scope(), off).is_err());
    assert!(lock_manager().unwrap().contains_key(&fixture.dir));
    fs::write(crate::config_path(&fixture.dir), managed).unwrap();
    stop(&fixture);
}

#[test]
fn persisted_directory_scopes_resolve_to_the_interactive_runtime_key() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let scope = normalized_path_scope(&fixture.dir);
    let stored = stored_directories().unwrap();
    let restored = stored
        .iter()
        .find(|dir| normalized_path_scope(dir) == scope)
        .unwrap();
    assert_eq!(*restored, directory(fixture.scope()).unwrap());
    assert!(lock_manager().unwrap().contains_key(restored));
}

#[test]
fn exit_restores_and_rejects_queued_enabling_then_restart_resumes_same_port() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    shutdown_all().unwrap();
    assert!(!fixture.text().contains("http://127.0.0.1:"));
    let record = load_record(&fixture.dir).unwrap();
    assert!(record.settings.router_enabled && record.settings.takeover_enabled);
    assert!(save_settings(fixture.scope(), fixture.settings()).is_err());
    SHUTTING_DOWN.store(false, Ordering::Release);
    initialize().unwrap();
    let status = get_status(fixture.scope()).unwrap();
    assert!(status.takeover_active);
    assert_eq!(status.settings.listen_port, fixture.port);
}

#[test]
fn old_managed_backups_keep_their_original_provider_after_later_takeovers() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let old = fixture.text();
    with_provider_change(fixture.scope(), || {
        crate::providers::activate_saved_provider_inner(
            fixture.scope(),
            fixture.providers[2].id.clone(),
        )
    })
    .unwrap();
    stop(&fixture);
    fs::write(crate::config_path(&fixture.dir), old).unwrap();
    recover_stale_route(fixture.scope()).unwrap();
    assert_eq!(
        parsed(&fixture)["model_providers"]["custom"]["base_url"].as_str(),
        Some(fixture.providers[0].base_url.as_str())
    );
}

#[test]
fn old_enabled_backups_settings_migrate_to_full_p1_queue() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let old = serde_json::json!({"settings":{"enabled":true,"providerIds":[fixture.providers[1].id]},"journals":[{"primaryId":fixture.providers[0].id,"providerKey":"custom","originalTable":"name='old'","port":fixture.port,"token":"old-only-test-token"}]});
    crate::app_db::open()
        .unwrap()
        .execute(
            "INSERT INTO provider_failover(codex_dir,record_json) VALUES(?1,?2)",
            params![normalized_path_scope(&fixture.dir), old.to_string()],
        )
        .unwrap();
    let migrated = load_record(&fixture.dir).unwrap();
    assert_eq!(migrated.version, 2);
    assert!(
        migrated.settings.router_enabled
            && migrated.settings.takeover_enabled
            && migrated.settings.auto_failover_enabled
    );
    assert_eq!(
        migrated.settings.provider_ids,
        vec![
            fixture.providers[0].id.clone(),
            fixture.providers[1].id.clone()
        ]
    );
    assert_eq!(migrated.settings.tuning, RoutingTuning::default());
}

#[test]
fn status_never_serializes_provider_credentials_or_local_tokens() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let status = fixture.enable();
    let text = serde_json::to_string(&status).unwrap();
    assert!(!text.contains("fixture-provider-secret"));
    let record = load_record(&fixture.dir).unwrap();
    assert!(!text.contains(&record.journals.last().unwrap().token));
}

#[test]
fn deleting_one_queue_member_keeps_the_rest_and_listener() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    crate::providers::delete_provider_inner(&fixture.providers[1].id).unwrap();
    refresh_saved_routes().unwrap();
    let status = get_status(fixture.scope()).unwrap();
    assert!(status.running && status.takeover_active);
    assert_eq!(status.runtime.providers.len(), 1);
    assert_eq!(status.runtime.providers[0].id, fixture.providers[0].id);
}

#[test]
fn listener_can_run_without_a_configured_provider() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fs::remove_file(crate::config_path(&fixture.dir)).unwrap();
    let status = save_settings(
        fixture.scope(),
        FailoverSettings {
            router_enabled: true,
            listen_port: fixture.port,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(status.running && !status.takeover_active);
    assert!(!crate::config_path(&fixture.dir).exists());
}

#[test]
fn successful_fallback_updates_logical_provider_and_preserves_old_backup_identity() {
    let _guard = crate::app_db::test_db_guard();
    let mut fixture = Fixture::new();
    fixture.providers[1].model = "different-default".into();
    crate::providers::save_provider_inner(fixture.providers[1].clone()).unwrap();
    fixture.enable();
    let earlier_backup = fixture.text();
    let mut runtimes = lock_manager().unwrap();
    let runtime = runtimes.get(&fixture.dir).unwrap();
    let notice = SelectionNotice {
        dir: fixture.dir.clone(),
        instance_id: runtime.instance_id,
        token: runtime.token.clone(),
        event: ProxySelectionEvent {
            provider_id: fixture.providers[1].id.clone(),
            revision: runtime.proxy.revision(),
        },
    };
    record_selection(notice, &mut runtimes).unwrap();
    drop(runtimes);
    let selected = get_status(fixture.scope()).unwrap();
    assert_eq!(selected.primary.unwrap().id, fixture.providers[1].id);
    assert_eq!(parsed(&fixture)["model"].as_str(), Some("same-model"));
    let direct = direct_document(&fixture.dir, &parsed(&fixture)).unwrap();
    assert_eq!(
        direct["model_providers"]["custom"]["name"].as_str(),
        Some("Provider 1")
    );
    assert_eq!(
        direct["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
        Some("fixture-provider-secret-1")
    );
    stop(&fixture);
    assert_eq!(
        parsed(&fixture)["model_providers"]["custom"]["base_url"].as_str(),
        Some(fixture.providers[1].base_url.as_str())
    );
    fs::write(crate::config_path(&fixture.dir), earlier_backup).unwrap();
    recover_stale_route(fixture.scope()).unwrap();
    assert_eq!(
        parsed(&fixture)["model_providers"]["custom"]["base_url"].as_str(),
        Some(fixture.providers[0].base_url.as_str())
    );
}

#[test]
fn late_success_cannot_override_a_manual_switch_or_a_changed_queue() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let notice = {
        let runtimes = lock_manager().unwrap();
        let runtime = runtimes.get(&fixture.dir).unwrap();
        SelectionNotice {
            dir: fixture.dir.clone(),
            instance_id: runtime.instance_id,
            token: runtime.token.clone(),
            event: ProxySelectionEvent {
                provider_id: fixture.providers[1].id.clone(),
                revision: runtime.proxy.revision(),
            },
        }
    };
    with_provider_change(fixture.scope(), || {
        crate::providers::activate_saved_provider_inner(
            fixture.scope(),
            fixture.providers[2].id.clone(),
        )
    })
    .unwrap();
    record_selection(notice, &mut lock_manager().unwrap()).unwrap();
    assert_eq!(
        get_status(fixture.scope()).unwrap().primary.unwrap().id,
        fixture.providers[2].id
    );
}

#[test]
fn a_late_success_from_a_stopped_listener_cannot_change_the_restarted_route() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let notice = {
        let runtimes = lock_manager().unwrap();
        let runtime = runtimes.get(&fixture.dir).unwrap();
        SelectionNotice {
            dir: fixture.dir.clone(),
            instance_id: runtime.instance_id,
            token: runtime.token.clone(),
            event: ProxySelectionEvent {
                provider_id: fixture.providers[1].id.clone(),
                revision: runtime.proxy.revision(),
            },
        }
    };
    stop(&fixture);
    fixture.enable();
    let mut runtimes = lock_manager().unwrap();
    let restarted = runtimes.get(&fixture.dir).unwrap();
    assert_eq!(notice.token, restarted.token);
    assert_eq!(notice.event.revision, restarted.proxy.revision());
    assert_ne!(notice.instance_id, restarted.instance_id);
    record_selection(notice, &mut runtimes).unwrap();
    drop(runtimes);
    assert_eq!(
        get_status(fixture.scope()).unwrap().primary.unwrap().id,
        fixture.providers[0].id
    );
}

#[test]
fn a_journal_write_failure_after_p1_rolls_back_files_and_common_recovery_marker() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    let before = FileCheckpoint::capture(&fixture.dir).unwrap();
    let target = &fixture.providers[2].id;
    crate::app_db::open().unwrap().execute_batch(&format!("CREATE TRIGGER reject_p1_journal BEFORE UPDATE ON provider_failover WHEN json_extract(NEW.record_json,'$.journals[#-1].primaryId') = '{target}' BEGIN SELECT RAISE(ABORT,'fixture-journal-failure'); END;")).unwrap();
    let mut settings = fixture.settings();
    settings.provider_ids = vec![target.clone()];
    let result = save_settings(fixture.scope(), settings);
    crate::app_db::open()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_p1_journal")
        .unwrap();
    assert!(result.is_err());
    let after = FileCheckpoint::capture(&fixture.dir).unwrap();
    assert_eq!(before.config, after.config);
    assert_eq!(before.auth, after.auth);
    assert_eq!(before.selected, after.selected);
    assert_eq!(before.common_handled, after.common_handled);
    assert!(!get_status(fixture.scope()).unwrap().running);
}

#[test]
fn p1_journal_failure_restores_official_endpoint_before_oauth() {
    let _guard = crate::app_db::test_db_guard();
    let mut fixture = Fixture::new();
    fs::write(crate::config_path(&fixture.dir), "model='official-model'\n").unwrap();
    fs::write(crate::auth_path(&fixture.dir),r#"{"auth_mode":"chatgpt","tokens":{"access_token":"fixture-oauth","account_id":"fixture-account"}}"#).unwrap();
    fixture.providers[2].requires_openai_auth = true;
    fixture.providers[2] =
        crate::providers::save_provider_inner(fixture.providers[2].clone()).unwrap();
    let before = FileCheckpoint::capture(&fixture.dir).unwrap();
    let mut settings = fixture.settings();
    settings.provider_ids = vec![fixture.providers[2].id.clone()];
    // Verify the prepared direct route really reads the global auth file. This
    // is the interval a failed journal write must undo before publishing OAuth.
    switch_to_p1(&fixture.dir, &mut settings).unwrap();
    let prepared = FileCheckpoint::capture(&fixture.dir).unwrap();
    let doc = parsed(&fixture);
    let key = doc["model_provider"].as_str().unwrap();
    assert_eq!(
        doc["model_providers"][key]["requires_openai_auth"].as_bool(),
        Some(true)
    );
    assert!(doc["model_providers"][key]
        .get("experimental_bearer_token")
        .is_none());
    assert_eq!(
        crate::providers::replacement_write_order(
            prepared.config.as_deref(),
            before.config.as_deref()
        ),
        crate::providers::LiveWriteOrder::ConfigFirst
    );
    before.restore(&fixture.dir, &prepared).unwrap();
    let target = &fixture.providers[2].id;
    crate::app_db::open().unwrap().execute_batch(&format!("CREATE TRIGGER reject_official_p1 BEFORE UPDATE ON provider_failover WHEN json_extract(NEW.record_json,'$.journals[#-1].primaryId') = '{target}' BEGIN SELECT RAISE(ABORT,'fixture-journal-failure'); END;")).unwrap();
    let result = save_settings(fixture.scope(), settings);
    crate::app_db::open()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_official_p1")
        .unwrap();
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("fixture-journal-failure"));
    let after = FileCheckpoint::capture(&fixture.dir).unwrap();
    assert_eq!(before.config, after.config);
    assert_eq!(before.auth, after.auth);
    assert_eq!(before.selected, after.selected);
    assert_eq!(before.common_handled, after.common_handled);
    assert!(!get_status(fixture.scope()).unwrap().running);
}

#[test]
fn invalid_persisted_settings_restore_direct_without_restarting_listener() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    let runtime = lock_manager().unwrap().remove(&fixture.dir).unwrap();
    runtime.proxy.shutdown();
    let mut record = load_record(&fixture.dir).unwrap();
    record.settings.tuning.max_retries = 11;
    save_record(&fixture.dir, &record).unwrap();
    initialize().unwrap();
    let status = get_status(fixture.scope()).unwrap();
    assert!(!status.running && !status.takeover_active);
    assert!(!fixture.text().contains("http://127.0.0.1:"));
    assert!(status.message.unwrap().contains("路由设置需要检查"));
}

#[test]
fn startup_status_write_failure_keeps_the_attached_listener_alive() {
    let _guard = crate::app_db::test_db_guard();
    let fixture = Fixture::new();
    fixture.enable();
    shutdown_all().unwrap();
    let conn = crate::app_db::open().unwrap();
    conn.execute_batch("CREATE TRIGGER reject_redundant_status BEFORE UPDATE ON provider_failover WHEN NEW.record_json=OLD.record_json BEGIN SELECT RAISE(ABORT,'fixture-status-failure'); END;").unwrap();
    SHUTTING_DOWN.store(false, Ordering::Release);
    let result = initialize();
    conn.execute_batch("DROP TRIGGER reject_redundant_status")
        .unwrap();
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("fixture-status-failure"));
    let status = get_status(fixture.scope()).unwrap();
    assert!(status.running && status.takeover_active);
    assert!(fixture.text().contains("http://127.0.0.1:"));
    assert!(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, fixture.port)).is_ok());
    stop(&fixture);
}
