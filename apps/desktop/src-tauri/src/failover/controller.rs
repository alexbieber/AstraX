//! Listener, takeover and failover are separate states, as in CC Switch.
//! Recovery journals retain only locally owned transport changes; global Codex
//! configuration and account credentials are managed by the existing switchers.

use super::config::{self, RoutingTuning, MAX_QUEUE, ROUTE_GENERATION_HEADER, ROUTE_TOKEN_HEADER};
use super::native_official;
use super::proxy::{ProxyHandle, ProxyOptions, ProxyRoute, ProxySelectionEvent, ProxySnapshot};
use crate::error::{CodexxError, Result};
use crate::file_io::parse_toml_document;
use crate::live_config::{
    acquire_live_config_lock, atomic_write_if_unchanged, read_file_snapshot,
    restore_file_snapshot_if_unchanged, text_from_snapshot,
};
use crate::paths::normalized_path_scope;
use crate::providers::{
    detected_live_custom_provider, list_saved_providers_on_connection,
    matching_saved_provider_ids_for_live_on_connection, reconcile_active_provider_on_connection,
    SavedProvider,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Once, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;
use toml_edit::{value, DocumentMut, Item, Table};

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static NEXT_LISTENER_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct FailoverSettings {
    pub(crate) version: u32,
    pub(crate) router_enabled: bool,
    pub(crate) takeover_enabled: bool,
    pub(crate) auto_failover_enabled: bool,
    pub(crate) listen_address: String,
    pub(crate) listen_port: u16,
    pub(crate) provider_ids: Vec<String>,
    #[serde(flatten)]
    pub(crate) tuning: RoutingTuning,
}

impl Default for FailoverSettings {
    fn default() -> Self {
        Self {
            version: 2,
            router_enabled: false,
            takeover_enabled: false,
            auto_failover_enabled: false,
            listen_address: config::DEFAULT_LISTEN_ADDRESS.into(),
            listen_port: config::DEFAULT_LISTEN_PORT,
            provider_ids: vec![],
            tuning: RoutingTuning::default(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_address() -> String {
    config::DEFAULT_LISTEN_ADDRESS.into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyJournal {
    primary_id: String,
    provider_key: String,
    original_table: String,
    port: u16,
    token: String,
    #[serde(default)]
    lease_id: Option<String>,
    #[serde(default = "default_address")]
    listen_address: String,
    #[serde(default)]
    official: bool,
    #[serde(default = "default_true")]
    table_existed: bool,
    #[serde(default = "default_true")]
    providers_existed: bool,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Record {
    version: u32,
    settings: FailoverSettings,
    journals: Vec<ProxyJournal>,
    message: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderChoice {
    id: String,
    provider_name: String,
    base_url: String,
    model: String,
    models: Vec<String>,
    eligible: bool,
    reason: Option<String>,
    official: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FailoverStatus {
    settings: FailoverSettings,
    running: bool,
    takeover_active: bool,
    auto_failover_active: bool,
    address: Option<String>,
    primary: Option<ProviderChoice>,
    providers: Vec<ProviderChoice>,
    runtime: ProxySnapshot,
    message: Option<String>,
}

struct Running {
    proxy: ProxyHandle,
    instance_id: u64,
    token: String,
    journal: Option<ProxyJournal>,
    routes: Vec<ProxyRoute>,
    options: ProxyOptions,
}

struct SelectionNotice {
    dir: PathBuf,
    instance_id: u64,
    token: String,
    event: ProxySelectionEvent,
}

fn app_handle() -> &'static OnceLock<tauri::AppHandle> {
    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    &APP
}
pub(crate) fn attach_app_handle(app: tauri::AppHandle) {
    let _ = app_handle().set(app);
}
fn changed(dir: &Path) {
    if let Some(app) = app_handle().get() {
        let _ = app.emit(
            "provider-routing-changed",
            serde_json::json!({"codexDir":dir.display().to_string()}),
        );
    }
}

fn notices() -> &'static (
    mpsc::Sender<SelectionNotice>,
    Mutex<mpsc::Receiver<SelectionNotice>>,
) {
    static NOTICES: OnceLock<(
        mpsc::Sender<SelectionNotice>,
        Mutex<mpsc::Receiver<SelectionNotice>>,
    )> = OnceLock::new();
    NOTICES.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        (sender, Mutex::new(receiver))
    })
}

fn manager() -> &'static Mutex<HashMap<PathBuf, Running>> {
    static MANAGER: OnceLock<Mutex<HashMap<PathBuf, Running>>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(HashMap::new()))
}
fn lock_manager() -> Result<MutexGuard<'static, HashMap<PathBuf, Running>>> {
    manager()
        .lock()
        .map_err(|_| CodexxError::Config("Routing service is busy; please reopen Astra".into()))
}
fn directory(config_dir: Option<String>) -> Result<PathBuf> {
    let path = crate::resolve_codex_dir(config_dir)?;
    Ok(path.canonicalize().unwrap_or(path))
}

fn load_record(dir: &Path) -> Result<Record> {
    let text = crate::app_db::open()?
        .query_row(
            "SELECT record_json FROM provider_failover WHERE codex_dir=?1",
            [normalized_path_scope(dir)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let Some(text) = text else {
        return Ok(Record {
            version: 2,
            ..Record::default()
        });
    };
    let raw: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| CodexxError::Config("Unable to read routing settings; check app data backups".into()))?;
    let mut record: Record = serde_json::from_value(raw.clone())
        .map_err(|_| CodexxError::Config("Routing settings format is invalid".into()))?;
    if raw
        .get("settings")
        .and_then(|s| s.get("routerEnabled"))
        .is_none()
    {
        let enabled = raw
            .get("settings")
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        record.settings.router_enabled = enabled;
        record.settings.takeover_enabled = enabled;
        record.settings.auto_failover_enabled = enabled;
        if let Some(last) = record.journals.last() {
            record.settings.listen_port = last.port;
            record.settings.listen_address = last.listen_address.clone();
            if !record.settings.provider_ids.contains(&last.primary_id) {
                record
                    .settings
                    .provider_ids
                    .insert(0, last.primary_id.clone());
            }
        }
    }
    record.version = 2;
    Ok(record)
}

fn save_record(dir: &Path, record: &Record) -> Result<()> {
    save_record_on_connection(&crate::app_db::open()?, dir, record)
}
fn save_record_on_connection(
    conn: &rusqlite::Connection,
    dir: &Path,
    record: &Record,
) -> Result<()> {
    let text = serde_json::to_string(record)
        .map_err(|_| CodexxError::Config("Unable to save routing settings".into()))?;
    conn.execute("INSERT INTO provider_failover(codex_dir,record_json) VALUES(?1,?2) ON CONFLICT(codex_dir) DO UPDATE SET record_json=excluded.record_json", params![normalized_path_scope(dir),text])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}
fn read_document(dir: &Path) -> Result<(Option<Vec<u8>>, DocumentMut)> {
    let path = crate::config_path(dir);
    let bytes = read_file_snapshot(&path)?;
    let text = text_from_snapshot(&path, bytes.as_deref())?;
    Ok((bytes, parse_toml_document(&path, &text)?))
}
fn provider_table<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Table> {
    doc.get("model_providers")?.as_table()?.get(key)?.as_table()
}
fn journal_url(journal: &ProxyJournal) -> Result<String> {
    Ok(config::local_url(
        config::listen_ip(&journal.listen_address)?,
        journal.port,
    ))
}
fn owned_route(doc: &DocumentMut, journal: &ProxyJournal) -> bool {
    let Some(table) = provider_table(doc, &journal.provider_key) else {
        return false;
    };
    let Ok(url) = journal_url(journal) else {
        return false;
    };
    if table
        .get("base_url")
        .and_then(Item::as_str)
        .map(|s| s.trim_end_matches('/'))
        != Some(url.as_str())
    {
        return false;
    }
    let lease = table
        .get("http_headers")
        .and_then(Item::as_table_like)
        .and_then(|headers| headers.get(ROUTE_GENERATION_HEADER))
        .and_then(Item::as_str);
    if lease != journal.lease_id.as_deref() {
        return false;
    }
    let token = if journal.official {
        table
            .get("http_headers")
            .and_then(Item::as_table_like)
            .and_then(|headers| headers.get(ROUTE_TOKEN_HEADER))
            .and_then(Item::as_str)
    } else {
        table
            .get("experimental_bearer_token")
            .and_then(Item::as_str)
    };
    token == Some(journal.token.as_str())
}
fn selected_owned_route(doc: &DocumentMut, journal: &ProxyJournal) -> bool {
    let selected = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or("openai");
    let Some(table) = provider_table(doc, &journal.provider_key) else {
        return false;
    };
    selected == journal.provider_key
        && owned_route(doc, journal)
        && table
            .get("wire_api")
            .and_then(Item::as_str)
            .unwrap_or("responses")
            == "responses"
        && table.get("supports_websockets").and_then(Item::as_bool) == Some(false)
        && table.get("requires_openai_auth").and_then(Item::as_bool) == Some(journal.official)
}
fn original_table(journal: &ProxyJournal) -> Result<Table> {
    journal
        .original_table
        .parse::<DocumentMut>()
        .map(|doc| doc.as_table().clone())
        .map_err(|_| CodexxError::Config("Direct-connect config backup unreadable; current config was not overwritten".into()))
}
fn proxy_table(original: &Table, journal: &ProxyJournal) -> Result<Table> {
    // The local transport is independent of upstream-specific auth commands,
    // query strings and transport extensions. Restore them from the journal.
    let mut table = Table::new();
    table["name"] = value(if journal.official {
        "OpenAI"
    } else {
        "Astra Local Router"
    });
    table["base_url"] = value(journal_url(journal)?);
    table["wire_api"] = value("responses");
    table["supports_websockets"] = value(false);
    table["requires_openai_auth"] = value(journal.official);
    table["request_max_retries"] = value(0);
    table["stream_max_retries"] = value(0);
    let mut headers = Table::new();
    if journal.official {
        if let Some(existing) = original.get("http_headers").and_then(Item::as_table_like) {
            for (key, item) in existing.iter() {
                headers.insert(key, item.clone());
            }
        }
        headers.insert(ROUTE_TOKEN_HEADER, value(journal.token.clone()));
    } else {
        table["experimental_bearer_token"] = value(journal.token.clone());
    }
    if let Some(lease) = &journal.lease_id {
        headers.insert(ROUTE_GENERATION_HEADER, value(lease.clone()));
    }
    if !headers.is_empty() {
        table.insert("http_headers", Item::Table(headers));
    }
    Ok(table)
}
fn replace_table(doc: &mut DocumentMut, key: &str, table: Table) -> Result<()> {
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    doc.get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| CodexxError::Config("model_providers must be a configuration table".into()))?
        .insert(key, Item::Table(table));
    Ok(())
}
fn same_item(a: Option<&Item>, b: Option<&Item>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) if a.is_str() && b.is_str() => a.as_str() == b.as_str(),
        (Some(a), Some(b)) if a.is_bool() && b.is_bool() => a.as_bool() == b.as_bool(),
        (Some(a), Some(b)) if a.is_integer() && b.is_integer() => a.as_integer() == b.as_integer(),
        (Some(a), Some(b)) if a.as_table_like().is_some() && b.as_table_like().is_some() => {
            let a = a.as_table_like().unwrap();
            let b = b.as_table_like().unwrap();
            a.len() == b.len()
                && a.iter()
                    .all(|(key, item)| same_item(Some(item), b.get(key)))
        }
        (Some(a), Some(b)) => a.to_string() == b.to_string(),
        _ => false,
    }
}
fn restore_projected_item(
    original: Option<&Item>,
    current: Option<&Item>,
    installed: Option<&Item>,
) -> Option<Item> {
    if same_item(current, installed) {
        return original.cloned();
    }
    if installed.is_none() {
        return current.or(original).cloned();
    }
    let current = current?;
    if let (Some(current_table), Some(installed_table)) = (
        current.as_table_like(),
        installed.and_then(Item::as_table_like),
    ) {
        let original_table = original.and_then(Item::as_table_like);
        let mut result = Table::new();
        let mut keys = HashSet::new();
        if let Some(table) = original_table {
            keys.extend(table.iter().map(|(key, _)| key.to_owned()));
        }
        keys.extend(current_table.iter().map(|(key, _)| key.to_owned()));
        keys.extend(installed_table.iter().map(|(key, _)| key.to_owned()));
        for key in keys {
            if matches!(key.as_str(), ROUTE_TOKEN_HEADER | ROUTE_GENERATION_HEADER) {
                continue;
            }
            if let Some(item) = restore_projected_item(
                original_table.and_then(|table| table.get(&key)),
                current_table.get(&key),
                installed_table.get(&key),
            ) {
                result.insert(&key, item);
            }
        }
        if result.is_empty() && original.is_none() {
            None
        } else {
            Some(Item::Table(result))
        }
    } else {
        Some(current.clone())
    }
}
fn restored_document(doc: &DocumentMut, journal: &ProxyJournal) -> Result<DocumentMut> {
    let mut restored = doc.clone();
    if !owned_route(doc, journal) {
        return Ok(restored);
    }
    let original = original_table(journal)?;
    let expected = proxy_table(&original, journal)?;
    let live = provider_table(doc, &journal.provider_key).expect("owned route");
    let mut current = original.clone();
    let mut keys = HashSet::new();
    keys.extend(live.iter().map(|(key, _)| key.to_owned()));
    keys.extend(expected.iter().map(|(key, _)| key.to_owned()));
    for key in keys {
        let item = if matches!(key.as_str(), "base_url" | "experimental_bearer_token") {
            original.get(&key).cloned()
        } else {
            restore_projected_item(original.get(&key), live.get(&key), expected.get(&key))
        };
        match item {
            Some(item) => {
                current.insert(&key, item);
            }
            None => {
                current.remove(&key);
            }
        }
    }
    if !journal.table_existed {
        // Remove the generated built-in override only when no external fields
        // were added to it. Otherwise keep those user's changes without our URL.
        if current.is_empty() {
            if let Some(providers) = restored
                .get_mut("model_providers")
                .and_then(Item::as_table_mut)
            {
                providers.remove(&journal.provider_key);
            }
            if !journal.providers_existed
                && restored
                    .get("model_providers")
                    .and_then(Item::as_table)
                    .is_some_and(Table::is_empty)
            {
                restored.as_table_mut().remove("model_providers");
            }
            return Ok(restored);
        }
    }
    replace_table(&mut restored, &journal.provider_key, current)?;
    Ok(restored)
}

