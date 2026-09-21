use super::live::{
    apply_official_config_with_snapshot_locked, official_activation_config_text,
    persist_detected_live_custom_provider, read_live_file_snapshot, AppliedLiveFiles,
    LiveAuthAction,
};
use super::official_auth::{
    build_official_config_text, capture_live_official_config_before_provider_switch,
    document_is_official, is_chatgpt_auth, live_config_is_official, official_config_candidate,
    official_snapshot_path_for_profile, saved_official_profile_candidate,
    validate_official_config_text, write_official_profile_snapshot, OfficialConfigCandidate,
};
use super::{open_store, rollback_provider_store_inner, ProviderStoreRollback};
use crate::config_migration::migrate_legacy_prompt_config_locked;
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, parse_toml_document, read_to_string_if_exists};
use crate::live_config::{
    acquire_live_config_lock, read_file_snapshot, remove_file_if_unchanged,
    restore_file_snapshot_if_unchanged,
};
use crate::paths::normalized_path_scope;
use crate::state::{build_state_after_migration, ActionResult};
use crate::{now_rfc3339, resolve_codex_dir};
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_OFFICIAL_PROFILE_ID: &str = "openai-official";
const DEFAULT_OFFICIAL_PROFILE_NAME: &str = "OpenAI Official";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialProfileSummary {
    pub(crate) id: String,
    pub(crate) provider_name: String,
    pub(crate) model: Option<String>,
    pub(crate) has_auth: bool,
    pub(crate) has_owned_auth: bool,
    pub(crate) email: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) can_query_quota: bool,
    pub(crate) is_default: bool,
    pub(crate) is_current: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialProfileDetail {
    #[serde(flatten)]
    pub(crate) profile: OfficialProfileSummary,
    pub(crate) config_text: String,
    pub(crate) auth_json: String,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialProfileInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) provider_name: String,
    pub(crate) model: Option<String>,
    pub(crate) config_text: Option<String>,
    pub(crate) auth_json: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialProfileActionResult {
    #[serde(flatten)]
    pub(crate) action: ActionResult,
    pub(crate) profile: OfficialProfileSummary,
}

// Native-only credentials: intentionally neither Debug nor Serialize. The
// network caller reads these while holding the short live-config lock, then
// drops that lock before making a request.
pub(crate) struct OfficialProfileQuotaCredentials {
    pub(crate) access_token: String,
    pub(crate) account_id: Option<String>,
    pub(crate) email: Option<String>,
}

fn nonempty_auth_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn display_email(value: Option<&Value>) -> Option<String> {
    nonempty_auth_string(value).filter(|email| {
        email.len() <= 320
            && email.contains('@')
            && !email
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
    })
}

fn jwt_display_claims(token: Option<&Value>) -> Option<Value> {
    let token = token?.as_str()?;
    // Decode only a bounded payload. JWT claims are unverified display metadata,
    // never the source of account IDs, credentials, or authorization decisions.
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if header.is_empty()
        || payload.is_empty()
        || signature.is_empty()
        || parts.next().is_some()
        || payload.len() > 32 * 1024
    {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&payload).ok()
}

