use super::{clear_provider_selections_on_connection, open_store as open_db};
use crate::error::{CodexxError, Result};
use crate::{now_rfc3339, sanitize_id};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use toml_edit::{value, DocumentMut};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedProvider {
    pub(crate) id: String,
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
    pub(crate) toml_config: Option<String>,
    pub(crate) wire_api: String,
    pub(crate) requires_openai_auth: bool,
    #[serde(default)]
    pub(crate) model_mappings: Vec<super::model_catalog::ProviderModelMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredProvider {
    provider: SavedProvider,
    created_at: String,
    updated_at: String,
    source: String,
    source_id: Option<String>,
}

pub(crate) struct ProviderStoreRollback {
    before: Vec<StoredProvider>,
    after: Vec<StoredProvider>,
}

const MANUAL_PROVIDER_SOURCE: &str = "manual";
pub(crate) const CCSWITCH_PROVIDER_SOURCE: &str = "cc-switch";
const CCSWITCH_LOCAL_PROVIDER_SOURCE: &str = "cc-switch-local";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ProviderIdentity {
    Credential([u8; 32]),
    Unauthenticated { base_url: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderProfileIdentity {
    // Matching helper only; manually saved rows are always addressed by ID.
    provider: ProviderIdentity,
    model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderUpsertMode {
    Manual,
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderUpsertKind {
    Added,
    Updated,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderUpsertResult {
    pub(crate) provider: SavedProvider,
    pub(crate) kind: ProviderUpsertKind,
}

pub(crate) fn canonical_provider_base_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(parsed) = ureq::get(trimmed).request_url() {
        let url = parsed.as_url();
        if let Some(host) = url.host_str() {
            let mut canonical = format!("{}://", url.scheme().to_ascii_lowercase());
            if !url.username().is_empty() {
                canonical.push_str(url.username());
                if let Some(password) = url.password() {
                    canonical.push(':');
                    canonical.push_str(password);
                }
                canonical.push('@');
            }
            if host.contains(':') && !host.starts_with('[') {
                canonical.push('[');
                canonical.push_str(&host.to_ascii_lowercase());
                canonical.push(']');
            } else {
                canonical.push_str(&host.to_ascii_lowercase());
            }
            if let Some(port) = url.port() {
                canonical.push(':');
                canonical.push_str(&port.to_string());
            }
            let path = url.path().trim_end_matches('/');
            if !path.is_empty() {
                canonical.push_str(path);
            }
            if let Some(query) = url.query() {
                canonical.push('?');
                canonical.push_str(query);
            }
            return canonical;
        }
    }

    trimmed.trim_end_matches('/').to_string()
}

fn normalized_provider_name(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn is_placeholder_provider(provider_name: &str, base_url: &str) -> bool {
    normalized_provider_name(provider_name) == "your-provider"
        && canonical_provider_base_url(base_url) == "https://example.com/v1"
}

fn effective_provider_api_key(provider: &SavedProvider) -> Option<String> {
    provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            let text = provider.toml_config.as_deref()?;
            let doc = text.parse::<DocumentMut>().ok()?;
            let provider_id = doc
                .get("model_provider")
                .and_then(|item| item.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            experimental_bearer_token_from_doc(&doc, provider_id)
        })
}

pub(crate) fn provider_identity(provider: &SavedProvider) -> Option<ProviderIdentity> {
    use sha2::{Digest, Sha256};

    let base_url = canonical_provider_base_url(&provider.base_url);
    if base_url.is_empty() {
        return None;
    }
    if let Some(api_key) = effective_provider_api_key(provider) {
        // Hash the complete endpoint/credential tuple so neither the key nor a
        // reusable key-only fingerprint is persisted, logged, or sent to the UI.
        let mut hasher = Sha256::new();
        hasher.update(b"codex-x/provider-identity/v1\0");
        hasher.update(base_url.as_bytes());
        hasher.update(b"\0");
        hasher.update(api_key.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        return Some(ProviderIdentity::Credential(digest));
    }

    let name = normalized_provider_name(&provider.provider_name);
    (!name.is_empty()).then_some(ProviderIdentity::Unauthenticated { base_url, name })
}

fn provider_profile_identity(provider: &SavedProvider) -> Option<ProviderProfileIdentity> {
    provider_identity(provider).map(|identity| ProviderProfileIdentity {
        provider: identity,
        model: provider.model.trim().to_string(),
    })
}

pub(crate) fn provider_template_from_document(
    doc: &DocumentMut,
    provider_id: &str,
    model: &str,
) -> Result<String> {
    let provider_exists = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(provider_id))
        .and_then(|item| item.as_table())
        .is_some();
    if !provider_exists {
        return Err(CodexxError::Config(format!(
            "供应商 TOML 缺少 [model_providers.{provider_id}]"
        )));
    }

    let mut template = doc.clone();
    template["model_provider"] = value(provider_id);
    template["model"] = value(model);
    strip_provider_bearer_tokens(&mut template);
    Ok(template.to_string().trim_end().to_string())
}

pub(crate) fn strip_provider_bearer_tokens(doc: &mut DocumentMut) {
    doc.as_table_mut().remove("experimental_bearer_token");
    let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    else {
        return;
    };
    for (_, item) in providers.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            table.remove("experimental_bearer_token");
        }
    }
}

fn same_provider_endpoint(left: &SavedProvider, right: &SavedProvider) -> bool {
    let left = canonical_provider_base_url(&left.base_url);
    !left.is_empty() && left == canonical_provider_base_url(&right.base_url)
}

fn same_provider_endpoint_and_name(left: &SavedProvider, right: &SavedProvider) -> bool {
    same_provider_endpoint(left, right)
        && normalized_provider_name(&left.provider_name)
            == normalized_provider_name(&right.provider_name)
}

fn same_provider_profile_fallback(left: &SavedProvider, right: &SavedProvider) -> bool {
    same_provider_endpoint_and_name(left, right) && left.model.trim() == right.model.trim()
}

fn compatible_provider_match(left: &SavedProvider, right: &SavedProvider) -> bool {
    if !same_provider_profile_fallback(left, right) {
        return false;
    }
    match (
        effective_provider_api_key(left),
        effective_provider_api_key(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

pub(crate) fn matching_saved_provider_ids_for_live(
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Vec<String> {
    let candidates = providers.iter().collect::<Vec<_>>();
    if let Some(identity) = provider_profile_identity(live) {
        let exact = candidates
            .iter()
            .copied()
            .filter(|candidate| provider_profile_identity(candidate).as_ref() == Some(&identity))
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            return exact
                .into_iter()
                .map(|candidate| candidate.id.clone())
                .collect();
        }
    }

    let compatible = candidates
        .iter()
        .copied()
        .filter(|candidate| compatible_provider_match(live, candidate))
        .collect::<Vec<_>>();
    if !compatible.is_empty() {
        return compatible
            .into_iter()
            .map(|candidate| candidate.id.clone())
            .collect();
    }

    if !is_historical_custom_provider_id(&live.id) {
        return Vec::new();
    }
    candidates
        .into_iter()
        .filter(|candidate| same_provider_profile_fallback(live, candidate))
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn same_live_provider_route(live: &SavedProvider, saved: &SavedProvider) -> bool {
    if !same_provider_endpoint(live, saved) {
        return false;
    }
    match (
        effective_provider_api_key(live),
        effective_provider_api_key(saved),
    ) {
        (Some(live_key), Some(saved_key)) => live_key == saved_key,
        // Missing live authentication is tolerated for legacy providers, but
        // without credentials a changed name is not enough evidence to adopt it.
        _ => {
            normalized_provider_name(&live.provider_name)
                == normalized_provider_name(&saved.provider_name)
        }
    }
}

/// A saved record keeps its identity when Codex changes the runtime model or
/// display name. Prefer the remembered record only while its route/credential
/// still matches; otherwise fall back to the existing profile matching rules.
/// This lookup never changes a selection or writes a provider.
pub(crate) fn matching_saved_provider_ids_for_live_on_connection(
    conn: &Connection,
    codex_dir: &std::path::Path,
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Result<Vec<String>> {
    if let Some(selected) = super::selection::selected_provider_id_on_connection(conn, codex_dir)? {
        if providers
            .iter()
            .any(|provider| provider.id == selected && same_live_provider_route(live, provider))
        {
            return Ok(vec![selected]);
        }
    }
    let matches = matching_saved_provider_ids_for_live(live, providers)
        .into_iter()
        .filter(|id| {
            providers
                .iter()
                .any(|provider| &provider.id == id && same_live_provider_route(live, provider))
        })
        .collect::<Vec<_>>();
    if !matches.is_empty() {
        return Ok(matches);
    }
    // Several same-API records remain ambiguous without a remembered ID. Return
    // all candidates so callers can leave them alone rather than create a copy.
    Ok(providers
        .iter()
        .filter(|provider| same_live_provider_route(live, provider))
        .map(|provider| provider.id.clone())
        .collect())
}

#[cfg(test)]
fn unique_saved_provider_id_for_live(
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Option<String> {
    let matches = matching_saved_provider_ids_for_live(live, providers);
    (matches.len() == 1).then(|| matches[0].clone())
}

fn saved_provider_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedProvider> {
    Ok(SavedProvider {
        id: row.get(0)?,
        provider_name: row.get(1)?,
        base_url: row.get(2)?,
        model: row.get(3)?,
        api_key: row.get(4)?,
        toml_config: row.get(5)?,
        wire_api: row.get(6)?,
        requires_openai_auth: row.get::<_, i64>(7)? != 0,
        model_mappings: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

fn stored_providers_on_connection(conn: &Connection) -> Result<Vec<StoredProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth, model_mappings_json, created_at, updated_at, source, source_id
             FROM providers
             ORDER BY created_at ASC, rowid ASC",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StoredProvider {
                provider: saved_provider_from_row(row)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
                source: row.get(11)?,
                source_id: row.get(12)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        let mut stored = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        normalize_stored_provider_from_toml(&mut stored.provider);
        providers.push(stored);
    }
    Ok(providers)
}

pub(crate) fn list_saved_providers_on_connection(conn: &Connection) -> Result<Vec<SavedProvider>> {
    Ok(stored_providers_on_connection(conn)?
        .into_iter()
        .map(|stored| stored.provider)
        .collect())
}

pub(crate) fn list_saved_providers_inner() -> Result<Vec<SavedProvider>> {
    let conn = open_db()?;
    list_saved_providers_on_connection(&conn)
}

pub(crate) fn provider_by_id_on_connection(
    conn: &Connection,
    id: &str,
) -> Result<Option<SavedProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth, model_mappings_json
             FROM providers WHERE id = ?1 LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let provider = stmt
        .query_row([id], saved_provider_from_row)
        .map(Some)
        .or_else(|error| {
            if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(provider.map(|mut provider| {
        normalize_stored_provider_from_toml(&mut provider);
        provider
    }))
}

fn write_provider_with_origin(
    conn: &Connection,
    provider: &SavedProvider,
    origin: Option<(&str, &str)>,
) -> Result<()> {
    let now = now_rfc3339();
    let model_mappings_json = serde_json::to_string(&provider.model_mappings)
        .map_err(|error| CodexxError::Config(format!("序列化模型映射失败: {error}")))?;
    let (source, source_id) = origin
        .map(|(source, source_id)| (source, Some(source_id)))
        .unwrap_or((MANUAL_PROVIDER_SOURCE, None));
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, model_mappings_json, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            model = excluded.model,
            api_key = excluded.api_key,
            toml_config = excluded.toml_config,
            wire_api = excluded.wire_api,
            requires_openai_auth = excluded.requires_openai_auth,
            model_mappings_json = excluded.model_mappings_json,
            source = CASE
                WHEN excluded.source_id IS NULL THEN providers.source
                ELSE excluded.source
            END,
            source_id = CASE
                WHEN excluded.source_id IS NULL THEN providers.source_id
                ELSE excluded.source_id
            END,
            updated_at = excluded.updated_at",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            model_mappings_json,
            source,
            source_id,
            now,
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
fn write_provider_on_connection(conn: &Connection, provider: &SavedProvider) -> Result<()> {
    write_provider_with_origin(conn, provider, None)
}

fn unique_provider_id_on_connection(conn: &Connection, preferred: &str) -> Result<String> {
    if provider_by_id_on_connection(conn, preferred)?.is_none() {
        return Ok(preferred.to_string());
    }
    let mut index = 2usize;
    loop {
        let candidate = format!("{preferred}-{index}");
        if provider_by_id_on_connection(conn, &candidate)?.is_none() {
            return Ok(candidate);
        }
        index += 1;
    }
}

fn merge_authoritative_import(
    mut incoming: SavedProvider,
    existing: &SavedProvider,
) -> SavedProvider {
    incoming.id = existing.id.clone();
    if incoming.provider_name.trim().is_empty() {
        incoming.provider_name = existing.provider_name.clone();
    }
    if incoming.base_url.trim().is_empty() {
        incoming.base_url = existing.base_url.clone();
    }
    if incoming.model.trim().is_empty() {
        incoming.model = existing.model.clone();
    }
    if incoming
        .api_key
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        incoming.api_key = existing.api_key.clone();
    }
    if incoming
        .toml_config
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        incoming.toml_config = existing.toml_config.clone();
    }
    if incoming.wire_api.trim().is_empty() {
        incoming.wire_api = existing.wire_api.clone();
    }
    if incoming.model_mappings.is_empty() {
        incoming.model_mappings = existing.model_mappings.clone();
    }
    incoming
}

fn source_matches(row: &StoredProvider, origin: (&str, &str)) -> bool {
    let source_matches = row.source == origin.0
        || (origin.0 == CCSWITCH_PROVIDER_SOURCE && row.source == CCSWITCH_LOCAL_PROVIDER_SOURCE);
    source_matches && row.source_id.as_deref() == Some(origin.1)
}

fn upsert_provider_in_savepoint(
    conn: &Connection,
    mut provider: SavedProvider,
    mode: ProviderUpsertMode,
    origin: Option<(&str, &str)>,
) -> Result<ProviderUpsertResult> {
    let requested_id = provider.id.clone();
    let stored = stored_providers_on_connection(conn)?;
    let source_match = origin.and_then(|origin| {
        stored
            .iter()
            .find(|candidate| source_matches(candidate, origin))
    });
    let exact_id_match = stored
        .iter()
        .find(|candidate| candidate.provider.id == requested_id);

    let target = match mode {
        ProviderUpsertMode::Manual => exact_id_match,
        ProviderUpsertMode::Imported => source_match,
    };
    let kind = if let Some(target) = target {
        let existing = &target.provider;
        provider.id = existing.id.clone();
        if mode == ProviderUpsertMode::Imported {
            provider = merge_authoritative_import(provider, existing);
        }
        ProviderUpsertKind::Updated
    } else {
        if exact_id_match.is_some() {
            provider.id = unique_provider_id_on_connection(conn, &provider.id)?;
        }
        ProviderUpsertKind::Added
    };

    write_provider_with_origin(conn, &provider, origin)?;
    let provider = provider_by_id_on_connection(conn, &provider.id)?
        .ok_or_else(|| CodexxError::Database("provider saved but not found".to_string()))?;
    Ok(ProviderUpsertResult { provider, kind })
}

fn upsert_provider_with_origin(
    conn: &Connection,
    provider: SavedProvider,
    mode: ProviderUpsertMode,
    origin: Option<(&str, &str)>,
) -> Result<ProviderUpsertResult> {
    const SAVEPOINT: &str = "codex_x_provider_upsert";
    conn.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match upsert_provider_in_savepoint(conn, provider, mode, origin) {
        Ok(result) => {
            conn.execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT}"))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            Ok(result)
        }
        Err(error) => {
            let rollback = conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {SAVEPOINT}; RELEASE SAVEPOINT {SAVEPOINT}"
            ));
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CodexxError::Database(format!(
                    "{error}; provider upsert rollback failed: {rollback_error}"
                ))),
            }
        }
    }
}

pub(crate) fn upsert_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
    mode: ProviderUpsertMode,
) -> Result<ProviderUpsertResult> {
    upsert_provider_with_origin(conn, provider, mode, None)
}

pub(crate) fn upsert_ccswitch_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
    source_id: &str,
) -> Result<ProviderUpsertResult> {
    let source_id = source_id.trim();
    if source_id.is_empty() {
        return Err(CodexxError::Config(
            "cc-switch 供应商缺少稳定来源 ID".to_string(),
        ));
    }
    upsert_provider_with_origin(
        conn,
        provider,
        ProviderUpsertMode::Imported,
        Some((CCSWITCH_PROVIDER_SOURCE, source_id)),
    )
}

fn is_historical_custom_provider_id(id: &str) -> bool {
    let id = id.trim();
    id == "custom"
        || id.strip_prefix("custom-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub(crate) fn consolidate_legacy_provider_duplicates_on_connection(
    _conn: &Connection,
) -> Result<usize> {
    // Distinct IDs are independent records, even when every configuration
    // field matches. Keep the legacy hook for import-result compatibility.
    Ok(0)
}

fn apply_provider_toml_authority(provider: &mut SavedProvider) -> Result<()> {
    let Some(text) = provider.toml_config.as_deref() else {
        return Ok(());
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| CodexxError::Config(format!("供应商 TOML 无效: {error}")))?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".to_string()))?;

    let table = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(&provider_id))
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            CodexxError::Config(format!("供应商 TOML 缺少 [model_providers.{provider_id}]"))
        })?;

    let model = doc
        .get("model")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let provider_name = table
        .get("name")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let base_url = table
        .get("base_url")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let wire_api = table
        .get("wire_api")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let requires_openai_auth = table
        .get("requires_openai_auth")
        .and_then(|item| item.as_bool());
    let toml_api_key = experimental_bearer_token_from_doc(&doc, Some(&provider_id));

    if let Some(model) = model {
        provider.model = model;
    }
    if let Some(provider_name) = provider_name {
        provider.provider_name = provider_name;
    }
    if let Some(base_url) = base_url {
        provider.base_url = canonical_provider_base_url(&base_url);
    }
    if let Some(wire_api) = wire_api {
        provider.wire_api = wire_api;
    }
    if let Some(requires_openai_auth) = requires_openai_auth {
        provider.requires_openai_auth = requires_openai_auth;
    }
    if provider.api_key.is_none() {
        provider.api_key = toml_api_key;
    }

    strip_provider_bearer_tokens(&mut doc);
    provider.toml_config = Some(doc.to_string().trim_end().to_string());
    Ok(())
}

fn normalize_stored_provider_from_toml(provider: &mut SavedProvider) {
    // Old releases could persist stale scalar columns beside a newer complete
    // TOML template. Reads must trust a valid template without making one bad
    // legacy row prevent the provider list from loading.
    let _ = apply_provider_toml_authority(provider);
}

fn sync_provider_toml_from_fields(provider: &mut SavedProvider) -> Result<()> {
    let Some(text) = provider.toml_config.as_deref() else {
        return Ok(());
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| CodexxError::Config(format!("供应商 TOML 无效: {error}")))?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".to_string()))?;

    if provider.api_key.is_none() {
        provider.api_key = experimental_bearer_token_from_doc(&doc, Some(&provider_id));
    }
    strip_provider_bearer_tokens(&mut doc);
    doc["model"] = value(provider.model.clone());

    let table = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
        .and_then(|providers| providers.get_mut(&provider_id))
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| {
            CodexxError::Config(format!("供应商 TOML 缺少 [model_providers.{provider_id}]"))
        })?;
    table["name"] = value(provider.provider_name.clone());
    super::transport::configure_third_party_transport(table, &provider.base_url, true);
    table["base_url"] = value(provider.base_url.clone());
    table["wire_api"] = value(provider.wire_api.clone());
    table["requires_openai_auth"] = value(provider.requires_openai_auth);
    provider.toml_config = Some(provider_template_from_document(
        &doc,
        &provider_id,
        &provider.model,
    )?);
    Ok(())
}

