use super::official_profiles::{
    has_named_profiles, selected_profile_id, DEFAULT_OFFICIAL_PROFILE_ID,
};
use crate::backups::{action_backup_root, BackupMeta};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, parse_toml_document, write_private_json};
use crate::paths::app_home;
use crate::toml_utils::ensure_table;
use crate::{auth_path, config_path, string_value};
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::value;

const SNAPSHOT_VERSION: u32 = 3;
const MODEL_ONLY_SNAPSHOT_VERSION: u32 = 2;
const LEGACY_SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub(crate) struct OfficialConfigCandidate {
    pub(crate) auth: Option<Value>,
    pub(crate) config_text: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) source: String,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialConfigDraft {
    pub(crate) auth_json: String,
    pub(crate) config_text: String,
    pub(crate) model: Option<String>,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfficialConfigSnapshot {
    version: u32,
    codex_dir: String,
    captured_at: String,
    model: Option<String>,
    #[serde(default)]
    config: Option<String>,
    #[serde(default)]
    auth: Option<Value>,
}

enum SnapshotState {
    Missing,
    Reset(OfficialConfigCandidate),
    Ready(OfficialConfigCandidate),
}

fn canonical_identity(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

pub(crate) fn official_snapshot_path(codex_dir: &Path) -> Result<PathBuf> {
    let profile_id = selected_profile_id(codex_dir)?;
    official_snapshot_path_for_profile(codex_dir, &profile_id)
}

pub(crate) fn official_snapshot_path_for_profile(
    codex_dir: &Path,
    profile_id: &str,
) -> Result<PathBuf> {
    if profile_id.trim().is_empty() {
        return Err(CodexxError::Config("官方配置 ID 不能为空".to_string()));
    }
    let identity = canonical_identity(codex_dir);
    let digest = Sha256::digest(identity.as_bytes());
    let root = app_home()?.join("official-configs");
    if profile_id == DEFAULT_OFFICIAL_PROFILE_ID {
        // The built-in profile keeps the legacy recovery path so existing
        // installations can restore their saved official login unchanged.
        return Ok(root.join(format!("{digest:x}.json")));
    }
    let profile_digest = Sha256::digest(profile_id.as_bytes());
    Ok(root
        .join(format!("{digest:x}"))
        .join(format!("{profile_digest:x}.json")))
}

fn value_has_material(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => values.iter().any(value_has_material),
        Value::Object(values) => values.values().any(value_has_material),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

pub(crate) fn auth_value_has_material(value: &Value) -> bool {
    value.as_object().is_some_and(|auth| {
        auth.iter()
            .filter(|(key, _)| key.as_str() != "auth_mode")
            .any(|(_, value)| value_has_material(value))
    })
}

pub(crate) fn is_chatgpt_auth(value: &Value) -> bool {
    let mode = value.get("auth_mode").and_then(Value::as_str);
    let has_api_key = value
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|key| !key.trim().is_empty());
    let has_bedrock_key = value.get("bedrock_api_key").is_some_and(value_has_material);
    if has_api_key || has_bedrock_key {
        return false;
    }

    let has_tokens = value
        .get("tokens")
        .and_then(Value::as_object)
        .is_some_and(|tokens| {
            ["access_token", "refresh_token", "id_token"]
                .iter()
                .any(|key| {
                    tokens
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.trim().is_empty())
                })
        });
    let has_agent_identity = value.get("agent_identity").is_some_and(|identity| {
        identity.as_str().is_some_and(|jwt| !jwt.trim().is_empty())
            || identity.as_object().is_some_and(|record| {
                ["agent_runtime_id", "agent_private_key"].iter().all(|key| {
                    record
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                })
            })
    });
    let has_personal_access_token = value
        .get("personal_access_token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.trim().is_empty());

    match mode {
        // This matches Codex AuthDotJson::resolved_mode: legacy token files
        // default to ChatGPT, while PAT infers its own mode. Agent Identity
        // requires an explicit auth_mode.
        None => has_tokens || has_personal_access_token,
        Some(mode)
            if mode.eq_ignore_ascii_case("chatgpt")
                || mode.eq_ignore_ascii_case("chatgptAuthTokens") =>
        {
            has_tokens
        }
        Some(mode) if mode.eq_ignore_ascii_case("agentIdentity") => has_agent_identity,
        Some(mode) if mode.eq_ignore_ascii_case("personalAccessToken") => has_personal_access_token,
        Some(_) => false,
    }
}

fn has_openai_api_key(value: &Value) -> bool {
    value
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|key| !key.trim().is_empty())
}

fn read_auth_value(path: &Path) -> Result<Option<Value>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|error| io_err(path, error))?;
    // A malformed live auth file is not trusted authentication. Treat it as
    // absent so state reads and provider switches can continue and repair it.
    // Snapshot parsing remains strict in load_snapshot.
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if !value.is_object() || !auth_value_has_material(&value) {
        return Ok(None);
    }
    Ok(Some(value))
}

fn model_from_config(path: &Path, text: &str) -> Result<Option<String>> {
    let doc = parse_toml_document(path, text)?;
    Ok(string_value(&doc, "model"))
}

