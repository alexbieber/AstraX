//! Provider-local model menus using Codex's external ModelsResponse catalog.
//! A display name never changes the model slug sent to the provider. Catalogs
//! are immutable so a failed live-config transaction can keep its old pointer.

use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, write_private_json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use toml_edit::{value, DocumentMut};

const MAX_MAPPINGS: usize = 64;
const MAX_NAME_CHARS: usize = 200;
const MAX_CONTEXT_WINDOW: i64 = 10_000_000;
const DEFAULT_CONTEXT_WINDOW: i64 = 128_000;
const OWNED_DIRECTORY: &str = ".codex-x";
const CATALOG_DIRECTORY: &str = "model-catalogs";
const CATALOG_FIELD: &str = "model_catalog_json";
const OWNERSHIP_FIELD: &str = "_codex_x_model_catalog";
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;
const REASONING_EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
const BASE_INSTRUCTIONS: &str = "You are a coding assistant working with the user in a shared workspace. Use the available tools to inspect and edit project files, follow the user's requirements, and verify your changes.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderModelMapping {
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) context_window: Option<i64>,
}

fn validate_name(name: &str, label: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CodexxError::Config(format!("{label}不能为空")));
    }
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(CodexxError::Config(format!(
            "{label}最多 {MAX_NAME_CHARS} 个字符，且不能包含控制字符"
        )));
    }
    Ok(name.to_owned())
}

pub(crate) fn normalize_mappings(
    rows: &[ProviderModelMapping],
    default_model: &str,
) -> Result<Vec<ProviderModelMapping>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    if rows.len() > MAX_MAPPINGS {
        return Err(CodexxError::Config(format!(
            "最多添加 {MAX_MAPPINGS} 个模型"
        )));
    }
    let default_model = validate_name(default_model, "默认模型 ID")?;
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for row in rows {
        let model = validate_name(&row.model, "模型 ID")?;
        let display_name = if row.display_name.trim().is_empty() {
            model.clone()
        } else {
            validate_name(&row.display_name, "模型显示名称")?
        };
        if row
            .context_window
            .is_some_and(|window| !(1..=MAX_CONTEXT_WINDOW).contains(&window))
        {
            return Err(CodexxError::Config(format!(
                "模型上下文窗口须为 1 至 {MAX_CONTEXT_WINDOW} 之间的整数"
            )));
        }
        if seen.insert(model.clone()) {
            normalized.push(ProviderModelMapping {
                model,
                display_name,
                context_window: row.context_window,
            });
        }
    }
    if !seen.contains(&default_model) {
        if normalized.len() >= MAX_MAPPINGS {
            return Err(CodexxError::Config(format!(
                "模型列表需要包含默认模型，合计最多 {MAX_MAPPINGS} 个模型"
            )));
        }
        normalized.insert(
            0,
            ProviderModelMapping {
                display_name: default_model.clone(),
                model: default_model,
                context_window: None,
            },
        );
    }
    Ok(normalized)
}

fn model_entry(mapping: &ProviderModelMapping, priority: usize, default_context: i64) -> Value {
    let context_window = mapping.context_window.unwrap_or(default_context);
    // Schema: openai/codex rust-v0.153.4, protocol/src/openai_models.rs,
    // ModelInfo + ModelsResponse. Keep legacy required fields for older Codex.
    // Unlike cloning a GPT cache entry, this does not import proprietary model
    // instructions, hosted tools, service tiers or vision. Expose the same effort
    // menu for mapped models; the upstream API determines each effort's effect.
    json!({
        "slug": mapping.model,
        "display_name": mapping.display_name,
        "description": mapping.display_name,
        "base_instructions": BASE_INSTRUCTIONS,
        "default_reasoning_level": "high",
        "supported_reasoning_levels": REASONING_EFFORTS.iter().map(|effort| json!({"effort": effort, "description": if *effort == "none" { "Disable thinking".to_owned() } else { format!("{effort} reasoning effort") }})).collect::<Vec<_>>(),
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "additional_speed_tiers": [],
        "service_tiers": [],
        "availability_nux": null,
        "upgrade": null,
        // Older Codex gates the entire reasoning object (including effort) here.
        "supports_reasoning_summaries": true,
        "supports_reasoning_summary_parameter": false,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": { "mode": "bytes", "limit": 10000 },
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": context_window,
        "max_context_window": context_window,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": ["text"],
        "supports_search_tool": false,
        "use_responses_lite": false,
        "prefer_websockets": false
    })
}