pub(crate) fn direct_document(dir: &Path, doc: &DocumentMut) -> Result<DocumentMut> {
    let mut direct = doc.clone();
    let record = load_record(dir)?;
    for journal in record.journals.iter().rev() {
        if owned_route(&direct, journal) {
            direct = restored_document(&direct, journal)?;
        }
    }
    // A concurrent switch can publish the next generation just before its DB
    // transaction commits. Never treat that managed endpoint as a new supplier.
    for journal in &record.journals {
        let Some(table) = provider_table(&direct, &journal.provider_key) else {
            continue;
        };
        if table
            .get("base_url")
            .and_then(Item::as_str)
            .is_some_and(|url| {
                journal_url(journal).is_ok_and(|expected| url.trim_end_matches('/') == expected)
            })
        {
            let token = if journal.official {
                table
                    .get("http_headers")
                    .and_then(Item::as_table_like)
                    .and_then(|headers| headers.get(ROUTE_TOKEN_HEADER))
                    .and_then(Item::as_str)
            } else {
                table
                    .get("experimental_bearer_token")
                    .and_then(Item::as_str)
            };
            if token == Some(journal.token.as_str()) {
                return Err(CodexxError::Config(
                    "Routing config is updating or the restore marker changed; please try again later".into(),
                ));
            }
        }
    }
    Ok(direct)
}
fn restore_owned_locked(dir: &Path, record: &Record) -> Result<()> {
    let (before, mut doc) = read_document(dir)?;
    let mut changed = false;
    for journal in record.journals.iter().rev() {
        if owned_route(&doc, journal) {
            doc = restored_document(&doc, journal)?;
            changed = true;
        }
    }
    if changed {
        atomic_write_if_unchanged(
            &crate::config_path(dir),
            before.as_deref(),
            doc.to_string().as_bytes(),
        )?;
    }
    Ok(())
}

