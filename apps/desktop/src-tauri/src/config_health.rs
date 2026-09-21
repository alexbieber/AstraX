//! Read-only provider diagnostics, with a separate, explicit and guarded repair.
//! This deliberately validates only provider structure: Codex owns the full schema.
use crate::backups::create_backup;
use crate::error::{CodexxError, Result};
use crate::live_config::{acquire_live_config_lock, atomic_write_if_unchanged};
use crate::paths::normalized_path_scope;
use crate::{auth_path, config_path, resolve_codex_dir};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use toml_edit::{value, DocumentMut, Item, TableLike};

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;
const BUILTIN_PROVIDERS: &[&str] = &[
    "openai",
    "ollama",
    "lmstudio",
    "amazon-bedrock",
    "amazon-bedrock-runtime",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigHealthIssue {
    pub(crate) code: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) repairable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigHealthReport {
    pub(crate) codex_dir: String,
    pub(crate) fingerprint: String,
    pub(crate) status: &'static str,
    pub(crate) issues: Vec<ConfigHealthIssue>,
    pub(crate) can_repair: bool,
    pub(crate) repair_summary: Vec<String>,
    pub(crate) checked_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigHealthRepairResult {
    pub(crate) report: ConfigHealthReport,
    pub(crate) backup_id: Option<String>,
    pub(crate) changed: bool,
}

enum Snapshot {
    Missing,
    Bytes(Vec<u8>),
    Unavailable(&'static str),
}

fn bounded_snapshot(path: &Path) -> Snapshot {
    // Check metadata before opening so a FIFO/device cannot block the background job.
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Snapshot::Missing,
        Err(_) => return Snapshot::Unavailable("Unable to read the config file; check permissions and try again."),
    };
    if !metadata.is_file() {
        return Snapshot::Unavailable("The config path is occupied by a non-file; check and try again.");
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Snapshot::Unavailable("Config file is too large for automatic checks; please inspect it manually.");
    }
    let Ok(file) = fs::File::open(path) else {
        return Snapshot::Unavailable("Unable to read the config file; check permissions and try again.");
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Snapshot::Unavailable("Failed to read the config file; please try again later.");
    }
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Snapshot::Unavailable("Config file is too large for automatic checks; please inspect it manually.");
    }
    Snapshot::Bytes(bytes)
}

fn has_chatgpt_auth(codex_dir: &Path) -> bool {
    let Snapshot::Bytes(bytes) = bounded_snapshot(&auth_path(codex_dir)) else {
        return false;
    };
    let Ok(auth) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let nonempty = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    !nonempty(auth.get("OPENAI_API_KEY"))
        && auth
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|mode| mode == "chatgpt")
        && nonempty(
            auth.get("tokens")
                .and_then(|tokens| tokens.get("access_token")),
        )
}

struct SessionReferences {
    ids: Vec<String>,
    profile_provider_ids: Vec<String>,
    profile_fingerprint: String,
    profiles_complete: bool,
}

impl Default for SessionReferences {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            profile_provider_ids: Vec::new(),
            profile_fingerprint: String::new(),
            profiles_complete: true,
        }
    }
}

fn read_profile_definitions(codex_dir: &Path, references: &mut SessionReferences) {
    let mut digest = Sha256::new();
    let Ok(entries) = fs::read_dir(codex_dir) else {
        return;
    };
    let mut paths = Vec::new();
    let mut entries = entries.take(513);
    for index in 0..513 {
        let Some(entry) = entries.next() else { break };
        if index == 512 {
            references.profiles_complete = false;
            break;
        }
        let Ok(entry) = entry else {
            references.profiles_complete = false;
            continue;
        };
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".config.toml") && name != ".config.toml")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    if paths.len() > 32 {
        references.profiles_complete = false;
    }
    let mut remaining_bytes = 8 * 1024 * 1024;
    for path in paths.into_iter().take(32) {
        digest.update(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
        digest.update([0]);
        let snapshot = bounded_snapshot(&path);
        match &snapshot {
            Snapshot::Bytes(bytes) => {
                digest.update(bytes);
                if bytes.len() > remaining_bytes {
                    references.profiles_complete = false;
                    break;
                }
                remaining_bytes -= bytes.len();
                let Some(mut doc) = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.parse::<DocumentMut>().ok())
                else {
                    continue;
                };
                let Some(providers) = doc
                    .get_mut("model_providers")
                    .and_then(Item::as_table_like_mut)
                else {
                    continue;
                };
                for (id, provider) in providers.iter_mut() {
                    // A profile can supply a provider absent from the base user
                    // config. Accept only definitions that need no correction.
                    let mut validation = ConfigHealthReport {
                        codex_dir: String::new(),
                        fingerprint: String::new(),
                        status: "healthy",
                        issues: Vec::new(),
                        can_repair: false,
                        repair_summary: Vec::new(),
                        checked_at: String::new(),
                    };
                    check_provider_table(id.get(), provider, false, &mut validation);
                    if validation.issues.is_empty() {
                        references.profile_provider_ids.push(id.get().to_string());
                    }
                }
            }
            Snapshot::Missing => digest.update(b"missing"),
            Snapshot::Unavailable(message) => {
                digest.update(message.as_bytes());
                references.profiles_complete = false;
            }
        }
    }
    references.profile_provider_ids.sort();
    references.profile_provider_ids.dedup();
    digest.update([u8::from(references.profiles_complete)]);
    references.profile_fingerprint = format!("{:x}", digest.finalize());
}

