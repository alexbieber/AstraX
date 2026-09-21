use super::*;

struct OwnedRouteFixture {
    dir: PathBuf,
    raw: String,
    local_token: String,
}

impl OwnedRouteFixture {
    fn new() -> Self {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap()
        );
        let dir = std::env::temp_dir().join(format!("codex-x-failover-integration-{unique}"));
        fs::create_dir_all(&dir).expect("create isolated Codex directory");
        let local_token = format!("local-token-{unique}");
        let original = r#"name = "Primary"
base_url = "https://primary.example.test/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "real-test-key"
supports_websockets = true
"#;
        let raw = format!(
            r#"model_provider = "custom"
model = "test-model"
model_reasoning_effort = "high"

[model_providers.custom]
name = "Primary"
base_url = "http://127.0.0.1:45555/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "{local_token}"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0

[mcp_servers.example]
command = "local-fixture-command"
"#
        );
        fs::write(config_path(&dir), &raw).expect("write synthetic local route");
        let record = json!({
            "settings": {"enabled":false,"providerIds":[]},
            "journals": [{"primaryId":"fixture-primary", "providerKey":"custom",
                "originalTable":original, "port":45555, "token":local_token}],
        });
        app_db::open()
            .expect("open isolated test store")
            .execute(
                "INSERT INTO provider_failover (codex_dir, record_json) VALUES (?1, ?2)",
                params![paths::normalized_path_scope(&dir), record.to_string()],
            )
            .expect("seed owned route journal");
        Self {
            dir,
            raw,
            local_token,
        }
    }
}

