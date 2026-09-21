//! Native Codex official routing. Codex remains the owner of its login and token
//! refresh; this layer only verifies the selected live login and forwards it to
//! its fixed official origin. No credential is loaded from an inactive profile.

use super::proxy::ProxyRoute;
use crate::error::{CodexxError, Result};
use crate::providers::document_is_official;
use crate::providers::official_profiles::{list_official_profiles_inner, selected_profile_id};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use toml_edit::DocumentMut;

const CHATGPT_BASE: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUTH_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfficialRouteSpec {
    pub(crate) codex_dir: PathBuf,
    pub(crate) profile_id: String,
}

// Intentionally not Debug/Serialize: these values must never appear in status,
// frontend events, or error logs.
pub(crate) struct OfficialRequestAuth {
    pub(crate) authorization: String,
    pub(crate) account_id: Option<String>,
    pub(crate) base_url: String,
}

struct LiveAuth {
    token: String,
    account_id: Option<String>,
    base_url: &'static str,
}

fn error(message: &str) -> CodexxError {
    CodexxError::Config(message.to_owned())
}

fn read_bounded(path: &Path, limit: usize, missing_allowed: bool) -> Result<Vec<u8>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if missing_allowed && err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new())
        }
        Err(_) => return Err(error("无法读取当前官方登录配置，请检查文件后重试")),
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= limit as u64)
    {
        return Err(error("当前官方登录配置无法读取，请检查文件大小和格式"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("读取当前官方登录配置失败，请重试"))?;
    if bytes.len() > limit {
        return Err(error("当前官方登录配置过大，请检查文件"));
    }
    Ok(bytes)
}

fn logical_document(dir: &Path) -> Result<(Vec<u8>, DocumentMut)> {
    let bytes = read_bounded(&crate::config_path(dir), MAX_CONFIG_BYTES, true)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| error("当前 Codex 配置格式不正确，请先修复配置"))?;
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|_| error("当前 Codex 配置格式不正确，请先修复配置"))?;
    let doc = super::direct_document(dir, &doc)
        .map_err(|_| error("无法确认当前官方路由，请检查本地路由状态"))?;
    Ok((bytes, doc))
}

fn checked_secret(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
        && value != "PROXY_MANAGED")
        .then(|| value.to_owned())
}

fn checked_account(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 512 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(error("当前官方账号信息无效，请重新登录 Codex"));
    }
    Ok(Some(value.to_owned()))
}

fn parse_live_auth(bytes: &[u8]) -> Result<LiveAuth> {
    let auth: Value = serde_json::from_slice(bytes)
        .map_err(|_| error("官方账号尚未登录或认证无效，请在 Codex 中重新登录"))?;
    if !auth.is_object()
        || ["base_url", "baseUrl", "api_base", "endpoint"]
            .iter()
            .any(|key| auth.get(key).is_some())
    {
        return Err(error("当前官方认证格式无效，请在 Codex 中重新登录"));
    }
    let mode = auth.get("auth_mode").and_then(Value::as_str);
    let api_key = checked_secret(auth.get("OPENAI_API_KEY"));
    let tokens = auth.get("tokens");
    let has_tokens = tokens.is_some_and(|value| {
        !value.is_null() && value.as_object().is_none_or(|values| !values.is_empty())
    });
    if let Some(token) = api_key {
        if mode.is_some_and(|value| !value.eq_ignore_ascii_case("apikey")) || has_tokens {
            return Err(error("官方登录与 API Key 认证混用，请在 Codex 中重新登录"));
        }
        return Ok(LiveAuth {
            token,
            account_id: None,
            base_url: OPENAI_API_BASE,
        });
    }
    if auth.get("OPENAI_API_KEY").is_some_and(|value| {
        !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty())
    }) {
        return Err(error("当前 OpenAI API Key 无效，请重新设置"));
    }
    if mode.is_some_and(|value| {
        !value.eq_ignore_ascii_case("chatgpt") && !value.eq_ignore_ascii_case("chatgptAuthTokens")
    }) {
        return Err(error(
            "此登录方式暂不支持本地路由，请使用 ChatGPT 登录或 OpenAI API Key",
        ));
    }
    let token = checked_secret(auth.pointer("/tokens/access_token"))
        .ok_or_else(|| error("官方账号尚未登录，请在 Codex 中完成 ChatGPT 登录"))?;
    let account_id = checked_account(auth.pointer("/tokens/account_id").and_then(Value::as_str))?;
    Ok(LiveAuth {
        token,
        account_id,
        base_url: CHATGPT_BASE,
    })
}

