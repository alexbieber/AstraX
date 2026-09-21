use super::build_skills_mcp_state_inner;
use super::mcp::{
    commit_mcp_transaction_with_config, document_bytes, is_valid_mcp_config, json_to_toml_item,
    mcp_configs_equal, save_managed_mcp_on_connection,
};
use super::skills::{
    codex_skills_dir, copy_dir_recursive, disabled_skills_dir, read_skill_metadata,
    sanitize_dir_name,
};
use super::types::{SkillsMcpActionResult, SkillsMcpExportResult};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, parse_toml_document};
use crate::live_config::{acquire_live_config_lock, read_file_snapshot, text_from_snapshot};
use crate::paths::{app_home, normalized_path_scope};
use crate::toml_utils::ensure_table;
use crate::{config_path, now_rfc3339, open_db, resolve_codex_dir};
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zip::write::SimpleFileOptions;

const MANIFEST: &str = "codex-x-archive.json";
const FORMAT: &str = "codex-x-skills-mcp";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default)]
    skills: Vec<SkillEntry>,
    #[serde(default)]
    mcp_servers: Vec<McpEntry>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SkillEntry {
    directory: String,
    path: String,
    enabled: bool,
    note: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpEntry {
    id: String,
    name: String,
    config: Value,
    enabled: bool,
    note: Option<String>,
}

fn invalid(message: impl Into<String>) -> CodexxError {
    CodexxError::Config(message.into())
}
fn zip_error(error: impl std::fmt::Display) -> CodexxError {
    invalid(format!("Unable to read or write ZIP: {error}"))
}
fn unique_name(prefix: &str) -> String {
    format!(
        ".{prefix}-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    )
}
struct TempDir(PathBuf);
impl TempDir {
    fn new(parent: &Path) -> Result<Self> {
        ensure_directory(parent)?;
        let path = parent.join(unique_name("codex-x-archive"));
        fs::create_dir(&path).map_err(|error| io_err(&path, error))?;
        Ok(Self(path))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Reject Windows traversal/drive names on every OS too, so exported bundles can
// safely move between machines. Links and special files are never extracted.
fn safe_relative(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains(':') || name.contains('\0') {
        return Err(invalid("ZIP contains unsafe file paths"));
    }
    let path = Path::new(name);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid("ZIP contains unsafe file paths"));
    }
    for part in name.trim_end_matches('/').split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part.ends_with([' ', '.'])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(invalid("ZIP contains file paths that are not safe across platforms"));
        }
    }
    Ok(path.to_path_buf())
}
fn safe_directory(name: &str) -> Result<()> {
    let path = safe_relative(name)?;
    if path.components().count() != 1 || name.starts_with('.') {
        return Err(invalid("Invalid Skill directory name in archive"));
    }
    Ok(())
}

fn extract<R: Read + Seek>(reader: R, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(reader).map_err(zip_error)?;
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().trim_end_matches('/');
        let relative = safe_relative(name)?;
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        if mode != 0 && mode != 0o100000 && mode != 0o040000 {
            return Err(invalid("ZIP does not support symlinks or special files"));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid("ZIP contains duplicate file paths"));
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            ensure_directory(&output)?;
        } else {
            ensure_directory(output.parent().ok_or_else(|| invalid("Invalid ZIP path"))?)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|error| io_err(&output, error))?;
            std::io::copy(&mut entry, &mut file).map_err(|error| io_err(&output, error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    fs::set_permissions(&output, fs::Permissions::from_mode(mode & 0o777))
                        .map_err(|error| io_err(&output, error))?;
                }
            }
        }
    }
    Ok(())
}

