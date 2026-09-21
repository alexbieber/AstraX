use super::types::SavedPrompt;
use crate::error::{CodexxError, Result};
use crate::{now_rfc3339, open_db};
use rusqlite::params;
use std::collections::HashSet;

pub(crate) fn normalize_prompt_filename(input: &str, fallback: &str) -> String {
    let raw = input.trim().trim_end_matches(".md");
    let base = if raw.is_empty() { fallback } else { raw };
    let mut out = String::new();
    let mut last_dash = false;
    for ch in base.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-');
    format!("{}.md", if out.is_empty() { "custom-prompt" } else { out })
}

fn canonical_prompt_content(input: &str) -> String {
    input
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

pub(crate) fn list_saved_prompts_inner() -> Result<Vec<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT id, title, filename, content FROM prompts ORDER BY updated_at DESC, created_at DESC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedPrompt {
                id: row.get(0)?,
                title: row.get(1)?,
                filename: row.get(2)?,
                content: row.get(3)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut prompts = Vec::new();
    for row in rows {
        let prompt = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let filename_key = prompt.filename.to_ascii_lowercase();
        let content_key = canonical_prompt_content(&prompt.content);
        let duplicate_index = prompts.iter().position(|existing: &SavedPrompt| {
            existing.filename.to_ascii_lowercase() == filename_key
                || (canonical_prompt_content(&existing.content) == content_key
                    && (existing.id.starts_with("external-") || prompt.id.starts_with("external-")))
        });
        if let Some(index) = duplicate_index {
            let existing_is_external = prompts[index].id.starts_with("external-");
            let prompt_is_external = prompt.id.starts_with("external-");
            if existing_is_external && !prompt_is_external {
                prompts[index] = prompt;
            }
            continue;
        }
        prompts.push(prompt);
    }
    Ok(prompts)
}

pub(crate) fn save_prompt_inner(prompt: SavedPrompt) -> Result<SavedPrompt> {
    let conn = open_db()?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO prompts (id, title, filename, content, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            filename = excluded.filename,
            content = excluded.content,
            updated_at = excluded.updated_at",
        params![
            prompt.id,
            prompt.title,
            prompt.filename,
            prompt.content,
            now
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    list_saved_prompts_inner()?
        .into_iter()
        .find(|p| p.id == prompt.id)
        .ok_or_else(|| CodexxError::Database("prompt saved but not found".to_string()))
}

pub(crate) fn get_saved_prompt_inner(id: &str) -> Result<SavedPrompt> {
    list_saved_prompts_inner()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| CodexxError::Config(format!("提示词不存在: {id}")))
}

pub(crate) fn delete_prompt_inner(id: &str) -> Result<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM prompts WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

fn builtin_prompt_override_from_connection(
    conn: &rusqlite::Connection,
    template_id: &str,
) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT content FROM builtin_prompt_overrides WHERE template_id = ?1",
        [template_id],
        |row| row.get(0),
    ) {
        Ok(content) => Ok(Some(content)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

pub(crate) fn builtin_prompt_override_inner(template_id: &str) -> Result<Option<String>> {
    let conn = open_db()?;
    builtin_prompt_override_from_connection(&conn, template_id.trim())
}

pub(crate) fn builtin_prompt_override_ids_inner() -> Result<HashSet<String>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT template_id FROM builtin_prompt_overrides")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    rows.map(|row| row.map_err(|e| CodexxError::Database(e.to_string())))
        .collect()
}

fn save_builtin_prompt_override_on_connection(
    conn: &rusqlite::Connection,
    template_id: &str,
    content: &str,
) -> Result<()> {
    let id = template_id.trim();
    if id.is_empty() {
        return Err(CodexxError::Config("提示词模板标识不能为空".to_string()));
    }
    if content.trim().is_empty() {
        return Err(CodexxError::Config("提示词内容不能为空".to_string()));
    }
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO builtin_prompt_overrides (template_id, content, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(template_id) DO UPDATE SET
           content = excluded.content,
           updated_at = excluded.updated_at",
        params![id, content, now],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

pub(crate) fn save_builtin_prompt_override_inner(template_id: &str, content: &str) -> Result<()> {
    let conn = open_db()?;
    save_builtin_prompt_override_on_connection(&conn, template_id, content)
}

fn find_saved_prompt_by_content(content: &str) -> Result<Option<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT id, title, filename, content FROM prompts ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedPrompt {
                id: row.get(0)?,
                title: row.get(1)?,
                filename: row.get(2)?,
                content: row.get(3)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let target = canonical_prompt_content(content);
    for row in rows {
        let prompt = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        if canonical_prompt_content(&prompt.content) == target {
            return Ok(Some(prompt));
        }
    }
    Ok(None)
}

pub(super) fn find_saved_prompt_by_current_file(
    filename: &str,
    content: &str,
) -> Result<Option<SavedPrompt>> {
    if let Some(prompt) = find_saved_prompt_by_content(content)? {
        return Ok(Some(prompt));
    }
    let normalized_filename = normalize_prompt_filename(filename, "external-prompt");
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, filename, content FROM prompts
             WHERE lower(filename) = lower(?1)
             ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC
             LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([normalized_filename], |row| {
        Ok(SavedPrompt {
            id: row.get(0)?,
            title: row.get(1)?,
            filename: row.get(2)?,
            content: row.get(3)?,
        })
    }) {
        Ok(mut prompt) => {
            if canonical_prompt_content(&prompt.content) != canonical_prompt_content(content) {
                prompt.content = content.to_string();
            }
            Ok(Some(prompt))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn override_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open override database");
        conn.execute_batch(
            "CREATE TABLE builtin_prompt_overrides (
                template_id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .expect("create override table");
        conn
    }

    #[test]
    fn builtin_prompt_override_can_be_edited_repeatedly() {
        let conn = override_connection();
        save_builtin_prompt_override_on_connection(&conn, "template", "first")
            .expect("save initial override");
        save_builtin_prompt_override_on_connection(&conn, "template", "second")
            .expect("update override");

        assert_eq!(
            builtin_prompt_override_from_connection(&conn, "template")
                .expect("read override")
                .as_deref(),
            Some("second")
        );
    }

    #[test]
    fn builtin_prompt_override_rejects_empty_content() {
        let conn = override_connection();
        let error = save_builtin_prompt_override_on_connection(&conn, "template", "  \n")
            .expect_err("reject empty override");
        assert!(error.to_string().contains("内容不能为空"));
    }

    #[test]
    fn builtin_prompt_override_survives_database_reopen() {
        let database_path = std::env::temp_dir().join(format!(
            "codexx-prompt-override-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        {
            let conn = Connection::open(&database_path).expect("open override database");
            conn.execute_batch(
                "CREATE TABLE builtin_prompt_overrides (
                    template_id TEXT PRIMARY KEY,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .expect("create override table");
            save_builtin_prompt_override_on_connection(&conn, "template", "local content")
                .expect("save override");
        }

        let reopened = Connection::open(&database_path).expect("reopen override database");
        assert_eq!(
            builtin_prompt_override_from_connection(&reopened, "template")
                .expect("read persisted override")
                .as_deref(),
            Some("local content")
        );
        drop(reopened);
        std::fs::remove_file(database_path).expect("remove override database");
    }
}