impl Drop for OwnedRouteFixture {
    fn drop(&mut self) {
        if let Ok(conn) = app_db::open() {
            let _ = conn.execute(
                "DELETE FROM provider_failover WHERE codex_dir = ?1",
                [paths::normalized_path_scope(&self.dir)],
            );
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn managed_route_state_detection_and_drafts_use_original_provider_without_writes() {
    let fixture = OwnedRouteFixture::new();
    let mut detected = providers::detected_live_custom_provider(&fixture.dir)
        .expect("detect logical provider")
        .expect("third-party provider");
    assert_eq!(detected.base_url, "https://primary.example.test/v1");
    assert_eq!(detected.api_key.as_deref(), Some("real-test-key"));
    assert!(!detected
        .toml_config
        .as_deref()
        .unwrap()
        .contains(&fixture.local_token));
    detected.toml_config = None;
    let draft = build_provider_toml_draft_inner(detected, Some(fixture.dir.display().to_string()))
        .expect("inherit direct route in editor draft");
    assert!(!draft.contains("127.0.0.1"));
    assert!(!draft.contains(&fixture.local_token));
    assert!(draft.contains("local-fixture-command"));
    let state =
        build_state_after_migration(fixture.dir.clone()).expect("read logical application state");
    let state_json = serde_json::to_value(&state).expect("serialize public state");
    assert_eq!(
        state_json["providers"][0]["baseUrl"],
        "https://primary.example.test/v1"
    );
    assert_eq!(state.model.as_deref(), Some("test-model"));
    assert_eq!(
        fs::read_to_string(config_path(&fixture.dir)).unwrap(),
        fixture.raw
    );
    assert!(!auth_path(&fixture.dir).exists());
}

#[test]
fn cached_managed_toml_submission_restores_owned_route_but_preserves_new_api_key() {
    let fixture = OwnedRouteFixture::new();
    for (input_key, expected_key) in [
        (
            Some(fixture.local_token.clone()),
            Some("real-test-key".to_string()),
        ),
        (
            Some("new-user-key".to_string()),
            Some("new-user-key".to_string()),
        ),
        (
            Some(format!("  {}  ", fixture.local_token)),
            Some("real-test-key".to_string()),
        ),
        (None, None),
    ] {
        let input = direct_provider_toml_input(ProviderTomlInput {
            config_dir: Some(fixture.dir.display().to_string()),
            config_text: fixture.raw.replace("\"high\"", "\"low\""),
            api_key: input_key,
        })
        .expect("unwrap stale editor payload");
        assert_eq!(input.api_key, expected_key);
        let doc = input
            .config_text
            .parse::<DocumentMut>()
            .expect("parse direct payload");
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("low"));
        assert_eq!(
            doc["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://primary.example.test/v1")
        );
        assert!(!input.config_text.contains(&fixture.local_token));
        assert!(input.config_text.contains("local-fixture-command"));
    }
    assert_eq!(
        fs::read_to_string(config_path(&fixture.dir)).unwrap(),
        fixture.raw
    );
    assert!(!auth_path(&fixture.dir).exists());
}

#[test]
fn read_unwrap_handles_multiple_owned_tables_before_detecting_selected_provider() {
    let mut fixture = OwnedRouteFixture::new();
    let second_original = "name = \"Inactive\"\nbase_url = \"https://secondary.example.test/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\nexperimental_bearer_token = \"secondary-test-key\"\n";
    fixture.raw.push_str("\n[model_providers.inactive]\nname = \"Inactive\"\nbase_url = \"http://127.0.0.1:45556/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\nexperimental_bearer_token = \"secondary-local-token\"\n");
    fs::write(config_path(&fixture.dir), &fixture.raw).expect("write second local table");
    let conn = app_db::open().expect("read fixture store");
    let scope = paths::normalized_path_scope(&fixture.dir);
    let text: String = conn
        .query_row(
            "SELECT record_json FROM provider_failover WHERE codex_dir = ?1",
            [&scope],
            |row| row.get(0),
        )
        .expect("read recovery journal");
    let mut record: Value = serde_json::from_str(&text).expect("parse recovery journal");
    record["journals"].as_array_mut().unwrap().push(json!({
        "primaryId":"inactive", "providerKey":"inactive", "originalTable":second_original,
        "port":45556, "token":"secondary-local-token"
    }));
    conn.execute(
        "UPDATE provider_failover SET record_json = ?1 WHERE codex_dir = ?2",
        params![record.to_string(), scope],
    )
    .expect("append recovery identity");
    drop(conn);
    let detected = providers::detected_live_custom_provider(&fixture.dir)
        .expect("detect selected provider")
        .expect("selected third-party provider");
    assert_eq!(detected.base_url, "https://primary.example.test/v1");
    assert_eq!(detected.api_key.as_deref(), Some("real-test-key"));
    assert!(!detected.toml_config.unwrap().contains("127.0.0.1"));
    assert_eq!(
        fs::read_to_string(config_path(&fixture.dir)).unwrap(),
        fixture.raw
    );
}

#[test]
fn native_owned_route_keeps_official_status_quota_refresh_and_snapshot_credentials_scoped() {
    let _guard = app_db::test_db_guard();
    let mut fixture = OwnedRouteFixture::new();
    let original = "name = 'OpenAI'\nwire_api = 'responses'\nrequires_openai_auth = true\nsupports_websockets = true\n";
    fixture.raw = format!("model_provider='custom'\nmodel='official-model'\n[model_providers.custom]\nname='OpenAI'\nwire_api='responses'\nrequires_openai_auth=true\nsupports_websockets=false\nbase_url='http://127.0.0.1:45555/v1'\nhttp_headers={{'x-codex-x-route-token'='{}'}}\n[mcp_servers.keep]\ncommand='keep-native-mcp'\n", fixture.local_token);
    fs::write(config_path(&fixture.dir), &fixture.raw).unwrap();
    let record = json!({
        "version":2,
        "settings":{"routerEnabled":false,"takeoverEnabled":false,"autoFailoverEnabled":false,"providerIds":[]},
        "journals":[{"primaryId":"official:openai-official","providerKey":"custom","originalTable":original,
            "listenAddress":"127.0.0.1","port":45555,"token":fixture.local_token,"official":true,"tableExisted":true,"providersExisted":true}]
    });
    app_db::open()
        .unwrap()
        .execute(
            "UPDATE provider_failover SET record_json=?1 WHERE codex_dir=?2",
            params![
                record.to_string(),
                paths::normalized_path_scope(&fixture.dir)
            ],
        )
        .unwrap();
    let auth = json!({"auth_mode":"chatgpt","tokens":{"access_token":"native-current-access","refresh_token":"native-refresh","account_id":"native-account"}});
    fs::write(auth_path(&fixture.dir), auth.to_string()).unwrap();
    let config_before = fs::read(config_path(&fixture.dir)).unwrap();
    let auth_before = fs::read(auth_path(&fixture.dir)).unwrap();
    let state = build_state_after_migration(fixture.dir.clone()).unwrap();
    assert!(state.is_official_provider);
    assert_eq!(
        state.active_official_profile_id.as_deref(),
        Some("openai-official")
    );
    assert!(providers::detected_live_custom_provider(&fixture.dir)
        .unwrap()
        .is_none());
    let profiles = list_official_profiles_inner(Some(fixture.dir.display().to_string())).unwrap();
    assert!(profiles[0].is_current);
    assert!(profiles[0].can_query_quota);
    let credentials = providers::official_profiles::official_profile_quota_credentials(
        &fixture.dir,
        "openai-official",
    )
    .unwrap();
    assert_eq!(credentials.access_token, "native-current-access");
    let route = failover::native_official::route_for_current(&fixture.dir)
        .unwrap()
        .unwrap();
    assert!(route.api_key.is_none());
    let spec = route.official.unwrap();
    assert!(failover::native_official::verify_request(
        &spec,
        "Bearer native-current-access",
        Some("native-account")
    )
    .is_ok());
    assert_eq!(fs::read(config_path(&fixture.dir)).unwrap(), config_before);
    assert_eq!(fs::read(auth_path(&fixture.dir)).unwrap(), auth_before);
    let refreshed = json!({"auth_mode":"chatgpt","tokens":{"access_token":"native-refreshed-access","refresh_token":"native-refreshed-refresh","account_id":"native-account"}});
    fs::write(auth_path(&fixture.dir), refreshed.to_string()).unwrap();
    assert_eq!(
        providers::official_profiles::official_profile_quota_credentials(
            &fixture.dir,
            "openai-official"
        )
        .unwrap()
        .access_token,
        "native-refreshed-access"
    );
    assert!(failover::native_official::verify_request(
        &spec,
        "Bearer native-current-access",
        Some("native-account")
    )
    .is_err());
    assert!(failover::native_official::verify_request(
        &spec,
        "Bearer native-refreshed-access",
        Some("native-account")
    )
    .is_ok());
    assert!(providers::capture_live_chatgpt_config(&fixture.dir).unwrap());
    let snapshot_path = providers::official_snapshot_path_for_test(&fixture.dir).unwrap();
    let snapshot: Value = serde_json::from_slice(&fs::read(&snapshot_path).unwrap()).unwrap();
    let saved_config = snapshot["config"].as_str().unwrap();
    assert!(!saved_config.contains("127.0.0.1"));
    assert!(!saved_config.contains("x-codex-x-route-token"));
    assert!(!saved_config.contains(&fixture.local_token));
    assert!(saved_config.contains("keep-native-mcp"));
    assert_eq!(
        snapshot["auth"]["tokens"]["access_token"],
        "native-refreshed-access"
    );
    assert_eq!(fs::read(config_path(&fixture.dir)).unwrap(), config_before);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(auth_path(&fixture.dir)).unwrap()).unwrap(),
        refreshed
    );
    fs::remove_file(snapshot_path).unwrap();
}