fn live_official_config_text(codex_dir: &Path) -> Result<Option<String>> {
    let path = config_path(codex_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|error| io_err(&path, error))?;
    let doc = parse_toml_document(&path, &text)?;
    let direct = crate::failover::direct_document(codex_dir, &doc)?;
    let text = if direct.to_string() == doc.to_string() {
        text
    } else {
        direct.to_string()
    };
    Ok(document_is_official(&direct).then_some(text))
}

fn remove_bearer_tokens(doc: &mut toml_edit::DocumentMut) {
    doc.as_table_mut().remove("experimental_bearer_token");
    if let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    {
        for (_, item) in providers.iter_mut() {
            if let Some(table) = item.as_table_mut() {
                table.remove("experimental_bearer_token");
            }
        }
    }
}

pub(crate) fn build_official_config_text(
    codex_dir: &Path,
    model: Option<&str>,
    clear_model_if_none: bool,
) -> Result<String> {
    let path = config_path(codex_dir);
    let text = if path.is_file() {
        fs::read_to_string(&path).map_err(|error| io_err(&path, error))?
    } else {
        String::new()
    };
    let doc = parse_toml_document(&path, &text)?;
    let mut doc = crate::failover::direct_document(codex_dir, &doc)?;

    doc["model_provider"] = value("custom");
    doc.as_table_mut().remove("base_url");
    remove_bearer_tokens(&mut doc);
    let providers = ensure_table(doc.as_table_mut(), "model_providers")?;
    providers.remove("custom");
    let official = ensure_table(providers, "custom")?;
    official["name"] = value("OpenAI");
    official["requires_openai_auth"] = value(true);
    official["supports_websockets"] = value(true);
    official["wire_api"] = value("responses");

    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        doc["model"] = value(model);
    } else if clear_model_if_none {
        doc.as_table_mut().remove("model");
    }

    super::model_catalog::prepare_model_catalog(
        codex_dir,
        "official",
        &[],
        model.unwrap_or_default(),
        &mut doc,
    )?;

    Ok(doc.to_string())
}

pub(crate) fn validate_official_config_text(
    codex_dir: &Path,
    config_text: &str,
    model: Option<&str>,
) -> Result<(String, Option<String>)> {
    let path = config_path(codex_dir);
    let mut doc = parse_toml_document(&path, config_text)?;
    let has_legacy_proxy_endpoint = doc
        .get("base_url")
        .and_then(|item| item.as_str())
        .is_some_and(|value| !value.trim().is_empty());
    if !document_is_official(&doc) || has_legacy_proxy_endpoint {
        return Err(CodexxError::Config(
            "官方 config.toml 必须使用 OpenAI 官方路由，不能包含第三方 base_url".to_string(),
        ));
    }
    remove_bearer_tokens(&mut doc);
    // A supplied complete TOML document is authoritative. The separate model
    // field only fills a missing value for compatibility with older clients.
    if string_value(&doc, "model").is_none() {
        if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
            doc["model"] = value(model);
        }
    }
    super::model_catalog::prepare_model_catalog(
        codex_dir,
        "official",
        &[],
        model.unwrap_or_default(),
        &mut doc,
    )?;
    let text = doc.to_string();
    let model = string_value(&doc, "model");
    Ok((text, model))
}

pub(crate) fn live_config_is_official(codex_dir: &Path) -> Result<bool> {
    let path = config_path(codex_dir);
    if !path.is_file() {
        return Ok(true);
    }
    let text = fs::read_to_string(&path).map_err(|error| io_err(&path, error))?;
    let doc = parse_toml_document(&path, &text)?;
    let doc = crate::failover::direct_document(codex_dir, &doc)?;
    Ok(document_is_official(&doc))
}

pub(crate) fn document_is_official(doc: &toml_edit::DocumentMut) -> bool {
    let Some(provider) = string_value(doc, "model_provider") else {
        return true;
    };
    if provider.eq_ignore_ascii_case("openai") {
        return true;
    }
    if provider != "custom" {
        return false;
    }
    doc.get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get("custom"))
        .and_then(|item| item.as_table())
        .is_some_and(|table| {
            let has_no_endpoint = table
                .get("base_url")
                .and_then(|item| item.as_str())
                .is_none_or(|value| value.trim().is_empty());
            let is_openai = table
                .get("name")
                .and_then(|item| item.as_str())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("openai"));
            has_no_endpoint
                && is_openai
                && table
                    .get("requires_openai_auth")
                    .and_then(|item| item.as_bool())
                    == Some(true)
        })
}

fn write_snapshot(
    codex_dir: &Path,
    config: Option<String>,
    model: Option<String>,
    auth: Option<Value>,
) -> Result<()> {
    let profile_id = selected_profile_id(codex_dir)?;
    write_official_profile_snapshot(codex_dir, &profile_id, config, model, auth)
}