fn collect_entries(root: &Path, current: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(current).map_err(|error| io_err(current, error))?;
    if metadata.file_type().is_symlink() {
        return Err(invalid("Skill contains symlinks; replace them with real files before exporting"));
    }
    if metadata.is_file() {
        output.push(
            current
                .strip_prefix(root)
                .map_err(|_| invalid("Invalid Skill file path"))?
                .to_path_buf(),
        );
    } else if metadata.is_dir() {
        if current != root {
            output.push(
                current
                    .strip_prefix(root)
                    .map_err(|_| invalid("Invalid Skill directory path"))?
                    .to_path_buf(),
            );
        }
        for entry in fs::read_dir(current).map_err(|error| io_err(current, error))? {
            collect_entries(
                root,
                &entry.map_err(|error| io_err(current, error))?.path(),
                output,
            )?;
        }
    } else {
        return Err(invalid("Skill contains unsupported special files"));
    }
    Ok(())
}
fn directory_digest(path: &Path) -> Result<Vec<u8>> {
    let mut files = Vec::new();
    collect_entries(path, path, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    for relative in files {
        let name = relative.to_string_lossy().replace('\\', "/");
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        let file_path = path.join(relative);
        if file_path.is_dir() {
            digest.update(b"directory");
            continue;
        }
        digest.update(b"file");
        let mut file = File::open(&file_path).map_err(|error| io_err(&file_path, error))?;
        digest.update(
            file.metadata()
                .map_err(|error| io_err(&file_path, error))?
                .len()
                .to_le_bytes(),
        );
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| io_err(&file_path, error))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    }
    Ok(digest.finalize().to_vec())
}
fn write_archive(path: &Path, manifest: &Manifest, sources: &[PathBuf]) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_err(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| io_err(path, error))?;
    }
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    writer.start_file(MANIFEST, options).map_err(zip_error)?;
    serde_json::to_writer_pretty(&mut writer, manifest).map_err(|_| invalid("Unable to write archive manifest"))?;
    for (entry, source) in manifest.skills.iter().zip(sources) {
        let mut files = Vec::new();
        collect_entries(source, source, &mut files)?;
        files.sort();
        for relative in files {
            let relative_name = relative.to_string_lossy().replace('\\', "/");
            safe_relative(&relative_name)?;
            let source_file = source.join(&relative);
            let mut options = options;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                options = options.unix_permissions(
                    fs::metadata(&source_file)
                        .map_err(|error| io_err(&source_file, error))?
                        .permissions()
                        .mode()
                        & 0o777,
                );
            }
            if source_file.is_dir() {
                writer
                    .add_directory(format!("{}/{}/", entry.path, relative_name), options)
                    .map_err(zip_error)?;
                continue;
            }
            writer
                .start_file(format!("{}/{}", entry.path, relative_name), options)
                .map_err(zip_error)?;
            let mut input =
                File::open(&source_file).map_err(|error| io_err(&source_file, error))?;
            std::io::copy(&mut input, &mut writer).map_err(|error| io_err(path, error))?;
        }
    }
    writer
        .finish()
        .map_err(zip_error)?
        .sync_all()
        .map_err(|error| io_err(path, error))?;
    Ok(())
}