fn provider_models(provider: &SavedProvider) -> Vec<String> {
    let mut values = vec![provider.model.trim().to_owned()];
    values.extend(
        provider
            .model_mappings
            .iter()
            .map(|row| row.model.trim().to_owned()),
    );
    values.retain(|model| !model.is_empty());
    values.sort();
    values.dedup();
    values
}

fn route(provider: &SavedProvider) -> Result<ProxyRoute> {
    if provider.wire_api != "responses" {
        return Err(CodexxError::Config("A provider that supports Responses is required".into()));
    }
    let url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|_| CodexxError::Config("Invalid provider URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || matches!(url.host_str(), Some("chatgpt.com" | "chat.openai.com"))
    {
        return Err(CodexxError::Config(
            "A third-party API URL is required; official login is not used for auto-failover".into(),
        ));
    }
    let doc = provider
        .toml_config
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            text.parse::<DocumentMut>()
                .map_err(|_| CodexxError::Config("Provider config format is invalid".into()))
        })
        .transpose()?;
    let table = doc.as_ref().and_then(|doc| {
        let key = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .unwrap_or("custom");
        provider_table(doc, key)
    });
    if table
        .and_then(|table| table.get("query_params"))
        .and_then(Item::as_table_like)
        .is_some_and(|params| !params.is_empty())
    {
        // Client-generated query parameters belong to the primary provider and
        // may contain credentials. Do not reuse them with a different supplier.
        return Err(CodexxError::Config(
            "Providers with custom query parameters do not support auto-failover yet".into(),
        ));
    }
    let mut api_key = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            table?
                .get("experimental_bearer_token")?
                .as_str()
                .filter(|key| !key.trim().is_empty())
                .map(str::to_owned)
        });
    if api_key.is_none() {
        if let Some(env_key) = table
            .and_then(|table| table.get("env_key"))
            .and_then(Item::as_str)
        {
            api_key = Some(
                std::env::var(env_key)
                    .ok()
                    .filter(|key| !key.trim().is_empty())
                    .ok_or_else(|| {
                        CodexxError::Config("Environment variable required for the provider API key is unavailable".into())
                    })?,
            );
        }
    }
    if provider.requires_openai_auth && api_key.is_none() {
        return Err(CodexxError::Config("Please enter the API key for this provider first".into()));
    }
    if let Some(key) = &api_key {
        reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| CodexxError::Config("Provider API key format is invalid".into()))?;
    }
    let mut headers = Vec::new();
    if let Some(table) = table {
        for (field, from_env) in [("http_headers", false), ("env_http_headers", true)] {
            if let Some(values) = table.get(field).and_then(Item::as_table_like) {
                for (key, item) in values.iter() {
                    if matches!(
                        key.to_ascii_lowercase().as_str(),
                        "host"
                            | "connection"
                            | "content-length"
                            | "transfer-encoding"
                            | "cookie"
                            | "authorization"
                            | "proxy-authorization"
                            | "origin"
                    ) {
                        return Err(CodexxError::Config(
                            "This provider has custom auth or connection headers that are not compatible with auto-failover".into(),
                        ));
                    }
                    let text = item
                        .as_str()
                        .ok_or_else(|| CodexxError::Config("Provider request headers must be text values".into()))?;
                    let text = if from_env {
                        std::env::var(text).map_err(|_| {
                            CodexxError::Config("Environment variable required for provider request headers is unavailable".into())
                        })?
                    } else {
                        text.to_owned()
                    };
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())
                        .map_err(|_| CodexxError::Config("Invalid provider request header name".into()))?;
                    reqwest::header::HeaderValue::from_str(&text)
                        .map_err(|_| CodexxError::Config("Invalid provider request header value".into()))?;
                    headers.push((key.to_owned(), text));
                }
            }
        }
    }
    Ok(ProxyRoute {
        official: None,
        id: provider.id.clone(),
        name: provider.provider_name.clone(),
        base_url: provider.base_url.clone(),
        api_key,
        headers,
        models: provider_models(provider).into_iter().collect(),
    })
}

