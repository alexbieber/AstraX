//! Read-only Codex rollout usage accounting. Only metadata and token counters
//! survive parsing. Recent display titles come from the session index on demand;
//! conversation bodies, authentication, and provider settings are not cached.

use crate::error::{CodexxError, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

// Keep ordinary rollout records on the fast slice parser. Larger records use
// the streaming parser, which skips image/tool/message bodies without retaining
// their contents. This is a buffer threshold, never a record-size limit.
const MAX_BUFFERED_LINE_BYTES: usize = 64 * 1024;
const MAX_SCAN_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILES: usize = 25_000;
const MAX_DEPTH: usize = 6;
const MAX_RECENT_SESSIONS: usize = 10;
const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageTotals {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    session_count: usize,
    usage_event_count: usize,
}

impl UsageTotals {
    fn add(&mut self, value: Counters) {
        self.input_tokens = self.input_tokens.saturating_add(value.input);
        self.cached_input_tokens = self.cached_input_tokens.saturating_add(value.cached);
        self.output_tokens = self.output_tokens.saturating_add(value.output);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(value.reasoning);
        // Cached input is a subset of input; reasoning is a subset of output.
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
        self.usage_event_count += 1;
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageDay {
    date: String,
    #[serde(flatten)]
    totals: UsageTotals,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageModel {
    model: String,
    #[serde(flatten)]
    totals: UsageTotals,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageSession {
    id: String,
    title: String,
    model: String,
    started_at: String,
    last_active_at: String,
    #[serde(flatten)]
    totals: UsageTotals,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageCoverage {
    scanned_files: usize,
    matched_sessions: usize,
    skipped_files: usize,
    warnings: Vec<String>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageStatistics {
    totals: UsageTotals,
    days: Vec<UsageDay>,
    models: Vec<UsageModel>,
    sessions: Vec<UsageSession>,
    available_models: Vec<String>,
    coverage: UsageCoverage,
    range_start: Option<String>,
    range_end: String,
    refreshed_at: String,
    timezone: String,
    source_directories: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Counters {
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
}

impl Counters {
    fn delta(self, previous: Self) -> Self {
        Self {
            input: self.input.saturating_sub(previous.input),
            cached: self.cached.saturating_sub(previous.cached),
            output: self.output.saturating_sub(previous.output),
            reasoning: self.reasoning.saturating_sub(previous.reasoning),
        }
        .clamped()
    }

    fn high_water(self, other: Self) -> Self {
        Self {
            input: self.input.max(other.input),
            cached: self.cached.max(other.cached),
            output: self.output.max(other.output),
            reasoning: self.reasoning.max(other.reasoning),
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            cached: self.cached.saturating_add(other.cached),
            output: self.output.saturating_add(other.output),
            reasoning: self.reasoning.saturating_add(other.reasoning),
        }
    }

    fn clamped(self) -> Self {
        Self {
            cached: self.cached.min(self.input),
            reasoning: self.reasoning.min(self.output),
            ..self
        }
    }

    fn is_zero(self) -> bool {
        self.input == 0 && self.output == 0
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawCounters {
    input_tokens: Option<u64>,
    #[serde(alias = "cache_read_input_tokens")]
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
}

impl RawCounters {
    fn counters(&self) -> Option<Counters> {
        Some(Counters {
            input: self.input_tokens?,
            cached: self.cached_input_tokens.unwrap_or_default(),
            output: self.output_tokens?,
            reasoning: self.reasoning_output_tokens.unwrap_or_default(),
        })
    }

    fn incomplete_breakdown(&self) -> bool {
        self.cached_input_tokens.is_none() || self.reasoning_output_tokens.is_none()
    }
}

#[derive(Debug, Default, Deserialize)]
struct TokenInfo {
    total_token_usage: Option<RawCounters>,
    last_token_usage: Option<RawCounters>,
    model: Option<String>,
    model_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RateLimits {
    limit_id: Option<String>,
}

/// Keep only the source classification and parent ID used for grouping agents.
/// In particular, newer source metadata may embed large, unrelated payloads.
#[derive(Debug, Default)]
struct UsageSource {
    is_subagent: bool,
    parent: Option<String>,
}

struct UsageSourceSeed(usize);

impl<'de> DeserializeSeed<'de> for UsageSourceSeed {
    type Value = UsageSource;

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for UsageSourceSeed {
    type Value = UsageSource;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("session source metadata")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource {
            is_subagent: self.0 == 0 && crate::sessions::source_kind_is_internal(value),
            parent: (self.0 == 3).then(|| value.to_owned()),
        })
    }

    fn visit_map<M: MapAccess<'de>>(
        self,
        mut map: M,
    ) -> std::result::Result<Self::Value, M::Error> {
        const PATH: [&str; 3] = ["subagent", "thread_spawn", "parent_thread_id"];
        let mut source = UsageSource::default();
        while let Some(key) = map.next_key::<String>()? {
            if self.0 == 0 && key == "internal" {
                map.next_value::<IgnoredAny>()?;
                source.is_subagent = true;
            } else if PATH.get(self.0).copied() == Some(key.as_str()) {
                let child = map.next_value_seed(UsageSourceSeed(self.0 + 1))?;
                source.is_subagent |= self.0 == 0;
                source.parent = child.parent;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(source)
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut sequence: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(UsageSource::default())
    }

    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource::default())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource::default())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource::default())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource::default())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(UsageSource::default())
    }
}

impl<'de> Deserialize<'de> for UsageSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        UsageSourceSeed(0).deserialize(deserializer)
    }
}

#[derive(Debug, Default, Deserialize)]
struct LogPayload {
    #[serde(rename = "type", default)]
    kind: String,
    id: Option<String>,
    #[serde(alias = "threadId")]
    thread_id: Option<String>,
    forked_from_id: Option<String>,
    source: Option<UsageSource>,
    thread_source: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    info: Option<TokenInfo>,
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct LogRecord {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: LogPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Signature {
    total: Option<Counters>,
    last: Option<Counters>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenEvent {
    timestamp: Option<DateTime<Utc>>,
    signature: Signature,
    model: String,
    source: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ParseIssues {
    malformed_lines: usize,
    invalid_usage: usize,
    incomplete_breakdown: usize,
    invalid_timestamps: usize,
}

impl ParseIssues {
    fn add(&mut self, other: &Self) {
        self.malformed_lines += other.malformed_lines;
        self.invalid_usage += other.invalid_usage;
        self.incomplete_breakdown += other.incomplete_breakdown;
        self.invalid_timestamps += other.invalid_timestamps;
    }
}

#[derive(Debug, Clone)]
struct ParsedLog {
    id: Option<String>,
    meta_seen: bool,
    started_at: Option<DateTime<Utc>>,
    project_title: Option<String>,
    max_timestamp: Option<DateTime<Utc>>,
    parent: Option<String>,
    invalid_parent: bool,
    replays_parent: bool,
    is_subagent: bool,
    spawn_parent: Option<String>,
    invalid_spawn_parent: bool,
    model: String,
    events: Vec<TokenEvent>,
    issues: ParseIssues,
    pending_line: bool,
}

impl Default for ParsedLog {
    fn default() -> Self {
        Self {
            id: None,
            meta_seen: false,
            started_at: None,
            project_title: None,
            max_timestamp: None,
            parent: None,
            invalid_parent: false,
            replays_parent: false,
            is_subagent: false,
            spawn_parent: None,
            invalid_spawn_parent: false,
            model: UNKNOWN_MODEL.into(),
            events: Vec::new(),
            issues: ParseIssues::default(),
            pending_line: false,
        }
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

impl ParsedLog {
    fn parse_record(&mut self, record: LogRecord) {
        let time = timestamp(record.timestamp.as_deref());
        if let Some(time) = time {
            self.max_timestamp = Some(
                self.max_timestamp
                    .map_or(time, |previous| previous.max(time)),
            );
        }
        let payload = record.payload;
        match record.kind.as_str() {
            "session_meta" if !self.meta_seen => {
                self.meta_seen = true;
                self.id = nonempty(payload.id.or(payload.thread_id));
                self.started_at = time;
                self.project_title = payload
                    .cwd
                    .as_deref()
                    .and_then(crate::sessions::session_project_title);
                let spawned = payload
                    .source
                    .as_ref()
                    .and_then(|value| value.parent.clone());
                let forked = nonempty(payload.forked_from_id);
                let spawned = nonempty(spawned);
                self.is_subagent = payload
                    .source
                    .as_ref()
                    .is_some_and(|source| source.is_subagent)
                    || payload
                        .thread_source
                        .as_deref()
                        .is_some_and(crate::sessions::thread_source_is_internal);
                self.replays_parent = forked.is_some();
                self.spawn_parent = spawned.clone();
                self.invalid_spawn_parent = self.id.is_some() && self.id == self.spawn_parent;
                self.parent = forked.or(spawned);
                self.invalid_parent |= self.id.is_some() && self.id == self.parent;
            }
            "session_meta" => {
                // Imported parent metadata proves replay, but must never replace
                // the child's own identity or its conversation ownership.
                let inherited_id = nonempty(payload.id.or(payload.thread_id));
                if inherited_id.is_some() && inherited_id != self.id {
                    self.replays_parent = true;
                    if self.parent.is_none() {
                        self.parent = inherited_id;
                    }
                }
            }
            "turn_context" => {
                if let Some(model) = nonempty(
                    payload
                        .model
                        .or_else(|| payload.info.and_then(|info| info.model)),
                ) {
                    self.model = model;
                }
            }
            "event_msg" if payload.kind == "token_count" => {
                let Some(info) = payload.info else {
                    return;
                };
                if let Some(model) = nonempty(info.model.or(info.model_name).or(payload.model)) {
                    self.model = model;
                }
                let total = info
                    .total_token_usage
                    .as_ref()
                    .and_then(RawCounters::counters);
                let last = info
                    .last_token_usage
                    .as_ref()
                    .and_then(RawCounters::counters);
                if total.is_none() && last.is_none() {
                    self.issues.invalid_usage += 1;
                    return;
                }
                let effective = if last.is_some() {
                    info.last_token_usage.as_ref()
                } else {
                    info.total_token_usage.as_ref()
                };
                if effective.is_some_and(RawCounters::incomplete_breakdown) {
                    self.issues.incomplete_breakdown += 1;
                }
                if time.is_none() {
                    self.issues.invalid_timestamps += 1;
                }
                self.events.push(TokenEvent {
                    timestamp: time,
                    signature: Signature { total, last },
                    model: self.model.clone(),
                    source: payload
                        .rate_limits
                        .and_then(|value| nonempty(value.limit_id)),
                });
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileStamp {
    fn new(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }

    fn same_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            let _ = other;
            true
        }
    }
}

#[derive(Debug)]
struct CachedFile {
    stamp: FileStamp,
    offset: u64,
    boundary_hash: [u8; 32],
    prefix_hash: [u8; 32],
    parsed: ParsedLog,
    complete: bool,
}

#[derive(Default)]
struct DirectoryCache {
    files: HashMap<PathBuf, CachedFile>,
    // A forced verification is a round, which may span several UI refreshes.
    // Keep the last usable result visible until its replacement is complete.
    force_remaining: BTreeSet<PathBuf>,
    force_staging: HashMap<PathBuf, CachedFile>,
}

fn caches() -> &'static Mutex<HashMap<String, Arc<Mutex<DirectoryCache>>>> {
    static CACHES: OnceLock<Mutex<HashMap<String, Arc<Mutex<DirectoryCache>>>>> = OnceLock::new();
    CACHES.get_or_init(Mutex::default)
}

fn io_error(path: &Path, source: std::io::Error) -> CodexxError {
    CodexxError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn hash_bytes(file: &mut File, start: u64, len: usize) -> std::io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    Ok(Sha256::digest(&bytes).into())
}

fn anchors(file: &mut File, offset: u64) -> std::io::Result<([u8; 32], [u8; 32])> {
    let prefix = hash_bytes(file, 0, offset.min(256) as usize)?;
    let boundary = hash_bytes(file, offset.saturating_sub(256), offset.min(256) as usize)?;
    Ok((prefix, boundary))
}

struct ParsedLine {
    record: std::result::Result<Option<LogRecord>, serde_json::Error>,
    consumed: u64,
    terminated: bool,
}

/// Presents exactly one physical JSONL line to Serde. Caching the next newline
/// boundary avoids rescanning the entire buffer for each byte Serde requests.
/// Syntax errors can be drained to the same boundary, preserving the next event.
struct JsonLineReader<'a, R> {
    reader: &'a mut R,
    available: usize,
    ends_line: bool,
    consumed: u64,
    terminated: bool,
    non_whitespace: bool,
}

impl<'a, R: BufRead> JsonLineReader<'a, R> {
    fn new(reader: &'a mut R) -> Self {
        Self {
            reader,
            available: 0,
            ends_line: false,
            consumed: 0,
            terminated: false,
            non_whitespace: false,
        }
    }

    fn drain(&mut self) -> std::io::Result<()> {
        std::io::copy(self, &mut std::io::sink())?;
        Ok(())
    }
}

impl<R: BufRead> Read for JsonLineReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.terminated || output.is_empty() {
            return Ok(0);
        }
        let buffer = self.reader.fill_buf()?;
        if self.available == 0 {
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            self.available = newline.map_or(buffer.len(), |index| index + 1);
            self.ends_line = newline.is_some();
        }
        let count = output.len().min(self.available);
        output[..count].copy_from_slice(&buffer[..count]);
        self.non_whitespace = self.non_whitespace
            || output[..count]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace());
        self.reader.consume(count);
        self.consumed += count as u64;
        self.available -= count;
        self.terminated = self.available == 0 && self.ends_line;
        Ok(count)
    }
}

fn parse_record_slice(bytes: &[u8]) -> std::result::Result<Option<LogRecord>, serde_json::Error> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        Ok(None)
    } else {
        serde_json::from_slice(bytes).map(Some)
    }
}

fn read_log_record(reader: &mut impl BufRead) -> std::io::Result<ParsedLine> {
    let mut bytes = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(ParsedLine {
                record: parse_record_slice(&bytes),
                consumed: bytes.len() as u64,
                terminated: false,
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |index| index + 1);
        if bytes.len() + count > MAX_BUFFERED_LINE_BYTES {
            // Derived struct deserializers use IgnoredAny for unknown fields:
            // Serde validates/skips large strings and arrays directly from the
            // reader, rather than constructing a String, Value, or whole line.
            let mut tail = JsonLineReader::new(reader);
            let prefix_non_whitespace = bytes.iter().any(|byte| !byte.is_ascii_whitespace());
            let record = serde_json::from_reader(bytes.as_slice().chain(&mut tail)).map(Some);
            if let Err(error) = &record {
                if let Some(kind) = error.io_error_kind() {
                    return Err(std::io::Error::new(kind, "会话文件在读取期间发生错误"));
                }
            }
            // A bad record must not consume the next JSONL line, nor prevent it
            // from contributing usage. Also count all bytes against scan budget.
            tail.drain()?;
            return Ok(ParsedLine {
                record: if prefix_non_whitespace || tail.non_whitespace {
                    record
                } else {
                    Ok(None)
                },
                consumed: bytes.len() as u64 + tail.consumed,
                terminated: tail.terminated,
            });
        }
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(ParsedLine {
                record: parse_record_slice(&bytes),
                consumed: bytes.len() as u64,
                terminated: true,
            });
        }
    }
}

fn parse_file(
    path: &Path,
    previous: Option<CachedFile>,
    force: bool,
    budget: &mut u64,
) -> Result<CachedFile> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CodexxError::Config("用量统计只读取普通会话文件".into()));
    }
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    let stamp = FileStamp::new(&file.metadata().map_err(|error| io_error(path, error))?);
    if !stamp.same_file(&FileStamp::new(&metadata))
        || fs::symlink_metadata(path)
            .map_err(|error| io_error(path, error))?
            .file_type()
            .is_symlink()
    {
        return Err(CodexxError::Config(
            "会话文件在扫描期间发生变化，请刷新重试".into(),
        ));
    }
    let mut offset = 0;
    let mut parsed = ParsedLog::default();
    if let Some(old) = previous {
        if old.stamp == stamp && old.complete && !force {
            return Ok(old);
        }
        // Append-only logs resume at the last complete record. Digests detect
        // replacement/truncation without retaining fragments of chat text.
        let can_append = old.stamp.same_file(&stamp)
            && stamp.len >= old.stamp.len
            && (stamp.len > old.stamp.len || !old.complete);
        if can_append
            && anchors(&mut file, old.offset).ok() == Some((old.prefix_hash, old.boundary_hash))
        {
            offset = old.offset;
            parsed = old.parsed;
        }
    }
    parsed.pending_line = false;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(path, error))?;
    let mut reader = BufReader::new(file);
    let mut complete = true;
    loop {
        if offset >= stamp.len {
            break;
        }
        if *budget == 0 {
            complete = false;
            break;
        }
        let line = read_log_record(&mut reader).map_err(|error| io_error(path, error))?;
        if line.consumed == 0 {
            break;
        }
        *budget = budget.saturating_sub(line.consumed);
        match line.record {
            Ok(Some(record)) => parsed.parse_record(record),
            Ok(None) => {}
            Err(error) if !line.terminated && error.is_eof() => {
                parsed.pending_line = true;
                break;
            }
            Err(_) => parsed.issues.malformed_lines += 1,
        }
        offset += line.consumed;
    }
    let mut file = reader.into_inner();
    let (prefix_hash, boundary_hash) =
        anchors(&mut file, offset).map_err(|error| io_error(path, error))?;
    Ok(CachedFile {
        stamp,
        offset,
        prefix_hash,
        boundary_hash,
        parsed,
        complete,
    })
}