fn destination_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_err(path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid("Choose a regular ZIP file path; cannot overwrite a symlink or directory"));
    }
    let mut file = File::open(path).map_err(|error| io_err(path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_err(path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(Some(digest.finalize().to_vec()))
}

fn write_archive_atomic<BeforeReplace: FnOnce() -> Result<()>>(
    output: &Path,
    manifest: &Manifest,
    sources: &[PathBuf],
    before_replace: BeforeReplace,
) -> Result<()> {
    let initial = destination_snapshot(output)?;
    let parent = output
        .parent()
        .ok_or_else(|| invalid("Please choose a valid save directory"))?;
    let temporary = parent.join(unique_name("codex-x-export"));
    let result = (|| {
        write_archive(&temporary, manifest, sources)?;
        before_replace()?;
        if destination_snapshot(output)? != initial {
            return Err(invalid(
                "Save location was modified by another program; choose a location again and export",
            ));
        }
        fs::rename(&temporary, output).map_err(|error| io_err(output, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_export_destination(output: &Path, codex_dir: &Path) -> Result<PathBuf> {
    if !output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err(invalid("Please save the export as a .zip file"));
    }
    let parent = output
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| invalid("Please choose a valid save directory"))?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| io_err(parent, error))?;
    for protected in [codex_dir.to_path_buf(), app_home()?] {
        let canonical = fs::canonicalize(&protected).unwrap_or(protected);
        if canonical_parent.starts_with(canonical) {
            return Err(invalid(
                "Save the ZIP to Downloads, Desktop, or similar — not inside Codex config or app data directories",
            ));
        }
    }
    destination_snapshot(output)?;
    Ok(canonical_parent)
}

pub(crate) fn export_skills_mcp_archive_inner(
    config_dir: Option<String>,
    kind: String,
    destination: String,
) -> Result<SkillsMcpExportResult> {
    if !matches!(kind.as_str(), "skills" | "mcp") {
        return Err(invalid("Please choose Skills or MCP to export"));
    }
    let state = build_skills_mcp_state_inner(config_dir)?;
    let mut manifest = Manifest {
        format: FORMAT.to_owned(),
        version: 1,
        skills: vec![],
        mcp_servers: vec![],
    };
    let mut sources = Vec::new();
    if kind == "skills" {
        for skill in state.skills {
            safe_directory(&skill.directory)?;
            manifest.skills.push(SkillEntry {
                path: format!("skills/{}", skill.directory),
                directory: skill.directory,
                enabled: skill.enabled,
                note: skill.note,
            });
            sources.push(PathBuf::from(skill.path));
        }
    } else {
        for mcp in state.mcp_servers {
            manifest.mcp_servers.push(McpEntry {
                id: mcp.id,
                name: mcp.name,
                config: mcp.config_json,
                enabled: mcp.enabled,
                note: mcp.note,
            });
        }
    }
    if manifest.skills.is_empty() && manifest.mcp_servers.is_empty() {
        return Err(invalid("Nothing to export right now"));
    }
    let output = PathBuf::from(destination);
    let canonical_parent = validate_export_destination(&output, Path::new(&state.codex_dir))?;
    for source in &sources {
        let canonical_source = fs::canonicalize(source).map_err(|error| io_err(source, error))?;
        if canonical_parent.starts_with(canonical_source) {
            return Err(invalid("Please save the ZIP outside the Skill directory"));
        }
    }
    write_archive_atomic(&output, &manifest, &sources, || Ok(()))?;
    Ok(SkillsMcpExportResult {
        path: output.display().to_string(),
        exported_skills: manifest.skills.len(),
        exported_mcp: manifest.mcp_servers.len(),
    })
}

pub(crate) fn install_skill_archive_path_inner(
    config_dir: Option<String>,
    path: String,
) -> Result<SkillsMcpActionResult> {
    let path = Path::new(&path);
    let file = File::open(path).map_err(|error| io_err(path, error))?;
    install_archive_reader(
        config_dir,
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("skills.zip")
            .to_owned(),
        file,
    )
}

fn find_skill_roots(path: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if path.join("SKILL.md").is_file() {
        output.push(path.to_path_buf());
        return Ok(());
    }
    for entry in fs::read_dir(path).map_err(|error| io_err(path, error))? {
        let entry = entry.map_err(|error| io_err(path, error))?;
        if entry
            .file_type()
            .map_err(|error| io_err(path, error))?
            .is_dir()
        {
            find_skill_roots(&entry.path(), output)?;
        }
    }
    Ok(())
}
fn manifest_from_extracted(path: &Path, file_name: &str) -> Result<Manifest> {
    let manifest_path = path.join(MANIFEST);
    if manifest_path.exists() {
        let file = File::open(&manifest_path).map_err(|error| io_err(&manifest_path, error))?;
        // This limits only metadata, not ZIP contents or Skill file sizes.
        let manifest: Manifest = serde_json::from_reader(file.take(8 * 1024 * 1024))
            .map_err(|_| invalid("Astra archive manifest is invalid or too large"))?;
        if manifest.format != FORMAT || manifest.version != 1 {
            return Err(invalid("This archive version is not supported yet; please update Astra"));
        }
        return Ok(manifest);
    }
    let mut roots = Vec::new();
    find_skill_roots(path, &mut roots)?;
    let mut manifest = Manifest {
        format: FORMAT.to_owned(),
        version: 1,
        skills: vec![],
        mcp_servers: vec![],
    };
    for root in roots {
        let fallback = if root == path {
            file_name.trim_end_matches(".zip")
        } else {
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(file_name.trim_end_matches(".zip"))
        };
        let (name, _) = read_skill_metadata(&root, fallback);
        let relative = root
            .strip_prefix(path)
            .map_err(|_| invalid("Invalid Skill path"))?
            .to_string_lossy()
            .replace('\\', "/");
        manifest.skills.push(SkillEntry {
            directory: sanitize_dir_name(&name, "skill"),
            path: relative,
            enabled: true,
            note: None,
        });
    }
    Ok(manifest)
}

pub(super) fn install_archive_reader<R: Read + Seek>(
    config_dir: Option<String>,
    file_name: String,
    reader: R,
) -> Result<SkillsMcpActionResult> {
    let temporary = TempDir::new(&app_home()?.join("tmp"))?;
    extract(reader, &temporary.0)?;
    let manifest = manifest_from_extracted(&temporary.0, &file_name)?;
    let (imported_skills, imported_mcp, skipped) =
        import_manifest(config_dir.clone(), &temporary.0, manifest)?;
    Ok(SkillsMcpActionResult {
        imported_skills,
        imported_mcp,
        message: format!(
            "Imported {imported_skills} Skills and {imported_mcp} MCP{}",
            if skipped > 0 {
                format!(", skipped {skipped} existing items")
            } else {
                String::new()
            }
        ),
        state: build_skills_mcp_state_inner(config_dir)?,
    })
}

fn import_manifest(
    config_dir: Option<String>,
    root: &Path,
    manifest: Manifest,
) -> Result<(usize, usize, usize)> {
    if manifest.skills.is_empty() && manifest.mcp_servers.is_empty() {
        return Err(invalid("ZIP has no importable Skills or MCP"));
    }
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    let current = build_skills_mcp_state_inner(config_dir)?;
    let cfg = config_path(&codex_dir);
    let before = read_file_snapshot(&cfg)?;
    let mut document = parse_toml_document(&cfg, &text_from_snapshot(&cfg, before.as_deref())?)?;
    let mut skill_ids = HashSet::new();
    let mut mcp_ids = HashSet::new();
    let existing_skills: HashMap<_, _> = current
        .skills
        .iter()
        .map(|skill| (skill.directory.as_str(), skill))
        .collect();
    let existing_mcp: HashMap<_, _> = current
        .mcp_servers
        .iter()
        .map(|mcp| (mcp.id.as_str(), mcp))
        .collect();
    let mut skill_plan = Vec::new();
    let mut mcp_plan = Vec::new();
    let mut skipped = 0;
    for skill in manifest.skills {
        safe_directory(&skill.directory)?;
        if !skill_ids.insert(sanitize_dir_name(&skill.directory, "skill")) {
            return Err(invalid("Archive contains a Skill with a duplicate name"));
        }
        let relative = if skill.path.is_empty() {
            PathBuf::new()
        } else {
            safe_relative(&skill.path)?
        };
        let source = root.join(relative);
        if !source.join("SKILL.md").is_file() {
            return Err(invalid(format!("Skill {} is missing SKILL.md", skill.directory)));
        }
        if let Some(existing) = existing_skills.get(skill.directory.as_str()) {
            if directory_digest(&source)? == directory_digest(Path::new(&existing.path))? {
                skipped += 1;
                continue;
            }
            return Err(invalid(format!(
                "Skill "{}" already exists with different content. Rename or remove it first; import will not overwrite it.",
                skill.directory
            )));
        }
        let parent = if skill.enabled {
            codex_skills_dir(&codex_dir)
        } else {
            disabled_skills_dir()?
        };
        let destination = parent.join(&skill.directory);
        if destination.exists() {
            return Err(invalid(format!(
                "Skill directory "{}" already exists; not overwritten",
                skill.directory
            )));
        }
        skill_plan.push((skill, source, destination));
    }
    for mcp in manifest.mcp_servers {
        if mcp.id.trim().is_empty()
            || !mcp_ids.insert(mcp.id.clone())
            || !is_valid_mcp_config(&mcp.config)
        {
            return Err(invalid("Archive contains invalid or duplicate MCP configs"));
        }
        if let Some(existing) = existing_mcp.get(mcp.id.as_str()) {
            if mcp_configs_equal(&existing.config_json, &mcp.config) {
                skipped += 1;
                continue;
            }
            return Err(invalid(format!(
                "MCP "{}" already exists with a different config. Rename or remove it first; import will not overwrite it.",
                mcp.name
            )));
        }
        // A malformed live entry is still user data; do not overwrite it.
        if document
            .get("mcp_servers")
            .and_then(|item| item.as_table())
            .is_some_and(|table| table.contains_key(&mcp.id))
        {
            return Err(invalid(format!("MCP "{}" already exists; not overwritten", mcp.name)));
        }
        mcp_plan.push(mcp);
    }
    let mut connection = open_db()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let scope = normalized_path_scope(&codex_dir);
    let save_note = |kind: &str, id: &str, note: &Option<String>| -> Result<()> {
        if let Some(note) = note.as_deref().filter(|note| !note.trim().is_empty()) {
            if note.chars().count() > super::SKILLS_MCP_NOTE_MAX_CHARS {
                return Err(invalid("Notes in the archive are too long"));
            }
            transaction.execute("INSERT INTO skills_mcp_notes (codex_dir,item_kind,item_id,note,updated_at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(codex_dir,item_kind,item_id) DO NOTHING", params![scope, kind, id, note, now_rfc3339()]).map_err(|error| CodexxError::Database(error.to_string()))?;
        }
        Ok(())
    };
    for mcp in &mcp_plan {
        save_managed_mcp_on_connection(&transaction, &mcp.id, &mcp.name, &mcp.config, mcp.enabled)?;
        if mcp.enabled {
            ensure_table(document.as_table_mut(), "mcp_servers")?
                .insert(&mcp.id, json_to_toml_item(&mcp.config));
        }
        save_note("mcp", &mcp.id, &mcp.note)?;
    }
    for (skill, _, _) in &skill_plan {
        save_note(
            "skill",
            &sanitize_dir_name(&skill.directory, "skill"),
            &skill.note,
        )?;
    }
    let mut created = Vec::new();
    let apply = (|| -> Result<()> {
        for (_, source, destination) in &skill_plan {
            let staging = TempDir::new(
                destination
                    .parent()
                    .ok_or_else(|| invalid("Invalid Skill destination directory"))?,
            )?;
            copy_dir_recursive(source, &staging.0)?;
            if destination.exists() {
                return Err(invalid("A Skill with the same name appeared during import; stopped without overwriting"));
            }
            fs::rename(&staging.0, destination).map_err(|error| io_err(destination, error))?;
            created.push(destination.clone());
        }
        let after = document_bytes(before.as_deref(), &document);
        commit_mcp_transaction_with_config(
            transaction,
            &codex_dir,
            before,
            after,
            "import-skills-mcp-zip",
            |_| Ok(()),
            |_| Ok(()),
        )
    })();
    if apply.is_err() {
        for path in created {
            let _ = fs::remove_dir_all(path);
        }
    }
    apply?;
    Ok((skill_plan.len(), mcp_plan.len(), skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};

    fn manifest() -> Manifest {
        Manifest {
            format: FORMAT.into(),
            version: 1,
            skills: vec![],
            mcp_servers: vec![],
        }
    }
    fn test_dir() -> TempDir {
        TempDir::new(&std::env::temp_dir().join("codex-x-archive-tests")).unwrap()
    }
    fn codex(dir: &Path) -> Option<String> {
        fs::create_dir_all(dir).unwrap();
        Some(dir.display().to_string())
    }
    fn skill(root: &Path, directory: &str, enabled: bool) -> SkillEntry {
        let path = root.join("skills").join(directory);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {directory}\ndescription: Example\n---\n# Readme"),
        )
        .unwrap();
        fs::write(path.join(".extra"), b"hidden file").unwrap();
        fs::create_dir_all(path.join("empty-resources")).unwrap();
        SkillEntry {
            directory: directory.into(),
            path: format!("skills/{directory}"),
            enabled,
            note: Some("My skill note".into()),
        }
    }
    fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn clean_mcp(id: &str) {
        let db = open_db().unwrap();
        db.execute("DELETE FROM managed_mcp_servers WHERE id=?1", [id])
            .unwrap();
        db.execute("DELETE FROM skills_mcp_notes WHERE item_id=?1", [id])
            .unwrap();
    }

    #[test]
    fn zip_bundle_round_trip_preserves_files_notes_disabled_skills_and_mcp_settings() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        let source = temporary.0.join("source");
        let destination = temporary.0.join("codex");
        let directory = format!("archive-roundtrip-{}", std::process::id());
        let id = format!("archive-roundtrip-mcp-{}", std::process::id());
        clean_mcp(&id);
        let disabled = disabled_skills_dir().unwrap().join(&directory);
        let _ = fs::remove_dir_all(&disabled);
        let mut bundle = manifest();
        bundle.skills.push(skill(&source, &directory, false));
        bundle.mcp_servers.push(McpEntry { id: id.clone(), name: "Portable MCP".into(), config: json!({"command":"example-server", "args":["--safe"], "env":{"TOKEN":"synthetic-test-token"}}), enabled: true, note: Some("MCP note".into()) });
        let output = temporary.0.join("bundle.zip");
        write_archive(&output, &bundle, &[source.join("skills").join(&directory)]).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(
            destination.join("config.toml"),
            "# Keep this\nmodel = \"gpt-test\"\n",
        )
        .unwrap();
        let imported =
            install_skill_archive_path_inner(codex(&destination), output.display().to_string())
                .unwrap();
        assert_eq!((imported.imported_skills, imported.imported_mcp), (1, 1));
        let skill = imported
            .state
            .skills
            .iter()
            .find(|skill| skill.id == directory)
            .unwrap();
        assert!(!skill.enabled);
        assert_eq!(skill.note.as_deref(), Some("My skill note"));
        assert_eq!(fs::read(disabled.join(".extra")).unwrap(), b"hidden file");
        assert!(disabled.join("empty-resources").is_dir());
        let mcp = imported
            .state
            .mcp_servers
            .iter()
            .find(|mcp| mcp.id == id)
            .unwrap();
        assert_eq!(mcp.note.as_deref(), Some("MCP note"));
        assert!(mcp.enabled);
        assert!(fs::read_to_string(destination.join("config.toml"))
            .unwrap()
            .starts_with("# Keep this"));
        let second =
            install_skill_archive_path_inner(codex(&destination), output.display().to_string())
                .unwrap();
        assert_eq!((second.imported_skills, second.imported_mcp), (0, 0));
        fs::remove_dir_all(disabled).unwrap();
        clean_mcp(&id);
    }

    #[test]
    fn regular_skill_zip_larger_than_twenty_megabytes_installs_without_size_limit() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        let payload = vec![b'x'; 21 * 1024 * 1024];
        let bytes = zip_entries(&[
            (
                "package/SKILL.md",
                b"---\nname: large-archive-fixture\n---\n",
            ),
            ("package/data.bin", &payload),
        ]);
        assert!(bytes.len() > 20 * 1024 * 1024);
        let result = install_archive_reader(
            codex(&temporary.0.join("codex")),
            "large.zip".into(),
            Cursor::new(bytes),
        )
        .unwrap();
        assert_eq!(result.imported_skills, 1);
        assert_eq!(
            fs::metadata(
                temporary
                    .0
                    .join("codex/skills/large-archive-fixture/data.bin")
            )
            .unwrap()
            .len(),
            payload.len() as u64
        );
    }

    #[test]
    fn conflicting_bundle_is_rejected_before_any_skill_or_mcp_is_written() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        let source = temporary.0.join("source");
        let destination = temporary.0.join("codex");
        let mut bundle = manifest();
        bundle
            .skills
            .push(skill(&source, "archive-new-first", true));
        bundle
            .skills
            .push(skill(&source, "archive-existing-second", true));
        let existing = destination.join("skills/archive-existing-second");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("SKILL.md"), "original").unwrap();
        assert!(import_manifest(codex(&destination), &source, bundle).is_err());
        assert!(!destination.join("skills/archive-new-first").exists());
        assert_eq!(
            fs::read_to_string(existing.join("SKILL.md")).unwrap(),
            "original"
        );
    }

    #[test]
    fn unsafe_duplicate_and_link_zip_entries_are_rejected() {
        let temporary = test_dir();
        for name in [
            "../escape",
            "/absolute",
            "C:/drive",
            "folder\\escape",
            "CON.txt",
        ] {
            let target = TempDir::new(&temporary.0).unwrap();
            assert!(
                extract(Cursor::new(zip_entries(&[(name, b"unsafe")])), &target.0).is_err(),
                "accepted {name}"
            );
        }
        let target = TempDir::new(&temporary.0).unwrap();
        assert!(extract(
            Cursor::new(zip_entries(&[("a", b"1"), ("A", b"2")])),
            &target.0
        )
        .is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("link", "../outside", SimpleFileOptions::default())
            .unwrap();
        assert!(extract(
            Cursor::new(writer.finish().unwrap().into_inner()),
            &TempDir::new(&temporary.0).unwrap().0
        )
        .is_err());
    }

    #[test]
    fn empty_and_unknown_version_archives_are_rejected() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        assert!(install_archive_reader(
            codex(&temporary.0.join("codex")),
            "empty.zip".into(),
            Cursor::new(zip_entries(&[("readme.txt", b"No skills")]))
        )
        .is_err());
        let mut unknown = manifest();
        unknown.version = 99;
        let bytes = serde_json::to_vec(&unknown).unwrap();
        assert!(install_archive_reader(
            codex(&temporary.0.join("codex")),
            "future.zip".into(),
            Cursor::new(zip_entries(&[(MANIFEST, &bytes)]))
        )
        .is_err());
    }

    #[test]
    fn exported_skill_archive_can_be_imported_with_original_state() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        let source = temporary.0.join("source");
        let destination = temporary.0.join("destination");
        let directory = format!("archive-export-{}", std::process::id());
        skill(&source, &directory, true);
        let archive = temporary.0.join("export.zip");
        let result = export_skills_mcp_archive_inner(
            codex(&source),
            "skills".into(),
            archive.display().to_string(),
        )
        .unwrap();
        assert!(result.exported_skills >= 1);
        assert_eq!(result.exported_mcp, 0);
        let imported =
            install_skill_archive_path_inner(codex(&destination), archive.display().to_string())
                .unwrap();
        assert!(imported
            .state
            .skills
            .iter()
            .any(|skill| skill.directory == directory && skill.enabled));
        assert_eq!(
            directory_digest(&source.join("skills").join(&directory)).unwrap(),
            directory_digest(&destination.join("skills").join(&directory)).unwrap()
        );
    }

    #[test]
    fn same_mcp_id_with_different_configuration_does_not_overwrite_live_config() {
        let _guard = crate::app_db::test_db_guard();
        let temporary = test_dir();
        let destination = temporary.0.join("codex");
        codex(&destination);
        let original = "[mcp_servers.archive_conflict]\ncommand = \"existing\"\n";
        fs::write(destination.join("config.toml"), original).unwrap();
        let mut bundle = manifest();
        bundle.mcp_servers.push(McpEntry {
            id: "archive_conflict".into(),
            name: "Conflict".into(),
            config: json!({"command":"different"}),
            enabled: true,
            note: None,
        });
        assert!(import_manifest(codex(&destination), &temporary.0, bundle).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("config.toml")).unwrap(),
            original
        );
    }
    #[test]
    fn export_refuses_changed_destinations_and_cleans_temporary_output() {
        let temporary = test_dir();
        let output = temporary.0.join("export.zip");
        fs::write(&output, b"before").unwrap();
        let result = write_archive_atomic(&output, &manifest(), &[], || {
            fs::write(&output, b"external-update").unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&output).unwrap(), b"external-update");
        assert_eq!(fs::read_dir(&temporary.0).unwrap().count(), 1);
    }

    #[test]
    fn export_rejects_codex_data_and_non_zip_destinations() {
        let temporary = test_dir();
        let codex = temporary.0.join("codex");
        fs::create_dir_all(&codex).unwrap();
        assert!(validate_export_destination(&codex.join("backup.zip"), &codex).is_err());
        assert!(validate_export_destination(&temporary.0.join("auth.json"), &codex).is_err());
        assert!(validate_export_destination(&temporary.0.join("backup.zip"), &codex).is_ok());
        #[cfg(unix)]
        {
            let file = temporary.0.join("original.zip");
            fs::write(&file, "original").unwrap();
            let link = temporary.0.join("symlink.zip");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(validate_export_destination(&link, &codex).is_err());
            assert_eq!(fs::read_to_string(file).unwrap(), "original");
        }
    }
}
