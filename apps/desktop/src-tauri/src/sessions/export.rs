use super::storage::{
    is_canonical_rollout_storage_path, list_session_previews_with_paths, session_project_title,
    sqlite_candidate_paths,
};
use super::types::{RolloutScan, SessionPreview};
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zip::{write::SimpleFileOptions, ZipWriter};

// Irrelevant fields are discarded by serde without allocating their contents.
// A bound on an individual record also bounds retained, user-visible message text.
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MESSAGE_KEYS: usize = 250_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionExportResult {
    pub(crate) path: String,
    pub(crate) exported_sessions: usize,
    pub(crate) failed_sessions: usize,
    pub(crate) warnings: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    payload: Payload,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    role: Option<String>,
    channel: Option<String>,
    phase: Option<String>,
    recipient: Option<String>,
    message: Option<String>,
    content: Vec<ContentPart>,
    images: Presence,
    local_images: Presence,
}

#[derive(Default)]
struct Presence(bool);
impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct PresenceVisitor;
        impl<'de> serde::de::Visitor<'de> for PresenceVisitor {
            type Value = Presence;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an attachment list")
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Presence, E> {
                Ok(Presence(false))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Presence, A::Error> {
                let mut any = false;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(Presence(any))
            }
        }
        deserializer.deserialize_any(PresenceVisitor)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

struct Message {
    role: &'static str,
    body: String,
    timestamp: Option<String>,
    event: bool,
}

fn injected_user_context(text: &str) -> bool {
    let value = text.trim_start();
    [
        "# AGENTS.md instructions",
        "<environment_context>",
        "<permissions instructions>",
        "<app-context>",
        "<user_instructions>",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn visible_message(record: Record) -> Option<Message> {
    let payload = record.payload;
    if matches!(
        payload.channel.as_deref(),
        Some("analysis" | "justify" | "confidence")
    ) || matches!(payload.phase.as_deref(), Some("analysis"))
        || payload
            .recipient
            .as_deref()
            .is_some_and(|value| value != "all")
    {
        return None;
    }
    let (role, mut body, event) = match (record.kind.as_str(), payload.kind.as_str()) {
        ("event_msg", "user_message") => ("user", payload.message.unwrap_or_default(), true),
        ("event_msg", "agent_message") => ("assistant", payload.message.unwrap_or_default(), true),
        ("response_item", "message") => {
            let role = match payload.role.as_deref()? {
                "user" => "user",
                "assistant" => "assistant",
                _ => return None,
            };
            let body = payload
                .content
                .into_iter()
                .filter_map(|part| match part.kind.as_str() {
                    "input_text" | "output_text" | "text" => part.text,
                    "input_image" | "image" => Some("[Image]".to_string()),
                    "input_audio" | "audio" => Some("[Audio]".to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if role == "user" && injected_user_context(&body) {
                return None;
            }
            (role, body, false)
        }
        _ => return None,
    };
    if payload.images.0 || payload.local_images.0 {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("[Image]");
    }
    if body.trim().is_empty() {
        return None;
    }
    Some(Message {
        role,
        body,
        timestamp: record.timestamp,
        event,
    })
}

fn message_key(message: &Message) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(message.role);
    // The two rollout representations can differ in line endings or trailing spaces.
    for line in message.body.trim().lines() {
        hash.update(line.trim_end());
        hash.update(b"\n");
    }
    hash.finalize().into()
}

// Let serde stream one JSONL record, without materializing a giant image/tool row.
struct JsonLine<'a, R: BufRead> {
    reader: &'a mut R,
    ended: bool,
    consumed: u64,
    buffered: usize,
    buffer_ends_line: bool,
    has_content: bool,
}
impl<R: BufRead> Read for JsonLine<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.ended || out.is_empty() {
            return Ok(0);
        }
        let available = self.reader.fill_buf()?;
        if available.is_empty() {
            self.ended = true;
            return Ok(0);
        }
        if self.consumed >= MAX_RECORD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "record too large",
            ));
        }
        if self.buffered == 0 {
            let newline = available.iter().position(|byte| *byte == b'\n');
            self.buffered = newline.map_or(available.len(), |index| index + 1);
            self.buffer_ends_line = newline.is_some();
        }
        let count = out
            .len()
            .min(self.buffered)
            .min((MAX_RECORD_BYTES - self.consumed) as usize);
        out[..count].copy_from_slice(&available[..count]);
        self.has_content |= out[..count].iter().any(|byte| !byte.is_ascii_whitespace());
        self.buffered -= count;
        self.ended = self.buffered == 0 && self.buffer_ends_line;
        self.reader.consume(count);
        self.consumed += count as u64;
        Ok(count)
    }
}