pub(crate) fn write_official_profile_snapshot(
    codex_dir: &Path,
    profile_id: &str,
    config: Option<String>,
    model: Option<String>,
    auth: Option<Value>,
) -> Result<()> {
    if auth
        .as_ref()
        .is_some_and(|auth| !auth.is_object() || !auth_value_has_material(auth))
    {
        return Err(CodexxError::Config(
            "官方 auth.json 没有可用认证信息，请先完成官方登录".to_string(),
        ));
    }
    let path = official_snapshot_path_for_profile(codex_dir, profile_id)?;
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    let snapshot = OfficialConfigSnapshot {
        version: SNAPSHOT_VERSION,
        codex_dir: canonical_identity(codex_dir),
        captured_at: Local::now().to_rfc3339(),
        model,
        config,
        auth,
    };
    let value = serde_json::to_value(snapshot)
        .map_err(|error| CodexxError::Config(format!("序列化官方配置快照失败: {error}")))?;
    write_private_json(&path, &value)
}

pub(crate) fn save_official_config_snapshot(
    codex_dir: &Path,
    config: Option<String>,
    model: Option<String>,
    auth: &Value,
) -> Result<()> {
    write_snapshot(codex_dir, config, model, Some(auth.clone()))
}

#[cfg(test)]
pub(crate) fn mark_official_config_reset(
    codex_dir: &Path,
    config: Option<String>,
    model: Option<String>,
) -> Result<()> {
    write_snapshot(codex_dir, config, model, None)
}

pub(crate) fn capture_live_official_config_before_provider_switch(
    codex_dir: &Path,
) -> Result<bool> {
    if !live_config_is_official(codex_dir)? {
        return Ok(false);
    }

    let live_config = live_official_config_text(codex_dir)?;
    let live_model = live_config
        .as_deref()
        .map(|text| model_from_config(&config_path(codex_dir), text))
        .transpose()?
        .flatten();
    let has_explicit_official_route = live_config
        .as_deref()
        .map(|text| {
            let doc = parse_toml_document(&config_path(codex_dir), text)?;
            Ok::<_, CodexxError>(string_value(&doc, "model_provider").is_some())
        })
        .transpose()?
        .unwrap_or(false);
    let live_auth = read_auth_value(&auth_path(codex_dir))?.filter(|auth| {
        is_chatgpt_auth(auth) || (has_explicit_official_route && has_openai_api_key(auth))
    });

    let profile_id = selected_profile_id(codex_dir)?;
    let previous = match load_snapshot_for_profile(codex_dir, &profile_id)? {
        SnapshotState::Ready(candidate) | SnapshotState::Reset(candidate) => Some(candidate),
        SnapshotState::Missing => None,
    };
    let config = live_config.or_else(|| {
        previous
            .as_ref()
            .and_then(|candidate| candidate.config_text.clone())
    });
    let model = live_model.or_else(|| {
        previous
            .as_ref()
            .and_then(|candidate| candidate.model.clone())
    });
    let previous_auth = previous
        .as_ref()
        .and_then(|candidate| candidate.auth.clone());
    let auth = if profile_id == DEFAULT_OFFICIAL_PROFILE_ID {
        prefer_chatgpt_auth(live_auth, previous_auth)
    } else {
        // A named profile is the user's selected working configuration. Its
        // current official login belongs to that profile, including an
        // intentional change between OAuth and API-key authentication.
        live_auth.or(previous_auth)
    };
    if config.is_none() && auth.is_none() {
        return Ok(false);
    }
    write_official_profile_snapshot(codex_dir, &profile_id, config, model, auth)?;
    Ok(true)
}