pub(crate) fn normalize_saved_provider(provider: SavedProvider) -> Result<SavedProvider> {
    let raw_id = provider.id.trim();
    if raw_id.is_empty() {
        return Err(CodexxError::Config("provider id 不能为空".to_string()));
    }
    let model_mappings =
        super::model_catalog::normalize_mappings(&provider.model_mappings, &provider.model)?;
    let mut normalized = SavedProvider {
        id: custom_provider_id(raw_id),
        provider_name: provider.provider_name.trim().to_string(),
        base_url: canonical_provider_base_url(&provider.base_url),
        model: provider.model.trim().to_string(),
        api_key: provider
            .api_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        toml_config: provider
            .toml_config
            .map(|value| value.trim_end().to_string())
            .filter(|value| !value.trim().is_empty()),
        wire_api: if provider.wire_api.trim().is_empty() {
            "responses".to_string()
        } else {
            provider.wire_api.trim().to_string()
        },
        requires_openai_auth: provider.requires_openai_auth,
        model_mappings,
    };
    if normalized.provider_name.is_empty() {
        return Err(CodexxError::Config("供应商名称不能为空".to_string()));
    }
    if normalized.base_url.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if normalized.model.is_empty() {
        return Err(CodexxError::Config("model 不能为空".to_string()));
    }
    if is_placeholder_provider(&normalized.provider_name, &normalized.base_url) {
        return Err(CodexxError::Config(
            "供应商名称和 base_url 不能使用示例占位值，请填写实际配置".to_string(),
        ));
    }
    // Imported/read records are hydrated from their complete TOML before they
    // reach this path. For an explicit user edit, the latest form fields win
    // while the full TOML (comments, MCP, projects, desktop settings, etc.) is
    // retained verbatim apart from the standard provider fields.
    sync_provider_toml_from_fields(&mut normalized)?;
    Ok(normalized)
}