fn drain_line<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |index| index + 1);
        reader.consume(count);
        if end.is_some() {
            return Ok(());
        }
    }
}

fn visit_records(
    file: &mut File,
    length: u64,
    expected_id: &str,
    mut visitor: impl FnMut(Message) -> Result<()>,
) -> Result<usize> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| CodexxError::Config(format!("Unable to read session: {error}")))?;
    let mut reader = BufReader::new(file.take(length));
    let mut malformed = 0;
    let mut verified_meta = false;
    loop {
        if reader
            .fill_buf()
            .map_err(|error| CodexxError::Config(format!("Unable to read session: {error}")))?
            .is_empty()
        {
            break;
        }
        let mut line = JsonLine {
            reader: &mut reader,
            ended: false,
            consumed: 0,
            buffered: 0,
            buffer_ends_line: false,
            has_content: false,
        };
        let parsed = serde_json::from_reader::<_, Record>(&mut line);
        let ended = line.ended;
        let has_content = line.has_content;
        if !ended {
            drain_line(&mut reader)
                .map_err(|error| CodexxError::Config(format!("Unable to read session: {error}")))?;
        }
        match parsed {
            Ok(record) if record.kind == "session_meta" => {
                if record.payload.id.as_deref() != Some(expected_id) {
                    return Err(CodexxError::Config(
                        "Session file does not match the selected session; export stopped".to_string(),
                    ));
                }
                verified_meta = true;
            }
            Ok(record) => {
                if let Some(message) = visible_message(record) {
                    if !verified_meta {
                        return Err(CodexxError::Config(
                            "Session file is missing a valid identity record; export stopped".to_string(),
                        ));
                    }
                    visitor(message)?;
                }
            }
            Err(_) if has_content => malformed += 1,
            Err(_) => {}
        }
    }
    if !verified_meta {
        return Err(CodexxError::Config(
            "Session file is missing a valid identity record; export stopped".to_string(),
        ));
    }
    Ok(malformed)
}

fn safe_rollout_path(codex_dir: &Path, session: &SessionPreview) -> Result<PathBuf> {
    let raw = session
        .rollout_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| CodexxError::Config("Chat history file for this session was not found".to_string()))?;
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        codex_dir.join(path)
    };
    let metadata = fs::symlink_metadata(&path).map_err(|error| io_err(&path, error))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !path.to_string_lossy().ends_with(".jsonl")
    {
        return Err(CodexxError::Config("This session file type is not supported".to_string()));
    }
    let canonical = path.canonicalize().map_err(|error| io_err(&path, error))?;
    if !is_canonical_rollout_storage_path(codex_dir, &canonical) {
        return Err(CodexxError::Config(
            "Session file path does not belong to the selected session; export stopped".to_string(),
        ));
    }
    Ok(canonical)
}