fn display_plan_type(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    if raw.len() > 64 || raw.chars().any(char::is_control) {
        return None;
    }
    let plan = raw.trim();
    (!plan.is_empty()
        && plan
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then(|| plan.to_string())
}

#[derive(Default)]
struct OfficialAuthDisplayMetadata {
    email: Option<String>,
    plan_type: Option<String>,
}

fn auth_display_metadata(auth: &Value) -> OfficialAuthDisplayMetadata {
    let chatgpt_auth = is_chatgpt_auth(auth);
    let mut metadata = OfficialAuthDisplayMetadata {
        email: display_email(auth.get("email")),
        plan_type: chatgpt_auth
            .then(|| {
                display_plan_type(auth.get("plan_type"))
                    .or_else(|| display_plan_type(auth.get("chatgpt_plan_type")))
            })
            .flatten(),
    };
    for token in [
        auth.pointer("/tokens/id_token"),
        auth.pointer("/tokens/access_token"),
    ] {
        if metadata.email.is_some() && (metadata.plan_type.is_some() || !chatgpt_auth) {
            break;
        }
        let Some(claims) = jwt_display_claims(token) else {
            continue;
        };
        metadata.email = metadata.email.or_else(|| {
            display_email(claims.get("email")).or_else(|| {
                display_email(claims.pointer("/https:~1~1api.openai.com~1profile/email"))
            })
        });
        if chatgpt_auth && metadata.plan_type.is_none() {
            // Codex's login/token_data.rs reads this namespaced ID-token claim.
            // Keep its raw identifier for display; it never grants access.
            metadata.plan_type = display_plan_type(
                claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type"),
            );
        }
    }
    metadata
}

fn auth_display_email(auth: &Value) -> Option<String> {
    auth_display_metadata(auth).email
}

fn quota_access_token(auth: &Value) -> Option<String> {
    is_chatgpt_auth(auth)
        .then(|| nonempty_auth_string(auth.pointer("/tokens/access_token")))
        .flatten()
}

fn profile_owned_auth(codex_dir: &Path, profile_id: &str) -> Result<Option<Value>> {
    profile_name(codex_dir, profile_id)?;
    let is_current = selected_profile_id(codex_dir)? == profile_id
        && live_config_is_official(codex_dir)
            .map_err(|_| CodexxError::Config("无法确认当前官方登录状态，请刷新配置".to_string()))?;
    if is_current {
        let text = read_to_string_if_exists(&crate::auth_path(codex_dir))
            .map_err(|_| CodexxError::Config("无法读取当前官方认证，请重新登录".to_string()))?;
        // Live logout/refresh owns the current profile. Do not resurrect a stale
        // snapshot or consult another account's historical recovery credentials.
        return parse_auth(&text)
            .map_err(|_| CodexxError::Config("当前官方认证无效，请重新登录".to_string()));
    }
    saved_official_profile_candidate(codex_dir, profile_id)
        .map(|candidate| candidate.and_then(|candidate| candidate.auth))
        .map_err(|_| {
            CodexxError::Config("无法读取此官方配置的认证，请重新保存登录信息".to_string())
        })
}

pub(crate) fn official_profile_quota_credentials(
    codex_dir: &Path,
    profile_id: &str,
) -> Result<OfficialProfileQuotaCredentials> {
    let auth = profile_owned_auth(codex_dir, profile_id)?.ok_or_else(|| {
        CodexxError::Config("此官方配置尚未登录，请先在 Codex 中登录".to_string())
    })?;
    let access_token = quota_access_token(&auth).ok_or_else(|| {
        CodexxError::Config("此配置没有可用的 ChatGPT 登录凭据，无法查询订阅额度".to_string())
    })?;
    Ok(OfficialProfileQuotaCredentials {
        access_token,
        account_id: nonempty_auth_string(auth.pointer("/tokens/account_id")),
        email: auth_display_email(&auth),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileRecord {
    id: String,
    provider_name: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreSnapshot {
    profiles: Vec<ProfileRecord>,
    selection: Option<(String, String)>,
}

fn database_error(error: rusqlite::Error) -> CodexxError {
    CodexxError::Database(error.to_string())
}

fn read_store(conn: &Connection, codex_dir: &Path) -> Result<StoreSnapshot> {
    let scope = normalized_path_scope(codex_dir);
    let mut statement = conn
        .prepare(
            "SELECT id, provider_name, created_at, updated_at FROM official_profiles
         WHERE codex_dir = ?1 ORDER BY created_at, id",
        )
        .map_err(database_error)?;
    let profiles = statement
        .query_map([&scope], |row| {
            Ok(ProfileRecord {
                id: row.get(0)?,
                provider_name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })
        .map_err(database_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(database_error)?;
    let selection = conn
        .query_row(
            "SELECT profile_id, updated_at FROM official_profile_selections WHERE codex_dir = ?1",
            [&scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    Ok(StoreSnapshot {
        profiles,
        selection,
    })
}

pub(crate) fn selected_profile_id(codex_dir: &Path) -> Result<String> {
    let store = read_store(&open_store()?, codex_dir)?;
    Ok(store
        .selection
        .as_ref()
        .map(|(id, _)| id)
        .filter(|id| {
            id.as_str() == DEFAULT_OFFICIAL_PROFILE_ID
                || store.profiles.iter().any(|profile| &profile.id == *id)
        })
        .cloned()
        .unwrap_or_else(|| DEFAULT_OFFICIAL_PROFILE_ID.to_string()))
}

pub(crate) fn has_named_profiles(codex_dir: &Path) -> Result<bool> {
    Ok(read_store(&open_store()?, codex_dir)?
        .profiles
        .iter()
        .any(|profile| profile.id != DEFAULT_OFFICIAL_PROFILE_ID))
}

pub(crate) fn active_profile_id(codex_dir: &Path, is_official: bool) -> Result<Option<String>> {
    if is_official {
        selected_profile_id(codex_dir).map(Some)
    } else {
        Ok(None)
    }
}

fn profile_name(codex_dir: &Path, id: &str) -> Result<String> {
    let store = read_store(&open_store()?, codex_dir)?;
    if let Some(profile) = store.profiles.iter().find(|profile| profile.id == id) {
        return Ok(profile.provider_name.clone());
    }
    if id == DEFAULT_OFFICIAL_PROFILE_ID {
        return Ok(DEFAULT_OFFICIAL_PROFILE_NAME.to_string());
    }
    Err(CodexxError::Config(format!("未找到官方配置: {id}")))
}

fn normalize_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(CodexxError::Config(
            "官方配置名称必须为 1–100 个字符".to_string(),
        ));
    }
    Ok(name.to_string())
}

fn bounded_copy_name(base_name: &str, ordinal: Option<usize>) -> String {
    let base_name = base_name.trim();
    let (stem, marker) = [" 副本", " Copy"]
        .into_iter()
        .find_map(|marker| base_name.strip_suffix(marker).map(|stem| (stem, marker)))
        .unwrap_or((base_name, ""));
    let ending = match ordinal {
        Some(ordinal) => format!("{marker} {ordinal}"),
        None => marker.to_string(),
    };
    // The limit is in Unicode scalar values, matching normalize_name. Reserve
    // room for both the copy label and the full number before shortening a name.
    let stem = stem
        .chars()
        .take(100usize.saturating_sub(ending.chars().count()))
        .collect::<String>();
    format!("{}{ending}", stem.trim_end())
}

fn profile_candidate(codex_dir: &Path, id: &str) -> Result<Option<OfficialConfigCandidate>> {
    profile_name(codex_dir, id)?;
    if selected_profile_id(codex_dir)? == id {
        // The selected profile owns live login/refresh changes. Inactive
        // profiles are always read from their explicit isolated snapshot.
        return official_config_candidate(codex_dir, false);
    }
    saved_official_profile_candidate(codex_dir, id)
}

fn detail(codex_dir: &Path, id: &str) -> Result<OfficialProfileDetail> {
    let provider_name = profile_name(codex_dir, id)?;
    let candidate = profile_candidate(codex_dir, id)?;
    if candidate.is_none() && id != DEFAULT_OFFICIAL_PROFILE_ID {
        return Err(CodexxError::Config(format!(
            "官方配置 {provider_name} 的快照缺失，未使用其他账号替代"
        )));
    }
    let model = candidate
        .as_ref()
        .and_then(|candidate| candidate.model.clone());
    let config_text = match candidate
        .as_ref()
        .and_then(|candidate| candidate.config_text.clone())
    {
        Some(config) => config,
        None => build_official_config_text(codex_dir, model.as_deref(), model.is_none())?,
    };
    let auth_json = candidate
        .as_ref()
        .and_then(|candidate| candidate.auth.as_ref())
        .map(serde_json::to_string_pretty)
        .transpose()
        .map_err(|error| CodexxError::Config(format!("读取官方认证失败: {error}")))?
        .unwrap_or_default();
    let owned_auth = profile_owned_auth(codex_dir, id).ok().flatten();
    let metadata = owned_auth
        .as_ref()
        .map(auth_display_metadata)
        .unwrap_or_default();
    Ok(OfficialProfileDetail {
        profile: OfficialProfileSummary {
            id: id.to_string(),
            provider_name,
            model,
            has_auth: !auth_json.is_empty(),
            has_owned_auth: owned_auth.is_some(),
            email: metadata.email,
            plan_type: metadata.plan_type,
            can_query_quota: owned_auth.as_ref().and_then(quota_access_token).is_some(),
            is_default: id == DEFAULT_OFFICIAL_PROFILE_ID,
            is_current: live_config_is_official(codex_dir)?
                && selected_profile_id(codex_dir)? == id,
        },
        config_text,
        auth_json,
        source: candidate
            .map(|candidate| candidate.source)
            .unwrap_or_else(|| "根据当前 config.toml 生成".to_string()),
    })
}

pub(crate) fn get_official_profile_inner(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<OfficialProfileDetail> {
    detail(&resolve_codex_dir(config_dir)?, &profile_id)
}

pub(crate) fn list_official_profiles_inner(
    config_dir: Option<String>,
) -> Result<Vec<OfficialProfileSummary>> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let store = read_store(&open_store()?, &codex_dir)?;
    let default_name = store
        .profiles
        .iter()
        .find(|profile| profile.id == DEFAULT_OFFICIAL_PROFILE_ID)
        .map(|profile| profile.provider_name.clone())
        .unwrap_or_else(|| DEFAULT_OFFICIAL_PROFILE_NAME.to_string());
    let mut profiles = vec![(DEFAULT_OFFICIAL_PROFILE_ID.to_string(), default_name)];
    profiles.extend(
        store
            .profiles
            .into_iter()
            .filter(|profile| profile.id != DEFAULT_OFFICIAL_PROFILE_ID)
            .map(|profile| (profile.id, profile.provider_name)),
    );
    let selected = selected_profile_id(&codex_dir)?;
    let config_path = crate::config_path(&codex_dir);
    let config = parse_toml_document(&config_path, &read_to_string_if_exists(&config_path)?)?;
    let config = crate::failover::direct_document(&codex_dir, &config)?;
    let is_official = document_is_official(&config);
    let live_model = crate::string_value(&config, "model");
    let live_auth = is_official
        .then(|| {
            read_to_string_if_exists(&crate::auth_path(&codex_dir))
                .ok()
                .and_then(|text| parse_auth(&text).ok().flatten())
        })
        .flatten();
    // Startup only needs metadata and the explicit profile snapshot. Do not
    // call the editor/recovery candidate path: its legacy fallback scans the
    // complete backup history and synthesizes full config/auth documents.
    Ok(profiles
        .into_iter()
        .map(|(id, provider_name)| {
            let saved = saved_official_profile_candidate(&codex_dir, &id)
                .ok()
                .flatten();
            let is_current = is_official && selected == id;
            let owned_auth = if is_current {
                live_auth.as_ref()
            } else {
                saved.as_ref().and_then(|saved| saved.auth.as_ref())
            };
            let metadata = owned_auth.map(auth_display_metadata).unwrap_or_default();
            OfficialProfileSummary {
                model: if is_current {
                    live_model.clone()
                } else {
                    saved.as_ref().and_then(|saved| saved.model.clone())
                },
                has_auth: (is_current && live_auth.is_some())
                    || saved.as_ref().is_some_and(|saved| saved.auth.is_some()),
                has_owned_auth: owned_auth.is_some(),
                email: metadata.email,
                plan_type: metadata.plan_type,
                can_query_quota: owned_auth.and_then(quota_access_token).is_some(),
                is_default: id == DEFAULT_OFFICIAL_PROFILE_ID,
                id,
                provider_name,
                is_current,
            }
        })
        .collect())
}

fn parse_auth(text: &str) -> Result<Option<Value>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let auth: Value = serde_json::from_str(text)
        .map_err(|error| CodexxError::Config(format!("官方 auth.json 不是有效 JSON: {error}")))?;
    let has_endpoint = ["base_url", "baseUrl", "api_base", "endpoint"]
        .iter()
        .any(|key| auth.get(key).is_some());
    let official_api_key = auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|key| !key.trim().is_empty())
        && auth
            .get("auth_mode")
            .and_then(Value::as_str)
            .is_none_or(|mode| mode.eq_ignore_ascii_case("apikey"))
        && auth.get("tokens").is_none_or(|tokens| {
            tokens.is_null() || tokens.as_object().is_some_and(|tokens| tokens.is_empty())
        });
    if !auth.is_object() || has_endpoint || (!is_chatgpt_auth(&auth) && !official_api_key) {
        return Err(CodexxError::Config(
            "官方认证必须是有效的 Codex 官方登录认证或 OpenAI API Key；留空可保存待登录配置"
                .to_string(),
        ));
    }
    Ok(Some(auth))
}

struct SnapshotChange {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

struct Mutation {
    codex_dir: PathBuf,
    store_before: StoreSnapshot,
    store_after: Option<StoreSnapshot>,
    snapshots: Vec<SnapshotChange>,
    live: Option<AppliedLiveFiles>,
    provider: Option<ProviderStoreRollback>,
}

impl Mutation {
    fn new(codex_dir: &Path) -> Result<Self> {
        Ok(Self {
            codex_dir: codex_dir.to_path_buf(),
            store_before: read_store(&open_store()?, codex_dir)?,
            store_after: None,
            snapshots: Vec::new(),
            live: None,
            provider: None,
        })
    }

    fn snapshot(&mut self, id: &str, write: impl FnOnce() -> Result<()>) -> Result<()> {
        let path = official_snapshot_path_for_profile(&self.codex_dir, id)?;
        let before = read_file_snapshot(&path)?;
        write()?;
        let after = read_file_snapshot(&path)?;
        self.snapshots.push(SnapshotChange {
            path,
            before,
            after,
        });
        Ok(())
    }

    fn update_store(&mut self, update: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
        let mut conn = open_store()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        update(&transaction)?;
        let after = read_store(&transaction, &self.codex_dir)?;
        transaction.commit().map_err(database_error)?;
        self.store_after = Some(after);
        Ok(())
    }

    fn rollback(mut self, error: CodexxError) -> CodexxError {
        let mut failures = Vec::new();
        if let Some(live) = self.live.as_ref() {
            if let Err(rollback) = live.rollback() {
                // Keep per-account recovery snapshots and the new selection
                // when an external writer prevents restoring the live pair.
                return CodexxError::Config(format!(
                    "{error}；live 配置回滚失败，已保留账号恢复快照：{rollback}"
                ));
            }
        }
        for snapshot in self.snapshots.iter().rev() {
            if let Err(rollback) = restore_file_snapshot_if_unchanged(
                &snapshot.path,
                snapshot.after.as_deref(),
                snapshot.before.as_deref(),
            ) {
                failures.push(rollback.to_string());
            }
        }
        if let Some(after) = self.store_after.as_ref() {
            let rollback = (|| -> Result<()> {
                let mut conn = open_store()?;
                let transaction = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(database_error)?;
                if read_store(&transaction, &self.codex_dir)? != *after {
                    return Err(CodexxError::Config(
                        "官方配置记录已被其他操作修改，拒绝覆盖".to_string(),
                    ));
                }
                let scope = normalized_path_scope(&self.codex_dir);
                transaction
                    .execute(
                        "DELETE FROM official_profiles WHERE codex_dir = ?1",
                        [&scope],
                    )
                    .map_err(database_error)?;
                transaction
                    .execute(
                        "DELETE FROM official_profile_selections WHERE codex_dir = ?1",
                        [&scope],
                    )
                    .map_err(database_error)?;
                for profile in &self.store_before.profiles {
                    transaction.execute("INSERT INTO official_profiles (codex_dir,id,provider_name,created_at,updated_at) VALUES (?1,?2,?3,?4,?5)", params![scope,profile.id,profile.provider_name,profile.created_at,profile.updated_at]).map_err(database_error)?;
                }
                if let Some((id, updated)) = &self.store_before.selection {
                    transaction.execute("INSERT INTO official_profile_selections (codex_dir,profile_id,updated_at) VALUES (?1,?2,?3)", params![scope,id,updated]).map_err(database_error)?;
                }
                transaction.commit().map_err(database_error)
            })();
            if let Err(rollback) = rollback {
                failures.push(rollback.to_string());
            }
        }
        if let Some(provider) = self.provider.take() {
            if let Err(rollback) = rollback_provider_store_inner(provider) {
                failures.push(rollback.to_string());
            }
        }
        if failures.is_empty() {
            error
        } else {
            CodexxError::Config(format!("{error}；回滚失败：{}", failures.join("；")))
        }
    }
}

fn with_mutation<T>(codex_dir: &Path, run: impl FnOnce(&mut Mutation) -> Result<T>) -> Result<T> {
    let mut mutation = Mutation::new(codex_dir)?;
    match run(&mut mutation) {
        Ok(value) => Ok(value),
        Err(error) => Err(mutation.rollback(error)),
    }
}

fn store_profile(conn: &Connection, codex_dir: &Path, id: &str, name: &str) -> Result<()> {
    let scope = normalized_path_scope(codex_dir);
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO official_profiles (codex_dir,id,provider_name,created_at,updated_at) VALUES (?1,?2,?3,?4,?4)
         ON CONFLICT(codex_dir,id) DO UPDATE SET provider_name=excluded.provider_name, updated_at=excluded.updated_at",
        params![scope,id,name,now],
    ).map_err(database_error)?;
    Ok(())
}

fn select_profile(conn: &Connection, codex_dir: &Path, id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO official_profile_selections (codex_dir,profile_id,updated_at) VALUES (?1,?2,?3)
         ON CONFLICT(codex_dir) DO UPDATE SET profile_id=excluded.profile_id,updated_at=excluded.updated_at",
        params![normalized_path_scope(codex_dir),id,now_rfc3339()],
    ).map_err(database_error)?;
    Ok(())
}

fn new_profile_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "official-{}-{}-{}",
        chrono::Local::now()
            .timestamp_nanos_opt()
            .unwrap_or_default(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn action(codex_dir: &Path, message: String, backup_id: Option<String>) -> Result<ActionResult> {
    Ok(ActionResult {
        ok: true,
        message,
        backup_id,
        state: build_state_after_migration(codex_dir.to_path_buf())?,
    })
}

fn save_locked(
    codex_dir: &Path,
    input: OfficialProfileInput,
    activate: bool,
) -> Result<OfficialProfileActionResult> {
    let live_before = read_live_file_snapshot(codex_dir)?;
    let name = normalize_name(&input.provider_name)?;
    let existing = input
        .id
        .as_deref()
        .map(|id| detail(codex_dir, id))
        .transpose()?;
    let id = input.id.unwrap_or_else(new_profile_id);
    let config = match input
        .config_text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        Some(config) => config.to_string(),
        None => match existing.as_ref() {
            Some(detail) => detail.config_text.clone(),
            None => build_official_config_text(
                codex_dir,
                input.model.as_deref(),
                input.model.is_none(),
            )?,
        },
    };
    let (config, model) =
        validate_official_config_text(codex_dir, &config, input.model.as_deref())?;
    let auth = parse_auth(input.auth_json.as_deref().unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|detail| detail.auth_json.as_str())
            .unwrap_or("")
    }))?;
    let is_current = existing
        .as_ref()
        .is_some_and(|detail| detail.profile.is_current);
    with_mutation(codex_dir, |mutation| {
        if activate {
            let previous_id = selected_profile_id(codex_dir)?;
            if live_config_is_official(codex_dir)? {
                mutation.snapshot(&previous_id, || {
                    capture_live_official_config_before_provider_switch(codex_dir).map(|_| ())
                })?;
            }
            mutation.provider = persist_detected_live_custom_provider(codex_dir)?;
        }
        let backup_id = if is_current || activate {
            let (backup_id, live) = apply_official_config_with_snapshot_locked(
                codex_dir,
                Some(&config),
                model.as_deref(),
                model.is_none(),
                auth.clone()
                    .map(LiveAuthAction::Replace)
                    .unwrap_or(LiveAuthAction::Remove),
                "save-official-profile",
                live_before,
            )?;
            mutation.live = Some(live);
            backup_id
        } else {
            None
        };
        mutation.snapshot(&id, || {
            write_official_profile_snapshot(codex_dir, &id, Some(config), model, auth)
        })?;
        mutation.update_store(|conn| {
            store_profile(conn, codex_dir, &id, &name)?;
            if activate {
                select_profile(conn, codex_dir, &id)?;
            }
            Ok(())
        })?;
        let profile = detail(codex_dir, &id)?.profile;
        Ok(OfficialProfileActionResult {
            action: action(codex_dir, format!("已保存官方配置 {name}"), backup_id)?,
            profile,
        })
    })
}

pub(crate) fn save_official_profile_inner(
    input: OfficialProfileInput,
) -> Result<OfficialProfileActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    save_locked(&codex_dir, input, false)
}

pub(crate) fn duplicate_official_profile_inner(
    config_dir: Option<String>,
    profile_id: String,
    provider_name: Option<String>,
) -> Result<OfficialProfileActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    let source = detail(&codex_dir, &profile_id)?;
    let base_name =
        provider_name.unwrap_or_else(|| format!("{} 副本", source.profile.provider_name));
    let saved = read_store(&open_store()?, &codex_dir)?;
    let default_name = profile_name(&codex_dir, DEFAULT_OFFICIAL_PROFILE_ID)?;
    let mut copy_name = bounded_copy_name(&base_name, None);
    let mut suffix = 2;
    while saved
        .profiles
        .iter()
        .any(|profile| profile.provider_name == copy_name)
        || default_name == copy_name
    {
        copy_name = bounded_copy_name(&base_name, Some(suffix));
        suffix += 1;
    }
    save_locked(
        &codex_dir,
        OfficialProfileInput {
            config_dir: None,
            id: None,
            provider_name: copy_name,
            model: source.profile.model,
            config_text: Some(source.config_text),
            auth_json: Some(source.auth_json),
        },
        false,
    )
}