fn collect_files(dir: &Path, depth: usize, files: &mut Vec<PathBuf>, coverage: &mut UsageCoverage) {
    let metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            coverage.skipped_files += 1;
            coverage
                .warnings
                .push("部分会话目录无法读取，统计可能不完整。".into());
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        coverage.skipped_files += 1;
        coverage
            .warnings
            .push("已跳过符号链接，统计仅包含配置目录中的普通会话文件。".into());
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            coverage.skipped_files += 1;
            coverage
                .warnings
                .push("部分会话目录无法读取，统计可能不完整。".into());
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            coverage.skipped_files += 1;
            continue;
        };
        if files.len() >= MAX_FILES {
            coverage.truncated = true;
            coverage
                .warnings
                .push("会话文件数量超过单次扫描上限，当前统计不完整。".into());
            break;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            coverage.skipped_files += 1;
            continue;
        };
        if kind.is_symlink() {
            coverage.skipped_files += 1;
            coverage
                .warnings
                .push("已跳过符号链接，统计仅包含配置目录中的普通会话文件。".into());
        } else if kind.is_dir() {
            if depth < MAX_DEPTH {
                collect_files(&path, depth + 1, files, coverage);
            } else {
                coverage.truncated = true;
                coverage
                    .warnings
                    .push("已跳过层级过深的会话目录，统计可能不完整。".into());
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
}

#[derive(Default)]
struct SessionLog {
    started_at: Option<DateTime<Utc>>,
    project_title: Option<String>,
    max_timestamp: Option<DateTime<Utc>>,
    parent: Option<String>,
    invalid_parent: bool,
    replays_parent: bool,
    is_subagent: bool,
    spawn_parent: Option<String>,
    invalid_spawn_parent: bool,
    events: Vec<TokenEvent>,
    files: usize,
}

fn combine_files(
    cache: &DirectoryCache,
    coverage: &mut UsageCoverage,
    indexed_identities: &HashMap<String, crate::sessions::UsageThreadIdentity>,
) -> BTreeMap<String, SessionLog> {
    let mut sessions: BTreeMap<String, SessionLog> = BTreeMap::new();
    let mut issues = ParseIssues::default();
    let mut without_identity = 0;
    let mut pending_lines = 0;
    let mut ordered_files: Vec<_> = cache.files.iter().collect();
    ordered_files.sort_by(|a, b| a.0.cmp(b.0));
    for (_, entry) in ordered_files {
        let parsed = &entry.parsed;
        issues.add(&parsed.issues);
        pending_lines += usize::from(parsed.pending_line);
        if !entry.complete {
            coverage.truncated = true;
        }
        let Some(id) = parsed.id.as_ref() else {
            without_identity += 1;
            coverage.skipped_files += 1;
            continue;
        };
        let session = sessions.entry(id.clone()).or_default();
        if session.files > 0 && session.parent != parsed.parent {
            session.invalid_parent = true;
        }
        session.files += 1;
        if session.project_title.is_none() {
            session.project_title = parsed.project_title.clone();
        }
        session.started_at = match (session.started_at, parsed.started_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        session.max_timestamp = match (session.max_timestamp, parsed.max_timestamp) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        session.parent = session.parent.take().or_else(|| parsed.parent.clone());
        session.invalid_parent |= parsed.invalid_parent;
        session.replays_parent |= parsed.replays_parent;
        session.is_subagent |= parsed.is_subagent;
        session.invalid_spawn_parent |= parsed.invalid_spawn_parent
            || session
                .spawn_parent
                .as_ref()
                .zip(parsed.spawn_parent.as_ref())
                .is_some_and(|(a, b)| a != b);
        if session.spawn_parent.is_none() {
            session.spawn_parent = parsed.spawn_parent.clone();
        }
        session.events.extend(parsed.events.iter().cloned());
    }
    let mut canonical_files: Option<HashMap<PathBuf, &CachedFile>> = None;
    for (id, session) in &mut sessions {
        // Keep token records from every copy, but use the current database's
        // actual rollout for identity. A stale archived/internal copy with the
        // same ID must not hide a user conversation or supply a different parent.
        let authoritative = indexed_identities
            .get(id)
            .and_then(|identity| identity.rollout_path.as_ref())
            .and_then(|path| {
                cache.files.get(path).or_else(|| {
                    let canonical = path.canonicalize().ok()?;
                    canonical_files
                        .get_or_insert_with(|| {
                            cache
                                .files
                                .iter()
                                .filter_map(|(path, entry)| {
                                    path.canonicalize().ok().map(|path| (path, entry))
                                })
                                .collect()
                        })
                        .get(&canonical)
                        .copied()
                })
            })
            .map(|entry| &entry.parsed)
            .filter(|parsed| parsed.meta_seen && parsed.id.as_ref() == Some(id));
        if let Some(parsed) = authoritative {
            session.is_subagent = parsed.is_subagent;
            session.spawn_parent.clone_from(&parsed.spawn_parent);
            session.invalid_spawn_parent = parsed.invalid_spawn_parent;
        }
        // The same rollout can coexist in sessions and archived_sessions, or
        // have overlapping resume segments. Merge its token records once.
        let mut seen = HashSet::new();
        session.events.retain(|event| {
            seen.insert((
                event.timestamp,
                event.signature.clone(),
                event.model.clone(),
            ))
        });
        session.events.sort_by_key(|event| event.timestamp);
    }
    for (count, message) in [
        (issues.malformed_lines, "行日志无法解析"),
        (issues.invalid_usage, "条用量记录缺少有效的输入或输出计数"),
        (issues.invalid_timestamps, "条用量记录缺少有效时间"),
        (without_identity, "个会话文件缺少会话标识，已跳过"),
        (pending_lines, "个会话文件末尾仍在写入，未计入未完成记录"),
    ] {
        if count > 0 {
            coverage
                .warnings
                .push(format!("{count} {message}；统计可能不完整。"));
        }
    }
    if issues.incomplete_breakdown > 0 {
        coverage.warnings.push(format!(
            "{} 条记录未提供完整的缓存或推理明细；这两项仅统计日志明确记录的数量。",
            issues.incomplete_breakdown
        ));
    }
    if coverage.truncated {
        coverage.warnings.push(
            "已达到本次扫描限制，当前显示已读取的部分用量。再次刷新或打开统计页面可继续扫描。"
                .into(),
        );
    }
    sessions
}

fn replay_prefix(session: &SessionLog, sessions: &BTreeMap<String, SessionLog>) -> Option<usize> {
    // A spawn edge establishes ownership, not inherited token history. A fresh
    // child may have the same counters as its parent for a separate request.
    if !session.replays_parent {
        return Some(0);
    }
    if session.invalid_parent {
        return None;
    }
    let Some(parent_id) = session.parent.as_ref() else {
        return Some(0);
    };
    let parent = sessions.get(parent_id)?;
    let cutoff = session.started_at?;
    if parent
        .max_timestamp
        .is_none_or(|timestamp| timestamp < cutoff)
    {
        return None;
    }
    if parent.events.iter().any(|event| event.timestamp.is_none()) {
        return None;
    }
    let mut ancestors = HashSet::new();
    let mut ancestor = Some(parent_id);
    while let Some(id) = ancestor {
        if !ancestors.insert(id) {
            return None;
        }
        ancestor = sessions.get(id).and_then(|session| session.parent.as_ref());
    }
    let signatures: Vec<_> = parent
        .events
        .iter()
        .filter(|event| event.timestamp.is_some_and(|timestamp| timestamp <= cutoff))
        .map(|event| &event.signature)
        .collect();
    let mut parent_offset = 0;
    let mut matched = 0;
    let mut previous_matched = None;
    for event in &session.events {
        if previous_matched == Some(&event.signature) {
            matched += 1;
            continue;
        }
        let Some(relative) = signatures[parent_offset..]
            .iter()
            .position(|signature| **signature == event.signature)
        else {
            break;
        };
        parent_offset += relative + 1;
        matched += 1;
        previous_matched = Some(&event.signature);
    }
    Some(matched)
}

struct MeasuredEvent<'a> {
    timestamp: DateTime<Utc>,
    model: &'a str,
    counters: Counters,
}

#[derive(Default)]
struct MeasurementIssues {
    regressions: usize,
    missing_request_details: usize,
    recovered_baselines: usize,
}

fn measured_events(
    session: &SessionLog,
    prefix: usize,
) -> (Vec<MeasuredEvent<'_>>, MeasurementIssues) {
    let mut high_water = Counters::default();
    let mut by_source: HashMap<Option<&str>, &Signature> = HashMap::new();
    let mut previous_signature: Option<&Signature> = None;
    let mut result = Vec::new();
    let mut issues = MeasurementIssues::default();
    let mut had_total = false;
    for (index, event) in session.events.iter().enumerate() {
        let signature = &event.signature;
        let duplicate = signature.total.is_some()
            && (by_source.get(&event.source.as_deref()).copied() == Some(signature)
                || previous_signature == Some(signature));
        if signature.total.is_some() {
            by_source.insert(event.source.as_deref(), signature);
        }
        previous_signature = Some(signature);
        let counters = if duplicate {
            Counters::default()
        } else if let Some(last) = signature.last {
            // The exact request counters remain valid across cumulative resets
            // and independently advancing rate-limit lanes.
            if index >= prefix
                && signature.total.is_some_and(|total| {
                    let delta = total.delta(high_water);
                    delta.input > last.input || delta.output > last.output
                })
            {
                issues.missing_request_details += 1;
            }
            last.clamped()
        } else if let Some(total) = signature.total {
            if total.input < high_water.input || total.output < high_water.output {
                issues.regressions += 1;
            }
            total.delta(high_water)
        } else {
            continue;
        };
        if let Some(total) = signature.total {
            if !had_total && !high_water.is_zero() && signature.last.is_none() && index >= prefix {
                issues.recovered_baselines += 1;
            }
            had_total = true;
            high_water = high_water.high_water(total);
        } else if !duplicate {
            // Some Codex versions temporarily omit cumulative counters. Keep
            // their exact requests in the baseline when totals later return.
            high_water = high_water.plus(counters);
        }
        if index < prefix || counters.is_zero() {
            continue;
        }
        let Some(timestamp) = event.timestamp else {
            continue;
        };
        result.push(MeasuredEvent {
            timestamp,
            model: &event.model,
            counters,
        });
    }
    (result, issues)
}

#[derive(Default)]
struct Bucket {
    totals: UsageTotals,
    sessions: HashSet<String>,
}

impl Bucket {
    fn add(&mut self, owner: Option<&str>, counters: Counters) {
        self.totals.add(counters);
        if let Some(id) = owner {
            self.sessions.insert(id.to_owned());
        }
        self.totals.session_count = self.sessions.len();
    }
}

fn conversation_identities(
    sessions: &BTreeMap<String, SessionLog>,
    indexed_identities: HashMap<String, crate::sessions::UsageThreadIdentity>,
) -> HashMap<String, crate::sessions::UsageThreadIdentity> {
    let mut identities = sessions
        .iter()
        .map(|(id, session)| {
            (
                id.clone(),
                crate::sessions::UsageThreadIdentity {
                    is_subagent: session.is_subagent,
                    classification_known: session.is_subagent,
                    parent_id: session.spawn_parent.clone(),
                    parent_conflict: session.invalid_spawn_parent,
                    rollout_path: None,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    for (id, mut indexed) in indexed_identities {
        // Older indexes may have only id/title columns. Absence of a source
        // marker must not erase a positive subagent classification in the log.
        if !indexed.classification_known && identities.contains_key(&id) {
            continue;
        }
        // A generic user/default database marker must not promote a thread
        // whose own rollout explicitly identifies it as an internal task.
        if identities.get(&id).is_some_and(|log| log.is_subagent) {
            indexed.is_subagent = true;
            indexed.classification_known = true;
        }
        // A source-only internal marker may still need its rollout's parent ID.
        if indexed.is_subagent && indexed.parent_id.is_none() && !indexed.parent_conflict {
            if let Some(log) = identities.get(&id) {
                indexed.parent_id.clone_from(&log.parent_id);
                indexed.parent_conflict = log.parent_conflict;
            }
        }
        identities.insert(id, indexed);
    }
    identities
}

fn conversation_owner(
    id: &str,
    identities: &HashMap<String, crate::sessions::UsageThreadIdentity>,
) -> Option<String> {
    let mut current = id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current) {
            return None;
        }
        let identity = identities.get(current)?;
        if !identity.is_subagent {
            return Some(current.to_owned());
        }
        if identity.parent_conflict {
            return None;
        }
        current = identity.parent_id.as_deref()?;
    }
}

#[derive(Default)]
struct ConversationUsage {
    totals: UsageTotals,
    models: BTreeSet<String>,
    first_event: Option<DateTime<Utc>>,
    last_event: Option<DateTime<Utc>>,
}

fn midnight<T: TimeZone>(timezone: &T, date: NaiveDate) -> Option<DateTime<T>> {
    let midnight = date.and_hms_opt(0, 0, 0)?;
    // A few local zones skip midnight when entering daylight saving time.
    (0..=180).find_map(|minutes| {
        timezone
            .from_local_datetime(&(midnight + Duration::minutes(minutes)))
            .earliest()
    })
}

fn aggregate<T: TimeZone>(
    cache: &DirectoryCache,
    mut coverage: UsageCoverage,
    codex_dir: &Path,
    range: &str,
    model: Option<&str>,
    now: DateTime<T>,
) -> Result<UsageStatistics>
where
    T::Offset: std::fmt::Display,
{
    let days_back = match range {
        "today" => Some(0),
        "7d" => Some(6),
        "30d" => Some(29),
        "all" => None,
        _ => {
            return Err(CodexxError::Config(
                "用量时间范围必须为 today、7d、30d 或 all".into(),
            ))
        }
    };
    let timezone = now.timezone();
    let today = now.date_naive();
    let start_date = days_back.map(|days| today - Duration::days(days));
    let range_start = start_date
        .and_then(|date| midnight(&timezone, date))
        .map(|time| time.with_timezone(&Utc));
    let range_end = now.with_timezone(&Utc);
    let indexed_identities = crate::sessions::usage_thread_identities(codex_dir);
    let sessions = combine_files(cache, &mut coverage, &indexed_identities);
    let identities = conversation_identities(&sessions, indexed_identities);
    let mut total = Bucket::default();
    let mut days: BTreeMap<NaiveDate, Bucket> = BTreeMap::new();
    let mut models: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut available_models = BTreeSet::new();
    let mut conversations: BTreeMap<String, ConversationUsage> = BTreeMap::new();
    let mut unowned_subagents = HashSet::new();
    let mut missing_usage = 0;
    let mut unresolved_parent = 0;
    let mut regressions = 0;
    let mut missing_request_details = 0;
    let mut recovered_baselines = 0;
    let mut unknown_model = false;
    for (id, session) in &sessions {
        if session.events.is_empty() {
            missing_usage += 1;
            continue;
        }
        let Some(prefix) = replay_prefix(session, &sessions) else {
            unresolved_parent += 1;
            coverage.skipped_files += session.files;
            continue;
        };
        let (events, issues) = measured_events(session, prefix);
        regressions += issues.regressions;
        missing_request_details += issues.missing_request_details;
        recovered_baselines += issues.recovered_baselines;
        let owner = conversation_owner(id, &identities);
        for event in events {
            if event.timestamp > range_end
                || range_start.is_some_and(|start| event.timestamp < start)
            {
                continue;
            }
            available_models.insert(event.model.to_owned());
            if model.is_some_and(|model| model != event.model) {
                continue;
            }
            unknown_model |= event.model == UNKNOWN_MODEL;
            total.add(owner.as_deref(), event.counters);
            days.entry(event.timestamp.with_timezone(&timezone).date_naive())
                .or_default()
                .add(owner.as_deref(), event.counters);
            models
                .entry(event.model.to_owned())
                .or_default()
                .add(owner.as_deref(), event.counters);
            if let Some(owner) = owner.as_ref() {
                let conversation = conversations.entry(owner.clone()).or_default();
                conversation.totals.add(event.counters);
                conversation.models.insert(event.model.to_owned());
                conversation.first_event = Some(
                    conversation
                        .first_event
                        .map_or(event.timestamp, |time| time.min(event.timestamp)),
                );
                conversation.last_event = Some(
                    conversation
                        .last_event
                        .map_or(event.timestamp, |time| time.max(event.timestamp)),
                );
            } else {
                // Keep actual usage in overall/day/model totals, but never invent
                // a user conversation for an orphaned or conflicting subagent.
                unowned_subagents.insert(id);
            }
        }
    }
    let mut recent_sessions = conversations
        .into_iter()
        .filter_map(|(id, mut conversation)| {
            let first = conversation.first_event?;
            let last = conversation.last_event?;
            let session = sessions.get(&id);
            conversation.totals.session_count = 1;
            Some(UsageSession {
                title: session
                    .and_then(|session| session.project_title.clone())
                    .unwrap_or_else(|| "未命名会话".into()),
                model: if conversation.models.len() > 1 {
                    "multiple".into()
                } else {
                    conversation.models.into_iter().next().unwrap_or_default()
                },
                started_at: session
                    .and_then(|session| session.started_at)
                    .unwrap_or(first)
                    .to_rfc3339(),
                last_active_at: last.to_rfc3339(),
                totals: conversation.totals,
                id,
            })
        })
        .collect::<Vec<_>>();
    if missing_usage > 0 {
        coverage.warnings.push(format!("{missing_usage} 个会话没有可用的 token_count 记录；历史版本或供应商未记录的用量无法补算。"));
    }
    if unresolved_parent > 0 {
        coverage.warnings.push(format!(
            "{unresolved_parent} 个分叉会话的父会话信息不完整，暂未计入，以免重复统计继承的历史。"
        ));
    }
    if !unowned_subagents.is_empty() {
        coverage.warnings.push(format!("{} 个子代理无法确定所属主会话；其可核实的 Token 已计入总量，但不单独计为会话或列入最近会话。", unowned_subagents.len()));
    }
    if regressions > 0 {
        coverage.warnings.push(format!("{regressions} 条累计用量发生回退且缺少单次明细，已按历史最高值保守去重，统计可能偏低。"));
    }
    if missing_request_details > 0 {
        coverage.warnings.push(format!("{missing_request_details} 条累计快照包含未找到单次明细的用量；仅计入日志明确记录的单次数量，未推测其日期或模型。"));
    }
    if recovered_baselines > 0 {
        coverage.warnings.push(format!("{recovered_baselines} 处早期记录只有单次用量，已扣除已计入部分后衔接累计快照；早期累计基线无法核实。"));
    }
    if unknown_model {
        coverage
            .warnings
            .push("部分用量未记录模型名称，已归入“未知模型”。".into());
    }
    coverage.matched_sessions = total.totals.session_count;
    coverage.warnings.sort();
    coverage.warnings.dedup();

    let first_day = start_date.or_else(|| days.keys().next().copied());
    if let Some(first_day) = first_day {
        if today.signed_duration_since(first_day).num_days() <= 366 {
            let mut day = first_day;
            while day <= today {
                days.entry(day).or_default();
                let Some(next) = day.succ_opt() else {
                    break;
                };
                day = next;
            }
        }
    }
    let mut model_totals: Vec<_> = models
        .into_iter()
        .map(|(model, bucket)| UsageModel {
            model,
            totals: bucket.totals,
        })
        .collect();
    model_totals.sort_by(|a, b| {
        b.totals
            .total_tokens
            .cmp(&a.totals.total_tokens)
            .then_with(|| a.model.cmp(&b.model))
    });
    recent_sessions.sort_by(|a, b| {
        b.last_active_at
            .cmp(&a.last_active_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    recent_sessions.truncate(MAX_RECENT_SESSIONS);
    // Resolve only the displayed rows. Read the same title source as session
    // management each time so renamed chats update even if token logs are cached.
    let recent_ids = recent_sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    let titles = crate::sessions::session_titles_by_id(codex_dir, &recent_ids);
    for session in &mut recent_sessions {
        if let Some(title) = titles.get(&session.id) {
            session.title.clone_from(title);
        }
    }
    Ok(UsageStatistics {
        totals: total.totals,
        days: days
            .into_iter()
            .map(|(date, bucket)| UsageDay {
                date: date.to_string(),
                totals: bucket.totals,
            })
            .collect(),
        models: model_totals,
        sessions: recent_sessions,
        available_models: available_models.into_iter().collect(),
        coverage,
        range_start: range_start.map(|time| time.to_rfc3339()),
        range_end: range_end.to_rfc3339(),
        refreshed_at: Utc::now().to_rfc3339(),
        timezone: format!("UTC{}", now.format("%:z")),
        source_directories: ["sessions", "archived_sessions"]
            .iter()
            .map(|name| codex_dir.join(name).display().to_string())
            .collect(),
    })
}

fn refresh_cache(cache: &mut DirectoryCache, codex_dir: &Path, force: bool) -> UsageCoverage {
    refresh_cache_with_budget(cache, codex_dir, force, MAX_SCAN_BYTES)
}

fn refresh_cache_with_budget(
    cache: &mut DirectoryCache,
    codex_dir: &Path,
    force: bool,
    mut budget: u64,
) -> UsageCoverage {
    let mut coverage = UsageCoverage::default();
    let mut files = Vec::new();
    for directory in ["sessions", "archived_sessions"] {
        collect_files(&codex_dir.join(directory), 0, &mut files, &mut coverage);
    }
    files.sort();
    let present: HashSet<_> = files.iter().cloned().collect();
    cache.files.retain(|path, _| present.contains(path));
    cache.force_remaining.retain(|path| present.contains(path));
    cache.force_staging.retain(|path, _| present.contains(path));
    if force && cache.force_remaining.is_empty() {
        cache.force_remaining.extend(files.iter().cloned());
    }

    // Complete in-progress files first. In particular, another click on Refresh
    // continues this round instead of spending its budget on the same prefix.
    let mut ordered: Vec<_> = cache.force_staging.keys().cloned().collect();
    ordered.sort();
    let mut incomplete: Vec<_> = cache
        .files
        .iter()
        .filter(|(_, entry)| !entry.complete)
        .map(|(path, _)| path.clone())
        .collect();
    incomplete.sort();
    ordered.extend(incomplete);
    ordered.extend(cache.force_remaining.iter().cloned());
    ordered.extend(files);
    let mut seen = HashSet::new();
    let mut retained_cached_results = false;
    for path in ordered.into_iter().filter(|path| seen.insert(path.clone())) {
        let verifying = cache.force_remaining.contains(&path);
        if !verifying
            && cache.files.get(&path).is_some_and(|old| {
                old.complete
                    && fs::symlink_metadata(&path).ok().is_some_and(|metadata| {
                        metadata.is_file()
                            && !metadata.file_type().is_symlink()
                            && FileStamp::new(&metadata) == old.stamp
                    })
            })
        {
            continue;
        }
        if budget == 0 {
            coverage.truncated = true;
            retained_cached_results |= cache.files.contains_key(&path);
            continue;
        }
        let old = if verifying {
            // Only staging from this verification round may resume. A new
            // forced verification reads from byte zero, including on append.
            cache.force_staging.remove(&path)
        } else {
            cache.files.remove(&path)
        };
        match parse_file(&path, old, false, &mut budget) {
            Ok(entry) => {
                if verifying && !entry.complete {
                    retained_cached_results |= cache.files.contains_key(&path);
                    cache.force_staging.insert(path, entry);
                    coverage.truncated = true;
                } else {
                    cache.force_remaining.remove(&path);
                    cache.files.insert(path, entry);
                }
            }
            Err(_) => {
                cache.force_remaining.remove(&path);
                coverage.skipped_files += 1;
                retained_cached_results |= verifying && cache.files.contains_key(&path);
                coverage
                    .warnings
                    .push("部分会话文件无法读取，统计可能不完整。".into());
            }
        }
    }
    coverage.scanned_files = cache.files.len();
    coverage.truncated |= !cache.force_remaining.is_empty();
    if retained_cached_results {
        coverage.warnings.push(
            "部分文件尚未完成复核，已保留其上次成功读取的缓存用量；再次刷新将继续本轮扫描。".into(),
        );
    }
    coverage
}

pub(crate) fn get_usage_statistics_inner(
    config_dir: Option<String>,
    range: Option<String>,
    model: Option<String>,
    force_refresh: Option<bool>,
) -> Result<UsageStatistics> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    let scope = crate::paths::normalized_path_scope(&codex_dir);
    let cache = {
        let mut caches = caches()
            .lock()
            .map_err(|_| CodexxError::Config("用量缓存不可用".into()))?;
        // Bound retained scopes when users repeatedly switch config directories.
        if caches.len() >= 4 && !caches.contains_key(&scope) {
            caches.clear();
        }
        Arc::clone(caches.entry(scope).or_default())
    };
    let mut cache = cache
        .lock()
        .map_err(|_| CodexxError::Config("用量缓存不可用".into()))?;
    let coverage = refresh_cache(&mut cache, &codex_dir, force_refresh.unwrap_or(false));
    aggregate(
        &cache,
        coverage,
        &codex_dir,
        range.as_deref().unwrap_or("7d"),
        nonempty(model).as_deref(),
        Local::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use serde_json::{json, Value};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        dir: PathBuf,
        cache: DirectoryCache,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "codex-x-usage-{}-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(dir.join("sessions")).unwrap();
            fs::create_dir_all(dir.join("archived_sessions")).unwrap();
            Self {
                dir,
                cache: DirectoryCache::default(),
            }
        }

        fn write(&self, name: &str, records: &[Value]) -> PathBuf {
            let path = self.dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text = records
                .iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>();
            fs::write(&path, text).unwrap();
            path
        }

        fn append(&self, path: &Path, records: &[Value]) {
            let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
            for record in records {
                writeln!(file, "{record}").unwrap();
            }
        }

        fn stats(&mut self, range: &str, model: Option<&str>) -> UsageStatistics {
            self.stats_at(range, model, false, now())
        }

        fn stats_with_budget(&mut self, force: bool, budget: u64) -> UsageStatistics {
            let coverage = refresh_cache_with_budget(&mut self.cache, &self.dir, force, budget);
            aggregate(&self.cache, coverage, &self.dir, "all", None, now()).unwrap()
        }

        fn stats_at(
            &mut self,
            range: &str,
            model: Option<&str>,
            force: bool,
            now: DateTime<FixedOffset>,
        ) -> UsageStatistics {
            let coverage = refresh_cache(&mut self.cache, &self.dir, force);
            aggregate(&self.cache, coverage, &self.dir, range, model, now).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-09-08T12:00:00+08:00").unwrap()
    }

    fn meta(id: &str, at: &str, parent: Option<&str>) -> Value {
        json!({"type":"session_meta","timestamp":at,"payload":{"id":id,"forked_from_id":parent}})
    }

    fn context(model: &str) -> Value {
        json!({"type":"turn_context","payload":{"model":model,"user_instructions":"PRIVATE TEXT MUST NOT LEAVE PARSER"}})
    }

    fn spawn_meta(id: &str, parent: &str, at: &str) -> Value {
        let mut record = meta(id, at, None);
        record["payload"]["source"] =
            json!({"subagent":{"thread_spawn":{"parent_thread_id":parent}}});
        record
    }

    fn count(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        json!({"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":reasoning,"total_tokens":input+output})
    }

    fn usage(at: &str, total: Option<Value>, last: Option<Value>) -> Value {
        json!({"type":"event_msg","timestamp":at,"payload":{"type":"token_count","info":{"total_token_usage":total,"last_token_usage":last}}})
    }

    fn total(at: &str, input: u64, output: u64) -> Value {
        usage(at, Some(count(input, 0, output, 0)), None)
    }

    fn heartbeat(at: &str) -> Value {
        json!({"type":"event_msg","timestamp":at,"payload":{"type":"task_complete","last_agent_message":"PRIVATE TEXT MUST NOT LEAVE PARSER"}})
    }

    #[test]
    fn cumulative_counts_deduplicate_and_subsets_are_not_added_twice() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                usage("2026-09-08T01:00:01Z", Some(count(100, 60, 20, 8)), None),
                usage("2026-09-08T01:00:02Z", Some(count(100, 60, 20, 8)), None),
                usage("2026-09-08T01:00:03Z", Some(count(150, 90, 35, 17)), None),
            ],
        );
        let stats = fixture.stats("today", None);
        assert_eq!(stats.totals.input_tokens, 150);
        assert_eq!(stats.totals.cached_input_tokens, 90);
        assert_eq!(stats.totals.output_tokens, 35);
        assert_eq!(stats.totals.reasoning_tokens, 17);
        assert_eq!(stats.totals.total_tokens, 185);
        assert_eq!(stats.totals.usage_event_count, 2);
        assert_eq!(stats.totals.session_count, 1);
        assert!(stats.coverage.warnings.is_empty());
    }

    #[test]
    fn identical_last_only_requests_at_different_times_both_count() {
        let mut fixture = Fixture::new();
        let first = usage("2026-09-08T01:00:01Z", None, Some(count(100, 50, 20, 5)));
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                first.clone(),
                first,
                usage("2026-09-08T01:00:02Z", None, Some(count(100, 50, 20, 5))),
            ],
        );
        let stats = fixture.stats("today", None);
        assert_eq!(stats.totals.total_tokens, 240);
        assert_eq!(stats.totals.usage_event_count, 2);
    }

    #[test]
    fn exact_last_usage_handles_cumulative_lanes_and_reemitted_rate_limits() {
        let mut fixture = Fixture::new();
        let mut lane_a = usage(
            "2026-09-08T01:00:01Z",
            Some(count(1000, 500, 100, 50)),
            Some(count(100, 50, 10, 5)),
        );
        lane_a["payload"]["rate_limits"] = json!({"limit_id":"codex"});
        let mut repeat = lane_a.clone();
        repeat["timestamp"] = json!("2026-09-08T01:00:02Z");
        repeat["payload"]["rate_limits"] = json!({"limit_id":"bengal"});
        let mut lane_b = usage(
            "2026-09-08T01:00:03Z",
            Some(count(500, 200, 80, 30)),
            Some(count(80, 40, 20, 10)),
        );
        lane_b["payload"]["rate_limits"] = json!({"limit_id":"bengal"});
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                lane_a,
                repeat,
                lane_b,
            ],
        );
        let stats = fixture.stats("today", None);
        assert_eq!(stats.totals.input_tokens, 180);
        assert_eq!(stats.totals.output_tokens, 30);
        assert_eq!(stats.totals.usage_event_count, 2);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("未找到单次明细")));
    }

    #[test]
    fn archive_and_resume_overlap_are_merged_by_session_identity() {
        let mut fixture = Fixture::new();
        let records = vec![
            meta("one", "2026-09-08T01:00:00Z", None),
            context("gpt-5.4"),
            total("2026-09-08T01:00:01Z", 100, 10),
        ];
        fixture.write("archived_sessions/old.jsonl", &records);
        let active = fixture.write("sessions/2026/09/08/resumed.jsonl", &records);
        fixture.append(&active, &[total("2026-09-08T01:00:02Z", 150, 25)]);
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 175);
        assert_eq!(stats.totals.usage_event_count, 2);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.coverage.scanned_files, 2);
    }

    #[test]
    fn unrelated_sessions_with_equal_token_events_are_not_deduplicated() {
        let mut fixture = Fixture::new();
        for id in ["one", "two"] {
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    meta(id, "2026-09-08T01:00:00Z", None),
                    context("gpt-5.4"),
                    total("2026-09-08T01:00:01Z", 100, 10),
                ],
            );
        }
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 220);
        assert_eq!(stats.totals.session_count, 2);
    }

    #[test]
    fn forked_replay_prefix_is_excluded_but_child_new_usage_remains() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/parent.jsonl",
            &[
                meta("parent", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
                total("2026-09-08T01:00:02Z", 200, 20),
                heartbeat("2026-09-08T01:00:06Z"),
            ],
        );
        fixture.write(
            "sessions/child.jsonl",
            &[
                meta("child", "2026-09-08T01:00:05Z", Some("parent")),
                context("gpt-5.4"),
                total("2026-09-08T01:00:05Z", 100, 10),
                total("2026-09-08T01:00:06Z", 100, 10), // A replayed rate-limit refresh.
                total("2026-09-08T01:00:07Z", 200, 20),
                total("2026-09-08T01:00:08Z", 250, 25),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 275);
        assert_eq!(stats.totals.usage_event_count, 3);
        assert_eq!(stats.totals.session_count, 2);
        assert_eq!(
            stats
                .sessions
                .iter()
                .find(|session| session.id == "child")
                .unwrap()
                .totals
                .total_tokens,
            55
        );
    }

    #[test]
    fn fork_parent_future_usage_cannot_extend_replayed_prefix() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/parent.jsonl",
            &[
                meta("parent", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
                total("2026-09-08T01:00:09Z", 200, 20),
            ],
        );
        fixture.write(
            "sessions/child.jsonl",
            &[
                meta("child", "2026-09-08T01:00:05Z", Some("parent")),
                context("gpt-5.4"),
                total("2026-09-08T01:00:06Z", 100, 10),
                total("2026-09-08T01:00:07Z", 200, 20),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 330);
        assert_eq!(
            stats
                .sessions
                .iter()
                .find(|session| session.id == "child")
                .unwrap()
                .totals
                .total_tokens,
            110
        );
    }

    #[test]
    fn missing_or_incomplete_parent_defers_child_and_reports_coverage() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/child.jsonl",
            &[
                meta("child", "2026-09-08T01:00:05Z", Some("parent")),
                context("gpt-5.4"),
                total("2026-09-08T01:00:06Z", 100, 10),
                total("2026-09-08T01:00:07Z", 200, 20),
            ],
        );
        let missing = fixture.stats("all", None);
        assert_eq!(missing.totals.total_tokens, 0);
        assert_eq!(missing.coverage.skipped_files, 1);
        let parent = fixture.write(
            "sessions/parent.jsonl",
            &[
                meta("parent", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let incomplete = fixture.stats("all", None);
        assert_eq!(incomplete.totals.total_tokens, 110);
        assert_eq!(incomplete.coverage.skipped_files, 1);
        fixture.append(&parent, &[heartbeat("2026-09-08T01:00:05Z")]);
        let complete = fixture.stats("all", None);
        assert_eq!(complete.totals.total_tokens, 220);
        assert_eq!(complete.coverage.skipped_files, 0);
    }

    #[test]
    fn spawned_parent_and_second_metadata_do_not_replace_child_identity() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/parent.jsonl",
            &[
                meta("parent", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
                heartbeat("2026-09-08T01:00:05Z"),
            ],
        );
        let mut child_meta = meta("child", "2026-09-08T01:00:05Z", None);
        child_meta["payload"]["source"] =
            json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}});
        fixture.write(
            "sessions/child.jsonl",
            &[
                child_meta,
                meta("parent", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:06Z", 100, 10),
                total("2026-09-08T01:00:07Z", 150, 15),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 165);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.sessions.len(), 1);
        assert_eq!(stats.sessions[0].id, "parent");
        assert_eq!(stats.sessions[0].totals.total_tokens, 165);
    }

    #[test]
    fn nested_subagents_share_conversation_counts_but_keep_event_models_and_dates() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/main.jsonl",
            &[
                meta("main", "2026-09-07T15:00:00Z", None),
                context("main-model"),
                total("2026-09-07T15:59:59Z", 100, 10),
            ],
        );
        let child = [
            spawn_meta("child", "main", "2026-09-07T16:00:00Z"),
            context("child-model"),
            total("2026-09-07T16:00:01Z", 50, 5),
        ];
        fixture.write("sessions/child.jsonl", &child);
        fixture.write("archived_sessions/child.jsonl", &child);
        fixture.write(
            "sessions/sibling.jsonl",
            &[
                spawn_meta("sibling", "main", "2026-09-07T16:00:00Z"),
                context("child-model"),
                total("2026-09-07T16:00:01Z", 50, 5),
            ],
        );
        fixture.write(
            "sessions/grandchild.jsonl",
            &[
                spawn_meta("grandchild", "child", "2026-09-07T16:00:02Z"),
                context("grand-model"),
                total("2026-09-07T16:00:03Z", 20, 2),
            ],
        );
        let all = fixture.stats("all", None);
        assert_eq!(all.totals.total_tokens, 242);
        assert_eq!(all.totals.session_count, 1);
        assert_eq!(all.sessions.len(), 1);
        assert_eq!(all.sessions[0].id, "main");
        assert_eq!(all.sessions[0].totals.total_tokens, 242);
        assert_eq!(all.sessions[0].model, "multiple");
        assert_eq!(all.days.len(), 2);
        assert!(all.days.iter().all(|day| day.totals.session_count == 1));
        assert_eq!(all.coverage.matched_sessions, 1);
        let today = fixture.stats("today", None);
        assert_eq!(today.totals.total_tokens, 132);
        assert_eq!(today.totals.session_count, 1);
        assert_eq!(today.sessions[0].id, "main");
        let filtered = fixture.stats("today", Some("child-model"));
        assert_eq!(filtered.totals.total_tokens, 110);
        assert_eq!(filtered.models[0].totals.session_count, 1);
        assert_eq!(filtered.sessions[0].totals.total_tokens, 110);
        assert_eq!(filtered.sessions[0].model, "child-model");
    }

    #[test]
    fn fresh_spawn_with_identical_request_counters_is_not_replayed_parent_usage() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/main.jsonl",
            &[
                meta("main", "2026-09-08T01:00:00Z", None),
                context("same-model"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        fixture.write(
            "sessions/child.jsonl",
            &[
                spawn_meta("child", "main", "2026-09-08T01:00:05Z"),
                context("same-model"),
                total("2026-09-08T01:00:06Z", 100, 10),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 220);
        assert_eq!(stats.totals.usage_event_count, 2);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.sessions[0].totals.total_tokens, 220);
        assert_eq!(stats.coverage.skipped_files, 0);
    }

    #[test]
    fn user_forks_remain_conversations_and_own_their_subagent_usage() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/original.jsonl",
            &[
                meta("original", "2026-09-08T01:00:00Z", None),
                context("model"),
                total("2026-09-08T01:00:01Z", 100, 10),
                heartbeat("2026-09-08T01:00:10Z"),
            ],
        );
        fixture.write(
            "sessions/fork.jsonl",
            &[
                meta("fork", "2026-09-08T01:00:05Z", Some("original")),
                context("model"),
                total("2026-09-08T01:00:06Z", 100, 10),
                total("2026-09-08T01:00:07Z", 150, 15),
            ],
        );
        fixture.write(
            "sessions/child.jsonl",
            &[
                spawn_meta("child", "fork", "2026-09-08T01:00:08Z"),
                context("model"),
                total("2026-09-08T01:00:09Z", 20, 2),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 187);
        assert_eq!(stats.totals.session_count, 2);
        assert_eq!(stats.sessions.len(), 2);
        assert_eq!(stats.sessions[0].id, "fork");
        assert_eq!(stats.sessions[0].totals.total_tokens, 77);
        assert_eq!(stats.sessions[1].id, "original");
        assert_eq!(stats.sessions[1].totals.total_tokens, 110);
    }

    #[test]
    fn sqlite_only_spawn_edges_use_main_title_even_without_a_parent_rollout() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/child.jsonl",
            &[
                meta("child", "2026-09-08T01:00:00Z", None),
                context("model"),
                total("2026-09-08T01:00:01Z", 50, 5),
            ],
        );
        let db = rusqlite::Connection::open(fixture.dir.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, thread_source TEXT);
            CREATE TABLE thread_spawn_edges (parent_thread_id TEXT, child_thread_id TEXT);
            INSERT INTO threads VALUES ('main', '优化主对话', 'user'), ('child', '内部实现任务', NULL);
            INSERT INTO thread_spawn_edges VALUES ('main', 'child');").unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 55);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.sessions[0].id, "main");
        assert_eq!(stats.sessions[0].title, "优化主对话");
        db.execute(
            "UPDATE threads SET thread_source = 'user' WHERE id = 'child'",
            [],
        )
        .unwrap();
        let changed = fixture.stats("all", None);
        assert_eq!(changed.sessions[0].id, "main");
        assert_eq!(changed.totals.total_tokens, 55);
    }

    #[test]
    fn title_only_sqlite_rows_do_not_erase_rollout_subagent_classification() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/main.jsonl",
            &[
                meta("main", "2026-09-08T01:00:00Z", None),
                context("model"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        fixture.write(
            "sessions/child.jsonl",
            &[
                spawn_meta("child", "main", "2026-09-08T01:00:02Z"),
                context("model"),
                total("2026-09-08T01:00:03Z", 50, 5),
            ],
        );
        let db = rusqlite::Connection::open(fixture.dir.join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT);
            INSERT INTO threads VALUES ('main', '主对话标题'), ('child', '内部子任务');",
        )
        .unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 165);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.sessions.len(), 1);
        assert_eq!(stats.sessions[0].title, "主对话标题");
        db.execute_batch(
            "ALTER TABLE threads ADD COLUMN thread_source TEXT;
            ALTER TABLE threads ADD COLUMN source TEXT;",
        )
        .unwrap();
        assert_eq!(fixture.stats("all", None).totals.session_count, 1);
        db.execute(
            "UPDATE threads SET thread_source = 'user' WHERE id = 'child'",
            [],
        )
        .unwrap();
        assert_eq!(fixture.stats("all", None).totals.session_count, 1);
    }

    #[test]
    fn guardian_and_memory_usage_stays_counted_without_creating_user_conversation_rows() {
        let mut fixture = Fixture::new();
        for (index, source) in [
            json!({"internal":"guardian"}),
            json!({"internal":"memory_consolidation"}),
            json!("internal_guardian"),
            json!("subagent_review"),
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("internal-{index}");
            let mut metadata = meta(&id, "2026-09-08T01:00:00Z", None);
            metadata["payload"]["source"] = source;
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    metadata,
                    context("codex-auto-review"),
                    total("2026-09-08T01:00:01Z", 50, 5),
                ],
            );
        }
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 220);
        assert_eq!(stats.totals.session_count, 0);
        assert!(stats.sessions.is_empty());
    }

    #[test]
    fn current_rollout_controls_classification_while_duplicate_usage_is_preserved() {
        let mut fixture = Fixture::new();
        let current = [
            meta("main", "2026-09-08T01:00:00Z", None),
            context("model"),
            total("2026-09-08T01:00:01Z", 100, 10),
        ];
        fixture.write("sessions/current.jsonl", &current);
        let mut stale = current.to_vec();
        stale[0]["payload"]["source"] = json!({"internal":"guardian"});
        stale.push(total("2026-09-08T01:00:02Z", 150, 15));
        fixture.write("archived_sessions/stale-copy.jsonl", &stale);
        let db = rusqlite::Connection::open(fixture.dir.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, thread_source TEXT, source TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('main', 'User conversation', 'user', 'cli', 'sessions/current.jsonl');").unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 165);
        assert_eq!(stats.totals.session_count, 1);
        assert_eq!(stats.sessions.len(), 1);
        assert_eq!(stats.sessions[0].id, "main");
        assert_eq!(stats.sessions[0].totals.total_tokens, 165);

        // An explicit internal source in the authoritative file still wins
        // over generic user/cli labels, even with a normal duplicate present.
        db.execute("UPDATE threads SET rollout_path = 'archived_sessions/stale-copy.jsonl' WHERE id = 'main'", []).unwrap();
        let internal = fixture.stats("all", None);
        assert_eq!(internal.totals.total_tokens, 165);
        assert_eq!(internal.totals.session_count, 0);
        assert!(internal.sessions.is_empty());
    }

    #[test]
    fn first_metadata_thread_source_hides_internal_tasks_without_database_markers() {
        let mut fixture = Fixture::new();
        for (index, source) in ["guardian_review", "memory_consolidation"]
            .into_iter()
            .enumerate()
        {
            let id = format!("internal-{index}");
            let mut metadata = meta(&id, "2026-09-08T01:00:00Z", None);
            metadata["payload"]["source"] = json!("cli");
            metadata["payload"]["thread_source"] = json!(source);
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    metadata,
                    context("model"),
                    total("2026-09-08T01:00:01Z", 50, 5),
                ],
            );
        }
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 110);
        assert_eq!(stats.totals.session_count, 0);
        assert!(stats.sessions.is_empty());
    }

    #[test]
    fn orphaned_and_cyclic_subagents_keep_tokens_without_becoming_user_conversations() {
        let mut fixture = Fixture::new();
        for (id, parent) in [
            ("orphan", "missing"),
            ("cycle-a", "cycle-b"),
            ("cycle-b", "cycle-a"),
        ] {
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    spawn_meta(id, parent, "2026-09-08T01:00:00Z"),
                    context("model"),
                    total("2026-09-08T01:00:01Z", 50, 5),
                ],
            );
        }
        let mut unlinked = meta("unlinked", "2026-09-08T01:00:00Z", None);
        unlinked["payload"]["source"] = json!("subagent");
        fixture.write(
            "sessions/unlinked.jsonl",
            &[
                unlinked,
                context("model"),
                total("2026-09-08T01:00:01Z", 50, 5),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 220);
        assert_eq!(stats.models[0].totals.total_tokens, 220);
        assert_eq!(stats.days[0].totals.total_tokens, 220);
        assert_eq!(stats.totals.session_count, 0);
        assert_eq!(stats.models[0].totals.session_count, 0);
        assert_eq!(stats.days[0].totals.session_count, 0);
        assert!(stats.sessions.is_empty());
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("4 个子代理")));
    }

    #[test]
    fn conflicting_sqlite_parents_do_not_attribute_usage_to_an_arbitrary_conversation() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/child.jsonl",
            &[
                spawn_meta("child", "first", "2026-09-08T01:00:00Z"),
                context("model"),
                total("2026-09-08T01:00:01Z", 50, 5),
            ],
        );
        let db = rusqlite::Connection::open(fixture.dir.join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, thread_source TEXT);
            CREATE TABLE thread_spawn_edges (parent_thread_id TEXT, child_thread_id TEXT);
            INSERT INTO threads VALUES ('first', 'user'), ('second', 'user'), ('child', 'subagent');
            INSERT INTO thread_spawn_edges VALUES ('first', 'child'), ('second', 'child');",
        )
        .unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 55);
        assert_eq!(stats.totals.session_count, 0);
        assert!(stats.sessions.is_empty());
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("无法确定所属主会话")));
    }

    #[test]
    fn model_changes_keep_cumulative_baseline_and_local_midnight_splits_days() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-07T15:50:00Z", None),
                context("gpt-5.4"),
                total("2026-09-07T15:59:59Z", 100, 10),
                context("gpt-5.5"),
                total("2026-09-07T16:00:00Z", 150, 15),
                total("2026-09-08T04:00:01Z", 300, 30), // Future data must not leak into the range.
            ],
        );
        let all = fixture.stats("all", None);
        assert_eq!(all.totals.total_tokens, 165);
        assert_eq!(all.models.len(), 2);
        assert_eq!(all.days[0].date, "2026-09-07");
        assert_eq!(all.days[0].totals.total_tokens, 110);
        assert_eq!(all.days[1].totals.total_tokens, 55);
        let today = fixture.stats("today", None);
        assert_eq!(today.totals.total_tokens, 55);
        assert_eq!(
            today.range_start.as_deref(),
            Some("2026-09-07T16:00:00+00:00")
        );
        assert_eq!(today.timezone, "UTC+08:00");
        let filtered = fixture.stats("7d", Some("gpt-5.4"));
        assert_eq!(filtered.totals.total_tokens, 110);
        assert_eq!(filtered.available_models, vec!["gpt-5.4", "gpt-5.5"]);
        assert_eq!(filtered.days.len(), 7);
    }

    #[test]
    fn seven_and_thirty_days_use_calendar_boundaries_and_fill_empty_days() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-08-01T00:00:00Z", None),
                context("gpt-5.4"),
                total("2026-08-01T00:00:01Z", 10, 1),
                total("2026-08-09T16:00:00Z", 20, 2),
                total("2026-09-01T15:59:59Z", 30, 3),
                total("2026-09-01T16:00:00Z", 40, 4),
            ],
        );
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 44);
        let month = fixture.stats("30d", None);
        assert_eq!(month.totals.total_tokens, 33);
        assert_eq!(month.days.len(), 30);
        let week = fixture.stats("7d", None);
        assert_eq!(week.totals.total_tokens, 11);
        assert_eq!(week.days.len(), 7);
    }

    #[test]
    fn last_only_counters_advance_baseline_before_total_returns() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
                usage("2026-09-08T01:00:02Z", None, Some(count(50, 0, 5, 0))),
                total("2026-09-08T01:00:03Z", 200, 20),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 220);
        assert_eq!(stats.totals.usage_event_count, 3);
    }

    #[test]
    fn early_last_only_records_are_reconciled_with_an_explicit_coverage_note() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                usage("2026-09-08T01:00:01Z", None, Some(count(100, 0, 10, 0))),
                total("2026-09-08T01:00:02Z", 150, 15),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 165);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("早期累计基线无法核实")));
    }

    #[test]
    fn cumulative_regression_without_last_is_conservative_and_visible() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
                total("2026-09-08T01:00:02Z", 20, 2),
                total("2026-09-08T01:00:03Z", 120, 12),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 132);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("回退")));
    }

    #[test]
    fn cache_reuses_unchanged_file_and_appends_only_new_records() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        let first_offset = fixture.cache.files[&path].offset;
        let first_stamp = fixture.cache.files[&path].stamp.clone();
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        assert_eq!(fixture.cache.files[&path].offset, first_offset);
        assert_eq!(fixture.cache.files[&path].stamp, first_stamp);
        fixture.append(&path, &[total("2026-09-08T01:00:02Z", 150, 15)]);
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 165);
        assert_eq!(fixture.cache.files[&path].parsed.events.len(), 2);
        assert!(fixture.cache.files[&path].offset > first_offset);
    }

    #[test]
    fn changed_prefix_and_same_length_rewrites_replace_cached_counters() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        let old_len = fs::metadata(&path).unwrap().len();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 200, 20),
            ],
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), old_len);
        assert_eq!(
            fixture
                .stats_at("all", None, true, now())
                .totals
                .total_tokens,
            220
        );
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("another-session", "2026-09-08T01:00:00Z", None),
                context("gpt-5.5"),
                total("2026-09-08T01:00:01Z", 300, 30),
            ],
        );
        let replaced = fixture.stats("all", None);
        assert_eq!(replaced.totals.total_tokens, 330);
        assert_eq!(replaced.sessions[0].id, "another-session");
    }

    #[test]
    fn partial_tail_is_retried_without_duplicate_or_permanent_parse_error() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let next = format!("{}\n", total("2026-09-08T01:00:02Z", 200, 20));
        let midpoint = next.len() / 2;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&next.as_bytes()[..midpoint]).unwrap();
        let partial = fixture.stats("all", None);
        assert_eq!(partial.totals.total_tokens, 110);
        assert!(partial
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("仍在写入")));
        file.write_all(&next.as_bytes()[midpoint..]).unwrap();
        let complete = fixture.stats("all", None);
        assert_eq!(complete.totals.total_tokens, 220);
        assert!(complete.coverage.warnings.is_empty());
    }

    #[test]
    fn complete_last_record_without_newline_can_be_counted_then_appended() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
            ],
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{}", total("2026-09-08T01:00:01Z", 100, 10)).unwrap();
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        writeln!(file).unwrap();
        fixture.append(&path, &[total("2026-09-08T01:00:02Z", 150, 15)]);
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 165);
    }

    #[test]
    fn streaming_reader_recovers_after_large_invalid_lines_and_reports_bad_or_old_usage() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                usage(
                    "2026-09-08T01:00:01Z",
                    Some(json!({"total_tokens":110})),
                    None,
                ),
                usage(
                    "2026-09-08T01:00:02Z",
                    Some(json!({"input_tokens":100,"output_tokens":10})),
                    None,
                ),
            ],
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{not json}\n").unwrap();
        file.write_all(&vec![b'x'; 2 * 1024 * 1024 + 10]).unwrap();
        file.write_all(b"\n").unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 110);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("2 行日志无法解析")));
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("无法解析")));
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("缓存或推理")));
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("有效的输入或输出")));
    }

    #[test]
    fn large_tool_image_and_token_records_keep_all_usage_without_coverage_warnings() {
        let mut fixture = Fixture::new();
        let huge = "x".repeat(2 * 1024 * 1024 + 100);
        // Field ordering is deliberate: counters/metadata can follow the large
        // value, so looking only at a record's prefix cannot recover usage.
        let text = format!(
            "{{\"payload\":{{\"instructions\":\"{huge}\",\"id\":\"one\"}},\"type\":\"session_meta\",\"timestamp\":\"2026-09-08T01:00:00Z\"}}\n\
             {{\"payload\":{{\"input_image\":{{\"image_url\":\"data:image/png;base64,{huge}\"}},\"output\":\"{huge}\"}},\"type\":\"response_item\"}}\n\
             {{\"payload\":{{\"instructions\":\"{huge}\",\"model\":\"gpt-large-fixture\"}},\"type\":\"turn_context\"}}\n\
             {{\"payload\":{{\"info\":{{\"ignored\":\"{huge}\",\"total_token_usage\":{{\"input_tokens\":100,\"cached_input_tokens\":20,\"output_tokens\":10,\"reasoning_output_tokens\":5}}}},\"type\":\"token_count\"}},\"type\":\"event_msg\",\"timestamp\":\"2026-09-08T01:00:01Z\"}}\n"
        );
        let path = fixture.dir.join("sessions/one.jsonl");
        fs::write(&path, text).unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 110);
        assert_eq!(stats.totals.cached_input_tokens, 20);
        assert_eq!(stats.totals.reasoning_tokens, 5);
        assert_eq!(stats.sessions[0].model, "gpt-large-fixture");
        assert!(stats.coverage.warnings.is_empty(), "{:?}", stats.coverage);
        // Appended events still resume at the exact boundary after large lines.
        fixture.append(&path, &[total("2026-09-08T01:00:02Z", 150, 15)]);
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 165);
    }

    #[test]
    fn large_source_metadata_retains_agent_parent_without_materializing_extensions() {
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/root.jsonl",
            &[
                meta("root", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let mut child = spawn_meta("child", "root", "2026-09-08T01:00:02Z");
        child["payload"]["source"]["subagent"]["thread_spawn"]["instructions"] =
            json!("private extension ".repeat(140_000));
        fixture.write(
            "sessions/child.jsonl",
            &[
                child,
                context("gpt-5.4"),
                total("2026-09-08T01:00:03Z", 50, 5),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 165);
        assert_eq!(stats.sessions.len(), 1);
        assert_eq!(stats.sessions[0].id, "root");
        assert!(stats.coverage.warnings.is_empty());
    }

    #[test]
    fn large_broken_record_does_not_count_partial_tokens_or_consume_next_line() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
            ],
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let mut bad = total("2026-09-08T01:00:01Z", 1000, 100);
        bad["large_ignored"] = json!("x".repeat(2 * 1024 * 1024 + 100));
        writeln!(file, "{bad} trailing garbage").unwrap();
        fixture.append(&path, &[total("2026-09-08T01:00:02Z", 100, 10)]);
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 110);
        assert_eq!(stats.coverage.warnings.len(), 1);
        assert!(stats.coverage.warnings[0].contains("1 行日志无法解析"));
    }

    #[test]
    fn partially_written_large_record_retries_then_counts_once_when_completed() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(
            file,
            "{{\"ignored\":\"{}",
            "x".repeat(2 * 1024 * 1024 + 100)
        )
        .unwrap();
        let pending = fixture.stats("all", None);
        assert_eq!(pending.totals.total_tokens, 110);
        assert_eq!(pending.coverage.warnings.len(), 1);
        assert!(pending.coverage.warnings[0].contains("仍在写入"));
        let event = total("2026-09-08T01:00:02Z", 200, 20).to_string();
        // A complete final record is valid even before its newline is written.
        write!(file, "\",{}", &event[1..]).unwrap();
        let complete = fixture.stats("all", None);
        assert_eq!(complete.totals.total_tokens, 220);
        assert!(complete.coverage.warnings.is_empty());
        writeln!(file).unwrap();
        fixture.append(&path, &[total("2026-09-08T01:00:03Z", 300, 30)]);
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 330);
    }

    #[test]
    fn streaming_line_boundaries_accept_escaped_content_and_large_blank_lines() {
        let huge = "\\\"中文\n".repeat(300_000);
        let mut event = total("2026-09-08T01:00:01Z", 100, 10);
        event["unknown"] = json!(huge);
        let text = format!("{}\n{event}\n", " ".repeat(2 * 1024 * 1024 + 10));
        let mut reader = BufReader::with_capacity(127, text.as_bytes());
        let blank = read_log_record(&mut reader).unwrap();
        assert!(blank.record.unwrap().is_none());
        assert!(blank.terminated);
        let record = read_log_record(&mut reader).unwrap();
        assert_eq!(record.record.unwrap().unwrap().payload.kind, "token_count");
        assert!(record.terminated);
        assert_eq!(blank.consumed + record.consumed, text.len() as u64);
        assert_eq!(read_log_record(&mut reader).unwrap().consumed, 0);
    }

    #[test]
    fn scan_budget_resumes_after_large_record_without_skipping_its_tokens() {
        let fixture = Fixture::new();
        let mut large = total("2026-09-08T01:00:01Z", 100, 10);
        large["unknown"] = json!("x".repeat(2 * 1024 * 1024 + 10));
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                large,
                total("2026-09-08T01:00:02Z", 150, 15),
            ],
        );
        // Scan budgets stop between records, even if one large record crosses
        // the remaining budget. Its completed token event must be retained.
        let mut budget = MAX_BUFFERED_LINE_BYTES as u64;
        let partial = parse_file(&path, None, false, &mut budget).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.parsed.events.len(), 1);
        assert_eq!(budget, 0);
        let mut budget = MAX_SCAN_BYTES;
        let complete = parse_file(&path, Some(partial), false, &mut budget).unwrap();
        assert!(complete.complete);
        assert_eq!(complete.parsed.events.len(), 2);
        assert!(complete.parsed.issues.malformed_lines == 0);
    }

    #[test]
    fn missing_timestamp_is_not_fabricated_from_file_modified_time() {
        let mut fixture = Fixture::new();
        let mut invalid = total("2026-09-08T01:00:01Z", 100, 10);
        invalid["timestamp"] = Value::Null;
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                invalid,
                total("2026-09-08T01:00:02Z", 150, 15),
            ],
        );
        let stats = fixture.stats("today", None);
        assert_eq!(stats.totals.total_tokens, 55);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("有效时间")));
    }

    #[test]
    fn integer_counters_do_not_truncate_at_u32_and_subsets_are_clamped() {
        let mut fixture = Fixture::new();
        let input = u32::MAX as u64 + 500;
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                usage(
                    "2026-09-08T01:00:01Z",
                    Some(count(input, input + 50, 20, 50)),
                    None,
                ),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.input_tokens, input);
        assert_eq!(stats.totals.cached_input_tokens, input);
        assert_eq!(stats.totals.reasoning_tokens, 20);
        assert_eq!(stats.totals.total_tokens, input + 20);
    }

    #[test]
    fn empty_directory_and_old_sessions_have_distinct_coverage() {
        let mut fixture = Fixture::new();
        let empty = fixture.stats("today", None);
        assert_eq!(empty.totals.total_tokens, 0);
        assert!(empty.coverage.warnings.is_empty());
        fixture.write(
            "sessions/old.jsonl",
            &[
                meta("old", "2026-09-08T01:00:00Z", None),
                heartbeat("2026-09-08T01:00:01Z"),
            ],
        );
        let old = fixture.stats("today", None);
        assert_eq!(old.totals.total_tokens, 0);
        assert!(old
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("没有可用")));
    }

    #[test]
    fn deleted_files_are_removed_from_cache_and_aggregate() {
        let mut fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        fs::remove_file(path).unwrap();
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 0);
        assert!(fixture.cache.files.is_empty());
    }

    #[test]
    fn recent_titles_follow_chat_renames_without_reparsing_token_logs() {
        let mut fixture = Fixture::new();
        let mut root_meta = meta("named", "2026-09-08T01:00:00Z", None);
        root_meta["payload"]["cwd"] = json!("/fixture/projects/Codex-X");
        fixture.write(
            "sessions/named.jsonl",
            &[
                root_meta,
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let db = rusqlite::Connection::open(fixture.dir.join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, cwd TEXT);
            INSERT INTO threads VALUES ('named', '优化设置标签动画', '/fixture/projects/Codex-X');",
        )
        .unwrap();
        let first = fixture.stats("all", None);
        assert_eq!(first.sessions[0].title, "优化设置标签动画");
        db.execute(
            "UPDATE threads SET title = ?1 WHERE id = 'named'",
            ["完善用量统计展示"],
        )
        .unwrap();
        let renamed = fixture.stats("all", None);
        assert_eq!(renamed.sessions[0].title, "完善用量统计展示");
        assert_eq!(renamed.totals.total_tokens, first.totals.total_tokens);
        db.execute("DELETE FROM threads", []).unwrap();
        assert_eq!(fixture.stats("all", None).sessions[0].title, "Codex-X");
    }

    #[test]
    fn missing_chat_titles_use_projects_then_unnamed_instead_of_ids() {
        let mut fixture = Fixture::new();
        let mut root_meta = meta("with-project", "2026-09-08T01:00:00Z", None);
        root_meta["payload"]["cwd"] = json!(r"C:\projects\Codex-X");
        fixture.write(
            "sessions/project.jsonl",
            &[
                root_meta,
                context("gpt-5.4"),
                total("2026-09-08T01:00:02Z", 100, 10),
            ],
        );
        fixture.write(
            "sessions/untitled.jsonl",
            &[
                meta("without-title", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let stats = fixture.stats("all", None);
        assert_eq!(stats.sessions[0].title, "Codex-X");
        assert_eq!(stats.sessions[1].title, "未命名会话");
        assert_eq!(stats.totals.total_tokens, 220);
    }

    #[test]
    fn recent_session_limit_does_not_truncate_aggregate() {
        let mut fixture = Fixture::new();
        for index in 0..35 {
            let id = format!("session-{index:02}");
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    meta(&id, "2026-09-08T01:00:00Z", None),
                    context("gpt-5.4"),
                    total("2026-09-08T01:00:01Z", 100, 10),
                ],
            );
        }
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 35 * 110);
        assert_eq!(stats.totals.session_count, 35);
        assert_eq!(stats.sessions.len(), 10);
        assert!(!stats.coverage.truncated);
        let json = serde_json::to_string(&stats).unwrap();
        assert!(!json.contains("PRIVATE TEXT"));
        assert!(!json.contains("user_instructions"));
    }

    #[test]
    fn limited_scan_resumes_at_complete_record_boundary() {
        let fixture = Fixture::new();
        let path = fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        let mut budget = 1;
        let partial = parse_file(&path, None, false, &mut budget).unwrap();
        assert!(!partial.complete);
        assert!(partial.parsed.events.is_empty());
        let mut budget = MAX_SCAN_BYTES;
        let complete = parse_file(&path, Some(partial), false, &mut budget).unwrap();
        assert!(complete.complete);
        assert_eq!(complete.parsed.events.len(), 1);
    }

    #[test]
    fn forced_budget_exhaustion_keeps_complete_cached_totals_until_round_finishes() {
        let mut fixture = Fixture::new();
        for id in ["one", "two", "three"] {
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    meta(id, "2026-09-08T01:00:00Z", None),
                    context("gpt-5.4"),
                    total("2026-09-08T01:00:01Z", 100, 10),
                ],
            );
        }
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 330);
        let exhausted = fixture.stats_with_budget(true, 0);
        assert_eq!(exhausted.totals.total_tokens, 330);
        assert_eq!(exhausted.coverage.scanned_files, 3);
        assert!(exhausted.coverage.truncated);
        assert!(exhausted
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("缓存用量")));
        let mut rounds = 0;
        while !fixture.cache.force_remaining.is_empty() && rounds < 20 {
            let current = fixture.stats_with_budget(true, 1);
            assert_eq!(current.totals.total_tokens, 330);
            assert_eq!(current.coverage.scanned_files, 3);
            rounds += 1;
        }
        assert!(rounds > 1);
        assert!(fixture.cache.force_remaining.is_empty());
        assert!(fixture.cache.force_staging.is_empty());
        assert!(!fixture.stats_with_budget(false, 0).coverage.truncated);
    }

    #[test]
    fn repeated_forced_refreshes_complete_new_files_without_restarting_the_prefix() {
        let mut fixture = Fixture::new();
        for id in ["one", "two", "three"] {
            fixture.write(
                &format!("sessions/{id}.jsonl"),
                &[
                    meta(id, "2026-09-08T01:00:00Z", None),
                    context("gpt-5.4"),
                    total("2026-09-08T01:00:01Z", 100, 10),
                ],
            );
        }
        let initial = fixture.stats_with_budget(true, 1);
        assert!(initial.coverage.truncated);
        let mut previous_total = initial.totals.total_tokens;
        for _ in 0..20 {
            let current = fixture.stats_with_budget(true, 1);
            assert!(current.totals.total_tokens >= previous_total);
            previous_total = current.totals.total_tokens;
            if fixture.cache.force_remaining.is_empty() {
                break;
            }
        }
        assert_eq!(previous_total, 330);
        assert!(fixture.cache.force_remaining.is_empty());
        assert_eq!(fixture.cache.files.len(), 3);
    }

    #[test]
    fn forced_verification_rereads_middle_even_when_file_also_appended() {
        let mut fixture = Fixture::new();
        let mut records = vec![
            meta("one", "2026-09-08T01:00:00Z", None),
            context("gpt-5.4"),
        ];
        records.extend((0..10).map(|_| heartbeat("2026-09-08T01:00:00Z")));
        let token_index = records.len();
        records.push(usage(
            "2026-09-08T01:00:01Z",
            Some(count(100, 0, 10, 0)),
            Some(count(100, 0, 10, 0)),
        ));
        records.extend((0..10).map(|_| heartbeat("2026-09-08T01:00:02Z")));
        let path = fixture.write("sessions/one.jsonl", &records);
        assert_eq!(fixture.stats("all", None).totals.total_tokens, 110);
        let old_offset = fixture.cache.files[&path].offset;
        let old_hashes = (
            fixture.cache.files[&path].prefix_hash,
            fixture.cache.files[&path].boundary_hash,
        );
        records[token_index] = usage(
            "2026-09-08T01:00:01Z",
            Some(count(200, 0, 20, 0)),
            Some(count(200, 0, 20, 0)),
        );
        records.push(usage(
            "2026-09-08T01:00:03Z",
            Some(count(250, 0, 25, 0)),
            Some(count(50, 0, 5, 0)),
        ));
        fixture.write("sessions/one.jsonl", &records);
        assert_eq!(
            anchors(&mut File::open(&path).unwrap(), old_offset).unwrap(),
            old_hashes
        );
        let mut completed = false;
        for _ in 0..50 {
            let current = fixture.stats_with_budget(true, 1);
            if fixture.cache.force_remaining.is_empty() {
                assert_eq!(current.totals.total_tokens, 275);
                completed = true;
                break;
            }
            assert_eq!(current.totals.total_tokens, 110);
        }
        assert!(completed);
    }

    #[cfg(unix)]
    #[test]
    fn linked_files_and_directories_are_skipped() {
        let mut fixture = Fixture::new();
        let other = Fixture::new();
        let target = other.write(
            "sessions/secret.jsonl",
            &[
                meta("secret", "2026-09-08T01:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T01:00:01Z", 100, 10),
            ],
        );
        std::os::unix::fs::symlink(target, fixture.dir.join("sessions/linked.jsonl")).unwrap();
        std::os::unix::fs::symlink(
            other.dir.join("sessions"),
            fixture.dir.join("sessions/linked-dir"),
        )
        .unwrap();
        let stats = fixture.stats("all", None);
        assert_eq!(stats.totals.total_tokens, 0);
        assert_eq!(stats.coverage.skipped_files, 2);
        assert!(stats
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.contains("符号链接")));
    }

    #[test]
    fn negative_local_offset_uses_local_midnight() {
        // FixedOffset checks the non-UTC contract; midnight() separately handles
        // local gaps via LocalResult rather than assuming 24-hour UTC days.
        let mut fixture = Fixture::new();
        fixture.write(
            "sessions/one.jsonl",
            &[
                meta("one", "2026-09-08T03:00:00Z", None),
                context("gpt-5.4"),
                total("2026-09-08T03:59:59Z", 100, 10),
                total("2026-09-08T04:00:00Z", 150, 15),
            ],
        );
        let west = DateTime::parse_from_rfc3339("2026-09-08T12:00:00-04:00").unwrap();
        let stats = fixture.stats_at("today", None, false, west);
        assert_eq!(stats.totals.total_tokens, 55);
        assert_eq!(
            stats.range_start.as_deref(),
            Some("2026-09-08T04:00:00+00:00")
        );
    }
}