#[cfg(test)]
fn capture_live_official_auth(
    codex_dir: &Path,
    is_trusted: impl FnOnce(&Value) -> bool,
) -> Result<bool> {
    if !live_config_is_official(codex_dir)? {
        return Ok(false);
    }
    let Some(auth) = read_auth_value(&auth_path(codex_dir))? else {
        return Ok(false);
    };
    if !is_trusted(&auth) {
        return Ok(false);
    }
    let config = live_official_config_text(codex_dir)?;
    let model = config
        .as_deref()
        .map(|text| model_from_config(&config_path(codex_dir), text))
        .transpose()?
        .flatten();
    save_official_config_snapshot(codex_dir, config, model, &auth)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn capture_live_chatgpt_config(codex_dir: &Path) -> Result<bool> {
    capture_live_official_auth(codex_dir, is_chatgpt_auth)
}

fn load_snapshot_for_profile(codex_dir: &Path, profile_id: &str) -> Result<SnapshotState> {
    let path = official_snapshot_path_for_profile(codex_dir, profile_id)?;
    if !path.is_file() {
        return Ok(SnapshotState::Missing);
    }
    let text = fs::read_to_string(&path).map_err(|error| io_err(&path, error))?;
    let snapshot: OfficialConfigSnapshot = match serde_json::from_str(&text) {
        Ok(snapshot) => snapshot,
        Err(_) => return Ok(SnapshotState::Missing),
    };
    // This is an app-owned recovery cache, not the live source of truth. A
    // truncated, stale, or incompatible cache must never block startup or a
    // provider switch; valid live auth/history can repair it on the next write.
    Ok(snapshot_state(codex_dir, &path, snapshot).unwrap_or(SnapshotState::Missing))
}

pub(crate) fn saved_official_profile_candidate(
    codex_dir: &Path,
    profile_id: &str,
) -> Result<Option<OfficialConfigCandidate>> {
    // Explicit account selection must never consult the live login or an
    // unrelated historical backup, even when the requested snapshot is empty.
    match load_snapshot_for_profile(codex_dir, profile_id)? {
        SnapshotState::Ready(candidate) | SnapshotState::Reset(candidate) => Ok(Some(candidate)),
        SnapshotState::Missing => Ok(None),
    }
}

fn prefer_chatgpt_auth(primary: Option<Value>, fallback: Option<Value>) -> Option<Value> {
    if primary.as_ref().is_some_and(is_chatgpt_auth) {
        return primary;
    }
    if fallback.as_ref().is_some_and(is_chatgpt_auth) {
        return fallback;
    }
    primary.or(fallback)
}

fn snapshot_state(
    codex_dir: &Path,
    path: &Path,
    snapshot: OfficialConfigSnapshot,
) -> Result<SnapshotState> {
    if !matches!(
        snapshot.version,
        SNAPSHOT_VERSION | MODEL_ONLY_SNAPSHOT_VERSION | LEGACY_SNAPSHOT_VERSION
    ) || snapshot.codex_dir != canonical_identity(codex_dir)
    {
        return Err(CodexxError::Config(format!(
            "官方配置快照与当前 CODEX_HOME 不匹配: {}",
            path.display()
        )));
    }
    let source = "Astra 官方配置快照".to_string();
    let Some(auth) = snapshot.auth else {
        return Ok(SnapshotState::Reset(OfficialConfigCandidate {
            auth: None,
            config_text: snapshot.config,
            model: snapshot.model,
            source,
        }));
    };
    if !auth.is_object() || !auth_value_has_material(&auth) {
        return Err(CodexxError::Config(format!(
            "官方配置快照不包含可用认证: {}",
            path.display()
        )));
    }
    // Version 1 could be populated automatically from a proxy API key. Its
    // API-key-only snapshots are ambiguous, so never restore or promote them.
    if snapshot.version == LEGACY_SNAPSHOT_VERSION && !is_chatgpt_auth(&auth) {
        return Ok(SnapshotState::Missing);
    }
    Ok(SnapshotState::Ready(OfficialConfigCandidate {
        auth: Some(auth),
        config_text: snapshot.config,
        model: snapshot.model,
        source,
    }))
}

fn backup_config_is_official(dir: &Path, meta: &BackupMeta) -> bool {
    if !meta.had_config {
        return true;
    }
    let path = dir.join("config.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(doc) = parse_toml_document(&path, &text) else {
        return false;
    };
    document_is_official(&doc)
}

fn backup_config(dir: &Path, meta: &BackupMeta) -> Option<String> {
    if !meta.had_config {
        return None;
    }
    let path = dir.join("config.toml");
    fs::read_to_string(&path).ok()
}

fn latest_official_backup(codex_dir: &Path) -> Result<Option<OfficialConfigCandidate>> {
    let root = action_backup_root(codex_dir)?;
    if !root.is_dir() {
        return Ok(None);
    }
    let identity = canonical_identity(codex_dir);
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| io_err(&root, error))? {
        let entry = entry.map_err(|error| io_err(&root, error))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta_path = dir.join("meta.json");
        let Ok(meta_text) = fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<BackupMeta>(&meta_text) else {
            continue;
        };
        if !meta.had_auth
            || canonical_identity(Path::new(&meta.codex_dir)) != identity
            || !backup_config_is_official(&dir, &meta)
        {
            continue;
        }
        let Ok(Some(auth)) = read_auth_value(&dir.join("auth.json")) else {
            continue;
        };
        // Old Astra versions could mark config.toml as official while leaving
        // a proxy API key in auth.json. Historical auto-recovery therefore only
        // trusts unambiguous ChatGPT login backups. Official API keys remain
        // supported through an explicit Astra snapshot/save.
        if !is_chatgpt_auth(&auth) {
            continue;
        }
        let config_text = backup_config(&dir, &meta);
        let model = config_text.as_deref().and_then(|text| {
            model_from_config(&dir.join("config.toml"), text)
                .ok()
                .flatten()
        });
        candidates.push((meta.created_at.clone(), auth, config_text, model));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    Ok(candidates
        .into_iter()
        .next()
        .map(
            |(created_at, auth, config_text, model)| OfficialConfigCandidate {
                auth: Some(auth),
                config_text,
                model,
                source: format!("Astra 历史备份 {created_at}"),
            },
        ))
}

fn live_auth_candidate(
    codex_dir: &Path,
    require_official_route: bool,
    is_trusted: impl FnOnce(&Value) -> bool,
) -> Result<Option<OfficialConfigCandidate>> {
    let is_official = live_config_is_official(codex_dir)?;
    if require_official_route && !is_official {
        return Ok(None);
    }
    let Some(auth) = read_auth_value(&auth_path(codex_dir))? else {
        return Ok(None);
    };
    if !is_trusted(&auth) {
        return Ok(None);
    }
    let config_text = if is_official {
        live_official_config_text(codex_dir)?
    } else {
        None
    };
    let model = config_text
        .as_deref()
        .map(|text| model_from_config(&config_path(codex_dir), text))
        .transpose()?
        .flatten();
    Ok(Some(OfficialConfigCandidate {
        auth: Some(auth),
        config_text,
        model,
        source: "当前 OpenAI 官方认证".to_string(),
    }))
}

fn live_chatgpt_candidate(
    codex_dir: &Path,
    allow_proxy_route: bool,
) -> Result<Option<OfficialConfigCandidate>> {
    live_auth_candidate(codex_dir, !allow_proxy_route, is_chatgpt_auth)
}

fn live_official_auth_candidate(codex_dir: &Path) -> Result<Option<OfficialConfigCandidate>> {
    live_auth_candidate(codex_dir, true, |auth| {
        is_chatgpt_auth(auth) || has_openai_api_key(auth)
    })
}

fn complete_candidate_config(
    codex_dir: &Path,
    mut candidate: OfficialConfigCandidate,
    backup: Option<&OfficialConfigCandidate>,
) -> Result<OfficialConfigCandidate> {
    if candidate.config_text.is_none() {
        candidate.config_text = live_official_config_text(codex_dir)?
            .or_else(|| backup.and_then(|value| value.config_text.clone()));
    }
    if candidate.config_text.is_none() {
        candidate.config_text = Some(build_official_config_text(
            codex_dir,
            candidate.model.as_deref(),
            false,
        )?);
    }
    if let Some(config_text) = candidate.config_text.as_deref() {
        candidate.model =
            model_from_config(&config_path(codex_dir), config_text)?.or(candidate.model);
    }
    Ok(candidate)
}

fn selected_named_profile_candidate(
    codex_dir: &Path,
    profile_id: &str,
) -> Result<Option<OfficialConfigCandidate>> {
    let saved = saved_official_profile_candidate(codex_dir, profile_id)?;
    // The selected profile owns login/refreshes only while its official
    // route is active. OAuth left under a proxy route may belong to a
    // different account and must never replace an explicitly saved login.
    if let Some(live) = live_official_auth_candidate(codex_dir)? {
        return complete_candidate_config(codex_dir, live, saved.as_ref()).map(Some);
    }
    saved
        .map(|candidate| complete_candidate_config(codex_dir, candidate, None))
        .transpose()
}

pub(crate) fn official_config_candidate(
    codex_dir: &Path,
    include_history_after_reset: bool,
) -> Result<Option<OfficialConfigCandidate>> {
    let profile_id = selected_profile_id(codex_dir)?;
    if profile_id != DEFAULT_OFFICIAL_PROFILE_ID {
        return selected_named_profile_candidate(codex_dir, &profile_id);
    }
    // Legacy backups and OAuth left under proxy routes do not record which
    // account they belong to. Keep their old recovery behavior only while
    // there are no named accounts that they could be confused with.
    let allow_legacy_recovery = !has_named_profiles(codex_dir)?;
    match load_snapshot_for_profile(codex_dir, &profile_id)? {
        SnapshotState::Ready(candidate) => {
            // A third-party route may deliberately keep the user's ChatGPT
            // login in auth.json and carry its own key in
            // experimental_bearer_token. In legacy single-account mode,
            // preserve token refreshes made during proxy use. With named
            // profiles, only trust live OAuth under the official route.
            if let Some(live) = live_chatgpt_candidate(codex_dir, allow_legacy_recovery)? {
                return complete_candidate_config(codex_dir, live, Some(&candidate)).map(Some);
            }
            // Never let an API-key-only live file silently downgrade a trusted
            // OAuth snapshot. Older Astra builds could leave exactly that
            // polluted state while config.toml already pointed at OpenAI.
            if candidate.auth.as_ref().is_some_and(is_chatgpt_auth) {
                return complete_candidate_config(codex_dir, candidate, None).map(Some);
            }
            if let Some(live) = live_official_auth_candidate(codex_dir)? {
                return complete_candidate_config(codex_dir, live, Some(&candidate)).map(Some);
            }
            return complete_candidate_config(codex_dir, candidate, None).map(Some);
        }
        SnapshotState::Reset(reset) if !include_history_after_reset || !allow_legacy_recovery => {
            if let Some(live) = live_official_auth_candidate(codex_dir)? {
                return complete_candidate_config(codex_dir, live, Some(&reset)).map(Some);
            }
            if let Some(live) = live_chatgpt_candidate(codex_dir, allow_legacy_recovery)? {
                return complete_candidate_config(codex_dir, live, Some(&reset)).map(Some);
            }
            return complete_candidate_config(codex_dir, reset, None).map(Some);
        }
        SnapshotState::Reset(reset) => {
            if let Some(live) = live_official_auth_candidate(codex_dir)? {
                return complete_candidate_config(codex_dir, live, Some(&reset)).map(Some);
            }
            if let Some(live) = live_chatgpt_candidate(codex_dir, allow_legacy_recovery)? {
                return complete_candidate_config(codex_dir, live, Some(&reset)).map(Some);
            }
            if let Some(mut backup) = latest_official_backup(codex_dir)? {
                if reset.config_text.is_some() {
                    backup.config_text = reset.config_text;
                    backup.model = reset.model.or(backup.model);
                }
                return complete_candidate_config(codex_dir, backup, None).map(Some);
            }
            return complete_candidate_config(codex_dir, reset, None).map(Some);
        }
        SnapshotState::Missing => {}
    }

    let backup = if allow_legacy_recovery {
        latest_official_backup(codex_dir)?
    } else {
        None
    };
    if let Some(candidate) = live_chatgpt_candidate(codex_dir, allow_legacy_recovery)? {
        return complete_candidate_config(codex_dir, candidate, backup.as_ref()).map(Some);
    }
    backup
        .map(|candidate| complete_candidate_config(codex_dir, candidate, None))
        .transpose()
}

pub(crate) fn official_auth_available(codex_dir: &Path) -> Result<bool> {
    let profile_id = selected_profile_id(codex_dir)?;
    match load_snapshot_for_profile(codex_dir, &profile_id)? {
        SnapshotState::Ready(_) => return Ok(true),
        SnapshotState::Reset(_) => return Ok(live_official_auth_candidate(codex_dir)?.is_some()),
        SnapshotState::Missing => {}
    }
    if live_official_auth_candidate(codex_dir)?.is_some() {
        return Ok(true);
    }
    if profile_id != DEFAULT_OFFICIAL_PROFILE_ID || has_named_profiles(codex_dir)? {
        return Ok(false);
    }
    if let Some(auth) = read_auth_value(&auth_path(codex_dir))? {
        if is_chatgpt_auth(&auth) {
            return Ok(true);
        }
    }
    // Historical backup discovery is intentionally deferred to restore. A
    // status refresh must not traverse every backup on the startup path.
    Ok(false)
}

#[cfg(test)]
pub(crate) fn get_official_config_draft_inner(
    config_dir: Option<String>,
) -> Result<Option<OfficialConfigDraft>> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    let candidate = match official_config_candidate(&codex_dir, false)? {
        Some(candidate) => candidate,
        None => {
            let config_text = build_official_config_text(&codex_dir, None, false)?;
            let model = model_from_config(&config_path(&codex_dir), &config_text)?;
            OfficialConfigCandidate {
                auth: None,
                config_text: Some(config_text),
                model,
                source: "根据当前 config.toml 生成".to_string(),
            }
        }
    };
    official_config_draft(candidate).map(Some)
}