pub(crate) fn normalize_saved_provider_for_save(
    conn: &Connection,
    provider: SavedProvider,
) -> Result<SavedProvider> {
    let requested_id = provider.id.trim().to_string();
    let mut normalized = normalize_saved_provider(provider)?;
    if provider_by_id_on_connection(conn, &requested_id)?.is_some() {
        // Existing IDs are record identities. Preserve legacy reserved IDs such
        // as `custom` instead of treating an edit as a new normalized record.
        normalized.id = requested_id;
    } else if requested_id != normalized.id
        && provider_by_id_on_connection(conn, &normalized.id)?.is_some()
    {
        return Err(CodexxError::Config(format!(
            "供应商 ID {} 规范化后与现有供应商冲突，请更换名称或 ID",
            requested_id
        )));
    }
    Ok(normalized)
}

pub(crate) fn save_manual_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
) -> Result<SavedProvider> {
    let provider = normalize_saved_provider_for_save(conn, provider)?;
    Ok(upsert_provider_on_connection(conn, provider, ProviderUpsertMode::Manual)?.provider)
}

pub(crate) fn save_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let conn = open_db()?;
    save_manual_provider_on_connection(&conn, provider)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DuplicateProviderResult {
    pub(crate) provider: SavedProvider,
    pub(crate) providers: Vec<SavedProvider>,
    pub(crate) active_provider_id: Option<String>,
}

pub(crate) fn duplicate_provider_inner(
    config_dir: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<DuplicateProviderResult> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    let _lock = crate::live_config::acquire_live_config_lock(&codex_dir)?;
    let live = super::live::detected_live_custom_provider(&codex_dir)?;
    duplicate_provider_on_connection(
        &mut open_db()?,
        &codex_dir,
        live,
        provider_id.as_deref(),
        provider_name.as_deref(),
    )
}