fn session_references(codex_dir: &Path) -> SessionReferences {
    let mut references = SessionReferences::default();
    let timeout = Duration::from_millis(150);
    for path in crate::sessions::sqlite_candidate_paths_with_timeout(codex_dir, timeout) {
        let Ok(conn) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        if conn.busy_timeout(timeout).is_err() {
            continue;
        }
        // Bound both the number of rows read and the size/count of returned IDs.
        // The live thread database is enough; do not scan rollouts or old backups.
        let Ok(mut statement) = conn.prepare("SELECT DISTINCT model_provider FROM (SELECT model_provider FROM threads ORDER BY rowid DESC LIMIT 10000) WHERE typeof(model_provider) = 'text' AND length(model_provider) BETWEEN 1 AND 256 LIMIT 65") else { continue };
        let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for id in rows.flatten() {
            if !BUILTIN_PROVIDERS.contains(&id.as_str()) && !id.chars().any(char::is_control) {
                references.ids.push(id);
            }
        }
    }
    references.ids.sort();
    references.ids.dedup();
    read_profile_definitions(codex_dir, &mut references);
    references
}

fn fingerprint(
    codex_dir: &Path,
    snapshot: &Snapshot,
    chatgpt_auth: bool,
    sessions: &SessionReferences,
) -> String {
    let mut digest = Sha256::new();
    digest.update(normalized_path_scope(codex_dir).as_bytes());
    digest.update([0, u8::from(chatgpt_auth)]);
    match snapshot {
        Snapshot::Missing => digest.update(b"missing"),
        Snapshot::Bytes(bytes) => {
            digest.update(b"present\0");
            digest.update(bytes);
        }
        Snapshot::Unavailable(message) => digest.update(message.as_bytes()),
    }
    for id in &sessions.ids {
        digest.update([0]);
        digest.update(id.as_bytes());
    }
    digest.update([0]);
    digest.update(sessions.profile_fingerprint.as_bytes());
    format!("{:x}", digest.finalize())
}

fn add_issue(
    report: &mut ConfigHealthReport,
    code: &str,
    title: &str,
    description: &str,
    repair: Option<&str>,
) {
    report.issues.push(ConfigHealthIssue {
        code: code.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        repairable: repair.is_some(),
    });
    if let Some(summary) = repair {
        if !report
            .repair_summary
            .iter()
            .any(|existing| existing == summary)
        {
            report.repair_summary.push(summary.to_string());
        }
    }
}

fn bool_replacement(item: &Item) -> Option<bool> {
    match item.as_str()?.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn is_official_login_table(table: &dyn TableLike, chatgpt_auth: bool) -> bool {
    chatgpt_auth
        && table.get("name").and_then(Item::as_str) == Some("OpenAI")
        && ![
            "base_url",
            "env_key",
            "env_key_instructions",
            "experimental_bearer_token",
            "auth",
            "http_headers",
            "env_http_headers",
        ]
        .iter()
        .any(|key| table.contains_key(key))
}

fn check_provider_table(
    id: &str,
    item: &mut Item,
    chatgpt_auth: bool,
    report: &mut ConfigHealthReport,
) {
    let Some(table) = item.as_table_like_mut() else {
        add_issue(
            report,
            "provider-table-type",
            "Provider config format is invalid",
            "A provider entry is not a valid config table; recheck it on the Providers page.",
            None,
        );
        return;
    };
    // Codex performs a second name validation after deserialization. Bedrock's
    // builtin overrides alone allow the name to be omitted.
    if !matches!(id, "amazon-bedrock" | "amazon-bedrock-runtime")
        && table
            .get("name")
            .is_none_or(|item| item.as_str().is_some_and(|name| name.trim().is_empty()))
    {
        if !id.trim().is_empty() {
            table.insert("name", value(id));
        }
        add_issue(
            report,
            "provider-name-missing",
            "Provider config is missing a name",
            "A provider entry has no name; its existing id can be used as the display name.",
            (!id.trim().is_empty()).then_some("Fill missing display names from provider ids"),
        );
    }
    for key in ["name", "base_url"] {
        if table.get(key).is_some_and(|item| item.as_str().is_none()) {
            add_issue(
                report,
                "provider-text-type",
                "Provider text settings format is invalid",
                "Provider name or API URL should be text; cannot auto-correct yet.",
                None,
            );
        }
    }
    for key in ["requires_openai_auth", "supports_websockets"] {
        if let Some(item) = table.get_mut(key) {
            if item.as_bool().is_some() {
                continue;
            }
            if let Some(replacement) = bool_replacement(item) {
                // Change the value only; keep adjacent comments and formatting.
                let decor = item.as_value().map(|value| value.decor().clone());
                *item = value(replacement);
                if let Some(decor) = decor {
                    *item.as_value_mut().expect("boolean value").decor_mut() = decor;
                }
                add_issue(
                    report,
                    "provider-boolean-text",
                    "Provider toggle format is invalid",
                    "A toggle was written as text, which may prevent Codex from loading the config.",
                    Some("Restore text-written toggles to the correct format"),
                );
            } else {
                add_issue(
                    report,
                    "provider-boolean-type",
                    "Provider toggle is unrecognized",
                    "A toggle is not a valid on/off value and needs manual confirmation.",
                    None,
                );
            }
        }
    }
    if table
        .get("base_url")
        .and_then(Item::as_str)
        .is_some_and(crate::providers::transport::is_deepseek_http_endpoint)
        && table.get("supports_websockets").and_then(Item::as_bool) == Some(true)
    {
        let item = table
            .get_mut("supports_websockets")
            .expect("enabled switch");
        let decor = item.as_value().map(|value| value.decor().clone());
        *item = value(false);
        if let Some(decor) = decor {
            *item.as_value_mut().expect("boolean value").decor_mut() = decor;
        }
        add_issue(
            report,
            "deepseek-websocket-unsupported",
            "DeepSeek connection mode needs adjustment",
            "DeepSeek official API does not support the enabled WebSocket mode; it may error and retry before responding.",
            Some("Switch DeepSeek official API to HTTP to avoid repeated retries when sending messages"),
        );
    }
    if let Some(item) = table.get_mut("wire_api") {
        if item.as_str() == Some("responses") { /* default protocol */
        } else if item
            .as_str()
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("responses"))
        {
            let decor = item.as_value().map(|value| value.decor().clone());
            *item = value("responses");
            if let Some(decor) = decor {
                *item.as_value_mut().expect("string value").decor_mut() = decor;
            }
            add_issue(
                report,
                "provider-protocol-spelling",
                "Provider API format spelling is incorrect",
                "API format has extra spaces or wrong casing, which may prevent Codex from loading the config.",
                Some("Fix spaces or casing in the API format"),
            );
        } else {
            add_issue(
                report,
                "provider-protocol-unsupported",
                "Provider API format is not supported",
                "Current Codex requires the Responses API. Confirm what your provider supports, or use a compatible gateway.",
                None,
            );
        }
    }
    if is_official_login_table(table, chatgpt_auth)
        && table
            .get("requires_openai_auth")
            .is_none_or(|item| item.as_bool() == Some(false))
    {
        table.insert("requires_openai_auth", value(true));
        add_issue(
            report,
            "official-login-auth-disabled",
            "Official login auth settings are incomplete",
            "Official login info was found, but this official provider config does not enable login auth.",
            Some("Enable the auth toggle for official login"),
        );
    }
}