fn ensure_selected(spec: &OfficialRouteSpec) -> Result<(Vec<u8>, DocumentMut)> {
    let (bytes, doc) = logical_document(&spec.codex_dir)?;
    if !document_is_official(&doc)
        || selected_profile_id(&spec.codex_dir)
            .map_err(|_| error("无法确认当前官方账号，请刷新后重试"))?
            != spec.profile_id
    {
        return Err(error("当前官方账号已切换，请重启 Codex 或新建会话后重试"));
    }
    Ok((bytes, doc))
}

pub(crate) fn route_for_current(dir: &Path) -> Result<Option<ProxyRoute>> {
    let (_, doc) = logical_document(dir)?;
    if !document_is_official(&doc) {
        return Ok(None);
    }
    let profile_id = selected_profile_id(dir)?;
    let name = list_official_profiles_inner(Some(dir.display().to_string()))?
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .map(|profile| profile.provider_name)
        .unwrap_or_else(|| "OpenAI Official".to_owned());
    // A logged-out official route can still be taken over. Codex may complete
    // login or refresh after startup; every request re-reads and verifies it.
    let base_url = read_bounded(&crate::auth_path(dir), MAX_AUTH_BYTES, true)
        .ok()
        .and_then(|bytes| parse_live_auth(&bytes).ok())
        .map_or(CHATGPT_BASE, |auth| auth.base_url)
        .to_owned();
    let models = crate::string_value(&doc, "model")
        .into_iter()
        .collect::<HashSet<_>>();
    Ok(Some(ProxyRoute {
        id: format!("official:{profile_id}"),
        name,
        base_url,
        api_key: None,
        headers: Vec::new(),
        models,
        official: Some(OfficialRouteSpec {
            codex_dir: dir.to_path_buf(),
            profile_id,
        }),
    }))
}