fn markdown_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write_markdown(codex_dir: &Path, session: &SessionPreview, output: &mut File) -> Result<usize> {
    let path = safe_rollout_path(codex_dir, session)?;
    let mut file = File::open(&path).map_err(|error| io_err(&path, error))?;
    let length = file.metadata().map_err(|error| io_err(&path, error))?.len();
    let mut event_counts = HashMap::<[u8; 32], usize>::new();
    let malformed = visit_records(&mut file, length, &session.id, |message| {
        if message.event {
            if event_counts.len() >= MAX_MESSAGE_KEYS {
                return Err(CodexxError::Config(
                    "This session has too many messages; split it before exporting".to_string(),
                ));
            }
            *event_counts.entry(message_key(&message)).or_default() += 1;
        }
        Ok(())
    })?;
    let mut write = |text: &str| {
        output
            .write_all(text.as_bytes())
            .map_err(|error| io_err(&path, error))
    };
    write(&format!("# {}\n\n", markdown_label(&session.title)))?;
    if let Some(project) = session.cwd.as_deref().and_then(session_project_title) {
        write(&format!("- Project: {}\n", markdown_label(&project)))?;
    }
    if let Some(model) = &session.model {
        write(&format!("- Model: {}\n", markdown_label(model)))?;
    }
    if let Some(at) = session
        .updated_at_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
    {
        write(&format!(
            "- Last activity: {}\n",
            at.to_rfc3339()
        ))?;
    }
    write(&format!(
        "- Session: {}\n\n",
        markdown_label(&session.id)
    ))?;
    let mut count = 0;
    visit_records(&mut file, length, &session.id, |message| {
        if !message.event {
            if let Some(remaining) = event_counts.get_mut(&message_key(&message)) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(());
                }
            }
        }
        let name = if message.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        write(&format!("---\n\n## {name}\n\n"))?;
        if let Some(at) = message
            .timestamp
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        {
            write(&format!("*{}*\n\n", at.to_rfc3339()))?;
        }
        write(message.body.trim())?;
        write("\n\n")?;
        count += 1;
        Ok(())
    })?;
    if count == 0 {
        return Err(CodexxError::Config(
            "This session has no exportable user or assistant messages".to_string(),
        ));
    }
    if malformed > 0 {
        write(&format!(
            "> {malformed} incomplete or oversized records could not be exported.\n"
        ))?;
    }
    output.flush().map_err(|error| io_err(&path, error))?;
    Ok(malformed)
}