#[cfg(test)]
fn official_config_draft(candidate: OfficialConfigCandidate) -> Result<OfficialConfigDraft> {
    let auth_json = candidate
        .auth
        .as_ref()
        .map(serde_json::to_string_pretty)
        .transpose()
        .map_err(|error| CodexxError::Config(format!("格式化官方配置快照失败: {error}")))?
        .unwrap_or_default();
    Ok(OfficialConfigDraft {
        auth_json,
        config_text: candidate.config_text.unwrap_or_default(),
        model: candidate.model,
        source: candidate.source,
    })
}

#[cfg(test)]
pub(crate) fn official_snapshot_path_for_test(codex_dir: &Path) -> Result<PathBuf> {
    official_snapshot_path(codex_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile_test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codex-x-official-profile-{name}-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).expect("create profile test Codex directory");
        path
    }

    #[test]
    fn profile_snapshot_paths_keep_legacy_default_and_isolate_accounts_and_homes() {
        let codex_dir = profile_test_dir("paths");
        let other_home = profile_test_dir("other-home");
        let digest = Sha256::digest(canonical_identity(&codex_dir).as_bytes());
        let default_path =
            official_snapshot_path_for_profile(&codex_dir, DEFAULT_OFFICIAL_PROFILE_ID)
                .expect("default snapshot path");
        assert_eq!(
            default_path,
            app_home()
                .unwrap()
                .join("official-configs")
                .join(format!("{digest:x}.json"))
        );
        let first = official_snapshot_path_for_profile(&codex_dir, "account-a").unwrap();
        let second = official_snapshot_path_for_profile(&codex_dir, "account-b").unwrap();
        let elsewhere = official_snapshot_path_for_profile(&other_home, "account-a").unwrap();
        assert_ne!(first, default_path);
        assert_ne!(first, second);
        assert_ne!(first, elsewhere);
        assert_eq!(first.parent(), second.parent());
        assert!(official_snapshot_path_for_profile(&codex_dir, " ").is_err());
        fs::remove_dir_all(codex_dir).unwrap();
        fs::remove_dir_all(other_home).unwrap();
    }

    #[test]
    fn explicit_profile_snapshot_reads_never_borrow_other_auth_and_preserve_resets() {
        let codex_dir = profile_test_dir("explicit-read");
        let live_auth = json!({"tokens": {"access_token": "other-account"}});
        fs::write(
            config_path(&codex_dir),
            "model_provider = \"openai\"\nmodel = \"live-model\"\n",
        )
        .unwrap();
        fs::write(
            auth_path(&codex_dir),
            serde_json::to_vec(&live_auth).unwrap(),
        )
        .unwrap();
        write_official_profile_snapshot(
            &codex_dir,
            DEFAULT_OFFICIAL_PROFILE_ID,
            None,
            None,
            Some(live_auth),
        )
        .unwrap();
        assert!(
            saved_official_profile_candidate(&codex_dir, "missing-account")
                .unwrap()
                .is_none()
        );

        let profile_id = "saved-account";
        let saved_auth = json!({"tokens": {"access_token": "saved-account"}});
        write_official_profile_snapshot(
            &codex_dir,
            profile_id,
            None,
            Some("saved-model".to_string()),
            Some(saved_auth.clone()),
        )
        .unwrap();
        let candidate = saved_official_profile_candidate(&codex_dir, profile_id)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.auth, Some(saved_auth));
        assert!(candidate.config_text.is_none());
        assert_eq!(candidate.model.as_deref(), Some("saved-model"));

        let path = official_snapshot_path_for_profile(&codex_dir, profile_id).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(write_official_profile_snapshot(
            &codex_dir,
            profile_id,
            None,
            None,
            Some(json!({"auth_mode": "chatgpt"})),
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let reset_config = "model_provider = \"openai\"\nmodel = \"reset-model\"\n";
        write_official_profile_snapshot(
            &codex_dir,
            profile_id,
            Some(reset_config.to_string()),
            Some("reset-model".to_string()),
            None,
        )
        .unwrap();
        let reset = saved_official_profile_candidate(&codex_dir, profile_id)
            .unwrap()
            .unwrap();
        assert!(reset.auth.is_none());
        assert_eq!(reset.config_text.as_deref(), Some(reset_config));

        fs::remove_file(path).unwrap();
        fs::remove_file(
            official_snapshot_path_for_profile(&codex_dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap(),
        )
        .unwrap();
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn selected_named_profile_accepts_official_refresh_but_ignores_proxy_login() {
        let codex_dir = profile_test_dir("refresh");
        let profile_id = "selected-account";
        let saved_config = "model_provider = \"openai\"\nmodel = \"saved-model\"\n";
        let saved_auth = json!({"tokens": {"access_token": "saved-token"}});
        let refreshed_auth = json!({"tokens": {"access_token": "refreshed-token"}});
        write_official_profile_snapshot(
            &codex_dir,
            profile_id,
            Some(saved_config.to_string()),
            Some("saved-model".to_string()),
            Some(saved_auth.clone()),
        )
        .unwrap();
        fs::write(
            config_path(&codex_dir),
            "model_provider = \"custom\"\nmodel = \"proxy-model\"\n[model_providers.custom]\nname = \"Proxy\"\nbase_url = \"https://proxy.example/v1\"\n",
        )
        .unwrap();
        fs::write(
            auth_path(&codex_dir),
            serde_json::to_vec(&refreshed_auth).unwrap(),
        )
        .unwrap();
        let candidate = selected_named_profile_candidate(&codex_dir, profile_id)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.auth, Some(saved_auth));
        assert_eq!(candidate.config_text.as_deref(), Some(saved_config));

        write_official_profile_snapshot(
            &codex_dir,
            profile_id,
            Some(saved_config.to_string()),
            Some("saved-model".to_string()),
            None,
        )
        .unwrap();
        assert!(selected_named_profile_candidate(&codex_dir, profile_id)
            .unwrap()
            .unwrap()
            .auth
            .is_none());

        let live_config = "model_provider = \"openai\"\nmodel = \"fresh-model\"\n";
        fs::write(config_path(&codex_dir), live_config).unwrap();
        let refreshed = selected_named_profile_candidate(&codex_dir, profile_id)
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.auth, Some(refreshed_auth));
        assert_eq!(refreshed.config_text.as_deref(), Some(live_config));
        let api_key_auth = json!({"OPENAI_API_KEY": "official-key"});
        fs::write(
            auth_path(&codex_dir),
            serde_json::to_vec(&api_key_auth).unwrap(),
        )
        .unwrap();
        assert_eq!(
            selected_named_profile_candidate(&codex_dir, profile_id)
                .unwrap()
                .unwrap()
                .auth,
            Some(api_key_auth)
        );
        fs::remove_file(official_snapshot_path_for_profile(&codex_dir, profile_id).unwrap())
            .unwrap();
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn default_profile_ignores_proxy_oauth_once_named_profiles_exist() {
        let codex_dir = profile_test_dir("default-proxy-isolation");
        let default_auth = json!({"tokens": {"access_token": "default-account"}});
        let proxy_auth = json!({"tokens": {"access_token": "other-account"}});
        let official_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        fs::write(
            config_path(&codex_dir),
            "model_provider = \"custom\"\n[model_providers.custom]\nname = \"Proxy\"\nbase_url = \"https://proxy.example/v1\"\n",
        )
        .unwrap();
        fs::write(
            auth_path(&codex_dir),
            serde_json::to_vec(&proxy_auth).unwrap(),
        )
        .unwrap();
        write_official_profile_snapshot(
            &codex_dir,
            DEFAULT_OFFICIAL_PROFILE_ID,
            Some(official_config.to_string()),
            Some("official-model".to_string()),
            Some(default_auth.clone()),
        )
        .unwrap();

        // Preserve existing single-account recovery of OAuth refreshed while
        // a proxy route is active.
        assert!(!has_named_profiles(&codex_dir).unwrap());
        assert_eq!(
            official_config_candidate(&codex_dir, false)
                .unwrap()
                .unwrap()
                .auth,
            Some(proxy_auth.clone())
        );

        let conn = crate::providers::open_store().unwrap();
        let scope = crate::paths::normalized_path_scope(&codex_dir);
        conn.execute(
            "INSERT INTO official_profiles (codex_dir, id, provider_name, created_at, updated_at)
             VALUES (?1, 'another-account', 'Another account', 'test', 'test')",
            [&scope],
        )
        .unwrap();
        assert!(has_named_profiles(&codex_dir).unwrap());
        assert_eq!(
            selected_profile_id(&codex_dir).unwrap(),
            DEFAULT_OFFICIAL_PROFILE_ID
        );
        let saved = official_config_candidate(&codex_dir, false)
            .unwrap()
            .unwrap();
        assert_eq!(saved.auth, Some(default_auth));
        assert_eq!(saved.config_text.as_deref(), Some(official_config));

        write_official_profile_snapshot(
            &codex_dir,
            DEFAULT_OFFICIAL_PROFILE_ID,
            Some(official_config.to_string()),
            Some("official-model".to_string()),
            None,
        )
        .unwrap();
        for include_history in [false, true] {
            assert!(official_config_candidate(&codex_dir, include_history)
                .unwrap()
                .unwrap()
                .auth
                .is_none());
        }
        assert!(!official_auth_available(&codex_dir).unwrap());

        let default_path =
            official_snapshot_path_for_profile(&codex_dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        fs::remove_file(default_path).unwrap();
        for include_history in [false, true] {
            assert!(official_config_candidate(&codex_dir, include_history)
                .unwrap()
                .is_none());
        }
        assert!(!official_auth_available(&codex_dir).unwrap());

        // Login and refreshes performed in the selected official profile
        // remain usable after multiple accounts have been added.
        fs::write(config_path(&codex_dir), official_config).unwrap();
        assert_eq!(
            official_config_candidate(&codex_dir, false)
                .unwrap()
                .unwrap()
                .auth,
            Some(proxy_auth)
        );
        assert!(official_auth_available(&codex_dir).unwrap());
        conn.execute(
            "DELETE FROM official_profiles WHERE codex_dir = ?1",
            [&scope],
        )
        .unwrap();
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn legacy_oauth_without_auth_mode_is_trusted() {
        assert!(is_chatgpt_auth(&json!({
            "OPENAI_API_KEY": null,
            "tokens": {"access_token": "legacy-access"},
            "last_refresh": "2026-08-11T00:00:00Z"
        })));
        assert!(!is_chatgpt_auth(&json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": null,
            "tokens": {"access_token": "must-not-trust"}
        })));
        assert!(!is_chatgpt_auth(&json!({
            "tokens": {"access_token": "legacy-access"},
            "OPENAI_API_KEY": "sk-third-party"
        })));
        assert!(is_chatgpt_auth(&json!({
            "auth_mode": "chatgptAuthTokens",
            "tokens": {"access_token": "external-access"}
        })));
        assert!(is_chatgpt_auth(&json!({
            "auth_mode": "agentIdentity",
            "agent_identity": {
                "agent_runtime_id": "runtime-id",
                "agent_private_key": "private-key"
            }
        })));
        assert!(is_chatgpt_auth(&json!({
            "auth_mode": "personalAccessToken",
            "personal_access_token": "pat-test"
        })));
        assert!(!is_chatgpt_auth(&json!({
            "auth_mode": "bedrockApiKey",
            "bedrock_api_key": {"api_key": "bedrock-test", "region": "us-east-1"}
        })));
        assert!(!is_chatgpt_auth(&json!({
            "agent_identity": {
                "agent_runtime_id": "runtime-id",
                "agent_private_key": "private-key"
            }
        })));
    }

    #[test]
    fn v3_reset_snapshot_keeps_complete_config_without_restoring_auth() {
        let codex_dir = std::env::temp_dir().join(format!(
            "codex-x-official-reset-snapshot-{}",
            std::process::id()
        ));
        fs::create_dir_all(&codex_dir).expect("create test Codex directory");
        let config = r#"# keep-reset-config
model_provider = "openai"
model = "official-model"
approval_policy = "on-request"

[features]
js_repl = false
"#;
        let snapshot = OfficialConfigSnapshot {
            version: SNAPSHOT_VERSION,
            codex_dir: canonical_identity(&codex_dir),
            captured_at: "2026-08-11T00:00:00+08:00".to_string(),
            model: Some("official-model".to_string()),
            config: Some(config.to_string()),
            auth: None,
        };

        let candidate = match snapshot_state(&codex_dir, Path::new("snapshot.json"), snapshot)
            .expect("classify reset snapshot")
        {
            SnapshotState::Reset(candidate) => candidate,
            SnapshotState::Missing | SnapshotState::Ready(_) => {
                panic!("v3 auth-less snapshot must remain an explicit reset")
            }
        };
        let candidate = complete_candidate_config(&codex_dir, candidate, None)
            .expect("complete reset candidate");
        assert!(candidate.auth.is_none());
        assert_eq!(candidate.model.as_deref(), Some("official-model"));
        assert_eq!(candidate.config_text.as_deref(), Some(config));

        let draft = official_config_draft(candidate).expect("build reset draft");
        assert!(draft.auth_json.is_empty());
        assert_eq!(draft.model.as_deref(), Some("official-model"));
        assert_eq!(draft.config_text, config);

        fs::remove_dir_all(codex_dir).expect("remove test Codex directory");
    }
}