pub(crate) fn reset_default_official_profile_inner(
    input: super::live::OfficialConfigInput,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir)?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let config = match input.config_text.filter(|text| !text.trim().is_empty()) {
        Some(config) => config,
        None => {
            build_official_config_text(&codex_dir, input.model.as_deref(), input.model.is_none())?
        }
    };
    let mut result = save_locked(
        &codex_dir,
        OfficialProfileInput {
            config_dir: None,
            id: Some(DEFAULT_OFFICIAL_PROFILE_ID.to_string()),
            provider_name: profile_name(&codex_dir, DEFAULT_OFFICIAL_PROFILE_ID)?,
            model: input.model,
            config_text: Some(config),
            auth_json: Some(String::new()),
        },
        true,
    )?
    .action;
    result.message = "已新建默认官方配置，请在 Codex 中重新登录".to_string();
    Ok(result)
}

pub(crate) fn switch_official_profile_inner(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<ActionResult> {
    switch_official_profile_with_before_apply(config_dir, profile_id, |_| Ok(()))
}

fn switch_official_profile_with_before_apply(
    config_dir: Option<String>,
    profile_id: String,
    before_apply: impl FnOnce(&Path) -> Result<()>,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let live_before = read_live_file_snapshot(&codex_dir)?;
    let target = detail(&codex_dir, &profile_id)?;
    let (config, model) = validate_official_config_text(
        &codex_dir,
        &target.config_text,
        target.profile.model.as_deref(),
    )?;
    let config = official_activation_config_text(&codex_dir, &config)?;
    let auth = parse_auth(&target.auth_json)?;
    with_mutation(&codex_dir, |mutation| {
        let previous_id = selected_profile_id(&codex_dir)?;
        if live_config_is_official(&codex_dir)? {
            mutation.snapshot(&previous_id, || {
                capture_live_official_config_before_provider_switch(&codex_dir).map(|_| ())
            })?;
        }
        mutation.provider = persist_detected_live_custom_provider(&codex_dir)?;
        before_apply(&codex_dir)?;
        let (backup_id, live) = apply_official_config_with_snapshot_locked(
            &codex_dir,
            Some(&config),
            model.as_deref(),
            model.is_none(),
            auth.clone()
                .map(LiveAuthAction::Replace)
                .unwrap_or(LiveAuthAction::Remove),
            "switch-official-profile",
            live_before,
        )?;
        mutation.live = Some(live);
        mutation.snapshot(&profile_id, || {
            write_official_profile_snapshot(&codex_dir, &profile_id, Some(config), model, auth)
        })?;
        mutation.update_store(|conn| select_profile(conn, &codex_dir, &profile_id))?;
        let message = if target.profile.has_auth {
            format!("已切换到 {}", target.profile.provider_name)
        } else {
            format!(
                "已切换到 {}，请在 Codex 中完成登录",
                target.profile.provider_name
            )
        };
        action(&codex_dir, message, backup_id)
    })
}

pub(crate) fn delete_official_profile_inner(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<()> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    if profile_id == DEFAULT_OFFICIAL_PROFILE_ID {
        return Err(CodexxError::Config("不能删除默认官方配置".to_string()));
    }
    profile_name(&codex_dir, &profile_id)?;
    if live_config_is_official(&codex_dir)? && selected_profile_id(&codex_dir)? == profile_id {
        return Err(CodexxError::Config(
            "请先切换到其他配置，再删除当前官方配置".to_string(),
        ));
    }
    with_mutation(&codex_dir, |mutation| {
        let path = official_snapshot_path_for_profile(&codex_dir, &profile_id)?;
        let before = read_file_snapshot(&path)?;
        mutation.snapshot(&profile_id, || {
            remove_file_if_unchanged(&path, before.as_deref())
        })?;
        mutation.update_store(|conn| {
            conn.execute(
                "DELETE FROM official_profiles WHERE codex_dir=?1 AND id=?2",
                params![normalized_path_scope(&codex_dir), profile_id],
            )
            .map_err(database_error)?;
            if selected_profile_id(&codex_dir)? == profile_id {
                select_profile(conn, &codex_dir, DEFAULT_OFFICIAL_PROFILE_ID)?;
            }
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_io::{write_json, write_text};
    use crate::providers::{switch_provider_inner, ProviderInput};
    use crate::{auth_path, config_path};
    use serde_json::json;
    use std::fs;

    fn test_home(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codex-x-official-profiles-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        ensure_directory(&dir).unwrap();
        dir
    }

    fn config(label: &str) -> String {
        format!("# profile {label}\nmodel_provider = \"openai\"\nmodel = \"model-{label}\"\napproval_policy = \"never\"\n\n[features]\nweb_search = true\n")
    }

    fn auth(label: &str) -> Value {
        json!({"auth_mode":"chatgpt","tokens":{"account_id":format!("account-{label}"),"access_token":format!("access-{label}"),"refresh_token":format!("refresh-{label}")}})
    }

    fn display_jwt(claims: Value) -> String {
        format!(
            "e30.{}.fixture-signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn official_profile_email_decodes_only_bounded_display_claims() {
        let mut authentication = auth("email");
        authentication["tokens"]["id_token"] =
            json!(display_jwt(json!({"email":"id@example.test"})));
        authentication["tokens"]["access_token"] = json!(display_jwt(
            json!({"https://api.openai.com/profile":{"email":"access@example.test"}})
        ));
        assert_eq!(
            auth_display_email(&authentication).as_deref(),
            Some("id@example.test")
        );
        authentication["email"] = json!(" direct@example.test ");
        assert_eq!(
            auth_display_email(&authentication).as_deref(),
            Some("direct@example.test")
        );
        authentication["email"] = json!("\n");
        authentication["tokens"]["id_token"] = json!("invalid-token");
        assert_eq!(
            auth_display_email(&authentication).as_deref(),
            Some("access@example.test")
        );
        authentication["tokens"]["access_token"] = json!(format!("e30.{}.sig", "A".repeat(32769)));
        assert!(auth_display_email(&authentication).is_none());
        authentication["email"] = json!("unsafe\nemail@example.test");
        assert!(auth_display_email(&authentication).is_none());
    }

    #[test]
    fn official_profile_plan_type_uses_chatgpt_metadata_with_validated_fallbacks() {
        let mut authentication = auth("plan");
        authentication["tokens"]["id_token"] = json!(display_jwt(json!({
            "email": "id@example.test",
            "https://api.openai.com/auth": {"chatgpt_plan_type": "pro"}
        })));
        authentication["tokens"]["access_token"] = json!(display_jwt(json!({
            "https://api.openai.com/auth": {"chatgpt_plan_type": "team"}
        })));
        let metadata = auth_display_metadata(&authentication);
        assert_eq!(metadata.email.as_deref(), Some("id@example.test"));
        assert_eq!(metadata.plan_type.as_deref(), Some("pro"));

        authentication["plan_type"] = json!(" pro_lite ");
        authentication["chatgpt_plan_type"] = json!("business");
        assert_eq!(
            auth_display_metadata(&authentication).plan_type.as_deref(),
            Some("pro_lite")
        );
        authentication["plan_type"] = Value::Null;
        assert_eq!(
            auth_display_metadata(&authentication).plan_type.as_deref(),
            Some("business")
        );
        authentication["chatgpt_plan_type"] = Value::Null;
        authentication["tokens"]["id_token"] = json!("malformed-token");
        assert_eq!(
            auth_display_metadata(&authentication).plan_type.as_deref(),
            Some("team")
        );

        authentication["plan_type"] = json!("Future_Plan-2");
        assert_eq!(
            auth_display_metadata(&authentication).plan_type.as_deref(),
            Some("Future_Plan-2")
        );
        authentication["auth_mode"] = json!("apikey");
        authentication["OPENAI_API_KEY"] = json!("synthetic-api-key");
        assert!(auth_display_metadata(&authentication).plan_type.is_none());
        assert!(auth_display_metadata(&auth("missing-plan"))
            .plan_type
            .is_none());
    }

    #[test]
    fn official_profile_plan_type_rejects_unsafe_and_oversized_metadata() {
        for value in [
            json!(""),
            json!(" \t"),
            json!("\npro"),
            json!("pro\n"),
            json!("pro lite"),
            json!("pro\u{202e}lite"),
            json!("<script>"),
            json!("p".repeat(65)),
            json!(false),
            json!(["pro"]),
            json!({"plan":"pro"}),
        ] {
            let mut authentication = auth("invalid-plan");
            authentication["plan_type"] = value.clone();
            authentication["tokens"]["id_token"] = json!(display_jwt(json!({
                "https://api.openai.com/auth": {"chatgpt_plan_type": value}
            })));
            assert!(auth_display_metadata(&authentication).plan_type.is_none());
        }
        let valid_token =
            display_jwt(json!({"https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}));
        for token in [
            format!("e30.{}.sig", "A".repeat(32769)),
            format!("{valid_token}.extra"),
            valid_token.replacen("e30.", ".", 1),
            valid_token.replace("fixture-signature", ""),
            display_jwt(json!(["pro"])),
            display_jwt(
                json!({"https://api.openai.com/auth":{"chatgpt_plan_type":"pro"},"oversized":"x".repeat(32768)}),
            ),
        ] {
            let mut authentication = auth("invalid-jwt");
            authentication["tokens"]["id_token"] = json!(token);
            assert!(auth_display_metadata(&authentication).plan_type.is_none());
        }
    }

    #[test]
    fn official_quota_credentials_follow_owned_profiles_live_refresh_and_account_switches() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("quota-profile-isolation");
        let mut first_auth = auth("a");
        first_auth["tokens"]["id_token"] = json!(display_jwt(
            json!({"email":"first@example.test", "account_id":"untrusted-claim", "https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}})
        ));
        write_json(&auth_path(&dir), &first_auth).unwrap();
        let first = official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        assert_eq!(first.access_token, "access-a");
        assert_eq!(first.account_id.as_deref(), Some("account-a"));
        assert_eq!(first.email.as_deref(), Some("first@example.test"));
        let mut second_auth = auth("b");
        second_auth["email"] = json!("second@example.test");
        second_auth["tokens"]["id_token"] = json!(display_jwt(
            json!({"https://api.openai.com/auth":{"chatgpt_plan_type":"team"}})
        ));
        let second = save(
            &dir,
            None,
            "Second account",
            Some(config("b")),
            Some(second_auth.to_string()),
        );
        assert_eq!(second.profile.email.as_deref(), Some("second@example.test"));
        assert_eq!(second.profile.plan_type.as_deref(), Some("team"));
        assert!(second.profile.can_query_quota);
        let inactive = official_profile_quota_credentials(&dir, &second.profile.id).unwrap();
        assert_eq!(inactive.access_token, "access-b");
        assert_eq!(inactive.account_id.as_deref(), Some("account-b"));
        switch(&dir, &second.profile.id);
        let mut refreshed = auth("b-refreshed");
        refreshed["email"] = json!("refreshed@example.test");
        refreshed["chatgpt_plan_type"] = json!("prolite");
        write_json(&auth_path(&dir), &refreshed).unwrap();
        assert_eq!(
            official_profile_quota_credentials(&dir, &second.profile.id)
                .unwrap()
                .access_token,
            "access-b-refreshed"
        );
        assert_eq!(
            official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
                .unwrap()
                .access_token,
            "access-a"
        );
        let summaries = list_official_profiles_inner(scope(&dir)).unwrap();
        assert_eq!(
            summaries
                .iter()
                .find(|profile| profile.is_default)
                .unwrap()
                .plan_type
                .as_deref(),
            Some("pro")
        );
        assert_eq!(
            summaries
                .iter()
                .find(|profile| profile.is_current)
                .unwrap()
                .plan_type
                .as_deref(),
            Some("prolite")
        );
        assert_eq!(
            detail(&dir, &second.profile.id)
                .unwrap()
                .profile
                .plan_type
                .as_deref(),
            Some("prolite")
        );
        assert_eq!(
            summaries
                .iter()
                .find(|profile| profile.is_current)
                .unwrap()
                .email
                .as_deref(),
            Some("refreshed@example.test")
        );
        let serialized = serde_json::to_string(&summaries).unwrap();
        assert!(serialized.contains("\"planType\":\"prolite\""));
        assert!(!serialized.contains("access-b-refreshed"));
        assert!(!serialized.contains("refresh-b-refreshed"));
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(
            detail(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
                .unwrap()
                .profile
                .plan_type
                .as_deref(),
            Some("pro")
        );
        assert_eq!(
            detail(&dir, &second.profile.id)
                .unwrap()
                .profile
                .plan_type
                .as_deref(),
            Some("prolite")
        );
        assert_eq!(
            official_profile_quota_credentials(&dir, &second.profile.id)
                .unwrap()
                .email
                .as_deref(),
            Some("refreshed@example.test")
        );
        assert_eq!(
            official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
                .unwrap()
                .email
                .as_deref(),
            Some("first@example.test")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_quota_credentials_never_recover_history_or_a_proxy_login() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("quota-no-history");
        let mut historical = auth("historical");
        historical["plan_type"] = json!("pro");
        write_json(&auth_path(&dir), &historical).unwrap();
        crate::backups::create_backup(&dir, "quota-ignored-history").unwrap();
        write_text(&config_path(&dir), "model_provider='custom'\nmodel='proxy-model'\n[model_providers.custom]\nname='Proxy'\nbase_url='https://proxy.example.test/v1'\nrequires_openai_auth=true\n").unwrap();
        let mut foreign = auth("foreign-live");
        foreign["email"] = json!("foreign@example.test");
        foreign["plan_type"] = json!("team");
        write_json(&auth_path(&dir), &foreign).unwrap();
        assert!(official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID).is_err());
        let summaries = list_official_profiles_inner(scope(&dir)).unwrap();
        assert!(summaries[0].email.is_none());
        assert!(summaries[0].plan_type.is_none());
        assert!(!summaries[0].can_query_quota);
        assert!(
            !official_snapshot_path_for_profile(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
                .unwrap()
                .exists()
        );
        let named = save(
            &dir,
            None,
            "Missing snapshot",
            Some(config("b")),
            Some(auth("b").to_string()),
        );
        fs::remove_file(official_snapshot_path_for_profile(&dir, &named.profile.id).unwrap())
            .unwrap();
        assert!(official_profile_quota_credentials(&dir, &named.profile.id).is_err());
        assert!(list_official_profiles_inner(scope(&dir))
            .unwrap()
            .iter()
            .find(|profile| profile.id == named.profile.id)
            .unwrap()
            .plan_type
            .is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_quota_credentials_reject_logout_api_keys_and_missing_access_tokens() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("quota-auth-states");
        let mut authentication = auth("signed-in");
        authentication["plan_type"] = json!("pro");
        write_json(&auth_path(&dir), &authentication).unwrap();
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Default",
            None,
            None,
        );
        fs::remove_file(auth_path(&dir)).unwrap();
        let summary = list_official_profiles_inner(scope(&dir)).unwrap().remove(0);
        assert!(
            summary.has_auth,
            "legacy saved-auth availability is retained"
        );
        assert!(
            !summary.has_owned_auth,
            "a logged-out current profile must show the signed-out badge"
        );
        assert!(!summary.can_query_quota);
        assert!(summary.email.is_none());
        assert!(summary.plan_type.is_none());
        assert!(detail(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
            .unwrap()
            .profile
            .plan_type
            .is_none());
        assert!(official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID).is_err());
        write_text(
            &auth_path(&dir),
            "{\"tokens\":\"sensitive-test-marker\",invalid-json}",
        )
        .unwrap();
        let error = official_profile_quota_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID)
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("sensitive-test-marker"));
        write_json(&auth_path(&dir), &auth("a")).unwrap();
        for (name, authentication) in [
            (
                "API key",
                json!({"auth_mode":"apikey", "OPENAI_API_KEY":"sk-test-only"}),
            ),
            (
                "Refresh only",
                json!({"auth_mode":"chatgpt", "tokens":{"refresh_token":"refresh-only"}}),
            ),
        ] {
            let profile = save(
                &dir,
                None,
                name,
                Some(config("b")),
                Some(authentication.to_string()),
            );
            assert!(profile.profile.has_auth);
            assert!(profile.profile.has_owned_auth);
            assert!(!profile.profile.can_query_quota);
            assert!(official_profile_quota_credentials(&dir, &profile.profile.id).is_err());
        }
        let empty = save(
            &dir,
            None,
            "Not signed in",
            Some(config("empty")),
            Some(String::new()),
        );
        assert!(!empty.profile.has_auth);
        assert!(!empty.profile.has_owned_auth);
        assert!(!empty.profile.can_query_quota);
        assert!(empty.profile.email.is_none());
        assert!(empty.profile.plan_type.is_none());
        assert!(official_profile_quota_credentials(&dir, &empty.profile.id).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    fn prepare_home(label: &str) -> PathBuf {
        let dir = test_home(label);
        write_text(&config_path(&dir), &config("a")).unwrap();
        write_json(&auth_path(&dir), &auth("a")).unwrap();
        dir
    }

    fn scope(dir: &Path) -> Option<String> {
        Some(dir.display().to_string())
    }

    fn save(
        dir: &Path,
        id: Option<&str>,
        name: &str,
        configuration: Option<String>,
        authentication: Option<String>,
    ) -> OfficialProfileActionResult {
        save_official_profile_inner(OfficialProfileInput {
            config_dir: scope(dir),
            id: id.map(ToString::to_string),
            provider_name: name.to_string(),
            model: None,
            config_text: configuration,
            auth_json: authentication,
        })
        .unwrap()
    }

    fn switch(dir: &Path, id: &str) -> ActionResult {
        switch_official_profile_inner(scope(dir), id.to_string()).unwrap()
    }

    fn live_auth(dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(auth_path(dir)).unwrap()).unwrap()
    }

    #[test]
    fn official_profiles_default_name_and_copy_preserve_complete_config_and_auth() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("default-copy");
        let before = fs::read(config_path(&dir)).unwrap();
        let snapshot =
            official_snapshot_path_for_profile(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        assert!(!snapshot.exists());
        let profiles = list_official_profiles_inner(scope(&dir)).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, DEFAULT_OFFICIAL_PROFILE_ID);
        assert!(profiles[0].is_current);
        assert!(
            !snapshot.exists(),
            "listing must not materialize authentication snapshots"
        );
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before);

        let renamed = save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Work Account",
            None,
            None,
        );
        assert_eq!(renamed.profile.id, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(renamed.profile.provider_name, "Work Account");
        assert_eq!(live_auth(&dir), auth("a"));
        let before_copy = fs::read(config_path(&dir)).unwrap();
        let copy = duplicate_official_profile_inner(
            scope(&dir),
            DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
            Some("Work Copy".to_string()),
        )
        .unwrap();
        assert_ne!(copy.profile.id, DEFAULT_OFFICIAL_PROFILE_ID);
        let copied = get_official_profile_inner(scope(&dir), copy.profile.id.clone()).unwrap();
        assert_eq!(copied.config_text.trim_end(), config("a").trim_end());
        assert_eq!(
            serde_json::from_str::<Value>(&copied.auth_json).unwrap(),
            auth("a")
        );
        assert_eq!(
            copy.action.state.active_official_profile_id.as_deref(),
            Some(DEFAULT_OFFICIAL_PROFILE_ID)
        );
        assert_eq!(list_official_profiles_inner(scope(&dir)).unwrap().len(), 2);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_copy);
        let second_copy = duplicate_official_profile_inner(
            scope(&dir),
            DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
            Some("Work Copy".to_string()),
        )
        .unwrap();
        assert_eq!(second_copy.profile.provider_name, "Work Copy 2");
        assert_ne!(second_copy.profile.id, copy.profile.id);
        assert_eq!(
            second_copy
                .action
                .state
                .active_official_profile_id
                .as_deref(),
            Some(DEFAULT_OFFICIAL_PROFILE_ID)
        );
        assert_eq!(live_auth(&dir), auth("a"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profile_copies_bound_unicode_names_and_numbered_suffixes() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("long-copy-names");
        let original_name = "账🦀".repeat(50);
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            &original_name,
            None,
            None,
        );
        let before_config = fs::read(config_path(&dir)).unwrap();
        let before_auth = fs::read(auth_path(&dir)).unwrap();
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();

        for ordinal in 1..=12 {
            let copy = duplicate_official_profile_inner(
                scope(&dir),
                DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
                None,
            )
            .unwrap();
            let suffix = if ordinal == 1 {
                " 副本".to_string()
            } else {
                format!(" 副本 {ordinal}")
            };
            assert_eq!(copy.profile.provider_name.chars().count(), 100);
            assert!(copy.profile.provider_name.ends_with(&suffix));
            assert!(names.insert(copy.profile.provider_name));
            assert!(ids.insert(copy.profile.id));
            assert_eq!(
                copy.action.state.active_official_profile_id.as_deref(),
                Some(DEFAULT_OFFICIAL_PROFILE_ID)
            );
        }
        let english = duplicate_official_profile_inner(
            scope(&dir),
            DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
            Some(format!("{original_name} Copy")),
        )
        .unwrap();
        assert_eq!(english.profile.provider_name.chars().count(), 100);
        assert!(english.profile.provider_name.ends_with(" Copy"));
        let second_english = duplicate_official_profile_inner(
            scope(&dir),
            DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
            Some(format!("{original_name} Copy")),
        )
        .unwrap();
        assert_eq!(second_english.profile.provider_name.chars().count(), 100);
        assert!(second_english.profile.provider_name.ends_with(" Copy 2"));
        assert_eq!(
            profile_name(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap(),
            original_name
        );
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), before_auth);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_switch_accounts_and_preserve_refreshed_tokens_through_a_proxy() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("account-roundtrip");
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Account A",
            None,
            None,
        );
        let b = save(
            &dir,
            None,
            "Account B",
            Some(config("b")),
            Some(auth("b").to_string()),
        );
        assert_eq!(
            live_auth(&dir),
            auth("a"),
            "saving inactive B must not replace A"
        );
        let switched = switch(&dir, &b.profile.id);
        assert_eq!(
            switched.state.active_official_profile_id.as_deref(),
            Some(b.profile.id.as_str())
        );
        assert_eq!(live_auth(&dir), auth("b"));
        let configured = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(configured["model"].as_str(), Some("model-b"));
        assert!(document_is_official(&configured));
        assert_eq!(configured["approval_policy"].as_str(), Some("never"));
        assert_eq!(configured["features"]["web_search"].as_bool(), Some(true));

        let refreshed = auth("b-refreshed");
        write_json(&auth_path(&dir), &refreshed).unwrap();
        let b_config = format!("{}\n[desktop]\nnotify = true\n", config("b"));
        write_text(&config_path(&dir), &b_config).unwrap();
        // Rename with a null auth field after Codex refreshed the credential.
        save(&dir, Some(&b.profile.id), "Account B renamed", None, None);
        assert_eq!(live_auth(&dir), refreshed);
        let proxy = switch_provider_inner(ProviderInput {
            config_dir: scope(&dir),
            provider_id: Some("profile-test-proxy".to_string()),
            provider_name: "Profile Proxy".to_string(),
            base_url: "https://profile-proxy.example.com/v1".to_string(),
            model: "proxy-model".to_string(),
            api_key: Some("sk-proxy".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(true),
        })
        .unwrap();
        assert!(proxy.state.active_official_profile_id.is_none());
        write_json(&auth_path(&dir), &auth("unrelated-proxy-login")).unwrap();
        let b_saved = get_official_profile_inner(scope(&dir), b.profile.id.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&b_saved.auth_json).unwrap(),
            refreshed
        );
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(live_auth(&dir), auth("a"));
        // Refresh default while named profiles exist, then leave it again.
        write_json(&auth_path(&dir), &auth("a-refreshed")).unwrap();
        switch(&dir, &b.profile.id);
        assert_eq!(live_auth(&dir), refreshed);
        let configured = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(configured["model"].as_str(), Some("model-b"));
        assert!(document_is_official(&configured));
        assert_eq!(configured["desktop"]["notify"].as_bool(), Some(true));
        assert_eq!(configured["features"]["web_search"].as_bool(), Some(true));
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(live_auth(&dir), auth("a-refreshed"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_empty_login_belongs_to_its_profile_and_deletion_is_guarded() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("empty-login");
        let blank = save(
            &dir,
            None,
            "New Login",
            Some(config("blank")),
            Some(String::new()),
        );
        assert!(!blank.profile.has_auth);
        switch(&dir, &blank.profile.id);
        assert!(
            !auth_path(&dir).exists(),
            "blank login must never inherit default OAuth"
        );
        assert!(delete_official_profile_inner(scope(&dir), blank.profile.id.clone()).is_err());
        assert!(delete_official_profile_inner(
            scope(&dir),
            DEFAULT_OFFICIAL_PROFILE_ID.to_string()
        )
        .is_err());
        write_json(&auth_path(&dir), &auth("new-login")).unwrap();
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(live_auth(&dir), auth("a"));
        switch(&dir, &blank.profile.id);
        assert_eq!(live_auth(&dir), auth("new-login"));
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        let path = official_snapshot_path_for_profile(&dir, &blank.profile.id).unwrap();
        delete_official_profile_inner(scope(&dir), blank.profile.id).unwrap();
        assert!(!path.exists());
        assert_eq!(list_official_profiles_inner(scope(&dir)).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_reject_invalid_auth_and_third_party_config_without_writes() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("validation");
        let original_config = fs::read(config_path(&dir)).unwrap();
        let original_auth = fs::read(auth_path(&dir)).unwrap();
        for invalid in [
            "{",
            "{}",
            r#"{"other":"token"}"#,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":""}}"#,
            r#"{"OPENAI_API_KEY":"sk-proxy","base_url":"https://proxy.example.com"}"#,
        ] {
            assert!(save_official_profile_inner(OfficialProfileInput {
                config_dir: scope(&dir),
                id: None,
                provider_name: "Invalid".to_string(),
                model: None,
                config_text: Some(config("invalid")),
                auth_json: Some(invalid.to_string()),
            })
            .is_err());
        }
        assert!(save_official_profile_inner(OfficialProfileInput {
            config_dir: scope(&dir), id: None, provider_name: "Proxy".to_string(), model: None,
            config_text: Some("model_provider='custom'\nmodel='proxy'\n[model_providers.custom]\nname='Proxy'\nbase_url='https://proxy.example.com/v1'\nrequires_openai_auth=true\n".to_string()),
            auth_json: Some(auth("b").to_string()),
        }).is_err());
        assert_eq!(list_official_profiles_inner(scope(&dir)).unwrap().len(), 1);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), original_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), original_auth);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_switch_rolls_back_files_snapshots_and_selection_on_database_failure() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("switch-rollback");
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Account A",
            None,
            None,
        );
        let b = save(
            &dir,
            None,
            "Account B",
            Some(config("b")),
            Some(auth("b").to_string()),
        );
        let config_before = fs::read(config_path(&dir)).unwrap();
        let auth_before = fs::read(auth_path(&dir)).unwrap();
        let a_path = official_snapshot_path_for_profile(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        let b_path = official_snapshot_path_for_profile(&dir, &b.profile.id).unwrap();
        let a_before = fs::read(&a_path).unwrap();
        let b_before = fs::read(&b_path).unwrap();
        let conn = open_store().unwrap();
        let scope_sql = normalized_path_scope(&dir).replace('\'', "''");
        conn.execute_batch(&format!("CREATE TRIGGER reject_profile_selection BEFORE INSERT ON official_profile_selections WHEN NEW.codex_dir = '{scope_sql}' BEGIN SELECT RAISE(ABORT, 'injected selection failure'); END;")).unwrap();
        let result = switch_official_profile_inner(scope(&dir), b.profile.id);
        conn.execute_batch("DROP TRIGGER reject_profile_selection")
            .unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("injected selection failure"));
        assert_eq!(fs::read(config_path(&dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth_before);
        assert_eq!(fs::read(&a_path).unwrap(), a_before);
        assert_eq!(fs::read(&b_path).unwrap(), b_before);
        assert_eq!(
            selected_profile_id(&dir).unwrap(),
            DEFAULT_OFFICIAL_PROFILE_ID
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_default_reset_does_not_overwrite_a_named_account() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("default-reset");
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Default Renamed",
            None,
            None,
        );
        let b = save(
            &dir,
            None,
            "Account B",
            Some(config("b")),
            Some(auth("b").to_string()),
        );
        switch(&dir, &b.profile.id);
        write_json(&auth_path(&dir), &auth("b-latest")).unwrap();
        let reset = reset_default_official_profile_inner(super::super::live::OfficialConfigInput {
            config_dir: scope(&dir),
            model: None,
            auth_json: None,
            config_text: Some(config("default-reset")),
        })
        .unwrap();
        assert_eq!(
            reset.state.active_official_profile_id.as_deref(),
            Some(DEFAULT_OFFICIAL_PROFILE_ID)
        );
        assert!(!auth_path(&dir).exists());
        assert_eq!(
            profile_name(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap(),
            "Default Renamed"
        );
        switch(&dir, &b.profile.id);
        assert_eq!(live_auth(&dir), auth("b-latest"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_api_key_login_with_optional_null_fields_remains_independent() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("api-key");
        let api_auth = json!({"auth_mode":"apikey","OPENAI_API_KEY":"sk-official","tokens":null,"last_refresh":null});
        let api = save(
            &dir,
            None,
            "Official API Key",
            Some(config("api")),
            Some(api_auth.to_string()),
        );
        switch(&dir, &api.profile.id);
        assert_eq!(live_auth(&dir), api_auth);
        switch(&dir, DEFAULT_OFFICIAL_PROFILE_ID);
        assert_eq!(live_auth(&dir), auth("a"));
        switch(&dir, &api.profile.id);
        assert_eq!(live_auth(&dir), api_auth);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_missing_snapshot_does_not_hide_other_profiles_or_prevent_deletion() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("missing-snapshot");
        let broken = save(
            &dir,
            None,
            "Broken Profile",
            Some(config("broken")),
            Some(auth("broken").to_string()),
        );
        let path = official_snapshot_path_for_profile(&dir, &broken.profile.id).unwrap();
        fs::remove_file(&path).unwrap();
        let listed = list_official_profiles_inner(scope(&dir)).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|profile| profile.id == broken.profile.id && !profile.has_auth));
        assert!(switch_official_profile_inner(scope(&dir), broken.profile.id.clone()).is_err());
        assert_eq!(live_auth(&dir), auth("a"));
        delete_official_profile_inner(scope(&dir), broken.profile.id).unwrap();
        assert_eq!(list_official_profiles_inner(scope(&dir)).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_concurrent_token_refresh_cancels_switch_without_losing_the_refresh() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("refresh-race");
        save(
            &dir,
            Some(DEFAULT_OFFICIAL_PROFILE_ID),
            "Account A",
            None,
            None,
        );
        let b = save(
            &dir,
            None,
            "Account B",
            Some(config("b")),
            Some(auth("b").to_string()),
        );
        let config_before = fs::read(config_path(&dir)).unwrap();
        let error =
            switch_official_profile_with_before_apply(scope(&dir), b.profile.id, |codex_dir| {
                write_json(&auth_path(codex_dir), &auth("concurrent-refresh"))
            })
            .unwrap_err();
        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(live_auth(&dir), auth("concurrent-refresh"));
        assert_eq!(fs::read(config_path(&dir)).unwrap(), config_before);
        assert_eq!(
            selected_profile_id(&dir).unwrap(),
            DEFAULT_OFFICIAL_PROFILE_ID
        );
        let current =
            get_official_profile_inner(scope(&dir), DEFAULT_OFFICIAL_PROFILE_ID.to_string())
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&current.auth_json).unwrap(),
            auth("concurrent-refresh")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_profiles_startup_summary_skips_history_but_explicit_editor_can_recover_it() {
        let _guard = crate::app_db::test_db_guard();
        let dir = prepare_home("summary-no-history");
        crate::backups::create_backup(&dir, "official-summary-history").unwrap();
        write_text(&config_path(&dir), "model_provider='custom'\nmodel='proxy-model'\n[model_providers.custom]\nname='Proxy'\nbase_url='https://summary-proxy.example.com/v1'\nrequires_openai_auth=true\n").unwrap();
        write_json(&auth_path(&dir), &json!({"OPENAI_API_KEY":"sk-proxy"})).unwrap();
        let snapshot =
            official_snapshot_path_for_profile(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        assert!(!snapshot.exists());
        let summaries = list_official_profiles_inner(scope(&dir)).unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(
            !summaries[0].has_auth,
            "startup summary must not discover authentication from historical backups"
        );
        assert!(summaries[0].model.is_none());
        assert!(!snapshot.exists());

        let editor =
            get_official_profile_inner(scope(&dir), DEFAULT_OFFICIAL_PROFILE_ID.to_string())
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&editor.auth_json).unwrap(),
            auth("a")
        );
        assert!(
            !snapshot.exists(),
            "explicit editor recovery is still read-only"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let history = crate::backups::action_backup_root(&dir).unwrap();
            fs::set_permissions(&history, fs::Permissions::from_mode(0o000)).unwrap();
            let result = list_official_profiles_inner(scope(&dir));
            fs::set_permissions(&history, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(!result.expect("summary must not traverse unreadable history")[0].has_auth);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