struct TempOutput {
    path: PathBuf,
    file: Option<File>,
}
impl TempOutput {
    fn new(parent: &Path) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = parent.join(format!(
            ".codex-x-export-{}-{}-{}.tmp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|error| io_err(&path, error))?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
}
impl Drop for TempOutput {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

fn filename(title: &str, index: usize) -> String {
    let mut bytes = 0;
    let cleaned: String = title
        .chars()
        .map(|ch| {
            if ch.is_control() || "/\\:*?\"<>|".contains(ch) {
                '_'
            } else {
                ch
            }
        })
        .take_while(|ch| {
            bytes += ch.len_utf8();
            bytes <= 180
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.');
    format!(
        "{:03}-{}.md",
        index + 1,
        if cleaned.is_empty() {
            "Session"
        } else {
            cleaned
        }
    )
}

fn destination_snapshot(path: &Path) -> Result<Option<[u8; 32]>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_err(path, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CodexxError::Config(
            "Please choose a regular file as the export destination".to_string(),
        ));
    }
    let mut file = File::open(path).map_err(|error| io_err(path, error))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_err(path, error))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(Some(hash.finalize().into()))
}

pub(crate) fn export_codex_sessions_inner(
    config_dir: Option<String>,
    session_ids: Vec<String>,
    destination: String,
) -> Result<SessionExportResult> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    let mut seen = HashSet::new();
    let ids: Vec<_> = session_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty() && seen.insert(id.clone()))
        .collect();
    if ids.is_empty() {
        return Err(CodexxError::Config("Please select sessions to export first".to_string()));
    }
    let destination = PathBuf::from(destination);
    let extension = if ids.len() == 1 { "md" } else { "zip" };
    if !destination.is_absolute()
        || !destination
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
    {
        return Err(CodexxError::Config(format!(
            "Please choose a .{extension} file as the export destination"
        )));
    }
    let parent = destination
        .parent()
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| CodexxError::Config("Export folder does not exist".to_string()))?;
    if codex_dir
        .canonicalize()
        .is_ok_and(|root| parent.starts_with(root))
        || is_canonical_rollout_storage_path(&codex_dir, &parent)
    {
        return Err(CodexxError::Config(
            "Please save the export outside the Codex data directory".to_string(),
        ));
    }
    let destination = parent.join(
        destination
            .file_name()
            .ok_or_else(|| CodexxError::Config("Please choose an export filename".to_string()))?,
    );
    let snapshot = destination_snapshot(&destination)?;
    let paths = sqlite_candidate_paths(&codex_dir);
    let (sessions, mut warnings) =
        list_session_previews_with_paths(&paths, &RolloutScan::default(), "openai", usize::MAX)?;
    let sessions: HashMap<_, _> = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect();
    let mut prepared = Vec::new();
    for id in &ids {
        let Some(session) = sessions.get(id) else {
            warnings.push(format!(
                "Session {} is no longer in the current session list",
                id.chars().take(8).collect::<String>()
            ));
            continue;
        };
        let mut output = TempOutput::new(&parent)?;
        match write_markdown(&codex_dir, session, output.file.as_mut().unwrap()) {
            Ok(malformed) => {
                if malformed > 0 {
                    warnings.push(format!(
                        ""{}" had {malformed} incomplete or oversized records that could not be exported",
                        session.title.chars().take(60).collect::<String>()
                    ));
                }
                prepared.push((session.title.clone(), output));
            }
            Err(error) => warnings.push(format!(
                "Failed to export "{}": {error}",
                session.title.chars().take(60).collect::<String>()
            )),
        }
    }
    if prepared.is_empty() {
        return Err(CodexxError::Config(warnings.join("；")));
    }
    let exported_sessions = prepared.len();
    let mut archive;
    let output = if ids.len() == 1 {
        &mut prepared[0].1
    } else {
        archive = TempOutput::new(&parent)?;
        let mut zip = ZipWriter::new(archive.file.take().unwrap());
        for (index, (title, source)) in prepared.iter_mut().enumerate() {
            zip.start_file(
                filename(title, index),
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(0o600),
            )
            .map_err(|error| CodexxError::Config(format!("Unable to create session archive: {error}")))?;
            let file = source.file.as_mut().unwrap();
            file.seek(SeekFrom::Start(0))
                .map_err(|error| io_err(&source.path, error))?;
            std::io::copy(file, &mut zip).map_err(|error| io_err(&archive.path, error))?;
        }
        archive.file = Some(
            zip.finish()
                .map_err(|error| CodexxError::Config(format!("Unable to finalize session archive: {error}")))?,
        );
        &mut archive
    };
    output
        .file
        .as_mut()
        .unwrap()
        .sync_all()
        .map_err(|error| io_err(&output.path, error))?;
    output.file.take();
    if destination_snapshot(&destination)? != snapshot {
        return Err(CodexxError::Config(
            "Export destination was modified by another program; please export again".to_string(),
        ));
    }
    fs::rename(&output.path, &destination).map_err(|error| io_err(&destination, error))?;
    Ok(SessionExportResult {
        path: destination.display().to_string(),
        exported_sessions,
        failed_sessions: ids.len() - exported_sessions,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use serde_json::{json, Value};

    struct Fixture {
        root: PathBuf,
        codex: PathBuf,
        database: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "codex-x-session-export-test-{}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let codex = root.join("codex");
            fs::create_dir_all(codex.join("sessions")).unwrap();
            let database = codex.join("state_5.sqlite");
            Connection::open(&database).unwrap().execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, first_user_message TEXT, model_provider TEXT, model TEXT, cwd TEXT, rollout_path TEXT, updated_at_ms INTEGER, archived INTEGER, has_user_event INTEGER, source TEXT);").unwrap();
            Self {
                root,
                codex,
                database,
            }
        }
        fn add(&self, id: &str, title: &str, records: &[Value], subagent: bool) -> PathBuf {
            let path = self
                .codex
                .join("sessions")
                .join(format!("rollout-2026-09-12-{id}.jsonl"));
            let mut file = File::create(&path).unwrap();
            writeln!(
                file,
                "{}",
                json!({"type":"session_meta", "payload":{"id":id}})
            )
            .unwrap();
            for record in records {
                writeln!(file, "{record}").unwrap();
            }
            Connection::open(&self.database).unwrap().execute("INSERT INTO threads (id,title,model_provider,model,cwd,rollout_path,updated_at_ms,archived,has_user_event,source) VALUES (?1,?2,'openai','sample-model','/private/example/project',?3,1789200000000,0,1,?4)", params![id,title,path.display().to_string(),if subagent { "subagent" } else { "cli" }]).unwrap();
            path
        }
        fn export(&self, ids: &[&str], filename: &str) -> Result<SessionExportResult> {
            export_codex_sessions_inner(
                Some(self.codex.display().to_string()),
                ids.iter().map(|id| (*id).to_string()).collect(),
                self.root.join(filename).display().to_string(),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn event(role: &str, body: &str) -> Value {
        json!({"type":"event_msg","timestamp":"2026-09-12T10:00:00Z","payload":{"type":if role == "user" { "user_message" } else { "agent_message" },"message":body}})
    }
    fn response(role: &str, body: &str) -> Value {
        json!({"type":"response_item","timestamp":"2026-09-12T10:00:00Z","payload":{"type":"message","role":role,"content":[{"type":if role == "user" { "input_text" } else { "output_text" },"text":body}]}})
    }

    #[test]
    fn exports_markdown_titles_times_and_code_without_system_tools_or_reasoning() {
        let fixture = Fixture::new();
        let path = fixture.add("session-one", "My project", &[
            response("system", "SYSTEM-PRIVATE"), response("developer", "DEVELOPER-PRIVATE"),
            response("user", "# AGENTS.md instructions\nPRIVATE INSTRUCTIONS"),
            response("user", "<environment_context>PRIVATE ENV</environment_context>"),
            event("user", "Help me write code"),
            json!({"type":"event_msg","payload":{"type":"agent_reasoning","text":"REASONING-PRIVATE"}}),
            json!({"type":"response_item","payload":{"type":"function_call","arguments":"TOOL-PRIVATE"}}),
            event("assistant", "Sure:\n```rust\nprintln!(\"hello\");\n```"),
        ], false);
        let before = fs::read(&path).unwrap();
        let database = fs::read(&fixture.database).unwrap();
        let result = fixture.export(&["session-one"], "conversation.md").unwrap();
        assert_eq!(result.exported_sessions, 1);
        assert_eq!(result.failed_sessions, 0);
        assert!(result.warnings.is_empty());
        let markdown = fs::read_to_string(result.path).unwrap();
        assert!(markdown.starts_with("# My project"));
        assert!(markdown.contains("Project: project"));
        assert!(markdown.contains("2026-09-12T10:00:00+00:00"));
        assert!(markdown.contains("```rust\nprintln!(\"hello\");\n```"));
        assert!(!markdown.contains("PRIVATE"));
        assert!(!markdown.contains("/private/example"));
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(fs::read(&fixture.database).unwrap(), database);
    }

    #[test]
    fn deduplicates_both_representations_but_preserves_repeated_turns_and_fallbacks() {
        let fixture = Fixture::new();
        fixture.add(
            "repeated",
            "Q&A",
            &[
                event("user", "Continue"),
                response("user", "Continue\n"),
                response("assistant", "First reply"),
                event("assistant", "First reply"),
                event("user", "Continue"),
                response("user", "Continue"),
                event("assistant", "Second reply"),
                response("assistant", "Second reply"),
                response("user", "Old-format question only"),
                response("assistant", "Old-format answer only"),
            ],
            false,
        );
        let result = fixture.export(&["repeated"], "repeated.md").unwrap();
        let text = fs::read_to_string(result.path).unwrap();
        assert_eq!(text.matches("Continue").count(), 2);
        assert_eq!(text.matches("First reply").count(), 1);
        assert_eq!(text.matches("Second reply").count(), 1);
        assert_eq!(text.matches("## User").count(), 3);
        assert_eq!(text.matches("## Assistant").count(), 3);
        assert!(text.contains("Old-format answer only"));
    }

    #[test]
    fn images_are_placeholders_and_large_payload_does_not_hide_later_messages() {
        let fixture = Fixture::new();
        fixture.add("image", "Image", &[
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Look at the image"},{"type":"input_image","image_url":format!("data:image/png;base64,{}", "B".repeat(3 * 1024 * 1024))}]}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"T".repeat(3 * 1024 * 1024)}}),
            response("assistant", "This is an image."),
        ], false);
        let result = fixture.export(&["image"], "image.md").unwrap();
        let text = fs::read_to_string(result.path).unwrap();
        assert!(text.contains("[Image]"));
        assert!(text.contains("This is an image."));
        assert!(!text.contains("base64"));
        assert!(text.len() < 2000);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn exports_only_explicit_selection_and_does_not_auto_include_subagents() {
        let fixture = Fixture::new();
        fixture.add("parent", "Main session", &[event("user", "Parent session body")], false);
        fixture.add(
            "child",
            "Internal session",
            &[event("assistant", "Child session body")],
            true,
        );
        let result = fixture.export(&["parent"], "parent.md").unwrap();
        let text = fs::read_to_string(result.path).unwrap();
        assert!(!text.contains("Child session body"));
        let result = fixture.export(&["child"], "child.md").unwrap();
        assert!(fs::read_to_string(result.path)
            .unwrap()
            .contains("Child session body"));
    }

    #[test]
    fn exports_multiple_sessions_to_zip_with_safe_distinct_filenames() {
        let fixture = Fixture::new();
        fixture.add(
            "one",
            "../same:title",
            &[event("user", "First body")],
            false,
        );
        fixture.add(
            "two",
            "../same:title",
            &[event("assistant", "Second body")],
            false,
        );
        let result = fixture
            .export(&["one", "two", "one"], "sessions.zip")
            .unwrap();
        assert_eq!(result.exported_sessions, 2);
        let mut zip = zip::ZipArchive::new(File::open(result.path).unwrap()).unwrap();
        assert_eq!(zip.len(), 2);
        let names: Vec<_> = (0..2)
            .map(|index| zip.by_index(index).unwrap().name().to_string())
            .collect();
        assert_ne!(names[0], names[1]);
        assert!(names
            .iter()
            .all(|name| !name.contains('/') && name.ends_with(".md")));
        for index in 0..2 {
            let mut text = String::new();
            zip.by_index(index)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert!(text.contains("Body"));
        }
    }

    #[test]
    fn unreadable_or_missing_selection_is_reported_without_empty_zip_members() {
        let fixture = Fixture::new();
        fixture.add("good", "Readable", &[event("user", "Body")], false);
        let bad = fixture.add("bad", "Unreadable", &[event("user", "Bad record")], false);
        fs::remove_file(bad).unwrap();
        let result = fixture
            .export(&["good", "bad", "missing"], "partial.zip")
            .unwrap();
        assert_eq!(result.exported_sessions, 1);
        assert_eq!(result.failed_sessions, 2);
        assert_eq!(result.warnings.len(), 2);
        assert_eq!(
            zip::ZipArchive::new(File::open(result.path).unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rejects_outside_paths_and_mismatched_metadata_without_overwriting_destination() {
        let fixture = Fixture::new();
        let original = fixture.add("outside", "External", &[event("user", "PRIVATE")], false);
        let outside = fixture.root.join(original.file_name().unwrap());
        fs::rename(original, &outside).unwrap();
        Connection::open(&fixture.database)
            .unwrap()
            .execute(
                "UPDATE threads SET rollout_path = ?1 WHERE id = 'outside'",
                [outside.display().to_string()],
            )
            .unwrap();
        fs::write(fixture.root.join("protected.md"), "existing export").unwrap();
        assert!(fixture.export(&["outside"], "protected.md").is_err());
        assert_eq!(
            fs::read_to_string(fixture.root.join("protected.md")).unwrap(),
            "existing export"
        );
        let mismatch = fixture.add("mismatch", "Mismatch", &[event("user", "PRIVATE")], false);
        fs::write(
            mismatch,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"another\"}}\n",
        )
        .unwrap();
        assert!(fixture.export(&["mismatch"], "mismatch.md").is_err());
        assert!(!fixture.root.join("mismatch.md").exists());
        assert!(!fs::read_dir(&fixture.root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".codex-x-export-")));
    }

    #[test]
    fn rejects_destination_inside_codex_storage_and_wrong_file_extensions() {
        let fixture = Fixture::new();
        fixture.add("one", "One", &[event("user", "Body")], false);
        assert!(fixture.export(&["one"], "wrong.zip").is_err());
        assert!(fixture.export(&["one"], "codex/export.md").is_err());
        assert!(fixture.export(&[], "none.md").is_err());
    }

    #[test]
    fn sqlite_reference_supports_a_nonstandard_rollout_filename() {
        let fixture = Fixture::new();
        let original = fixture.add(
            "custom-name",
            "Custom filename",
            &[event("user", "Normal body")],
            false,
        );
        let renamed = fixture.codex.join("sessions/imported-conversation.jsonl");
        fs::rename(original, &renamed).unwrap();
        Connection::open(&fixture.database)
            .unwrap()
            .execute(
                "UPDATE threads SET rollout_path = ?1 WHERE id = 'custom-name'",
                [renamed.display().to_string()],
            )
            .unwrap();
        let result = fixture.export(&["custom-name"], "custom.md").unwrap();
        assert!(fs::read_to_string(result.path)
            .unwrap()
            .contains("Normal body"));
    }

    #[test]
    fn malformed_record_is_skipped_without_consuming_next_message() {
        let fixture = Fixture::new();
        let path = fixture.add("malformed", "Has corrupted records", &[event("user", "First")], false);
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{{this is not json").unwrap();
        writeln!(file, "{}", event("assistant", "Normal reply after corruption")).unwrap();
        let result = fixture.export(&["malformed"], "malformed.md").unwrap();
        let text = fs::read_to_string(result.path).unwrap();
        assert!(text.contains("Normal reply after corruption"));
        assert!(result.warnings[0].contains("1 item"));
    }

    #[test]
    fn multiline_title_cannot_inject_markdown_metadata() {
        assert_eq!(
            markdown_label("Name\n<script>*x*</script>"),
            "Name &lt;script&gt;\\*x\\*&lt;/script&gt;"
        );
    }

    #[test]
    fn exported_names_fit_common_filesystem_limits_with_emoji_titles() {
        let name = filename(&"😀".repeat(100), 0);
        assert!(name.len() < 240);
        assert!(name.ends_with(".md"));
    }

    #[test]
    fn blank_records_and_analysis_channel_do_not_show_as_incomplete_chat() {
        let fixture = Fixture::new();
        let mut analysis = response("assistant", "PRIVATE ANALYSIS");
        analysis["payload"]["channel"] = json!("analysis");
        let path = fixture.add(
            "blank",
            "Normal record",
            &[
                event("user", "Normal question"),
                analysis,
                event("assistant", "Normal answer"),
            ],
            false,
        );
        writeln!(
            OpenOptions::new().append(true).open(path).unwrap(),
            "\n   \t\r\n"
        )
        .unwrap();
        let result = fixture.export(&["blank"], "blank.md").unwrap();
        assert!(result.warnings.is_empty());
        assert!(!fs::read_to_string(result.path).unwrap().contains("PRIVATE"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_rollout_and_destination_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let path = fixture.add("link", "Link", &[event("user", "Body")], false);
        let real = fixture.codex.join("sessions/real.jsonl");
        fs::rename(&path, &real).unwrap();
        symlink(&real, &path).unwrap();
        assert!(fixture.export(&["link"], "link.md").is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&real, &path).unwrap();
        let target = fixture.root.join("target.md");
        fs::write(&target, "do not replace").unwrap();
        symlink(&target, fixture.root.join("export.md")).unwrap();
        assert!(fixture.export(&["link"], "export.md").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "do not replace");
    }

    #[test]
    fn ignores_legacy_database_rows_when_an_active_database_exists() {
        let fixture = Fixture::new();
        fixture.add("active", "Current", &[event("user", "Current body")], false);
        let legacy = fixture.codex.join("state_4.sqlite");
        fs::copy(&fixture.database, &legacy).unwrap();
        Connection::open(legacy)
            .unwrap()
            .execute("UPDATE threads SET id='legacy'", [])
            .unwrap();
        assert!(fixture.export(&["legacy"], "legacy.md").is_err());
    }
}
