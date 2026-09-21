//! Native file selection keeps large archives out of the renderer/IPC payload.
use crate::error::{CodexxError, Result};
use std::path::{Path, PathBuf};

fn export_name(suggested: &str, fallback: &str, extension: &str) -> String {
    let mut bytes = 0;
    let stem: String = suggested
        .trim()
        .chars()
        .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
        .take_while(|c| {
            bytes += c.len_utf8();
            bytes <= 180
        })
        .collect();
    let stem = stem.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let stem = if stem.is_empty() { fallback } else { stem };
    let prefix = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(prefix.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (prefix.len() == 4
            && (prefix.starts_with("COM") || prefix.starts_with("LPT"))
            && prefix.as_bytes()[3].is_ascii_digit());
    format!("{}{stem}.{extension}", if reserved { "_" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggested_names_fit_filesystems_without_losing_utf8_or_using_windows_devices() {
        for title in ["中".repeat(100), "🌏".repeat(100)] {
            let name = export_name(&title, "session", "md");
            assert!(name.len() <= 184);
            assert!(name.ends_with(".md"));
            assert!(!name.contains('\u{fffd}'));
        }
        for title in ["CON", "nul", "PRN.notes", "LPT1", "COM9"] {
            assert!(export_name(title, "session", "md").starts_with('_'));
        }
        assert_eq!(export_name(" . /\\ ", "session", "zip"), "session.zip");
    }
}

fn require_extension(path: &Path, extension: &str) -> Result<()> {
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        return Err(CodexxError::Config(format!(
            "请使用 .{extension} 文件名保存。"
        )));
    }
    Ok(())
}

async fn save_path(
    window: &tauri::WebviewWindow,
    name: &str,
    extension: &str,
    title: &str,
) -> Result<Option<PathBuf>> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_parent(window)
        .set_title(title)
        .set_file_name(name)
        .add_filter(extension, &[extension])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    require_extension(file.path(), extension)?;
    Ok(Some(file.path().to_path_buf()))
}

#[tauri::command]
pub(crate) async fn export_codex_sessions(
    window: tauri::WebviewWindow,
    config_dir: Option<String>,
    session_ids: Vec<String>,
    suggested_name: String,
    lang: Option<String>,
) -> Result<Option<crate::sessions::SessionExportResult>> {
    if session_ids.is_empty() {
        return Err(CodexxError::Config("请先选择要导出的会话。".into()));
    }
    let extension = if session_ids.len() == 1 { "md" } else { "zip" };
    let name = export_name(&suggested_name, "Codex-sessions", extension);
    let title = if lang.as_deref() == Some("en") {
        "Export conversations"
    } else {
        "导出会话"
    };
    let Some(destination) = save_path(&window, &name, extension, title).await? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::sessions::export_codex_sessions_inner(
            config_dir,
            session_ids,
            destination.to_string_lossy().into_owned(),
        )
        .map(Some)
    })
    .await
    .map_err(|_| CodexxError::Config("导出会话失败，请重试。".into()))?
}

#[tauri::command]
pub(crate) async fn export_skills_mcp_archive(
    window: tauri::WebviewWindow,
    config_dir: Option<String>,
    kind: String,
    lang: Option<String>,
) -> Result<Option<crate::skills_mcp::SkillsMcpExportResult>> {
    let label = match kind.as_str() {
        "mcp" => "MCP",
        "skills" => "Skills",
        _ => return Err(CodexxError::Config("不支持的导出类型。".into())),
    };
    let name = format!(
        "Astra-{label}-{}.zip",
        chrono::Local::now().format("%Y%m%d")
    );
    let title = if lang.as_deref() == Some("en") {
        format!("Export {label}")
    } else {
        format!("导出 {label}")
    };
    let Some(destination) = save_path(&window, &name, "zip", &title).await? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::skills_mcp::export_skills_mcp_archive_inner(
            config_dir,
            kind,
            destination.to_string_lossy().into_owned(),
        )
        .map(Some)
    })
    .await
    .map_err(|_| CodexxError::Config("导出 ZIP 失败，请重试。".into()))?
}

#[tauri::command]
pub(crate) async fn import_skills_mcp_archive(
    window: tauri::WebviewWindow,
    config_dir: Option<String>,
    lang: Option<String>,
) -> Result<Option<crate::skills_mcp::SkillsMcpActionResult>> {
    let title = if lang.as_deref() == Some("en") {
        "Import Skills / MCP ZIP"
    } else {
        "导入 Skills / MCP ZIP"
    };
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title(title)
        .add_filter("ZIP", &["zip"])
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path = file.path().to_string_lossy().into_owned();
    tauri::async_runtime::spawn_blocking(move || {
        crate::skills_mcp::install_skill_archive_path_inner(config_dir, path).map(Some)
    })
    .await
    .map_err(|_| CodexxError::Config("导入 ZIP 失败，请重试。".into()))?
}