fn build_catalog(mappings: &[ProviderModelMapping], doc: &DocumentMut) -> Value {
    let default_context = doc
        .get("model_context_window")
        .and_then(|item| item.as_integer())
        .filter(|window| (1..=MAX_CONTEXT_WINDOW).contains(window))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    json!({"models": mappings.iter().enumerate()
        .map(|(priority, mapping)| model_entry(mapping, priority, default_context))
        .collect::<Vec<_>>()})
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Include junctions/reparse points, not only ordinary symlinks.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn owned_catalog_directory(codex_dir: &Path) -> PathBuf {
    codex_dir.join(OWNED_DIRECTORY).join(CATALOG_DIRECTORY)
}

fn catalog_filename(catalog: &Value) -> Result<String> {
    let content = serde_json::to_vec(catalog)
        .map_err(|_| CodexxError::Config("无法生成供应商模型目录".into()))?;
    let mut digest = Sha256::new();
    digest.update(b"codex-x-provider-model-catalog-v2\0");
    digest.update(&content);
    Ok(format!("{:x}.json", digest.finalize()))
}

fn has_verified_ownership(path: &Path, filename: &str) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > MAX_CATALOG_BYTES {
        return false;
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_CATALOG_BYTES
    {
        return false;
    }
    let Ok(catalog) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(owner) = catalog.get(OWNERSHIP_FIELD) else {
        return false;
    };
    let Some(provider_hash) = owner.get("provider_hash").and_then(Value::as_str) else {
        return false;
    };
    owner.get("source").and_then(Value::as_str) == Some("codex-x")
        && owner.get("format").and_then(Value::as_u64) == Some(1)
        && provider_hash.len() == 64
        && provider_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && catalog
            .get("models")
            .and_then(Value::as_array)
            .is_some_and(|models| !models.is_empty() && models.len() <= MAX_MAPPINGS)
        && catalog_filename(&catalog).is_ok_and(|expected| expected == filename)
}

fn prepare_owned_directory(codex_dir: &Path) -> Result<PathBuf> {
    ensure_directory(codex_dir)?;
    // The user's CODEX_HOME may intentionally be a symlink/junction. Resolve
    // that once, then reject links inside our own generated-file directory.
    let mut directory = codex_dir
        .canonicalize()
        .map_err(|error| io_err(codex_dir, error))?;
    for component in [OWNED_DIRECTORY, CATALOG_DIRECTORY] {
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => {
                return Err(CodexxError::Config(
                    "模型目录被文件或链接占用，请检查 Astra 模型目录".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_directory(&directory)?;
            }
            Err(error) => return Err(io_err(&directory, error)),
        }
        let metadata =
            fs::symlink_metadata(&directory).map_err(|error| io_err(&directory, error))?;
        if is_link(&metadata) || !metadata.is_dir() {
            return Err(CodexxError::Config(
                "模型目录在创建时发生变化，请重试".into(),
            ));
        }
    }
    Ok(directory)
}

fn is_owned_pointer(codex_dir: &Path, pointer: &str) -> bool {
    let pointer = Path::new(pointer);
    // Never treat a traversal path as our generated pointer, even if part of
    // the spelling happens to resemble the owned directory.
    if pointer
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return false;
    }
    let Some(name) = pointer.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(hash) = name.strip_suffix(".json") else {
        return false;
    };
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let candidate = if pointer.is_absolute() {
        pointer.to_path_buf()
    } else {
        codex_dir.join(pointer)
    };
    let root = owned_catalog_directory(codex_dir);
    let Some(parent) = candidate.parent() else {
        return false;
    };
    let Some(owner_directory) = parent.parent() else {
        return false;
    };
    if parent
        .file_name()
        .is_none_or(|name| name != CATALOG_DIRECTORY)
        || owner_directory
            .file_name()
            .is_none_or(|name| name != OWNED_DIRECTORY)
    {
        return false;
    }
    // Only the CODEX_HOME ancestor may be redirected. A link placed inside the
    // app-owned path (or at the catalog itself) must not claim a user's target.
    for directory in [parent, owner_directory] {
        if fs::symlink_metadata(directory)
            .is_ok_and(|metadata| is_link(&metadata) || !metadata.is_dir())
        {
            return false;
        }
    }
    if fs::symlink_metadata(&candidate)
        .is_ok_and(|metadata| is_link(&metadata) || !metadata.is_file())
    {
        return false;
    }
    let same_parent = match (parent.canonicalize(), root.canonicalize()) {
        (Ok(parent), Ok(root)) => parent == root,
        _ => parent == root,
    };
    // Retain support for legacy/stale pointers in the current owned directory.
    // For another CODEX_HOME, require a marker and a digest covering the whole
    // catalog; a similarly named user catalog must remain untouched.
    same_parent || has_verified_ownership(&candidate, name)
}

pub(crate) fn prepare_model_catalog(
    codex_dir: &Path,
    provider_id: &str,
    mappings: &[ProviderModelMapping],
    model: &str,
    doc: &mut DocumentMut,
) -> Result<()> {
    let mappings = normalize_mappings(mappings, model)?;
    if mappings.is_empty() {
        if doc
            .get(CATALOG_FIELD)
            .and_then(|item| item.as_str())
            .is_some_and(|pointer| is_owned_pointer(codex_dir, pointer))
        {
            doc.as_table_mut().remove(CATALOG_FIELD);
        }
        return Ok(());
    }
    let mut catalog = build_catalog(&mappings, doc);
    catalog[OWNERSHIP_FIELD] = json!({
        "source": "codex-x", "format": 1,
        "provider_hash": format!("{:x}", Sha256::digest(provider_id.as_bytes()))
    });
    let filename = catalog_filename(&catalog)?;
    let directory = prepare_owned_directory(codex_dir)?;
    let path = directory.join(filename);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if is_link(&metadata) || !metadata.is_file() || metadata.len() > MAX_CATALOG_BYTES {
                return Err(CodexxError::Config(
                    "已生成的模型目录文件无效，请检查后重试".into(),
                ));
            }
            let existing = fs::read(&path).map_err(|error| io_err(&path, error))?;
            if serde_json::from_slice::<Value>(&existing).ok().as_ref() != Some(&catalog) {
                return Err(CodexxError::Config(
                    "已生成的模型目录文件被修改，未覆盖原文件".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_private_json(&path, &catalog)?
        }
        Err(error) => return Err(io_err(&path, error)),
    }
    // Change the caller's document only after a complete catalog is available.
    // The caller owns the live config transaction; no auth/config files are
    // read or written here, and models_cache.json is intentionally untouched.
    doc[CATALOG_FIELD] = value(path.to_string_lossy().to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "codex-x-catalog-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn mapping(model: &str, display_name: &str, context: Option<i64>) -> ProviderModelMapping {
        ProviderModelMapping {
            model: model.into(),
            display_name: display_name.into(),
            context_window: context,
        }
    }
    fn path(doc: &DocumentMut) -> PathBuf {
        PathBuf::from(doc[CATALOG_FIELD].as_str().unwrap())
    }
    fn catalog(doc: &DocumentMut) -> Value {
        serde_json::from_slice(&fs::read(path(doc)).unwrap()).unwrap()
    }

    #[test]
    fn every_mapped_model_has_the_complete_effort_menu_without_extra_settings() {
        for id in [
            "deepseek-flash",
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "vendor/custom-model",
            "gpt-5.5",
        ] {
            let entry = model_entry(&mapping(id, id, None), 0, 128000);
            let levels: Vec<_> = entry["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|level| level["effort"].as_str().unwrap())
                .collect();
            assert_eq!(levels, REASONING_EFFORTS);
            assert_eq!(entry["default_reasoning_level"], "high");
            assert_eq!(entry["supports_reasoning_summaries"], true);
            assert_eq!(entry["supports_reasoning_summary_parameter"], false);
            assert_eq!(entry["default_reasoning_summary"], "none");
            assert_eq!(entry["prefer_websockets"], false);
            assert!(!entry.to_string().contains("Maps to"));
        }
    }

    #[test]
    fn older_saved_rows_need_no_additional_reasoning_configuration() {
        let fixture = Fixture::new();
        for payload in [
            json!({"model":"custom-model"}),
            json!({"model":"custom-model","reasoningEfforts":[]}),
        ] {
            let row: ProviderModelMapping = serde_json::from_value(payload).unwrap();
            let mut doc = DocumentMut::new();
            prepare_model_catalog(&fixture.0, "provider", &[row], "custom-model", &mut doc)
                .unwrap();
            let generated = catalog(&doc);
            assert_eq!(
                generated["models"][0]["supported_reasoning_levels"]
                    .as_array()
                    .unwrap()
                    .len(),
                8
            );
            assert_eq!(
                generated["models"][0]["supported_reasoning_levels"][0]["effort"],
                "none"
            );
            assert_eq!(generated["models"][0]["default_reasoning_level"], "high");
        }
    }

    #[test]
    fn normalization_preserves_real_models_and_adds_missing_default() {
        let result = normalize_mappings(
            &[
                mapping(" vendor/model-A ", "菜单名称", Some(1_048_576)),
                mapping("vendor/model-A", "duplicate", Some(128_000)),
                mapping("vendor/model-a", "", None),
            ],
            "deepseek-chat",
        )
        .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], mapping("deepseek-chat", "deepseek-chat", None));
        assert_eq!(
            result[1],
            mapping("vendor/model-A", "菜单名称", Some(1_048_576))
        );
        assert_eq!(result[2].display_name, "vendor/model-a");
        assert!(normalize_mappings(&[], "").unwrap().is_empty());
    }

    #[test]
    fn rejects_invalid_ids_names_context_and_too_many_models() {
        for invalid in ["", "model\nname", &"a".repeat(201)] {
            assert!(normalize_mappings(&[mapping(invalid, "", None)], "default").is_err());
        }
        assert!(normalize_mappings(&[mapping("model", "bad\nname", None)], "model").is_err());
        for invalid in [0, -1, MAX_CONTEXT_WINDOW + 1] {
            assert!(normalize_mappings(&[mapping("model", "", Some(invalid))], "model").is_err());
        }
        let rows: Vec<_> = (0..64)
            .map(|index| mapping(&format!("model-{index}"), "", None))
            .collect();
        assert_eq!(normalize_mappings(&rows, "model-0").unwrap().len(), 64);
        assert!(normalize_mappings(&rows, "missing-default").is_err());
        let mut excessive = rows;
        excessive.push(mapping("model-64", "", None));
        assert!(normalize_mappings(&excessive, "model-0").is_err());
    }

    #[test]
    fn serialized_mapping_allows_omitted_display_name_and_context() {
        let mapping: ProviderModelMapping =
            serde_json::from_str(r#"{"model":"deepseek-chat"}"#).unwrap();
        let normalized = normalize_mappings(&[mapping], "deepseek-chat").unwrap();
        assert_eq!(
            serde_json::to_value(&normalized[0]).unwrap(),
            json!({"model":"deepseek-chat","displayName":"deepseek-chat","contextWindow":null})
        );
    }

    #[test]
    fn catalog_never_impersonates_gpt_or_copies_cached_capabilities() {
        let fixture = Fixture::new();
        let cache = json!({"models":[{"slug":"gpt-5.5","model_messages":{"instructions_template":"PRIVATE CACHED INSTRUCTIONS"},"apply_patch_tool_type":"freeform","experimental_supported_tools":["namespace-secret"],"supports_image_detail_original":true}]});
        let cache_text = serde_json::to_vec(&cache).unwrap();
        fs::write(fixture.0.join("models_cache.json"), &cache_text).unwrap();
        fs::write(fixture.0.join("config.toml"), "original-config").unwrap();
        fs::write(fixture.0.join("auth.json"), "original-auth").unwrap();
        let mut doc = DocumentMut::new();
        prepare_model_catalog(
            &fixture.0,
            "provider-a",
            &[mapping("deepseek-chat", "DeepSeek 常用", Some(131072))],
            "deepseek-chat",
            &mut doc,
        )
        .unwrap();
        let catalog = catalog(&doc);
        let model = &catalog["models"][0];
        assert_eq!(model["slug"], "deepseek-chat");
        assert_eq!(model["display_name"], "DeepSeek 常用");
        assert_eq!(model["context_window"], 131072);
        assert_eq!(model["apply_patch_tool_type"], Value::Null);
        assert_eq!(model["input_modalities"], json!(["text"]));
        assert!(!catalog.to_string().contains("gpt-5.5"));
        assert!(!catalog.to_string().contains("PRIVATE"));
        assert!(!catalog.to_string().contains("namespace-secret"));
        assert_eq!(
            fs::read(fixture.0.join("models_cache.json")).unwrap(),
            cache_text
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("config.toml")).unwrap(),
            "original-config"
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("auth.json")).unwrap(),
            "original-auth"
        );
    }

    #[test]
    fn providers_and_changed_content_have_distinct_immutable_catalogs() {
        let fixture = Fixture::new();
        let rows = [mapping("model", "Name", None)];
        let mut first = DocumentMut::new();
        let mut second = DocumentMut::new();
        prepare_model_catalog(&fixture.0, "../../provider-a", &rows, "model", &mut first).unwrap();
        let original_path = path(&first);
        let original_bytes = fs::read(&original_path).unwrap();
        prepare_model_catalog(&fixture.0, "provider-b", &rows, "model", &mut second).unwrap();
        assert_ne!(original_path, path(&second));
        assert!(
            original_path.starts_with(owned_catalog_directory(&fixture.0.canonicalize().unwrap()))
        );
        assert!(!original_path.to_string_lossy().contains("provider-a"));
        prepare_model_catalog(
            &fixture.0,
            "../../provider-a",
            &[mapping("model", "Changed", None)],
            "model",
            &mut first,
        )
        .unwrap();
        assert_ne!(original_path, path(&first));
        assert_eq!(fs::read(original_path).unwrap(), original_bytes);
    }

    #[test]
    fn unchanged_content_is_reused_and_tampered_file_is_not_overwritten() {
        let fixture = Fixture::new();
        let rows = [mapping("model", "Name", None)];
        let mut doc = DocumentMut::new();
        prepare_model_catalog(&fixture.0, "provider", &rows, "model", &mut doc).unwrap();
        let generated = path(&doc);
        let modified = fs::metadata(&generated).unwrap().modified().unwrap();
        prepare_model_catalog(&fixture.0, "provider", &rows, "model", &mut doc).unwrap();
        assert_eq!(
            fs::metadata(&generated).unwrap().modified().unwrap(),
            modified
        );
        fs::write(&generated, "keep this changed file").unwrap();
        let before = doc.to_string();
        assert!(prepare_model_catalog(&fixture.0, "provider", &rows, "model", &mut doc).is_err());
        assert_eq!(doc.to_string(), before);
        assert_eq!(
            fs::read_to_string(generated).unwrap(),
            "keep this changed file"
        );
    }

    #[test]
    fn disabling_removes_only_our_pointer_without_deleting_catalog() {
        let fixture = Fixture::new();
        let mut doc = DocumentMut::new();
        prepare_model_catalog(
            &fixture.0,
            "provider",
            &[mapping("model", "Name", None)],
            "model",
            &mut doc,
        )
        .unwrap();
        let generated = path(&doc);
        prepare_model_catalog(&fixture.0, "provider", &[], "model", &mut doc).unwrap();
        assert!(doc.get(CATALOG_FIELD).is_none());
        assert!(generated.is_file());
        for custom in [
            "custom-models.json",
            ".codex-x/model-catalogs/my-custom.json",
            ".codex-x/model-catalogs/../custom.json",
        ] {
            doc[CATALOG_FIELD] = value(custom);
            prepare_model_catalog(&fixture.0, "provider", &[], "model", &mut doc).unwrap();
            assert_eq!(doc[CATALOG_FIELD].as_str(), Some(custom));
        }
    }

    #[test]
    fn relative_and_stale_owned_pointers_can_be_removed() {
        let fixture = Fixture::new();
        let mut doc = DocumentMut::new();
        let pointer = format!(
            "{OWNED_DIRECTORY}/{CATALOG_DIRECTORY}/{}.json",
            "a".repeat(64)
        );
        doc[CATALOG_FIELD] = value(pointer);
        prepare_model_catalog(&fixture.0, "provider", &[], "", &mut doc).unwrap();
        assert!(doc.get(CATALOG_FIELD).is_none());
    }

    #[test]
    fn disabling_after_switching_codex_home_clears_verified_generated_pointer() {
        let first_home = Fixture::new();
        let second_home = Fixture::new();
        let mut doc = DocumentMut::new();
        prepare_model_catalog(
            &first_home.0,
            "provider-a",
            &[mapping("deepseek-v4-pro", "DeepSeek", None)],
            "deepseek-v4-pro",
            &mut doc,
        )
        .unwrap();
        let generated = path(&doc);
        let before = fs::read(&generated).unwrap();
        prepare_model_catalog(&second_home.0, "provider-b", &[], "another-model", &mut doc)
            .unwrap();
        assert!(doc.get(CATALOG_FIELD).is_none());
        assert_eq!(fs::read(&generated).unwrap(), before);
        assert!(!owned_catalog_directory(&second_home.0).exists());
    }

    #[test]
    fn official_cleanup_recognizes_other_home_but_preserves_unverified_user_catalog() {
        let first_home = Fixture::new();
        let second_home = Fixture::new();
        let mut doc = DocumentMut::new();
        prepare_model_catalog(
            &first_home.0,
            "provider-a",
            &[mapping("deepseek-v4-pro", "DeepSeek", None)],
            "deepseek-v4-pro",
            &mut doc,
        )
        .unwrap();
        let generated = path(&doc);
        let original: Value = serde_json::from_slice(&fs::read(&generated).unwrap()).unwrap();
        prepare_model_catalog(&second_home.0, "official", &[], "", &mut doc).unwrap();
        assert!(doc.get(CATALOG_FIELD).is_none());

        // A matching-looking name without the app's marker is user content.
        let mut no_marker = original.clone();
        no_marker.as_object_mut().unwrap().remove(OWNERSHIP_FIELD);
        // A copied marker does not authorize a modified catalog either.
        let mut modified = original;
        modified["models"][0]["display_name"] = json!("User's own menu");
        for unverified in [no_marker, modified] {
            fs::write(&generated, serde_json::to_vec(&unverified).unwrap()).unwrap();
            doc[CATALOG_FIELD] = value(generated.to_string_lossy().to_string());
            prepare_model_catalog(&second_home.0, "official", &[], "", &mut doc).unwrap();
            assert_eq!(path(&doc), generated);
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(&generated).unwrap()).unwrap(),
                unverified
            );
        }
    }

    #[test]
    fn omitted_context_uses_explicit_live_context_or_neutral_default() {
        let rows = [
            mapping("model", "Model", None),
            mapping("other", "Other", Some(64000)),
        ];
        let mut doc: DocumentMut = "model_context_window = 1000000".parse().unwrap();
        let catalog = build_catalog(&rows, &doc);
        assert_eq!(catalog["models"][0]["context_window"], 1_000_000);
        assert_eq!(catalog["models"][1]["context_window"], 64_000);
        doc["model_context_window"] = value(-1);
        assert_eq!(
            build_catalog(&rows, &doc)["models"][0]["context_window"],
            DEFAULT_CONTEXT_WINDOW
        );
    }

    #[cfg(unix)]
    #[test]
    fn generated_files_are_private_and_owned_directory_links_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let mut doc = DocumentMut::new();
        let rows = [mapping("model", "Model", None)];
        prepare_model_catalog(&fixture.0, "provider", &rows, "model", &mut doc).unwrap();
        assert_eq!(
            fs::metadata(path(&doc)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let linked = Fixture::new();
        symlink(&fixture.0, linked.0.join(OWNED_DIRECTORY)).unwrap();
        let before = doc.to_string();
        assert!(prepare_model_catalog(&linked.0, "provider", &rows, "model", &mut doc).is_err());
        assert_eq!(doc.to_string(), before);
    }

    #[cfg(unix)]
    #[test]
    fn redirected_codex_home_is_supported_but_custom_file_links_are_not_removed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let links = Fixture::new();
        let linked_home = links.0.join("codex-home");
        symlink(&fixture.0, &linked_home).unwrap();
        let mut doc = DocumentMut::new();
        prepare_model_catalog(
            &linked_home,
            "provider",
            &[mapping("model", "Model", None)],
            "model",
            &mut doc,
        )
        .unwrap();
        let generated = path(&doc);
        prepare_model_catalog(&linked_home, "provider", &[], "model", &mut doc).unwrap();
        assert!(doc.get(CATALOG_FIELD).is_none());
        fs::remove_file(&generated).unwrap();
        let custom = links.0.join("custom.json");
        fs::write(&custom, "user catalog").unwrap();
        symlink(&custom, &generated).unwrap();
        doc[CATALOG_FIELD] = value(generated.to_string_lossy().to_string());
        prepare_model_catalog(&fixture.0, "provider", &[], "model", &mut doc).unwrap();
        assert!(doc.get(CATALOG_FIELD).is_some());
        assert_eq!(fs::read_to_string(custom).unwrap(), "user catalog");
    }
}