fn complete_alias_candidate(item: &Item) -> bool {
    let Some(table) = item.as_table_like() else {
        return false;
    };
    table
        .get("name")
        .and_then(Item::as_str)
        .is_some_and(|name| !name.trim().is_empty())
        && table
            .get("wire_api")
            .is_none_or(|item| item.as_str() == Some("responses"))
        && ["requires_openai_auth", "supports_websockets"]
            .iter()
            .all(|key| table.get(key).is_none_or(|item| item.as_bool().is_some()))
        && (table
            .get("base_url")
            .and_then(Item::as_str)
            .is_some_and(|url| !url.trim().is_empty())
            || table.get("requires_openai_auth").and_then(Item::as_bool) == Some(true))
}

fn collect_provider_reference(
    item: Option<&Item>,
    references: &mut Vec<String>,
    report: &mut ConfigHealthReport,
) {
    let Some(item) = item else { return };
    let Some(provider) = item.as_str().filter(|provider| !provider.trim().is_empty()) else {
        add_issue(
            report,
            "provider-selection-type",
            "Selected provider setting is invalid",
            "Provider id must be non-empty text; reselect it on the Providers page and save.",
            None,
        );
        return;
    };
    if !BUILTIN_PROVIDERS.contains(&provider)
        && !references.iter().any(|existing| existing == provider)
    {
        references.push(provider.to_string());
    }
}