pub(crate) fn verify_request(
    spec: &OfficialRouteSpec,
    authorization: &str,
    account_id: Option<&str>,
) -> Result<OfficialRequestAuth> {
    let (config_before, _) = ensure_selected(spec)?;
    if authorization.len() > MAX_TOKEN_BYTES + 32 || authorization.chars().any(char::is_control) {
        return Err(error("官方请求认证无效，请在 Codex 中重新登录"));
    }
    let parts: Vec<_> = authorization.split_whitespace().collect();
    if parts.len() != 2 || !parts[0].eq_ignore_ascii_case("Bearer") {
        return Err(error("缺少官方请求认证，请在 Codex 中完成登录"));
    }
    let auth_before = read_bounded(&crate::auth_path(&spec.codex_dir), MAX_AUTH_BYTES, true)?;
    let auth = parse_live_auth(&auth_before)?;
    if parts[1] != auth.token || checked_account(account_id)? != auth.account_id {
        return Err(error(
            "此会话没有加载当前官方账号，请重启 Codex 或新建会话后重试",
        ));
    }
    // Avoid trusting a token across a simultaneous account switch/logout/refresh.
    // This reads files only; acquiring the live mutation lock here would deadlock
    // callers constructing routes while holding their own configuration guard.
    let (config_after, _) = ensure_selected(spec)?;
    let auth_after = read_bounded(&crate::auth_path(&spec.codex_dir), MAX_AUTH_BYTES, true)?;
    if config_before != config_after || auth_before != auth_after {
        return Err(error("官方登录正在更新，请稍后重试"));
    }
    Ok(OfficialRequestAuth {
        authorization: format!("Bearer {}", auth.token),
        account_id: auth.account_id,
        base_url: auth.base_url.to_owned(),
    })
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(crate) fn native_models(spec: &OfficialRouteSpec) -> Result<Value> {
    let (_, doc) = ensure_selected(spec)?;
    let Some(pointer) = doc.get("model_catalog_json").and_then(|item| item.as_str()) else {
        return Ok(json!({"models":[]}));
    };
    let path = Path::new(pointer);
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Ok(json!({"models":[]}));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        spec.codex_dir.join(path)
    };
    let owned = spec.codex_dir.join(".codex-x").join("model-catalogs");
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(json!({"models":[]}));
    };
    if !name
        .strip_suffix(".json")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Ok(json!({"models":[]}));
    }
    for directory in [spec.codex_dir.join(".codex-x"), owned.clone()] {
        if !fs::symlink_metadata(directory)
            .is_ok_and(|metadata| !is_link(&metadata) && metadata.is_dir())
        {
            return Ok(json!({"models":[]}));
        }
    }
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| !is_link(&metadata) && metadata.is_file())
        || !path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(owned.canonicalize().ok())
            .is_some_and(|(parent, owned)| parent == owned)
    {
        return Ok(json!({"models":[]}));
    }
    let Ok(bytes) = read_bounded(&path, MAX_CATALOG_BYTES, false) else {
        return Ok(json!({"models":[]}));
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(json!({"models":[]}));
    };
    let mut digest = Sha256::new();
    digest.update(b"codex-x-provider-model-catalog-v2\0");
    digest.update(serde_json::to_vec(&value).map_err(|_| error("本地模型目录格式无效"))?);
    if value
        .pointer("/_codex_x_model_catalog/source")
        .and_then(Value::as_str)
        != Some("codex-x")
        || !value.get("models").is_some_and(Value::is_array)
        || format!("{:x}.json", digest.finalize()) != name
    {
        return Ok(json!({"models":[]}));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::official_profiles::{
        save_official_profile_inner, switch_official_profile_inner, OfficialProfileInput,
        DEFAULT_OFFICIAL_PROFILE_ID,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        dir: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static COUNT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "codex-x-native-official-{}-{}",
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(crate::config_path(&dir), "model_provider = 'custom'\nmodel='official-model'\n[model_providers.custom]\nname='OpenAI'\nrequires_openai_auth=true\nsupports_websockets=true\nwire_api='responses'\n[mcp_servers.keep]\ncommand='fixture-mcp'\n").unwrap();
            Self { dir }
        }
        fn oauth(&self, access: &str, account: &str) {
            fs::write(crate::auth_path(&self.dir), serde_json::to_vec(&json!({"auth_mode":"chatgpt","tokens":{"access_token":access,"refresh_token":"unused-refresh-fixture","account_id":account}})).unwrap()).unwrap();
        }
        fn spec(&self) -> OfficialRouteSpec {
            OfficialRouteSpec {
                codex_dir: self.dir.clone(),
                profile_id: DEFAULT_OFFICIAL_PROFILE_ID.into(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn native_official_checks_exact_live_token_even_for_team_members_sharing_workspace() {
        let fixture = Fixture::new();
        fixture.oauth("live-user-a-token", "shared-team-workspace");
        let auth_before = fs::read(crate::auth_path(&fixture.dir)).unwrap();
        let config_before = fs::read(crate::config_path(&fixture.dir)).unwrap();
        let spec = fixture.spec();
        let allowed = verify_request(
            &spec,
            "Bearer live-user-a-token",
            Some("shared-team-workspace"),
        )
        .unwrap();
        assert_eq!(allowed.base_url, CHATGPT_BASE);
        assert_eq!(allowed.account_id.as_deref(), Some("shared-team-workspace"));
        for (authorization, account) in [
            ("Bearer another-user-token", Some("shared-team-workspace")),
            ("Bearer live-user-a-token", Some("other-workspace")),
            ("Bearer live-user-a-token", None),
            ("Bearer PROXY_MANAGED", Some("shared-team-workspace")),
            ("Bearer\nlive-user-a-token", Some("shared-team-workspace")),
            ("", Some("shared-team-workspace")),
        ] {
            let failure = verify_request(&spec, authorization, account)
                .err()
                .expect("must reject stale or wrong account credentials")
                .to_string();
            assert!(!failure.contains("live-user-a-token"));
            assert!(!failure.contains("another-user-token"));
            assert!(!failure.contains("shared-team-workspace"));
        }
        assert_eq!(
            fs::read(crate::auth_path(&fixture.dir)).unwrap(),
            auth_before
        );
        assert_eq!(
            fs::read(crate::config_path(&fixture.dir)).unwrap(),
            config_before
        );
    }

    #[test]
    fn native_official_login_and_token_refresh_are_owned_by_codex_without_cached_credentials() {
        let fixture = Fixture::new();
        let route = route_for_current(&fixture.dir).unwrap().unwrap();
        assert!(route.api_key.is_none());
        assert!(route.headers.is_empty());
        let spec = route.official.unwrap();
        assert!(verify_request(&spec, "Bearer absent", None).is_err());
        fixture.oauth("first-token", "account");
        assert!(verify_request(&spec, "Bearer first-token", Some("account")).is_ok());
        fixture.oauth("refreshed-token", "account");
        assert!(verify_request(&spec, "Bearer first-token", Some("account")).is_err());
        assert!(verify_request(&spec, "Bearer refreshed-token", Some("account")).is_ok());
        let latest = fs::read(crate::auth_path(&fixture.dir)).unwrap();
        route_for_current(&fixture.dir).unwrap();
        assert_eq!(fs::read(crate::auth_path(&fixture.dir)).unwrap(), latest);
        fs::remove_file(crate::auth_path(&fixture.dir)).unwrap();
        assert!(verify_request(&spec, "Bearer refreshed-token", Some("account")).is_err());
        assert!(route_for_current(&fixture.dir).unwrap().is_some());
    }

    #[test]
    fn native_official_does_not_reuse_inactive_profile_or_third_party_oauth() {
        let _guard = crate::app_db::test_db_guard();
        let fixture = Fixture::new();
        fixture.oauth("account-a-token", "workspace");
        let scope = Some(fixture.dir.display().to_string());
        let other = save_official_profile_inner(OfficialProfileInput {
            config_dir: scope.clone(), id: None, provider_name: "Account B".into(),
            model: Some("official-model".into()), config_text: Some(fs::read_to_string(crate::config_path(&fixture.dir)).unwrap()),
            auth_json: Some(json!({"auth_mode":"chatgpt","tokens":{"access_token":"account-b-token","account_id":"workspace"}}).to_string()),
        }).unwrap();
        let spec = fixture.spec();
        assert!(verify_request(&spec, "Bearer account-b-token", Some("workspace")).is_err());
        switch_official_profile_inner(scope, other.profile.id.clone()).unwrap();
        assert!(verify_request(&spec, "Bearer account-b-token", Some("workspace")).is_err());
        let current = route_for_current(&fixture.dir)
            .unwrap()
            .unwrap()
            .official
            .unwrap();
        assert_eq!(current.profile_id, other.profile.id);
        assert!(verify_request(&current, "Bearer account-b-token", Some("workspace")).is_ok());
        fs::write(crate::config_path(&fixture.dir), "model_provider='custom'\nmodel='third'\n[model_providers.custom]\nname='Third'\nbase_url='https://third.example.test/v1'\nrequires_openai_auth=false\n").unwrap();
        assert!(route_for_current(&fixture.dir).unwrap().is_none());
        assert!(verify_request(&current, "Bearer account-b-token", Some("workspace")).is_err());
    }

    #[test]
    fn native_api_key_uses_only_openai_api_origin_and_never_accepts_oauth_or_mixed_auth() {
        let fixture = Fixture::new();
        fs::write(
            crate::auth_path(&fixture.dir),
            json!({"auth_mode":"apikey","OPENAI_API_KEY":"api-key-fixture","tokens":null})
                .to_string(),
        )
        .unwrap();
        let spec = fixture.spec();
        let verified = verify_request(&spec, "Bearer api-key-fixture", None).unwrap();
        assert_eq!(verified.base_url, OPENAI_API_BASE);
        assert!(verified.account_id.is_none());
        assert!(verify_request(&spec, "Bearer oauth-fixture", None).is_err());
        assert!(
            verify_request(&spec, "Bearer api-key-fixture", Some("old-oauth-account")).is_err()
        );
        fs::write(
            crate::auth_path(&fixture.dir),
            json!({"OPENAI_API_KEY":"api-key-fixture","tokens":{"access_token":"oauth-fixture"}})
                .to_string(),
        )
        .unwrap();
        assert!(verify_request(&spec, "Bearer api-key-fixture", None).is_err());
        assert!(verify_request(&spec, "Bearer oauth-fixture", None).is_err());
        fs::write(crate::auth_path(&fixture.dir), "{ broken-secret-json").unwrap();
        let failure = verify_request(&spec, "Bearer api-key-fixture", None)
            .err()
            .unwrap()
            .to_string();
        assert!(!failure.contains("broken-secret-json"));
        assert!(!failure.contains("api-key-fixture"));
    }

    #[test]
    fn native_models_serve_only_verified_owned_catalog_and_never_read_arbitrary_json() {
        let fixture = Fixture::new();
        let spec = fixture.spec();
        assert_eq!(native_models(&spec).unwrap(), json!({"models":[]}));
        let config_path = crate::config_path(&fixture.dir);
        let mut doc = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        crate::providers::model_catalog::prepare_model_catalog(
            &fixture.dir,
            "native-test",
            &[crate::providers::model_catalog::ProviderModelMapping {
                model: "local-model".into(),
                display_name: "Local".into(),
                context_window: None,
            }],
            "local-model",
            &mut doc,
        )
        .unwrap();
        fs::write(&config_path, doc.to_string()).unwrap();
        let owned = PathBuf::from(doc["model_catalog_json"].as_str().unwrap());
        let before = fs::read(&config_path).unwrap();
        assert_eq!(
            native_models(&spec).unwrap()["models"][0]["slug"],
            "local-model"
        );
        assert_eq!(fs::read(&config_path).unwrap(), before);
        let outside = fixture.dir.join("unrelated-auth.json");
        fs::write(&outside, r#"{"models":["must-not-leak"]}"#).unwrap();
        doc["model_catalog_json"] = toml_edit::value(outside.display().to_string());
        fs::write(&config_path, doc.to_string()).unwrap();
        assert_eq!(native_models(&spec).unwrap(), json!({"models":[]}));
        #[cfg(unix)]
        {
            fs::remove_file(&owned).unwrap();
            std::os::unix::fs::symlink(&outside, &owned).unwrap();
            doc["model_catalog_json"] = toml_edit::value(owned.display().to_string());
            fs::write(&config_path, doc.to_string()).unwrap();
            assert_eq!(native_models(&spec).unwrap(), json!({"models":[]}));
        }
    }
    #[test]
    fn native_local_models_endpoint_accepts_owned_live_oauth_without_network_refresh_or_fallback() {
        use super::super::config::{RoutingTuning, ROUTE_TOKEN_HEADER};
        use super::super::proxy::{ProxyHandle, ProxyOptions};
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::Duration;
        struct Server(ProxyHandle);
        impl Drop for Server {
            fn drop(&mut self) {
                self.0.shutdown();
            }
        }
        let fixture = Fixture::new();
        fixture.oauth("local-native-access", "local-native-account");
        let route = route_for_current(&fixture.dir).unwrap().unwrap();
        let local_token = "native-local-route-token-fixture-000000000000";
        let server = Server(
            ProxyHandle::start(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
                local_token.into(),
                vec![route],
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning::default(),
                },
            )
            .unwrap(),
        );
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        for access in ["local-native-access", "local-native-refreshed"] {
            fixture.oauth(access, "local-native-account");
            let auth_before = fs::read(crate::auth_path(&fixture.dir)).unwrap();
            let response = client
                .get(format!("http://127.0.0.1:{}/v1/models", server.0.port()))
                .header(ROUTE_TOKEN_HEADER, local_token)
                .header("authorization", format!("Bearer {access}"))
                .header("chatgpt-account-id", "local-native-account")
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let body: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
            assert_eq!(body, json!({"models":[]}));
            assert_eq!(
                fs::read(crate::auth_path(&fixture.dir)).unwrap(),
                auth_before
            );
        }
        let snapshot = server.0.snapshot();
        assert_eq!(snapshot.success_count, 2);
        assert_eq!(snapshot.failover_count, 0);
        assert_eq!(
            snapshot.last_provider_id.as_deref(),
            Some("official:openai-official")
        );
    }
}