fn duplicate_provider_on_connection(
    conn: &mut Connection,
    codex_dir: &std::path::Path,
    live: Option<SavedProvider>,
    provider_id: Option<&str>,
    provider_name: Option<&str>,
) -> Result<DuplicateProviderResult> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let mut saved = list_saved_providers_on_connection(&transaction)?;
    let source = if let Some(id) = provider_id {
        provider_by_id_on_connection(&transaction, id)?
            .ok_or_else(|| CodexxError::Config("供应商已不存在，请刷新列表后重试".to_string()))?
    } else {
        let mut detected = live.clone().ok_or_else(|| {
            CodexxError::Config("当前第三方配置已变更，请刷新列表后重试".to_string())
        })?;
        // Adopt an unsaved detected original before adding its copy. Otherwise
        // the copy would become the only matching row and appear to be enabled.
        let detected_matches = matching_saved_provider_ids_for_live_on_connection(
            &transaction,
            codex_dir,
            &detected,
            &saved,
        )?;
        if let [saved_id] = detected_matches.as_slice() {
            if let Some(original) = saved.iter().find(|provider| &provider.id == saved_id) {
                detected.id = original.id.clone();
                detected.model_mappings = original.model_mappings.clone();
                if detected.api_key.is_none() {
                    detected.api_key = original.api_key.clone();
                }
            }
        } else if detected_matches.is_empty() {
            detected.id = unique_provider_id_on_connection(
                &transaction,
                &custom_provider_id(&detected.provider_name),
            )?;
            detected = save_manual_provider_on_connection(&transaction, detected)?;
            saved.push(detected.clone());
        }
        detected
    };
    let active_provider_id = if let Some(live) = live.as_ref() {
        super::reconcile_active_provider_on_connection(
            &transaction,
            codex_dir,
            &matching_saved_provider_ids_for_live_on_connection(
                &transaction,
                codex_dir,
                live,
                &saved,
            )?,
        )?
    } else {
        None
    };
    let base_name = provider_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{} 副本", source.provider_name));
    let mut copy = source.clone();
    copy.id = unique_provider_id_on_connection(&transaction, &format!("{}-copy", source.id))?;
    copy.provider_name = base_name.clone();
    let mut suffix = 2;
    while saved
        .iter()
        .any(|provider| provider.provider_name == copy.provider_name)
    {
        copy.provider_name = format!("{base_name} {suffix}");
        suffix += 1;
    }
    let provider = save_manual_provider_on_connection(&transaction, copy)?;
    let providers = list_saved_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(DuplicateProviderResult {
        provider,
        providers,
        active_provider_id,
    })
}

pub(crate) fn save_provider_with_rollback_inner(
    provider: SavedProvider,
) -> Result<(SavedProvider, ProviderStoreRollback)> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let before = stored_providers_on_connection(&transaction)?;
    let saved = save_manual_provider_on_connection(&transaction, provider)?;
    let after = stored_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok((saved, ProviderStoreRollback { before, after }))
}

pub(crate) fn save_detected_provider_with_rollback_inner(
    codex_dir: &std::path::Path,
    mut live: SavedProvider,
) -> Result<Option<ProviderStoreRollback>> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let before = stored_providers_on_connection(&transaction)?;
    let saved = before
        .iter()
        .map(|stored| stored.provider.clone())
        .collect::<Vec<_>>();
    let matches =
        matching_saved_provider_ids_for_live_on_connection(&transaction, codex_dir, &live, &saved)?;
    match matches.as_slice() {
        [saved_id] => {
            if let Some(saved) = saved.iter().find(|provider| &provider.id == saved_id) {
                live.model_mappings = saved.model_mappings.clone();
            }
            if live.api_key.is_none() {
                live.api_key = saved
                    .iter()
                    .find(|provider| &provider.id == saved_id)
                    .and_then(|provider| provider.api_key.clone());
            }
            live.id = saved_id.clone();
        }
        [] => {
            // A detected live route is only a temporary UI row until a write
            // action adopts it. Preserve it before replacing config/auth, and
            // allocate its ID under the same transaction to avoid overwriting
            // an unrelated manual profile or a concurrent adoption.
            live.id = unique_provider_id_on_connection(
                &transaction,
                &custom_provider_id(&live.provider_name),
            )?;
        }
        _ => return Ok(None),
    }
    save_manual_provider_on_connection(&transaction, live)?;
    let after = stored_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(Some(ProviderStoreRollback { before, after }))
}

fn insert_stored_provider(conn: &Connection, stored: &StoredProvider) -> Result<()> {
    let provider = &stored.provider;
    let model_mappings_json = serde_json::to_string(&provider.model_mappings)
        .map_err(|error| CodexxError::Config(format!("序列化模型映射失败: {error}")))?;
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, model_mappings_json, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            model_mappings_json,
            stored.source,
            stored.source_id,
            stored.created_at,
            stored.updated_at,
        ],
    )
    .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn rollback_provider_store_inner(rollback: ProviderStoreRollback) -> Result<()> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let current = stored_providers_on_connection(&transaction)?;
    let ids = rollback
        .before
        .iter()
        .chain(&rollback.after)
        .map(|stored| stored.provider.id.clone())
        .collect::<HashSet<_>>();
    let changed_ids = ids
        .into_iter()
        .filter(|id| {
            rollback
                .before
                .iter()
                .find(|stored| stored.provider.id == *id)
                != rollback
                    .after
                    .iter()
                    .find(|stored| stored.provider.id == *id)
        })
        .collect::<Vec<_>>();
    for id in &changed_ids {
        let actual = current.iter().find(|stored| stored.provider.id == *id);
        let expected = rollback
            .after
            .iter()
            .find(|stored| stored.provider.id == *id);
        if actual != expected {
            return Err(CodexxError::Database(format!(
                "供应商 {id} 已被其他操作修改，拒绝覆盖并发变更"
            )));
        }
    }
    for id in &changed_ids {
        transaction
            .execute("DELETE FROM providers WHERE id = ?1", [id])
            .map_err(|error| CodexxError::Database(error.to_string()))?;
    }
    for stored in rollback
        .before
        .iter()
        .filter(|stored| changed_ids.contains(&stored.provider.id))
    {
        insert_stored_provider(&transaction, stored)?;
    }
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn delete_provider_inner(id: &str) -> Result<()> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    clear_provider_selections_on_connection(&transaction, id)?;
    transaction
        .execute("DELETE FROM providers WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn reserved_codex_provider_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "openai" | "custom" | "amazon-bedrock" | "ollama" | "lmstudio" | "oss"
    )
}

pub(crate) fn custom_provider_id(input: &str) -> String {
    let id = sanitize_id(input);
    if reserved_codex_provider_id(&id) {
        format!("{id}-custom")
    } else {
        id
    }
}