fn analyze(
    codex_dir: &Path,
    snapshot: &Snapshot,
    chatgpt_auth: bool,
    sessions: &SessionReferences,
) -> (ConfigHealthReport, Option<String>) {
    let mut report = ConfigHealthReport {
        codex_dir: codex_dir.display().to_string(),
        fingerprint: fingerprint(codex_dir, snapshot, chatgpt_auth, sessions),
        status: "healthy",
        issues: Vec::new(),
        can_repair: false,
        repair_summary: Vec::new(),
        checked_at: Utc::now().to_rfc3339(),
    };
    let bytes = match snapshot {
        Snapshot::Missing => {
            report.status = "missing";
            return (report, None);
        }
        Snapshot::Unavailable(message) => {
            report.status = "unavailable";
            add_issue(
                &mut report,
                "config-unavailable",
                "Unable to check configuration right now",
                message,
                None,
            );
            return (report, None);
        }
        Snapshot::Bytes(bytes) => bytes,
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        report.status = "issues";
        add_issue(
            &mut report,
            "config-encoding",
            "Config file text encoding is incorrect",
            "Config file must use UTF-8; inspect and save it manually.",
            None,
        );
        return (report, None);
    };
    let mut doc = match text.parse::<DocumentMut>() {
        Ok(doc) => doc,
        Err(error) => {
            report.status = "issues";
            // TOML's normal Display embeds entire source lines, including credentials.
            let description = error
                .span()
                .map(|span| {
                    let mut offset = span.start.min(text.len());
                    while !text.is_char_boundary(offset) {
                        offset -= 1;
                    }
                    let prefix = &text[..offset];
                    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
                    let column = prefix
                        .rsplit('\n')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .count()
                        + 1;
                    format!("Config file has a format error near line {line}, column {column}; manual inspection required.")
                })
                .unwrap_or_else(|| "Config file has format errors; manual inspection required.".to_string());
            add_issue(
                &mut report,
                "config-syntax",
                "Config file format is invalid",
                &description,
                None,
            );
            return (report, None);
        }
    };
    let mut references = Vec::new();
    collect_provider_reference(doc.get("model_provider"), &mut references, &mut report);
    // Profile loading changed between Codex versions. Leave legacy inline
    // profiles and unrelated fields to Codex instead of enforcing a guessed schema.
    if let Some(item) = doc.get_mut("model_providers") {
        if let Some(providers) = item.as_table_like_mut() {
            for (id, provider) in providers.iter_mut() {
                check_provider_table(id.get(), provider, chatgpt_auth, &mut report);
            }
        } else {
            add_issue(
                &mut report,
                "providers-table-type",
                "Provider list format is invalid",
                "model_providers should be a config table; cannot auto-restore providers yet.",
                None,
            );
        }
    }
    let providers = doc.get("model_providers").and_then(Item::as_table_like);
    let root_references = references.clone();
    for id in &sessions.ids {
        if sessions.profiles_complete
            && !sessions.profile_provider_ids.contains(id)
            && !references.contains(id)
        {
            references.push(id.clone());
        }
    }
    let missing: Vec<_> = references
        .into_iter()
        .filter(|id| providers.is_none_or(|providers| !providers.contains_key(id)))
        .collect();
    let candidates: Vec<_> = providers
        .map(|providers| {
            providers
                .iter()
                .filter(|(id, item)| {
                    !BUILTIN_PROVIDERS.contains(id) && complete_alias_candidate(item)
                })
                .map(|(_, item)| item.clone())
                .collect()
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        let history_only = !missing.iter().any(|id| root_references.contains(id));
        let selected_custom_exists = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .filter(|id| !BUILTIN_PROVIDERS.contains(id))
            .and_then(|id| providers.and_then(|providers| providers.get(id)))
            .is_some_and(complete_alias_candidate);
        if missing.len() == 1 && candidates.len() == 1 && (!history_only || selected_custom_exists)
        {
            let source_name: String = candidates[0]
                .as_table_like()
                .and_then(|table| table.get("name"))
                .and_then(Item::as_str)
                .unwrap_or("Current provider")
                .chars()
                .filter(|ch| !ch.is_control())
                .take(60)
                .collect();
            doc.get_mut("model_providers")
                .and_then(Item::as_table_like_mut)
                .expect("candidate implies table")
                .insert(&missing[0], candidates[0].clone());
            if history_only {
                let summary =
                    format!("Let affected old sessions keep using current provider "{source_name}", and preserve the original sessions");
                add_issue(&mut report, "session-provider-definition-missing", "Providers used by old sessions are missing config", &format!("Some old sessions still reference missing providers. After repair they will use current provider "{source_name}"; original sessions are kept."), Some(&summary));
            } else {
                let summary = format!("Fill missing mappings using existing provider "{source_name}" config");
                add_issue(&mut report, "provider-definition-missing", "Selected provider is missing config", &format!("Provider id does not match existing config. Fill the mapping using existing provider "{source_name}" and keep the original config."), Some(&summary));
            }
            // Keep routing decisions visible in the compact startup notice.
            if let Some(summary) = report.repair_summary.pop() {
                report.repair_summary.insert(0, summary);
            }
        } else {
            add_issue(
                &mut report,
                if history_only {
                    "session-provider-definition-missing"
                } else {
                    "provider-definition-missing"
                },
                if history_only {
                    "Providers used by old sessions are missing config"
                } else {
                    "Selected provider is missing config"
                },
                "No uniquely confirmable provider config found; reselect and enable on the Providers page, or restore the original config manually.",
                None,
            );
        }
    }
    report.status = if report.issues.is_empty() {
        "healthy"
    } else {
        "issues"
    };
    report.can_repair = !report.repair_summary.is_empty();
    if report.can_repair
        && fs::symlink_metadata(config_path(codex_dir))
            .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        report.can_repair = false;
        report.repair_summary.clear();
        for issue in &mut report.issues {
            issue.repairable = false;
        }
        add_issue(
            &mut report,
            "config-linked-file",
            "Config file is a symlink to another location",
            "Edit the original config file to preserve the existing symlink.",
            None,
        );
    }
    let replacement = report.can_repair.then(|| doc.to_string());
    (report, replacement)
}

pub(crate) fn check_codex_config_inner(config_dir: Option<String>) -> Result<ConfigHealthReport> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let snapshot = bounded_snapshot(&config_path(&codex_dir));
    Ok(analyze(
        &codex_dir,
        &snapshot,
        has_chatgpt_auth(&codex_dir),
        &session_references(&codex_dir),
    )
    .0)
}