fn current_and_saved(dir: &Path) -> Result<(Option<SavedProvider>, Vec<SavedProvider>)> {
    let conn = crate::app_db::open()?;
    let saved = list_saved_providers_on_connection(&conn)?;
    let primary = if let Some(live) = detected_live_custom_provider(dir)? {
        let candidates =
            matching_saved_provider_ids_for_live_on_connection(&conn, dir, &live, &saved)?;
        let selected = reconcile_active_provider_on_connection(&conn, dir, &candidates)?;
        selected
            .and_then(|id| saved.iter().find(|item| item.id == id).cloned())
            .map(|mut provider| {
                // Use the selected live model, including a manually configured model.
                provider.model = live.model;
                provider.base_url = live.base_url;
                provider.api_key = live.api_key;
                provider.wire_api = live.wire_api;
                provider.requires_openai_auth = live.requires_openai_auth;
                provider.toml_config = live.toml_config;
                provider
            })
    } else {
        None
    };
    Ok((primary, saved))
}

fn safe_base_url(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.into()
        }
        Err(_) => String::new(),
    }
}
fn choice(provider: &SavedProvider) -> ProviderChoice {
    let reason = route(provider).err().map(|error| error.to_string());
    ProviderChoice {
        id: provider.id.clone(),
        provider_name: provider.provider_name.clone(),
        base_url: safe_base_url(&provider.base_url),
        model: provider.model.clone(),
        models: provider_models(provider),
        eligible: reason.is_none(),
        reason,
        official: false,
    }
}
fn current_route(dir: &Path) -> Result<Option<ProxyRoute>> {
    let (primary, _) = current_and_saved(dir)?;
    if let Some(primary) = primary {
        return route(&primary).map(Some);
    }
    if let Some(official) = native_official::route_for_current(dir)? {
        return Ok(Some(official));
    }
    if let Some(mut live) = detected_live_custom_provider(dir)? {
        live.id = format!("live:{}", live.id);
        return route(&live).map(Some);
    }
    Ok(None)
}
fn queue_routes(settings: &FailoverSettings) -> Result<Vec<ProxyRoute>> {
    let saved = list_saved_providers_on_connection(&crate::app_db::open()?)?;
    Ok(settings
        .provider_ids
        .iter()
        .filter_map(|id| saved.iter().find(|provider| &provider.id == id))
        .filter_map(|provider| route(provider).ok())
        .collect())
}
fn route_plan(dir: &Path, settings: &FailoverSettings) -> Result<(Vec<ProxyRoute>, ProxyOptions)> {
    let options = ProxyOptions {
        auto_failover_enabled: false,
        tuning: settings.tuning,
    };
    if !settings.takeover_enabled {
        return Ok((vec![], options));
    }
    let primary = current_route(dir)?
        .ok_or_else(|| CodexxError::Config("Please configure a Codex provider first".into()))?;
    if primary.official.is_some() || !settings.auto_failover_enabled {
        return Ok((vec![primary], options));
    }
    Ok((
        queue_routes(settings)?,
        ProxyOptions {
            auto_failover_enabled: true,
            tuning: settings.tuning,
        },
    ))
}
fn normalized_settings(mut settings: FailoverSettings) -> Result<FailoverSettings> {
    settings.version = 2;
    settings.listen_address = config::listen_ip(&settings.listen_address)?.to_string();
    if settings.listen_port < 1024 {
        return Err(CodexxError::Config("Listen port must be between 1024 and 65535".into()));
    }
    settings.tuning.validate()?;
    if settings.provider_ids.len() > MAX_QUEUE {
        return Err(CodexxError::Config(format!(
            "Queue supports at most {MAX_QUEUE} providers"
        )));
    }
    let mut seen = HashSet::new();
    for id in &mut settings.provider_ids {
        *id = id.trim().to_owned();
        if id.is_empty() || !seen.insert(id.clone()) || id.starts_with("official:") {
            return Err(CodexxError::Config(
                "Queue contains duplicate, invalid, or official accounts; please reselect third-party providers".into(),
            ));
        }
    }
    if !settings.router_enabled {
        settings.takeover_enabled = false;
    }
    Ok(settings)
}
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| CodexxError::Config("Unable to create local routing credentials".into()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn start_listener(dir: &Path, record: &Record) -> Result<Running> {
    let address = config::listen_ip(&record.settings.listen_address)?;
    let token = record
        .journals
        .last()
        .filter(|j| {
            j.port == record.settings.listen_port
                && config::listen_ip(&j.listen_address).ok() == Some(address)
        })
        .map(|j| j.token.clone())
        .map(Ok)
        .unwrap_or_else(random_token)?;
    let options = ProxyOptions {
        auto_failover_enabled: false,
        tuning: record.settings.tuning,
    };
    let proxy = ProxyHandle::start(
        address,
        record.settings.listen_port,
        token.clone(),
        vec![],
        options.clone(),
    )?;
    let instance_id = NEXT_LISTENER_INSTANCE.fetch_add(1, Ordering::Relaxed);
    let scope = dir.to_owned();
    let callback_token = token.clone();
    let sender = notices().0.clone();
    proxy.set_selection_callback(Arc::new(move |event| {
        let _ = sender.send(SelectionNotice {
            dir: scope.clone(),
            instance_id,
            token: callback_token.clone(),
            event,
        });
    }));
    ensure_watcher();
    Ok(Running {
        proxy,
        instance_id,
        token,
        journal: None,
        routes: vec![],
        options,
    })
}
fn update_runtime(
    runtime: &mut Running,
    routes: Vec<ProxyRoute>,
    options: ProxyOptions,
) -> Result<()> {
    runtime.proxy.configure(routes.clone(), options.clone())?;
    runtime.routes = routes;
    runtime.options = options;
    Ok(())
}
fn save_journal(record: &mut Record, journal: ProxyJournal) {
    if let Some(existing) = record.journals.iter_mut().find(|j| {
        j.token == journal.token
            && j.provider_key == journal.provider_key
            && j.lease_id == journal.lease_id
    }) {
        *existing = journal;
    } else {
        record.journals.push(journal);
    }
}
fn attach_route(dir: &Path, record: &mut Record, runtime: &mut Running) -> Result<()> {
    let _guard = acquire_live_config_lock(dir)?;
    let (before, raw) = read_document(dir)?;
    let mut direct = direct_document(dir, &raw)?;
    let primary = current_route(dir)?
        .ok_or_else(|| CodexxError::Config("No Codex provider is available to take over".into()))?;
    let (routes, options) = route_plan(dir, &record.settings)?;
    let key = direct
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or("openai")
        .to_owned();
    let original = provider_table(&direct, &key).cloned();
    let mut original_doc = DocumentMut::new();
    *original_doc.as_table_mut() = original.clone().unwrap_or_default();
    runtime.proxy.validate_configuration(&routes, &options)?;
    let journal = ProxyJournal {
        primary_id: primary.id,
        provider_key: key.clone(),
        original_table: original_doc.to_string(),
        port: runtime.proxy.port(),
        token: runtime.token.clone(),
        lease_id: Some(random_token()?),
        listen_address: runtime.proxy.listen_address().to_string(),
        official: primary.official.is_some(),
        table_existed: original.is_some(),
        providers_existed: direct.get("model_providers").is_some(),
    };
    replace_table(
        &mut direct,
        &key,
        proxy_table(original_doc.as_table(), &journal)?,
    )?;
    let previous = load_record(dir)?;
    crate::create_backup(dir, "enable-local-routing")?;
    save_journal(record, journal.clone());
    save_record(dir, record)?;
    let replacement = direct.to_string();
    if let Err(error) = atomic_write_if_unchanged(
        &crate::config_path(dir),
        before.as_deref(),
        replacement.as_bytes(),
    ) {
        save_record(dir, &previous)?;
        return Err(error);
    }
    if let Err(error) = update_runtime(runtime, routes, options) {
        restore_file_snapshot_if_unchanged(
            &crate::config_path(dir),
            Some(replacement.as_bytes()),
            before.as_deref(),
        )?;
        save_record(dir, &previous)?;
        return Err(error);
    }
    runtime.journal = Some(journal);
    Ok(())
}
fn detach_route(dir: &Path, record: &Record, runtime: &mut Running) -> Result<()> {
    if !record.journals.is_empty() && dir.exists() {
        let _guard = acquire_live_config_lock(dir)?;
        restore_owned_locked(dir, record)?;
        let doc = read_document(dir)?.1;
        let key = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .unwrap_or("openai");
        if provider_table(&doc, key)
            .and_then(|table| table.get("base_url"))
            .and_then(Item::as_str)
            .is_some_and(|url| {
                url.trim_end_matches('/')
                    == config::local_url(runtime.proxy.listen_address(), runtime.proxy.port())
            })
        {
            return Err(CodexxError::Config(
                "Local routing config was modified; cannot confirm restore target. Service is still running — check config.toml first".into(),
            ));
        }
    }
    update_runtime(
        runtime,
        vec![],
        ProxyOptions {
            auto_failover_enabled: false,
            tuning: record.settings.tuning,
        },
    )?;
    runtime.journal = None;
    Ok(())
}
fn inspect_external_change(dir: &Path, record: &mut Record, runtime: &mut Running) -> Result<()> {
    let Some(journal) = runtime.journal.as_ref() else {
        return Ok(());
    };
    if !selected_owned_route(&read_document(dir)?.1, journal) {
        // Respect an external/manual route change. Keep the listener available,
        // but do not quietly take over a newly selected account.
        detach_route(dir, record, runtime)?;
        record.settings.takeover_enabled = false;
        record.message = Some("Codex config changed externally; takeover stopped. Local routing service is still running.".into());
        save_record(dir, record)?;
        changed(dir);
    }
    Ok(())
}

#[derive(Clone)]
struct FileCheckpoint {
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
    selected: Option<String>,
    common_handled: Option<String>,
}
impl FileCheckpoint {
    fn capture(dir: &Path) -> Result<Self> {
        let conn = crate::app_db::open()?;
        let selected = conn
            .query_row(
                "SELECT provider_id FROM active_provider_selections WHERE codex_dir=?1",
                [normalized_path_scope(dir)],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let common_handled = conn
            .query_row(
                "SELECT handled_at FROM provider_common_config_state WHERE codex_dir=?1",
                [normalized_path_scope(dir)],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        Ok(Self {
            config: read_file_snapshot(&crate::config_path(dir))?,
            auth: read_file_snapshot(&crate::auth_path(dir))?,
            selected,
            common_handled,
        })
    }
    fn restore(&self, dir: &Path, expected: &Self) -> Result<()> {
        let _guard = acquire_live_config_lock(dir)?;
        crate::live_config::ensure_file_snapshot_unchanged(
            &crate::config_path(dir),
            expected.config.as_deref(),
        )?;
        crate::live_config::ensure_file_snapshot_unchanged(
            &crate::auth_path(dir),
            expected.auth.as_deref(),
        )?;
        // P1 activation can fail before the local overlay is installed. Apply
        // the same ordering as a normal provider switch: never restore OAuth
        // while the prepared third-party endpoint can still read auth.json.
        let restore_auth = || {
            restore_file_snapshot_if_unchanged(
                &crate::auth_path(dir),
                expected.auth.as_deref(),
                self.auth.as_deref(),
            )
        };
        let restore_config = || {
            restore_file_snapshot_if_unchanged(
                &crate::config_path(dir),
                expected.config.as_deref(),
                self.config.as_deref(),
            )
        };
        match crate::providers::replacement_write_order(
            expected.config.as_deref(),
            self.config.as_deref(),
        ) {
            crate::providers::LiveWriteOrder::ConfigFirst => {
                restore_config()?;
                restore_auth()?;
            }
            crate::providers::LiveWriteOrder::AuthFirst => {
                restore_auth()?;
                restore_config()?;
            }
        }
        let conn = crate::app_db::open()?;
        if let Some(id) = &self.selected {
            crate::providers::remember_active_provider_on_connection(&conn, dir, id)?;
        } else {
            crate::providers::clear_active_provider_on_connection(&conn, dir)?;
        }
        if self.common_handled != expected.common_handled {
            let current: Option<String> = conn
                .query_row(
                    "SELECT handled_at FROM provider_common_config_state WHERE codex_dir=?1",
                    [normalized_path_scope(dir)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            if current == expected.common_handled {
                if let Some(value)=&self.common_handled {conn.execute("INSERT OR REPLACE INTO provider_common_config_state(codex_dir,handled_at) VALUES(?1,?2)",params![normalized_path_scope(dir),value])}
                else {conn.execute("DELETE FROM provider_common_config_state WHERE codex_dir=?1",[normalized_path_scope(dir)])}.map_err(|e|CodexxError::Database(e.to_string()))?;
            }
        }
        Ok(())
    }
}
fn switch_to_p1(dir: &Path, settings: &mut FailoverSettings) -> Result<()> {
    let saved = list_saved_providers_on_connection(&crate::app_db::open()?)?;
    if settings.provider_ids.is_empty() {
        let primary = current_route(dir)?
            .filter(|route| route.official.is_none())
            .ok_or_else(|| CodexxError::Config("Please add third-party providers to the failover queue first".into()))?;
        if !saved.iter().any(|provider| provider.id == primary.id) {
            return Err(CodexxError::Config(
                "Please save the current provider, or select a saved P1 provider".into(),
            ));
        }
        settings.provider_ids.push(primary.id);
    }
    let id = settings.provider_ids[0].clone();
    let provider = saved
        .iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| CodexxError::Config("P1 provider no longer exists; please update the queue".into()))?;
    route(provider)?;
    if current_route(dir)?.is_none_or(|current| current.id != id) {
        crate::providers::activate_saved_provider_inner(
            Some(dir.display().to_string()),
            id.clone(),
        )?;
    }
    crate::providers::remember_active_provider_on_connection(&crate::app_db::open()?, dir, &id)?;
    Ok(())
}

fn status_locked(dir: &Path, record: Record, runtime: Option<&Running>) -> Result<FailoverStatus> {
    let (primary, saved) = current_and_saved(dir)?;
    let primary = if let Some(provider) = primary {
        Some(choice(&provider))
    } else {
        match current_route(dir) {
            Ok(Some(route)) => {
                let mut models: Vec<_> = route.models.iter().cloned().collect();
                models.sort();
                let model = read_document(dir)?
                    .1
                    .get("model")
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some(ProviderChoice {
                    id: route.id,
                    provider_name: route.name,
                    base_url: if route.official.is_some() {
                        "https://chatgpt.com/codex".into()
                    } else {
                        safe_base_url(&route.base_url)
                    },
                    model,
                    models,
                    eligible: true,
                    reason: None,
                    official: route.official.is_some(),
                })
            }
            Ok(None) | Err(_) => None,
        }
    };
    Ok(FailoverStatus {
        settings: record.settings,
        running: runtime.is_some(),
        takeover_active: runtime.is_some_and(|r| r.journal.is_some()),
        auto_failover_active: runtime
            .is_some_and(|r| r.journal.is_some() && r.options.auto_failover_enabled),
        address: runtime.map(|r| config::local_url(r.proxy.listen_address(), r.proxy.port())),
        primary,
        providers: saved.iter().map(choice).collect(),
        runtime: runtime.map(|r| r.proxy.snapshot()).unwrap_or_default(),
        message: record.message,
    })
}
pub(crate) fn get_status(config_dir: Option<String>) -> Result<FailoverStatus> {
    let dir = directory(config_dir)?;
    let mut runtimes = lock_manager()?;
    let mut record = load_record(&dir)?;
    if let Some(runtime) = runtimes.get_mut(&dir) {
        inspect_external_change(&dir, &mut record, runtime)?;
    }
    status_locked(&dir, record, runtimes.get(&dir))
}

pub(crate) fn save_settings(
    config_dir: Option<String>,
    settings: FailoverSettings,
) -> Result<FailoverStatus> {
    let dir = directory(config_dir)?;
    let mut settings = normalized_settings(settings)?;
    let mut runtimes = lock_manager()?;
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(CodexxError::Config(
            "Astra is exiting; routing settings were not changed".into(),
        ));
    }
    let mut old = load_record(&dir)?;
    if let Some(runtime) = runtimes.get_mut(&dir) {
        inspect_external_change(&dir, &mut old, runtime)?;
    }
    if let Some(runtime) = runtimes.get(&dir) {
        if settings.router_enabled
            && (config::listen_ip(&settings.listen_address)? != runtime.proxy.listen_address()
                || settings.listen_port != runtime.proxy.port())
        {
            return Err(CodexxError::Config(
                "Please stop the routing service before changing the listen address or port".into(),
            ));
        }
    }
    let enable_auto = settings.auto_failover_enabled && !old.settings.auto_failover_enabled;
    if enable_auto && (!settings.router_enabled || !settings.takeover_enabled) {
        return Err(CodexxError::Config(
            "Please enable the routing service and Codex request takeover first".into(),
        ));
    }
    let before = FileCheckpoint::capture(&dir)?;
    let mut expected = before.clone();
    let mut runtime = runtimes.remove(&dir);
    let was_running = runtime.is_some();
    let old_journal = runtime.as_ref().and_then(|r| r.journal.clone());
    let old_routes = runtime
        .as_ref()
        .map(|r| r.routes.clone())
        .unwrap_or_default();
    let old_options = runtime
        .as_ref()
        .map(|r| r.options.clone())
        .unwrap_or(ProxyOptions {
            auto_failover_enabled: false,
            tuning: old.settings.tuning,
        });
    let mut record = old.clone();
    record.settings = settings.clone();
    record.message = None;
    let result = (|| -> Result<()> {
        if !settings.router_enabled {
            if let Some(runtime) = runtime.as_mut() {
                detach_route(&dir, &old, runtime)?;
            }
            expected = FileCheckpoint::capture(&dir)?;
            record.settings.takeover_enabled = false;
            save_record(&dir, &record)?;
            if let Some(runtime) = runtime.take() {
                runtime.proxy.shutdown();
            }
            return Ok(());
        }
        if runtime.is_none() {
            runtime = Some(start_listener(&dir, &record)?);
        }
        let active = runtime.as_mut().expect("started listener");
        // Fail before changing P1 when the settings store is not writable.
        save_record(&dir, &record)?;
        if enable_auto {
            detach_route(&dir, &old, active)?;
            expected = FileCheckpoint::capture(&dir)?;
            switch_to_p1(&dir, &mut settings)?;
            expected = FileCheckpoint::capture(&dir)?;
            record.settings = settings.clone();
        }
        if settings.takeover_enabled {
            if active.journal.is_none() {
                attach_route(&dir, &mut record, active)?;
            } else {
                let (routes, options) = route_plan(&dir, &settings)?;
                update_runtime(active, routes, options)?;
                save_record(&dir, &record)?;
            }
        } else {
            detach_route(&dir, &old, active)?;
            save_record(&dir, &record)?;
        }
        expected = FileCheckpoint::capture(&dir)?;
        Ok(())
    })();
    if let Err(error) = result {
        let restored = before
            .restore(&dir, &expected)
            .and_then(|()| save_record(&dir, &old));
        if let Some(active) = runtime.as_mut() {
            if was_running {
                active.journal = old_journal;
                let _ = update_runtime(active, old_routes, old_options);
            } else if restored.is_ok() {
                active.proxy.shutdown();
                runtime = None;
            }
        }
        if let Some(runtime) = runtime {
            runtimes.insert(dir.clone(), runtime);
        }
        return match restored {
            Ok(()) => Err(error),
            Err(rollback) => Err(CodexxError::Config(format!(
                "{error}; config restore did not finish; routing service remains available: {rollback}"
            ))),
        };
    }
    if let Some(runtime) = runtime {
        runtimes.insert(dir.clone(), runtime);
    }
    changed(&dir);
    status_locked(&dir, record, runtimes.get(&dir))
}

pub(crate) fn with_provider_change<T>(
    config_dir: Option<String>,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let dir = directory(config_dir)?;
    let mut runtimes = lock_manager()?;
    let mut record = load_record(&dir)?;
    let Some(runtime) = runtimes.get_mut(&dir) else {
        drop(runtimes);
        recover_stale_route(Some(dir.display().to_string()))?;
        return action();
    };
    let reconnect = runtime.journal.is_some() && record.settings.takeover_enabled;
    if reconnect {
        detach_route(&dir, &record, runtime)?;
    }
    let result = action();
    if reconnect {
        if let Err(error) = attach_route(&dir, &mut record, runtime) {
            record.settings.takeover_enabled = false;
            record.message = Some(format!(
                "Provider operation finished, but re-takeover failed; using direct connection: {error}"
            ));
            save_record(&dir, &record)?;
            changed(&dir);
            return match result {
                Ok(_) => Err(CodexxError::Config(record.message.unwrap())),
                Err(original) => Err(original),
            };
        }
    }
    changed(&dir);
    result
}

pub(crate) fn recover_stale_route(config_dir: Option<String>) -> Result<()> {
    let dir = directory(config_dir)?;
    let record = load_record(&dir)?;
    if dir.exists() && !record.journals.is_empty() {
        let _guard = acquire_live_config_lock(&dir)?;
        restore_owned_locked(&dir, &record)?;
    }
    Ok(())
}
fn refresh_routes_locked(dir: &Path, record: &mut Record, runtime: &mut Running) -> Result<()> {
    inspect_external_change(dir, record, runtime)?;
    if runtime.journal.is_none() {
        return Ok(());
    }
    match route_plan(dir, &record.settings)
        .and_then(|(routes, options)| update_runtime(runtime, routes, options))
    {
        Ok(()) => Ok(()),
        Err(error) => {
            detach_route(dir, record, runtime)?;
            record.settings.takeover_enabled = false;
            record.message = Some(format!("Current provider config is unavailable; restored direct connection: {error}"));
            save_record(dir, record)?;
            changed(dir);
            Ok(())
        }
    }
}
pub(crate) fn refresh_saved_routes() -> Result<()> {
    let mut runtimes = lock_manager()?;
    for (dir, runtime) in runtimes.iter_mut() {
        let mut record = load_record(dir)?;
        refresh_routes_locked(dir, &mut record, runtime)?;
    }
    Ok(())
}
pub(crate) fn reset_health(
    config_dir: Option<String>,
    provider_id: Option<String>,
) -> Result<FailoverStatus> {
    let dir = directory(config_dir)?;
    let runtimes = lock_manager()?;
    if let Some(runtime) = runtimes.get(&dir) {
        runtime.proxy.reset_breakers(provider_id.as_deref());
    }
    status_locked(&dir, load_record(&dir)?, runtimes.get(&dir))
}

fn record_selection(
    notice: SelectionNotice,
    runtimes: &mut HashMap<PathBuf, Running>,
) -> Result<()> {
    let Some(runtime) = runtimes.get_mut(&notice.dir) else {
        return Ok(());
    };
    if runtime.instance_id != notice.instance_id
        || runtime.token != notice.token
        || runtime.proxy.revision() != notice.event.revision
        || !runtime.options.auto_failover_enabled
    {
        return Ok(());
    }
    let Some(journal) = runtime.journal.as_mut() else {
        return Ok(());
    };
    if journal.official || journal.primary_id == notice.event.provider_id {
        return Ok(());
    }
    let mut record = load_record(&notice.dir)?;
    if !record
        .settings
        .provider_ids
        .contains(&notice.event.provider_id)
    {
        return Ok(());
    }
    if !selected_owned_route(&read_document(&notice.dir)?.1, journal) {
        return Ok(());
    }
    let conn = crate::app_db::open()?;
    let saved = list_saved_providers_on_connection(&conn)?;
    let Some(provider) = saved
        .into_iter()
        .find(|provider| provider.id == notice.event.provider_id)
    else {
        return Ok(());
    };
    let draft = crate::providers::build_provider_toml_draft_inner(
        provider.clone(),
        Some(notice.dir.display().to_string()),
    )?;
    let doc = parse_toml_document(&crate::config_path(&notice.dir), &draft)?;
    let key = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or("custom");
    let mut table = provider_table(&doc, key)
        .cloned()
        .ok_or_else(|| CodexxError::Config("Failover target provider config does not exist".into()))?;
    if let Some(api_key) = provider.api_key.as_deref().filter(|s| !s.is_empty()) {
        table["experimental_bearer_token"] = value(api_key);
        table["requires_openai_auth"] = value(false);
    }
    let mut original = DocumentMut::new();
    *original.as_table_mut() = table;
    let mut next = journal.clone();
    next.primary_id = provider.id.clone();
    next.original_table = original.to_string();
    next.lease_id = Some(random_token()?);
    let _guard = acquire_live_config_lock(&notice.dir)?;
    let (before, mut doc) = read_document(&notice.dir)?;
    if !selected_owned_route(&doc, journal) {
        return Ok(());
    }
    let table = doc
        .get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .and_then(|providers| providers.get_mut(&next.provider_key))
        .and_then(Item::as_table_mut)
        .expect("owned table");
    if table.get("http_headers").is_none() {
        table["http_headers"] = Item::Table(Table::new());
    }
    table
        .get_mut("http_headers")
        .and_then(Item::as_table_like_mut)
        .expect("owned headers")
        .insert(
            ROUTE_GENERATION_HEADER,
            value(next.lease_id.as_ref().unwrap().clone()),
        );
    let transaction = conn
        .unchecked_transaction()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    save_journal(&mut record, next.clone());
    save_record_on_connection(&transaction, &notice.dir, &record)?;
    crate::providers::remember_active_provider_on_connection(
        &transaction,
        &notice.dir,
        &provider.id,
    )?;
    let replacement = doc.to_string();
    atomic_write_if_unchanged(
        &crate::config_path(&notice.dir),
        before.as_deref(),
        replacement.as_bytes(),
    )?;
    if let Err(error) = transaction.commit() {
        restore_file_snapshot_if_unchanged(
            &crate::config_path(&notice.dir),
            Some(replacement.as_bytes()),
            before.as_deref(),
        )?;
        return Err(CodexxError::Database(error.to_string()));
    }
    *journal = next;
    changed(&notice.dir);
    Ok(())
}
fn stored_directories() -> Result<Vec<PathBuf>> {
    let conn = crate::app_db::open()?;
    let mut stmt = conn
        .prepare("SELECT codex_dir FROM provider_failover")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| CodexxError::Database(e.to_string()))?
        .map(|row| {
            let scope = row.map_err(|e| CodexxError::Database(e.to_string()))?;
            // Database scopes use lowercase forward slashes on Windows. They
            // are comparison keys, not the canonical native paths used by the
            // running-instance map (including Windows' verbatim \\?\ prefix).
            // Rehydrate using the same path resolution as interactive IPC.
            #[cfg(target_os = "windows")]
            let scope = scope.replace('/', "\\");
            directory(Some(scope))
        })
        .collect();
    paths
}
pub(crate) fn initialize() -> Result<()> {
    let mut runtimes = lock_manager()?;
    for dir in stored_directories()? {
        if runtimes.contains_key(&dir) {
            continue;
        }
        let mut record = load_record(&dir)?;
        let desired = record.settings.takeover_enabled;
        if !record.journals.is_empty() && dir.exists() {
            let _guard = acquire_live_config_lock(&dir)?;
            restore_owned_locked(&dir, &record)?;
        }
        if !record.settings.router_enabled {
            record.settings.takeover_enabled = false;
            save_record(&dir, &record)?;
            continue;
        }
        match normalized_settings(record.settings.clone()) {
            Ok(settings) => record.settings = settings,
            Err(error) => {
                record.settings.router_enabled = false;
                record.settings.takeover_enabled = false;
                record.message = Some(format!("Routing settings need review; using direct connection: {error}"));
                save_record(&dir, &record)?;
                continue;
            }
        }
        if let Some(last) = record.journals.last() {
            if desired && current_route(&dir)?.is_none_or(|route| route.id != last.primary_id) {
                record.settings.takeover_enabled = false;
                record.message =
                    Some("Current provider changed externally; kept direct connection. Re-enable takeover when needed.".into());
            }
        }
        let started = start_listener(&dir, &record);
        match started {
            Ok(mut runtime) => {
                if record.settings.takeover_enabled {
                    if let Err(error) = attach_route(&dir, &mut record, &mut runtime) {
                        record.settings.takeover_enabled = false;
                        record.message = Some(format!("Re-takeover failed; using direct connection: {error}"));
                    }
                }
                // Once the live configuration points here, a failed status
                // write must not drop the only listener serving that address.
                let saved = save_record(&dir, &record);
                runtimes.insert(dir, runtime);
                saved?;
            }
            Err(error) => {
                record.settings.router_enabled = false;
                record.settings.takeover_enabled = false;
                record.message = Some(format!("Routing is not started; using direct connection: {error}"));
                save_record(&dir, &record)?;
            }
        }
    }
    Ok(())
}
fn ensure_watcher() {
    static WATCHER: Once = Once::new();
    WATCHER.call_once(|| {
        std::thread::spawn(|| {
            let mut last_scan = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let Ok(mut runtimes) = lock_manager() else {
                    continue;
                };
                let events: Vec<_> = notices()
                    .1
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .try_iter()
                    .take(128)
                    .collect();
                for notice in events {
                    let _ = record_selection(notice, &mut runtimes);
                }
                if last_scan.elapsed() >= Duration::from_secs(3) {
                    last_scan = Instant::now();
                    for (dir, runtime) in runtimes.iter_mut() {
                        if let Ok(mut record) = load_record(dir) {
                            if let Err(error) = refresh_routes_locked(dir, &mut record, runtime) {
                                record.message = Some(format!("Routing config needs review: {error}"));
                                let _ = save_record(dir, &record);
                            }
                        }
                    }
                }
            }
        });
    });
}
pub(crate) fn shutdown_all() -> Result<()> {
    SHUTTING_DOWN.store(true, Ordering::Release);
    let result = (|| -> Result<()> {
        let mut runtimes = lock_manager()?;
        let dirs: Vec<_> = runtimes.keys().cloned().collect();
        for dir in dirs {
            let record = load_record(&dir)?;
            if let Some(runtime) = runtimes.get_mut(&dir) {
                detach_route(&dir, &record, runtime)?;
            }
            // Keep desired settings for next launch; restore first, stop last.
            if let Some(runtime) = runtimes.remove(&dir) {
                runtime.proxy.shutdown();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        SHUTTING_DOWN.store(false, Ordering::Release);
    }
    result
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