pub(crate) fn experimental_bearer_token_from_doc(
    doc: &DocumentMut,
    provider_id: Option<&str>,
) -> Option<String> {
    let token_from_table = provider_id.and_then(|id| {
        doc.get("model_providers")
            .and_then(|item| item.as_table())
            .and_then(|providers| providers.get(id))
            .and_then(|item| item.as_table())
            .and_then(|table| table.get("experimental_bearer_token"))
            .and_then(|item| item.as_str())
    });

    token_from_table
        .or_else(|| {
            doc.get("experimental_bearer_token")
                .and_then(|item| item.as_str())
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open provider test database");
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY, provider_name TEXT NOT NULL, base_url TEXT NOT NULL,
                model TEXT NOT NULL, api_key TEXT, toml_config TEXT,
                wire_api TEXT NOT NULL DEFAULT 'responses',
                requires_openai_auth INTEGER NOT NULL DEFAULT 1,
                model_mappings_json TEXT NOT NULL DEFAULT '[]',
                source TEXT NOT NULL DEFAULT 'manual',
                source_id TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .expect("create providers table");
        conn
    }

    fn provider(id: &str, name: &str, api_key: Option<&str>) -> SavedProvider {
        SavedProvider {
            id: id.to_string(),
            provider_name: name.to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: api_key.map(ToString::to_string),
            toml_config: None,
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
            model_mappings: Vec::new(),
        }
    }

    fn provider_count(conn: &Connection) -> usize {
        list_saved_providers_on_connection(conn).unwrap().len()
    }

    fn copy_test_connection() -> Connection {
        let conn = test_connection();
        conn.execute_batch("CREATE TABLE active_provider_selections (codex_dir TEXT PRIMARY KEY, provider_id TEXT NOT NULL, updated_at TEXT NOT NULL);").unwrap();
        conn
    }

    #[test]
    fn model_mappings_roundtrip_and_copy_preserve_independent_records() {
        let mut conn = copy_test_connection();
        let mut original = provider("mapping-original", "Original", Some("fixture-key"));
        original.model_mappings = vec![
            super::super::model_catalog::ProviderModelMapping {
                model: " gpt-5.5 ".into(),
                display_name: "GPT".into(),
                context_window: None,
            },
            super::super::model_catalog::ProviderModelMapping {
                model: "deepseek-chat".into(),
                display_name: "DeepSeek".into(),
                context_window: Some(128000),
            },
        ];
        let original = save_manual_provider_on_connection(&conn, original).unwrap();
        assert_eq!(original.model_mappings[0].model, "gpt-5.5");
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        let copied = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/model-copy"),
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_ne!(copied.provider.id, original.id);
        assert_eq!(copied.provider.model_mappings, original.model_mappings);
        let mut detected = original.clone();
        detected.id = "custom".into();
        detected.model = "deepseek-chat".into();
        detected.model_mappings.clear();
        let detected_copy = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/model-copy"),
            Some(detected),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            detected_copy.provider.model_mappings,
            original.model_mappings
        );
        let mut edited = copied.provider;
        edited.model_mappings[1].display_name = "Independent copy".into();
        save_manual_provider_on_connection(&conn, edited).unwrap();
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        let serialized = serde_json::to_value(&original).unwrap();
        assert_eq!(serialized["modelMappings"][1]["contextWindow"], 128000);
        let mut old_payload = serialized;
        old_payload.as_object_mut().unwrap().remove("modelMappings");
        assert!(serde_json::from_value::<SavedProvider>(old_payload)
            .unwrap()
            .model_mappings
            .is_empty());
    }

    #[test]
    fn cc_switch_refresh_preserves_locally_saved_model_mappings() {
        let conn = copy_test_connection();
        let mut original = provider("mapping-import", "Imported", Some("fixture-key"));
        original.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "gpt-5.5".into(),
            display_name: "Local label".into(),
            context_window: Some(256000),
        }];
        upsert_ccswitch_provider_on_connection(&conn, original.clone(), "mapping-source").unwrap();
        let incoming = provider("external-id", "Fresh import", Some("fixture-key"));
        let refreshed =
            upsert_ccswitch_provider_on_connection(&conn, incoming, "mapping-source").unwrap();
        assert_eq!(refreshed.provider.id, original.id);
        assert_eq!(refreshed.provider.provider_name, "Fresh import");
        assert_eq!(refreshed.provider.model_mappings, original.model_mappings);
    }

    #[test]
    fn model_mapping_rollback_restores_mappings_and_origin_metadata() {
        let _guard = crate::app_db::test_db_guard();
        let mut original = provider("mapping-rollback-record", "Original", Some("fixture-key"));
        original.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "gpt-5.5".into(),
            display_name: "Original model".into(),
            context_window: Some(128000),
        }];
        let conn = open_db().unwrap();
        upsert_ccswitch_provider_on_connection(&conn, original.clone(), "mapping-rollback-source")
            .unwrap();
        let metadata = |conn: &Connection| {
            conn.query_row(
            "SELECT source, source_id, created_at, updated_at FROM providers WHERE id = 'mapping-rollback-record'", [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        ).unwrap()
        };
        let before = metadata(&conn);
        let mut changed = original.clone();
        changed.model_mappings[0].display_name = "Changed".into();
        changed.model_mappings[0].context_window = Some(1000000);
        let (_, rollback) = save_provider_with_rollback_inner(changed).unwrap();
        rollback_provider_store_inner(rollback).unwrap();
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        assert_eq!(metadata(&conn), before);
        drop(conn);
        delete_provider_inner(&original.id).unwrap();
    }

    #[test]
    fn immediate_copy_preserves_original_selection_and_complete_template() {
        let mut conn = copy_test_connection();
        let dir = std::path::Path::new("/fixture/copy-selection");
        let mut original = provider("original", "Original", Some("fixture-key"));
        original.toml_config = Some("model = \"gpt-5.5\"\nmodel_provider = \"custom\"\nmodel_context_window = 1000000\n[model_providers.custom]\nname = \"Original\"\nbase_url = \"https://example.com/v1\"\nwire_api = \"responses\"\n[mcp_servers.keep]\ncommand = \"fixture\"\n".to_string());
        let original = save_manual_provider_on_connection(&conn, original).unwrap();
        let first = duplicate_provider_on_connection(
            &mut conn,
            dir,
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_eq!(
            first.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(first.provider.id, original.id);
        assert_eq!(first.provider.api_key, original.api_key);
        assert_eq!(first.provider.provider_name, "Original 副本");
        let config = first
            .provider
            .toml_config
            .as_ref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(config["model_context_window"].as_integer(), Some(1_000_000));
        assert_eq!(
            config["mcp_servers"]["keep"]["command"].as_str(),
            Some("fixture")
        );
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap(),
            original
        );
        let second = duplicate_provider_on_connection(
            &mut conn,
            dir,
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_eq!(second.provider.provider_name, "Original 副本 2");
        assert_eq!(
            second.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(second.provider.id, first.provider.id);
    }

    #[test]
    fn immediate_copy_adopts_detected_original_without_enabling_the_copy() {
        let mut conn = copy_test_connection();
        let dir = std::path::Path::new("/fixture/copy-detected");
        let live = provider("custom", "Detected", Some("fixture-key"));
        let result =
            duplicate_provider_on_connection(&mut conn, dir, Some(live.clone()), None, None)
                .unwrap();
        assert_eq!(result.providers.len(), 2);
        let original = result
            .providers
            .iter()
            .find(|provider| provider.id != result.provider.id)
            .unwrap();
        assert_eq!(original.provider_name, live.provider_name);
        assert_eq!(
            result.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(
            result.active_provider_id.as_deref(),
            Some(result.provider.id.as_str())
        );
    }

    #[test]
    fn failed_immediate_copy_rolls_back_detected_adoption_and_selection() {
        let mut conn = copy_test_connection();
        conn.execute_batch("CREATE TRIGGER fail_copy BEFORE INSERT ON providers WHEN NEW.provider_name LIKE '%副本%' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
        let result = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/copy-failure"),
            Some(provider("custom", "Detected", Some("fixture-key"))),
            None,
            None,
        );
        assert!(result.is_err());
        assert_eq!(provider_count(&conn), 0);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM active_provider_selections",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn provider_upsert_uses_record_ids_and_stable_import_sources() {
        let stable = test_connection();
        let mut existing = provider("cc-stable-id", "Old name", Some("sk-old"));
        existing.toml_config = Some("model = \"locally-preserved\"".to_string());
        upsert_ccswitch_provider_on_connection(&stable, existing, "source-row").unwrap();
        let mut imported = provider("cc-stable-id", "Current CCS name", Some("sk-new"));
        imported.model = "gpt-5.6".to_string();
        let updated =
            upsert_ccswitch_provider_on_connection(&stable, imported.clone(), "source-row")
                .unwrap();
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.api_key.as_deref(), Some("sk-new"));
        assert_eq!(updated.provider.provider_name, "Current CCS name");
        assert_eq!(updated.provider.model, "gpt-5.6");
        assert!(updated.provider.toml_config.is_some());
        upsert_ccswitch_provider_on_connection(&stable, imported, "source-row").unwrap();
        assert_eq!(provider_count(&stable), 1);

        let mut moved = provider("cc-stable-id", "Moved CCS provider", Some("sk-moved"));
        moved.base_url = "https://moved.example.com/v1".to_string();
        let moved = upsert_ccswitch_provider_on_connection(&stable, moved, "source-row")
            .expect("update stable CCS source after endpoint change");
        assert_eq!(moved.kind, ProviderUpsertKind::Updated);
        assert_eq!(moved.provider.base_url, "https://moved.example.com/v1");
        assert_eq!(moved.provider.api_key.as_deref(), Some("sk-moved"));
        assert_eq!(provider_count(&stable), 1);

        let id_collision = test_connection();
        let manual = provider("shared-id", "Manual", Some("sk-manual"));
        upsert_provider_on_connection(&id_collision, manual, ProviderUpsertMode::Manual).unwrap();
        let mut external = provider("shared-id", "Imported", Some("sk-imported"));
        external.base_url = "https://imported.example.com/v1".to_string();
        let imported = upsert_ccswitch_provider_on_connection(&id_collision, external, "shared-id")
            .expect("import id collision without overwriting manual record");
        assert_eq!(imported.kind, ProviderUpsertKind::Added);
        assert_eq!(imported.provider.id, "shared-id-2");
        assert_eq!(
            provider_by_id_on_connection(&id_collision, "shared-id")
                .unwrap()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-manual")
        );
        let mut changed = provider("shared-id", "Imported changed", Some("sk-next"));
        changed.base_url = "https://next.example.com/v1".to_string();
        let changed = upsert_ccswitch_provider_on_connection(&id_collision, changed, "shared-id")
            .expect("update imported row by source identity");
        assert_eq!(changed.provider.id, "shared-id-2");
        assert_eq!(changed.provider.base_url, "https://next.example.com/v1");
        assert_eq!(provider_count(&id_collision), 2);

        let compatible = test_connection();
        upsert_provider_on_connection(
            &compatible,
            provider("local", "Same API", None),
            ProviderUpsertMode::Manual,
        )
        .unwrap();
        let added = upsert_ccswitch_provider_on_connection(
            &compatible,
            provider("cc-import", " same   api ", Some("sk-imported")),
            "compatible-row",
        )
        .unwrap();
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "cc-import");
        assert_eq!(added.provider.api_key.as_deref(), Some("sk-imported"));
        assert_eq!(provider_count(&compatible), 2);

        let distinct = test_connection();
        upsert_provider_on_connection(
            &distinct,
            provider("local", "Same API", Some("sk-first")),
            ProviderUpsertMode::Manual,
        )
        .unwrap();
        let added = upsert_ccswitch_provider_on_connection(
            &distinct,
            provider("cc-import", "Same API", Some("sk-second")),
            "distinct-row",
        )
        .unwrap();
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(provider_count(&distinct), 2);
    }

    #[test]
    fn provider_upsert_keeps_manual_profiles_with_distinct_ids() {
        let conn = test_connection();
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        let added = upsert_provider_on_connection(&conn, gpt.clone(), ProviderUpsertMode::Manual)
            .expect("save GPT profile");
        assert_eq!(added.kind, ProviderUpsertKind::Added);

        let exact_copy = SavedProvider {
            id: "gpt-copy".to_string(),
            ..gpt.clone()
        };
        let added = upsert_provider_on_connection(&conn, exact_copy, ProviderUpsertMode::Manual)
            .expect("save an identical profile under a different ID");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "gpt-copy");

        let renamed_copy = SavedProvider {
            id: "gpt-renamed".to_string(),
            provider_name: "Renamed API".to_string(),
            ..gpt.clone()
        };
        let added = upsert_provider_on_connection(&conn, renamed_copy, ProviderUpsertMode::Manual)
            .expect("save a renamed profile under a different ID");
        assert_eq!(added.kind, ProviderUpsertKind::Added);

        let edited = SavedProvider {
            provider_name: "Edited GPT".to_string(),
            ..gpt
        };
        let updated = upsert_provider_on_connection(&conn, edited, ProviderUpsertMode::Manual)
            .expect("update the profile with the same ID");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "gpt-profile");
        assert_eq!(updated.provider.provider_name, "Edited GPT");

        let mut deepseek = provider("deepseek-profile", "Renamed API", Some("sk-shared"));
        deepseek.model = " deepseek-v3 ".to_string();
        let added = upsert_provider_on_connection(&conn, deepseek, ProviderUpsertMode::Manual)
            .expect("save DeepSeek profile with shared credentials");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(provider_count(&conn), 4);
    }

    #[test]
    fn provider_listing_preserves_insertion_order_when_timestamps_match() {
        let conn = test_connection();
        for id in ["z-original", "m-sibling", "a-copy"] {
            write_provider_on_connection(&conn, &provider(id, id, Some("sk-shared")))
                .expect("insert provider");
        }
        conn.execute(
            "UPDATE providers
             SET created_at = '2026-01-01T00:00:00Z',
                 updated_at = CASE id
                     WHEN 'z-original' THEN '2026-03-01T00:00:00Z'
                     WHEN 'm-sibling' THEN '2026-02-01T00:00:00Z'
                     ELSE '2026-01-01T00:00:00Z'
                 END",
            [],
        )
        .expect("align creation timestamps");

        let listed_ids = list_saved_providers_on_connection(&conn)
            .expect("list providers")
            .into_iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();
        assert_eq!(listed_ids, ["z-original", "m-sibling", "a-copy"]);

        let mut edited = provider("z-original", "Edited original", Some("sk-shared"));
        edited.model = "gpt-5.6".to_string();
        upsert_provider_on_connection(&conn, edited, ProviderUpsertMode::Manual)
            .expect("update first provider");
        let listed_ids = list_saved_providers_on_connection(&conn)
            .expect("list providers after edit")
            .into_iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();
        assert_eq!(listed_ids, ["z-original", "m-sibling", "a-copy"]);
    }

    #[test]
    fn manual_save_preserves_an_existing_legacy_reserved_id() {
        let conn = test_connection();
        let legacy = provider("custom", "Legacy Provider", Some("sk-legacy"));
        write_provider_on_connection(&conn, &legacy).expect("seed legacy custom record");

        let edited = SavedProvider {
            provider_name: "Edited Legacy Provider".to_string(),
            ..legacy
        };
        let saved = save_manual_provider_on_connection(&conn, edited)
            .expect("edit the existing legacy custom record");

        assert_eq!(saved.id, "custom");
        assert_eq!(saved.provider_name, "Edited Legacy Provider");
        assert_eq!(provider_count(&conn), 1);
        assert!(provider_by_id_on_connection(&conn, "custom-custom")
            .expect("look up normalized duplicate")
            .is_none());
    }

    #[test]
    fn ccswitch_import_keeps_ambiguous_manual_profiles_and_updates_by_source_id() {
        let conn = test_connection();
        let first = provider("manual-one", "Same API", Some("sk-shared"));
        let second = SavedProvider {
            id: "manual-two".to_string(),
            ..first.clone()
        };
        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual)
            .expect("save first manual profile");
        upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual)
            .expect("save second manual profile");

        let imported = provider("cc-profile", "Same API", Some("sk-shared"));
        let added = upsert_ccswitch_provider_on_connection(&conn, imported.clone(), "cc-source-id")
            .expect("import alongside ambiguous manual profiles");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "cc-profile");
        assert_eq!(provider_count(&conn), 3);

        let repeated = SavedProvider {
            provider_name: "Updated from CC Switch".to_string(),
            ..imported
        };
        let updated = upsert_ccswitch_provider_on_connection(&conn, repeated, "cc-source-id")
            .expect("update the same imported source row");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "cc-profile");
        assert_eq!(updated.provider.provider_name, "Updated from CC Switch");
        assert_eq!(provider_count(&conn), 3);
    }

    #[test]
    fn ccswitch_import_keeps_a_manual_copy_of_its_current_profile() {
        let conn = test_connection();
        let imported = provider("cc-profile", "Same API", Some("sk-shared"));
        upsert_ccswitch_provider_on_connection(&conn, imported.clone(), "cc-source-id")
            .expect("seed imported profile");
        let manual_copy = SavedProvider {
            id: "manual-copy".to_string(),
            ..imported.clone()
        };
        upsert_provider_on_connection(&conn, manual_copy, ProviderUpsertMode::Manual)
            .expect("save manual copy");

        let updated = upsert_ccswitch_provider_on_connection(&conn, imported, "cc-source-id")
            .expect("refresh imported profile by source ID");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "cc-profile");
        assert_eq!(provider_count(&conn), 2);
        assert!(provider_by_id_on_connection(&conn, "manual-copy")
            .expect("read manual copy")
            .is_some());
    }

    #[test]
    fn stable_import_updates_its_source_without_claiming_a_manual_id() {
        let conn = test_connection();
        let mut imported = provider("cc-old", "Imported old", Some("sk-old"));
        imported.base_url = "https://old.example.com/v1".to_string();
        upsert_ccswitch_provider_on_connection(&conn, imported, "cc-row")
            .expect("seed stable imported provider");

        let mut manual = provider("manual-local", "Locally edited", Some("sk-shared"));
        manual.base_url = "https://shared.example.com/v1".to_string();
        manual.model = "remote-model".to_string();
        manual.toml_config = Some("model = \"remote-model\"".to_string());
        upsert_provider_on_connection(&conn, manual, ProviderUpsertMode::Manual)
            .expect("seed manual provider");

        let mut changed = provider("cc-old", "Imported changed", Some("sk-shared"));
        changed.base_url = "https://shared.example.com/v1".to_string();
        changed.model = "remote-model".to_string();
        let result = upsert_ccswitch_provider_on_connection(&conn, changed, "cc-row")
            .expect("update the imported source record");

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "cc-old");
        assert_eq!(result.provider.provider_name, "Imported changed");
        assert_eq!(result.provider.model, "remote-model");
        let rows = stored_providers_on_connection(&conn).expect("read independent providers");
        assert_eq!(rows.len(), 2);
        let manual = rows
            .iter()
            .find(|row| row.provider.id == "manual-local")
            .expect("manual provider remains");
        assert_eq!(manual.provider.provider_name, "Locally edited");
        assert_eq!(manual.source, MANUAL_PROVIDER_SOURCE);
        let imported = rows
            .iter()
            .find(|row| row.provider.id == "cc-old")
            .expect("imported provider remains");
        assert_eq!(imported.source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(imported.source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn ccswitch_import_replaces_stale_local_values_with_complete_source() {
        let conn = test_connection();
        let mut stale = provider("legacy-local", "Local name", Some("sk-local"));
        stale.base_url = "https://old.example.com/v1".to_string();
        stale.model = "local-model".to_string();
        stale.toml_config = Some(
            r#"model_provider = "custom"
model = "local-model"

[model_providers.custom]
name = "Local name"
base_url = "https://old.example.com/v1"
wire_api = "chat"
requires_openai_auth = true
request_max_retries = 3
"#
            .to_string(),
        );
        stale.wire_api = "chat".to_string();
        write_provider_with_origin(
            &conn,
            &stale,
            Some((CCSWITCH_LOCAL_PROVIDER_SOURCE, "cc-row")),
        )
        .expect("seed stale local import");

        let mut imported = provider("cc-row", "CC name", Some("sk-imported"));
        imported.base_url = "https://new.example.com/v1".to_string();
        imported.model = "remote-model".to_string();
        imported.requires_openai_auth = false;
        imported.toml_config = Some(
            r#"# complete cc-switch template
model_provider = "custom"
model = "remote-model"
service_tier = "priority"

[model_providers.custom]
name = "CC name"
base_url = "https://new.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 7

[projects."/work/project"]
trust_level = "trusted"

[plugins."browser@openai-bundled"]
enabled = true
"#
            .to_string(),
        );
        let result = upsert_ccswitch_provider_on_connection(&conn, imported, "cc-row")
            .expect("replace stale local values from the authoritative cc-switch row");

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "legacy-local");
        assert_eq!(result.provider.provider_name, "CC name");
        assert_eq!(result.provider.base_url, "https://new.example.com/v1");
        assert_eq!(result.provider.model, "remote-model");
        assert_eq!(result.provider.api_key.as_deref(), Some("sk-imported"));
        assert_eq!(result.provider.wire_api, "responses");
        assert!(!result.provider.requires_openai_auth);
        let template = result
            .provider
            .toml_config
            .expect("complete cc-switch template");
        let doc = template
            .parse::<DocumentMut>()
            .expect("parse imported provider template");
        assert!(template.contains("# complete cc-switch template"));
        assert_eq!(doc["service_tier"].as_str(), Some("priority"));
        assert_eq!(doc["model"].as_str(), Some("remote-model"));
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("CC name")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["projects"]["/work/project"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            doc["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
            Some(true)
        );
        let rows = stored_providers_on_connection(&conn).expect("read imported row");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(rows[0].source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn ccswitch_import_falls_back_only_when_source_fields_are_missing() {
        let conn = test_connection();
        let mut existing = provider("local-id", "Existing name", Some("sk-existing"));
        existing.base_url = "https://existing.example.com/v1".to_string();
        existing.model = "existing-model".to_string();
        existing.toml_config = Some("model = \"existing-model\"".to_string());
        existing.wire_api = "responses".to_string();
        write_provider_with_origin(
            &conn,
            &existing,
            Some((CCSWITCH_LOCAL_PROVIDER_SOURCE, "cc-row")),
        )
        .expect("seed existing source row");

        let missing = SavedProvider {
            id: "remote-id".to_string(),
            provider_name: " ".to_string(),
            base_url: String::new(),
            model: "\t".to_string(),
            api_key: Some(" ".to_string()),
            toml_config: Some("\n".to_string()),
            wire_api: String::new(),
            requires_openai_auth: false,
            model_mappings: Vec::new(),
        };
        let result = upsert_ccswitch_provider_on_connection(&conn, missing, "cc-row")
            .expect("fill missing import fields from the existing source row");

        assert_eq!(result.provider.id, "local-id");
        assert_eq!(result.provider.provider_name, "Existing name");
        assert_eq!(result.provider.base_url, "https://existing.example.com/v1");
        assert_eq!(result.provider.model, "existing-model");
        assert_eq!(result.provider.api_key.as_deref(), Some("sk-existing"));
        assert_eq!(
            result.provider.toml_config.as_deref(),
            Some("model = \"existing-model\"")
        );
        assert_eq!(result.provider.wire_api, "responses");
        assert!(!result.provider.requires_openai_auth);
    }

    #[test]
    fn legacy_cleanup_preserves_historical_custom_ids() {
        let conn = test_connection();
        write_provider_on_connection(&conn, &provider("local", "Same API", Some("sk-current")))
            .unwrap();
        write_provider_on_connection(
            &conn,
            &provider("custom", " same   api ", Some("sk-old-live")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        let rows = list_saved_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.id == "local"));
        assert!(rows.iter().any(|row| row.id == "custom"));

        let ambiguous = test_connection();
        write_provider_on_connection(
            &ambiguous,
            &provider("local-a", "Same API", Some("sk-first")),
        )
        .unwrap();
        write_provider_on_connection(
            &ambiguous,
            &provider("local-b", "Same API", Some("sk-second")),
        )
        .unwrap();
        write_provider_on_connection(
            &ambiguous,
            &provider("custom-2", "Same API", Some("sk-old-live")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&ambiguous).unwrap(),
            0
        );
        assert_eq!(provider_count(&ambiguous), 3);

        let different_models = test_connection();
        let mut stable = provider("local", "Same API", Some("sk-current"));
        stable.model = "deepseek-v3".to_string();
        write_provider_on_connection(&different_models, &stable).unwrap();
        let mut ghost = provider("custom", " same   api ", Some("sk-old-live"));
        ghost.model = "gpt-5.6".to_string();
        write_provider_on_connection(&different_models, &ghost).unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&different_models).unwrap(),
            0
        );
        assert_eq!(provider_count(&different_models), 2);
    }

    #[test]
    fn legacy_consolidation_keeps_distinct_models_for_shared_credentials() {
        let conn = test_connection();
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        write_provider_on_connection(&conn, &gpt).unwrap();
        let mut deepseek = provider("deepseek-profile", "Same API", Some("sk-shared"));
        deepseek.model = "deepseek-v3".to_string();
        write_provider_on_connection(&conn, &deepseek).unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn legacy_consolidation_preserves_manual_and_imported_ids() {
        let conn = test_connection();
        let mut imported = provider("imported-old", "Imported old", Some("sk-same"));
        imported.model = "latest-model".to_string();
        write_provider_with_origin(&conn, &imported, Some((CCSWITCH_PROVIDER_SOURCE, "cc-row")))
            .unwrap();
        let mut manual = provider("manual-local", "Edited locally", Some("sk-same"));
        manual.model = "latest-model".to_string();
        manual.toml_config = Some("model = \"latest-model\"".to_string());
        write_provider_on_connection(&conn, &manual).unwrap();
        conn.execute(
            "UPDATE providers SET updated_at = CASE id
                WHEN 'imported-old' THEN '2026-02-01T00:00:00Z'
                ELSE '2026-01-01T00:00:00Z' END",
            [],
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.provider.id == "manual-local"));
        assert!(rows.iter().any(|row| row.provider.id == "imported-old"));

        let mut repeated = provider("imported-old", "Remote replacement", Some("sk-same"));
        repeated.model = "remote-model".to_string();
        repeated.toml_config = Some("model = \"remote-model\"".to_string());
        upsert_ccswitch_provider_on_connection(&conn, repeated, "cc-row")
            .expect("repeat import after legacy consolidation");

        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        let manual = rows
            .iter()
            .find(|row| row.provider.id == "manual-local")
            .expect("manual provider remains");
        assert_eq!(manual.provider.provider_name, "Edited locally");
        assert_eq!(
            manual.provider.toml_config.as_deref(),
            Some("model = \"latest-model\"")
        );
        let imported = rows
            .iter()
            .find(|row| row.provider.id == "imported-old")
            .expect("imported provider remains");
        assert_eq!(imported.provider.provider_name, "Remote replacement");
        assert_eq!(imported.provider.model, "remote-model");
        assert_eq!(imported.source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(imported.source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn legacy_consolidation_does_not_merge_different_external_sources() {
        let conn = test_connection();
        write_provider_with_origin(
            &conn,
            &provider("cc-one", "Same API", Some("sk-same")),
            Some((CCSWITCH_PROVIDER_SOURCE, "row-one")),
        )
        .unwrap();
        write_provider_with_origin(
            &conn,
            &provider("cc-two", "Same API", Some("sk-same")),
            Some((CCSWITCH_PROVIDER_SOURCE, "row-two")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn live_custom_matches_a_preserved_historical_record_by_its_profile() {
        let stable = provider("stable", "Same API", Some("sk-current"));
        let historical = provider("custom", "Same API", Some("sk-old-live"));
        let live = provider("custom", " same   api ", Some("sk-old-live"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable.clone(), historical]),
            Some("custom".to_string())
        );

        let second = provider("second", "Same API", Some("sk-second"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable, second]),
            None
        );
    }

    #[test]
    fn live_custom_matches_the_saved_profile_with_the_same_model() {
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        let mut deepseek = provider("deepseek-profile", "Same API", Some("sk-shared"));
        deepseek.model = "deepseek-v3".to_string();
        let mut live = provider("custom", "Same API", Some("sk-shared"));
        live.model = " deepseek-v3 ".to_string();

        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[gpt, deepseek]),
            Some("deepseek-profile".to_string())
        );
    }

    #[test]
    fn provider_toml_keeps_extensions_and_syncs_explicit_field_edits() {
        let mut item = provider("saved", "Edited Name", Some("sk-explicit"));
        item.base_url = "https://edited.example.com/v1/".to_string();
        item.model = "edited-model".to_string();
        item.wire_api = "chat_completions".to_string();
        item.requires_openai_auth = false;
        item.toml_config = Some(
            r#"model_provider = "proxy"
model = "stale-model"
approval_policy = "never"
experimental_bearer_token = "sk-top-level"

[model_providers.proxy]
name = "Stale Name"
base_url = "https://stale.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-stale"
request_max_retries = 7

[model_providers.proxy.http_headers]
X-Route = "keep-me"

[model_providers.unrelated]
name = "Keep this provider"
base_url = "https://unrelated.example.com/v1"
experimental_bearer_token = "sk-unrelated"

[mcp_servers.docs]
command = "keep-this-command"
"#
            .to_string(),
        );

        let normalized = normalize_saved_provider(item).expect("normalize provider TOML");
        let text = normalized.toml_config.as_deref().unwrap();
        let doc = text.parse::<DocumentMut>().expect("parse normalized TOML");
        assert_eq!(normalized.provider_name, "Edited Name");
        assert_eq!(normalized.base_url, "https://edited.example.com/v1");
        assert_eq!(normalized.model, "edited-model");
        assert_eq!(normalized.wire_api, "chat_completions");
        assert!(!normalized.requires_openai_auth);
        assert_eq!(doc["model"].as_str(), Some("edited-model"));
        assert_eq!(
            doc["model_providers"]["proxy"]["name"].as_str(),
            Some("Edited Name")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["base_url"].as_str(),
            Some("https://edited.example.com/v1")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["wire_api"].as_str(),
            Some("chat_completions")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["http_headers"]["X-Route"].as_str(),
            Some("keep-me")
        );
        assert_eq!(doc["model_provider"].as_str(), Some("proxy"));
        assert_eq!(doc["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            doc["model_providers"]["unrelated"]["name"].as_str(),
            Some("Keep this provider")
        );
        assert_eq!(
            doc["mcp_servers"]["docs"]["command"].as_str(),
            Some("keep-this-command")
        );
        assert!(doc.get("experimental_bearer_token").is_none());
        assert!(doc["model_providers"]
            .as_table()
            .expect("model providers table")
            .iter()
            .all(|(_, item)| item
                .as_table()
                .is_none_or(|table| table.get("experimental_bearer_token").is_none())));
        assert!(!text.contains("experimental_bearer_token"));
        assert_eq!(normalized.api_key.as_deref(), Some("sk-explicit"));
    }

    #[test]
    fn legacy_rows_are_repaired_from_complete_toml_on_read() {
        let conn = test_connection();
        let mut legacy = provider("legacy", "Wrong database name", None);
        legacy.base_url = "https://wrong.example.com/v1".to_string();
        legacy.model = "wrong-model".to_string();
        legacy.wire_api = "chat_completions".to_string();
        legacy.requires_openai_auth = true;
        legacy.toml_config = Some(
            r#"# authoritative saved template
model_provider = "proxy"
model = "gpt-5.6-sol"
service_tier = "priority"

[model_providers.proxy]
name = "Authoritative Name"
base_url = "https://RIGHT.example.com/v1/"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-from-template"
request_max_retries = 9
"#
            .to_string(),
        );
        write_provider_on_connection(&conn, &legacy).expect("seed legacy mismatch");

        let listed = list_saved_providers_on_connection(&conn).expect("read providers");
        assert_eq!(listed.len(), 1);
        let repaired = &listed[0];
        assert_eq!(repaired.provider_name, "Authoritative Name");
        assert_eq!(repaired.base_url, "https://right.example.com/v1");
        assert_eq!(repaired.model, "gpt-5.6-sol");
        assert_eq!(repaired.wire_api, "responses");
        assert!(!repaired.requires_openai_auth);
        assert_eq!(repaired.api_key.as_deref(), Some("sk-from-template"));
        let template = repaired.toml_config.as_deref().expect("saved template");
        assert!(template.contains("# authoritative saved template"));
        assert!(template.contains("request_max_retries = 9"));
        assert!(!template.contains("experimental_bearer_token"));

        let by_id = provider_by_id_on_connection(&conn, "legacy")
            .expect("read provider by id")
            .expect("legacy provider");
        assert_eq!(by_id, *repaired);

        let raw_model: String = conn
            .query_row(
                "SELECT model FROM providers WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .expect("read raw legacy scalar");
        assert_eq!(raw_model, "wrong-model");
    }

    #[test]
    fn malformed_legacy_toml_does_not_break_provider_reads() {
        let conn = test_connection();
        let mut legacy = provider("legacy", "Fallback Name", Some("sk-fallback"));
        legacy.base_url = "https://fallback.example.com/v1".to_string();
        legacy.model = "fallback-model".to_string();
        legacy.toml_config = Some("model = [".to_string());
        write_provider_on_connection(&conn, &legacy).expect("seed malformed legacy row");

        let listed = list_saved_providers_on_connection(&conn).expect("read malformed legacy row");
        assert_eq!(listed, vec![legacy]);
    }

    #[test]
    fn placeholder_provider_values_are_rejected() {
        let mut item = provider("placeholder", "your-provider", None);
        item.model = "gpt-5.5".to_string();
        let error = normalize_saved_provider(item).expect_err("placeholder must be rejected");
        assert!(error.to_string().contains("示例占位值"));
    }
}