fn repair_with_before_write<F: FnOnce()>(
    codex_dir: &Path,
    expected_fingerprint: &str,
    before_write: F,
) -> Result<ConfigHealthRepairResult> {
    // A stale or nonrepairable request must not even create the lock directory.
    let snapshot = bounded_snapshot(&config_path(codex_dir));
    let (report, replacement) = analyze(
        codex_dir,
        &snapshot,
        has_chatgpt_auth(codex_dir),
        &session_references(codex_dir),
    );
    if report.fingerprint != expected_fingerprint {
        return Err(CodexxError::Config(
            "Configuration changed; recheck before repairing.".to_string(),
        ));
    }
    if replacement.is_none() {
        return Ok(ConfigHealthRepairResult {
            report,
            backup_id: None,
            changed: false,
        });
    }
    let _lock = acquire_live_config_lock(codex_dir)?;
    let snapshot = bounded_snapshot(&config_path(codex_dir));
    let (report, replacement) = analyze(
        codex_dir,
        &snapshot,
        has_chatgpt_auth(codex_dir),
        &session_references(codex_dir),
    );
    if report.fingerprint != expected_fingerprint {
        return Err(CodexxError::Config(
            "Configuration changed; recheck before repairing.".to_string(),
        ));
    }
    let (Some(replacement), Snapshot::Bytes(before)) = (replacement, snapshot) else {
        return Ok(ConfigHealthRepairResult {
            report,
            backup_id: None,
            changed: false,
        });
    };
    // A config.toml symlink can point to an externally managed source; do not replace it.
    if fs::symlink_metadata(config_path(codex_dir)).is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(CodexxError::Config(
            "Config file is a symlink; inspect and edit the original file first.".to_string(),
        ));
    }
    let backup_id = create_backup(codex_dir, "repair-config")?;
    before_write();
    if fingerprint(
        codex_dir,
        &Snapshot::Bytes(before.clone()),
        has_chatgpt_auth(codex_dir),
        &session_references(codex_dir),
    ) != expected_fingerprint
    {
        return Err(CodexxError::Config(
            "Login state or session config changed; recheck before repairing.".to_string(),
        ));
    }
    atomic_write_if_unchanged(
        &config_path(codex_dir),
        Some(&before),
        replacement.as_bytes(),
    )?;
    let after = bounded_snapshot(&config_path(codex_dir));
    Ok(ConfigHealthRepairResult {
        report: analyze(
            codex_dir,
            &after,
            has_chatgpt_auth(codex_dir),
            &session_references(codex_dir),
        )
        .0,
        backup_id,
        changed: true,
    })
}

pub(crate) fn repair_codex_config_inner(
    config_dir: Option<String>,
    expected_fingerprint: String,
) -> Result<ConfigHealthRepairResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    repair_with_before_write(&codex_dir, &expected_fingerprint, || {})
}

pub(crate) fn open_codex_config_file_inner(config_dir: Option<String>) -> Result<()> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let path = config_path(&codex_dir);
    if !fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(CodexxError::Config(
            "No openable config.toml found; confirm the Codex config directory.".to_string(),
        ));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-t");
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("notepad.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| {
            CodexxError::Config(
                "Unable to open the config file; open config.toml manually in a text editor.".to_string(),
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(text: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "codex-x-health-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            fs::write(config_path(&dir), text).unwrap();
            Self(dir)
        }
        fn check(&self) -> ConfigHealthReport {
            check_codex_config_inner(Some(self.0.display().to_string())).unwrap()
        }
        fn repair(&self, report: &ConfigHealthReport) -> ConfigHealthRepairResult {
            repair_codex_config_inner(
                Some(self.0.display().to_string()),
                report.fingerprint.clone(),
            )
            .unwrap()
        }
        fn text(&self) -> String {
            fs::read_to_string(config_path(&self.0)).unwrap()
        }
        fn sessions(&self, ids: &[&str]) {
            let conn = Connection::open(self.0.join("state_5.sqlite")).unwrap();
            conn.execute_batch("CREATE TABLE IF NOT EXISTS threads (id INTEGER PRIMARY KEY, model_provider TEXT NOT NULL)").unwrap();
            for id in ids {
                conn.execute("INSERT INTO threads(model_provider) VALUES (?1)", [id])
                    .unwrap();
            }
        }
        fn login(&self) {
            fs::write(auth_path(&self.0), r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"access_token":"test-only-secret"}}"#).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const CUSTOM: &str = "model_provider = 'custom'\nmodel = 'private-model'\n[model_providers.custom]\nname = 'My API'\nbase_url = 'https://api.example.test/v1'\n";

    #[test]
    fn deepseek_transport_check_is_read_only_and_repair_preserves_other_options() {
        let original = "model_provider='custom'\nmodel='deepseek-chat'\n[model_providers.custom]\nname='DeepSeek'\nbase_url='https://api.deepseek.com'\nsupports_websockets=true # preserve comment\nrequest_max_retries=7\n[mcp_servers.local]\ncommand='fixture-mcp'\n";
        let fixture = Fixture::new(original);
        let report = fixture.check();
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, "deepseek-websocket-unsupported");
        assert!(report.can_repair);
        assert_eq!(fixture.text(), original);
        assert!(!fixture.0.join(".codexx-test-backups").exists());
        let result = fixture.repair(&report);
        assert!(result.changed);
        assert_eq!(result.report.status, "healthy");
        let backup = fixture
            .0
            .join(".codexx-test-backups")
            .join(result.backup_id.unwrap())
            .join("config.toml");
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        let repaired = fixture.text();
        assert!(repaired.contains("# preserve comment"));
        let doc = repaired.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["mcp_servers"]["local"]["command"].as_str(),
            Some("fixture-mcp")
        );
    }

    #[test]
    fn websocket_check_respects_official_and_independent_proxy_capabilities() {
        for endpoint in [
            "https://api.openai.com/v1",
            "https://api.deepseek.com.proxy.example/v1",
            "https://proxy.example/deepseek",
        ] {
            let fixture = Fixture::new(&format!(
                "{}supports_websockets=true\n",
                CUSTOM.replace("https://api.example.test/v1", endpoint)
            ));
            assert_eq!(fixture.check().status, "healthy");
            assert!(!fixture.check().can_repair);
        }
        for setting in ["", "supports_websockets=false\n"] {
            let fixture = Fixture::new(&format!(
                "{}{setting}",
                CUSTOM.replace("https://api.example.test/v1", "https://api.deepseek.com")
            ));
            assert_eq!(fixture.check().status, "healthy");
        }
    }

    #[test]
    fn defaults_and_all_builtin_ids_need_no_added_fields() {
        for builtin in BUILTIN_PROVIDERS {
            let fixture = Fixture::new(&format!("model_provider = '{builtin}'\n"));
            assert_eq!(fixture.check().status, "healthy", "{builtin}");
        }
        for text in [
            "",
            CUSTOM,
            "model_provider='custom'\n[model_providers.custom]\nname='Default endpoint'\n",
            "model_provider = 'amazon-bedrock'\n[model_providers.amazon-bedrock]\n",
        ] {
            let fixture = Fixture::new(text);
            assert_eq!(fixture.check().status, "healthy");
            assert!(!fixture.check().can_repair);
        }
    }

    #[test]
    fn absent_file_is_not_created_by_check_or_repair() {
        let fixture = Fixture::new("");
        fs::remove_file(config_path(&fixture.0)).unwrap();
        let report = fixture.check();
        assert_eq!(report.status, "missing");
        assert!(!fixture.repair(&report).changed);
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
    }

    #[test]
    fn fills_missing_name_without_changing_provider_identity() {
        let fixture = Fixture::new("model_provider='custom'\n[model_providers.custom]\nbase_url='https://api.example.test'\n");
        let report = fixture.check();
        assert!(report.can_repair);
        let repaired = fixture.repair(&report);
        assert_eq!(repaired.report.status, "healthy");
        let doc = fixture.text().parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("custom"));
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("custom")
        );
    }

    #[test]
    fn repairs_missing_alias_without_renaming_or_deleting_original() {
        let text = CUSTOM.replace("model_provider = 'custom'", "model_provider = 'my_codex'");
        let fixture = Fixture::new(&text);
        let report = fixture.check();
        assert!(report.can_repair);
        assert!(report
            .repair_summary
            .iter()
            .any(|summary| summary.contains("My API")));
        let repaired = fixture.repair(&report);
        assert_eq!(repaired.report.status, "healthy");
        let doc = fixture.text().parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("my_codex"));
        assert_eq!(
            doc["model_providers"]["my_codex"]["base_url"].as_str(),
            doc["model_providers"]["custom"]["base_url"].as_str()
        );
        assert!(doc["model_providers"]["custom"].is_table());
        assert_eq!(doc["model"].as_str(), Some("private-model"));
    }

    #[test]
    fn missing_provider_with_no_or_ambiguous_sources_is_not_guessed() {
        for text in [
            "model_provider = 'my_codex'".to_string(),
            format!("{}\n[model_providers.second]\nname='Other API'\nbase_url='https://other.example.test'\n", CUSTOM.replace("model_provider = 'custom'", "model_provider = 'missing'")),
        ] {
            let fixture = Fixture::new(&text);
            fixture.login();
            let report = fixture.check();
            assert_eq!(report.status, "issues");
            assert!(!report.can_repair);
            assert!(!fixture.repair(&report).changed);
            assert_eq!(fixture.text(), text);
        }
    }

    #[test]
    fn third_party_auth_is_never_changed_to_official_login() {
        let text = format!("{CUSTOM}requires_openai_auth = false\n");
        let fixture = Fixture::new(&text);
        fixture.login();
        assert_eq!(fixture.check().status, "healthy");
        assert_eq!(fixture.text(), text);
        let fixture = Fixture::new("model_provider='custom'\n[model_providers.custom]\nname='OpenAI'\nbase_url='https://third-party.example.test'\nrequires_openai_auth=false\n");
        fixture.login();
        assert_eq!(fixture.check().status, "healthy");
    }

    #[test]
    fn clear_official_login_evidence_can_restore_auth_flag_only() {
        let fixture =
            Fixture::new("model_provider='custom'\n[model_providers.custom]\nname='OpenAI'\n");
        assert_eq!(fixture.check().status, "healthy");
        fixture.login();
        let before_auth = fs::read(auth_path(&fixture.0)).unwrap();
        let report = fixture.check();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "official-login-auth-disabled"));
        assert_eq!(fixture.repair(&report).report.status, "healthy");
        assert_eq!(fs::read(auth_path(&fixture.0)).unwrap(), before_auth);
        let doc = fixture.text().parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["requires_openai_auth"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn explicit_auth_fields_prevent_official_login_guess() {
        for extra in [
            "env_key='PRIVATE_API_KEY'",
            "experimental_bearer_token='test-token'",
            "http_headers={Authorization='Bearer test'}",
            "auth={type='external'}",
        ] {
            let fixture = Fixture::new(&format!(
                "model_provider='custom'\n[model_providers.custom]\nname='OpenAI'\n{extra}\n"
            ));
            fixture.login();
            assert_eq!(fixture.check().status, "healthy");
        }
    }

    #[test]
    fn safe_value_repairs_keep_comments_and_unrelated_configuration() {
        let text = format!("{CUSTOM}requires_openai_auth = 'false' # keep this\nsupports_websockets = ' TRUE '\nwire_api = ' Responses '\n[mcp_servers.test]\ncommand = 'mcp-test'\n[projects.'/safe/project']\ntrust_level='trusted'\n");
        let fixture = Fixture::new(&text);
        let report = fixture.check();
        assert_eq!(report.issues.len(), 3);
        assert_eq!(fixture.repair(&report).report.status, "healthy");
        assert!(fixture.text().contains("# keep this"));
        let doc = fixture.text().parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(true)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            doc["mcp_servers"]["test"]["command"].as_str(),
            Some("mcp-test")
        );
        assert_eq!(
            doc["projects"]["/safe/project"]["trust_level"].as_str(),
            Some("trusted")
        );
    }

    #[test]
    fn unsupported_protocol_and_invalid_values_are_not_guessed() {
        for invalid in [
            "wire_api='chat'",
            "wire_api='anthropic'",
            "wire_api=42",
            "requires_openai_auth='maybe'",
            "supports_websockets=1",
        ] {
            let text = format!("{CUSTOM}{invalid}\n");
            let fixture = Fixture::new(&text);
            let report = fixture.check();
            assert_eq!(report.status, "issues");
            assert!(!report.can_repair);
            assert_eq!(fixture.text(), text);
        }
    }

    #[test]
    fn malformed_container_and_selection_types_are_detected() {
        for text in [
            "model_providers = []",
            "model_providers = { custom = 12 }",
            "model_provider = 7",
            "model_provider = ''",
            "[model_providers.custom]\nname = 10",
        ] {
            let fixture = Fixture::new(text);
            let report = fixture.check();
            assert_eq!(report.status, "issues", "{text}");
            assert!(!report.can_repair);
        }
    }

    #[test]
    fn syntax_errors_never_echo_source_lines_or_credentials() {
        let fixture = Fixture::new(
            "model_provider = 'custom'\nexperimental_bearer_token = 'super-private-secret\n",
        );
        let report = fixture.check();
        let json = serde_json::to_string(&report).unwrap();
        assert_eq!(report.status, "issues");
        assert!(json.contains("Line 2"));
        assert!(!json.contains("super-private-secret"));
        assert!(!json.contains("experimental_bearer_token"));
    }

    #[test]
    fn bounded_reads_and_encoding_fail_safely() {
        let fixture = Fixture::new("");
        fs::write(
            config_path(&fixture.0),
            vec![b' '; MAX_CONFIG_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(fixture.check().status, "unavailable");
        fs::write(config_path(&fixture.0), [0xff, 0xfe]).unwrap();
        assert_eq!(fixture.check().issues[0].code, "config-encoding");
    }

    #[test]
    fn check_does_not_write_config_auth_database_lock_or_backups() {
        let fixture = Fixture::new(&format!("{CUSTOM}wire_api=' RESPONSES '\n"));
        fixture.login();
        fixture.sessions(&["custom"]);
        let before: Vec<_> = fs::read_dir(&fixture.0)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        assert!(fixture.check().can_repair);
        for (path, bytes) in &before {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), before.len());
    }

    #[test]
    fn repair_creates_backup_and_is_idempotent() {
        let original = format!("{CUSTOM}wire_api=' RESPONSES '\n");
        let fixture = Fixture::new(&original);
        let repaired = fixture.repair(&fixture.check());
        assert!(repaired.changed);
        let backup = fixture
            .0
            .join(".codexx-test-backups")
            .join(repaired.backup_id.unwrap())
            .join("config.toml");
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        let no_op = fixture.repair(&repaired.report);
        assert!(!no_op.changed);
        assert!(no_op.backup_id.is_none());
    }

    #[test]
    fn changed_config_fingerprint_prevents_stale_repair() {
        let fixture = Fixture::new(&format!("{CUSTOM}wire_api=' RESPONSES '\n"));
        let report = fixture.check();
        fs::write(config_path(&fixture.0), CUSTOM).unwrap();
        assert!(repair_codex_config_inner(
            Some(fixture.0.display().to_string()),
            report.fingerprint
        )
        .is_err());
        assert_eq!(fixture.text(), CUSTOM);
        assert!(!fixture.0.join("tmp").exists());
    }

    #[test]
    fn external_write_after_backup_is_never_overwritten() {
        let fixture = Fixture::new(&format!("{CUSTOM}wire_api=' RESPONSES '\n"));
        let report = fixture.check();
        let error = repair_with_before_write(&fixture.0, &report.fingerprint, || {
            fs::write(config_path(&fixture.0), "# external modification\n").unwrap();
        })
        .unwrap_err();
        assert!(error.to_string().contains("modified by another program"));
        assert_eq!(fixture.text(), "# external modification\n");
    }

    #[test]
    fn historical_provider_references_are_checked_even_when_root_is_healthy() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["custom", "my_codex", "openai"]);
        let database_before = fs::read(fixture.0.join("state_5.sqlite")).unwrap();
        let report = fixture.check();
        assert_eq!(report.status, "issues");
        assert!(report.can_repair);
        assert!(report.repair_summary[0].contains("Old sessions keep using current provider "My API""));
        assert_eq!(fixture.repair(&report).report.status, "healthy");
        assert_eq!(
            fs::read(fixture.0.join("state_5.sqlite")).unwrap(),
            database_before
        );
        assert_eq!(
            fixture.text().parse::<DocumentMut>().unwrap()["model_provider"].as_str(),
            Some("custom")
        );
    }

    #[test]
    fn multiple_historical_sources_are_not_merged_into_one_provider() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["old_api_a", "old_api_b"]);
        assert_eq!(fixture.check().status, "issues");
        assert!(!fixture.check().can_repair);
    }

    #[test]
    fn history_is_not_mapped_to_an_inactive_custom_provider() {
        let fixture =
            Fixture::new(&CUSTOM.replace("model_provider = 'custom'", "model_provider = 'openai'"));
        fixture.sessions(&["my_codex"]);
        assert!(!fixture.check().can_repair);
    }

    #[test]
    fn new_session_provider_changes_invalidate_repair_proposal() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["my_codex"]);
        let report = fixture.check();
        fixture.sessions(&["other_api"]);
        assert!(repair_codex_config_inner(
            Some(fixture.0.display().to_string()),
            report.fingerprint
        )
        .is_err());
        assert_eq!(fixture.text(), CUSTOM);
    }

    #[test]
    fn invalid_session_database_does_not_become_a_configuration_error() {
        let fixture = Fixture::new(CUSTOM);
        fs::write(fixture.0.join("state_5.sqlite"), "not a database").unwrap();
        assert_eq!(fixture.check().status, "healthy");
    }

    #[test]
    fn repairable_subset_does_not_hide_manual_configuration_errors() {
        let fixture = Fixture::new(&format!(
            "{CUSTOM}requires_openai_auth='false'\nwire_api='anthropic'\n"
        ));
        let report = fixture.check();
        assert!(report.can_repair);
        let repaired = fixture.repair(&report);
        assert!(repaired.changed);
        assert_eq!(repaired.report.status, "issues");
        assert!(!repaired.report.can_repair);
        assert_eq!(
            repaired.report.issues[0].code,
            "provider-protocol-unsupported"
        );
    }

    #[test]
    fn history_routing_change_is_the_first_visible_repair_plan() {
        let fixture = Fixture::new(&format!("{CUSTOM}supports_websockets='false'\n"));
        fixture.sessions(&["my_codex"]);
        assert!(fixture.check().repair_summary[0].contains("Old sessions keep using current provider "My API""));
    }

    #[test]
    fn auth_change_after_backup_cancels_official_repair() {
        let original = "model_provider='custom'\n[model_providers.custom]\nname='OpenAI'\n";
        let fixture = Fixture::new(original);
        fixture.login();
        let report = fixture.check();
        let result = repair_with_before_write(&fixture.0, &report.fingerprint, || {
            fs::remove_file(auth_path(&fixture.0)).unwrap();
        });
        assert!(result.is_err());
        assert_eq!(fixture.text(), original);
    }

    #[cfg(unix)]
    #[test]
    fn linked_config_is_reported_but_not_replaced() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new(&format!("{CUSTOM}wire_api=' Responses '\n"));
        fs::rename(config_path(&fixture.0), fixture.0.join("managed.toml")).unwrap();
        symlink("managed.toml", config_path(&fixture.0)).unwrap();
        let report = fixture.check();
        assert_eq!(report.status, "issues");
        assert!(!report.can_repair);
        assert!(!fixture.repair(&report).changed);
        assert!(fs::symlink_metadata(config_path(&fixture.0))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn separate_profile_definition_explains_historical_provider_reference() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["profile_api"]);
        let profile = "model_provider='profile_api'\n[model_providers.profile_api]\nname='Profile API'\nbase_url='https://profile.example.test'\n";
        fs::write(fixture.0.join("work.config.toml"), profile).unwrap();
        assert_eq!(fixture.check().status, "healthy");
        assert_eq!(
            fs::read_to_string(fixture.0.join("work.config.toml")).unwrap(),
            profile
        );
    }

    #[test]
    fn invalid_profile_is_not_used_as_an_automatic_repair_source() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["profile_api"]);
        let profile = "[model_providers.profile_api]\nname='Profile API'\nwire_api='chat'\n";
        fs::write(fixture.0.join("work.config.toml"), profile).unwrap();
        let report = fixture.check();
        assert_eq!(report.status, "issues");
        assert!(report.repair_summary[0].contains("My API"));
        fixture.repair(&report);
        assert_eq!(
            fs::read_to_string(fixture.0.join("work.config.toml")).unwrap(),
            profile
        );
    }

    #[test]
    fn profile_changes_invalidate_an_existing_repair_proposal() {
        let fixture = Fixture::new(CUSTOM);
        fixture.sessions(&["profile_api"]);
        let report = fixture.check();
        assert!(report.can_repair);
        fs::write(
            fixture.0.join("work.config.toml"),
            "[model_providers.profile_api]\nname='Profile API'\n",
        )
        .unwrap();
        assert_eq!(fixture.check().status, "healthy");
        assert!(repair_codex_config_inner(
            Some(fixture.0.display().to_string()),
            report.fingerprint
        )
        .is_err());
        assert_eq!(fixture.text(), CUSTOM);
    }

    #[test]
    fn legacy_inline_profile_fields_are_left_to_codex() {
        let fixture = Fixture::new(&format!(
            "profiles = {{ legacy = {{ model_provider = 'legacy_api' }} }}\n{CUSTOM}"
        ));
        assert_eq!(fixture.check().status, "healthy");
    }
}
