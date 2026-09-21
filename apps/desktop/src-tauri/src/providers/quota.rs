//! Read-only subscription quota lookup for one saved official login. Credentials
//! stay in native code; neither network errors nor response bodies are logged.

use super::official_profiles::{
    official_profile_quota_credentials, OfficialProfileQuotaCredentials,
};
use crate::error::{CodexxError, Result};
use crate::live_config::acquire_live_config_lock;
use crate::remote::ensure_crypto_provider;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const QUOTA_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const RESET_CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
// The file lock is nonblocking. Concurrent quota and credit queries share this
// brief gate while reading credentials, so they cannot fail each other's read.
// Neither this gate nor the file lock is held during a network request.
static QUERY_CREDENTIAL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
enum OfficialQuery {
    Quota,
    ResetCredits,
}

impl OfficialQuery {
    fn url(self) -> &'static str {
        match self {
            Self::Quota => QUOTA_URL,
            Self::ResetCredits => RESET_CREDITS_URL,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Quota => "额度",
            Self::ResetCredits => "重置次数",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialResetCreditsSnapshot {
    profile_id: String,
    available_count: u32,
    checked_at: String,
}

#[derive(Deserialize)]
struct RawResetCreditsResponse {
    // Required and unsigned: unknown or invalid counts must never look like 0.
    available_count: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialQuotaSnapshot {
    profile_id: String,
    email: Option<String>,
    plan_type: Option<String>,
    limits: Vec<OfficialQuotaLimit>,
    checked_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialQuotaLimit {
    id: String,
    name: Option<String>,
    allowed: Option<bool>,
    limit_reached: Option<bool>,
    windows: Vec<OfficialQuotaWindow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialQuotaWindow {
    id: String,
    window_seconds: Option<i64>,
    used_percent: Option<f64>,
    remaining_percent: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawWindow {
    limit_window_seconds: Option<i64>,
    used_percent: Option<f64>,
    reset_at: Option<i64>,
    reset_after_seconds: Option<i64>,
}

#[derive(Default, Deserialize)]
struct RawLimit {
    allowed: Option<bool>,
    limit_reached: Option<bool>,
    primary_window: Option<RawWindow>,
    secondary_window: Option<RawWindow>,
}

#[derive(Deserialize)]
struct RawAdditionalLimit {
    limit_name: Option<String>,
    name: Option<String>,
    metered_feature: Option<String>,
    rate_limit: Option<RawLimit>,
    #[serde(flatten)]
    direct_limit: RawLimit,
}

#[derive(Deserialize)]
struct RawQuotaResponse {
    account_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    rate_limit: Option<RawLimit>,
    code_review_rate_limit: Option<RawLimit>,
    additional_rate_limits: Option<Vec<Option<RawAdditionalLimit>>>,
}

fn config_error(message: &str) -> CodexxError {
    CodexxError::Config(message.to_owned())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn display_email(value: Option<&str>) -> Option<String> {
    nonempty(value)
        .filter(|email| email.len() <= 320 && email.contains('@'))
        .filter(|email| {
            !email
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        })
        .map(str::to_owned)
}

fn quota_headers(credentials: &OfficialProfileQuotaCredentials) -> Result<HeaderMap> {
    let token = nonempty(Some(&credentials.access_token))
        .ok_or_else(|| config_error("此官方配置尚未登录，请先在 Codex 中登录"))?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| config_error("此官方配置的登录凭据无效，请重新登录"))?;
    authorization.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, authorization);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("codex-cli"));
    if let Some(account_id) = nonempty(credentials.account_id.as_deref()) {
        let mut account = HeaderValue::from_str(account_id)
            .map_err(|_| config_error("此官方配置的账号标识无效，请重新登录"))?;
        account.set_sensitive(true);
        headers.insert("ChatGPT-Account-Id", account);
    }
    Ok(headers)
}

fn quota_client() -> Result<Client> {
    ensure_crypto_provider();
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| config_error("官方账号查询客户端初始化失败"))
}

fn check_status(status: StatusCode, query: OfficialQuery) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let label = query.label();
    let message = match status.as_u16() {
        401 => format!("官方登录已失效，请重新登录此账号后查询{label}（HTTP 401）"),
        403 => format!("无法访问官方{label}服务（HTTP 403），请稍后重试或检查网络"),
        429 => format!("官方{label}查询过于频繁，请稍后重试（HTTP 429）"),
        300..=399 => format!("官方{label}接口发生重定向，已停止请求，请稍后重试"),
        code @ 500..=599 => format!("官方{label}服务暂不可用（HTTP {code}），请稍后重试"),
        code => format!("官方{label}查询失败（HTTP {code}），请稍后重试"),
    };
    Err(CodexxError::Config(message))
}

fn read_bounded_body(reader: impl Read, content_length: Option<u64>) -> Result<Vec<u8>> {
    if content_length.is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
        return Err(config_error("官方查询响应过大，已停止读取"));
    }
    let mut bytes = Vec::new();
    reader
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| config_error("读取官方查询响应失败，请稍后重试"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(config_error("官方查询响应过大，已停止读取"));
    }
    Ok(bytes)
}

fn window(id: &str, raw: RawWindow, checked_at: DateTime<Utc>) -> OfficialQuotaWindow {
    let used_percent = raw
        .used_percent
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0));
    let resets_at = raw
        .reset_at
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .or_else(|| {
            raw.reset_after_seconds
                .filter(|seconds| *seconds >= 0)
                .and_then(ChronoDuration::try_seconds)
                .and_then(|duration| checked_at.checked_add_signed(duration))
        })
        .map(|time| time.to_rfc3339());
    OfficialQuotaWindow {
        id: id.to_owned(),
        window_seconds: raw.limit_window_seconds.filter(|seconds| *seconds > 0),
        used_percent,
        remaining_percent: used_percent.map(|value| (100.0 - value).clamp(0.0, 100.0)),
        resets_at,
    }
}

fn limit(
    id: String,
    name: Option<String>,
    raw: RawLimit,
    checked_at: DateTime<Utc>,
) -> OfficialQuotaLimit {
    let windows = [
        ("primary", raw.primary_window),
        ("secondary", raw.secondary_window),
    ]
    .into_iter()
    .filter_map(|(id, raw)| raw.map(|raw| window(id, raw, checked_at)))
    .collect();
    OfficialQuotaLimit {
        id,
        name,
        allowed: raw.allowed,
        limit_reached: raw.limit_reached,
        windows,
    }
}

fn parse_quota_response(
    bytes: &[u8],
    profile_id: &str,
    credentials: &OfficialProfileQuotaCredentials,
    checked_at: DateTime<Utc>,
) -> Result<OfficialQuotaSnapshot> {
    let raw: RawQuotaResponse = serde_json::from_slice(bytes)
        .map_err(|_| config_error("官方额度响应格式无法识别，请稍后重试"))?;
    if nonempty(raw.account_id.as_deref())
        .zip(nonempty(credentials.account_id.as_deref()))
        .is_some_and(|(actual, expected)| actual != expected)
    {
        return Err(config_error(
            "额度响应与所选官方账号不一致，请重新登录此配置后重试",
        ));
    }
    let mut limits = Vec::new();
    if let Some(raw) = raw.rate_limit {
        limits.push(limit("main".to_owned(), None, raw, checked_at));
    }
    if let Some(raw) = raw.code_review_rate_limit {
        limits.push(limit("code_review".to_owned(), None, raw, checked_at));
    }
    for (index, additional) in raw
        .additional_rate_limits
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let Some(additional) = additional else {
            continue;
        };
        let name = nonempty(additional.limit_name.as_deref())
            .or_else(|| nonempty(additional.name.as_deref()))
            .or_else(|| nonempty(additional.metered_feature.as_deref()))
            .map(str::to_owned);
        limits.push(limit(
            format!("additional-{index}"),
            name,
            additional.rate_limit.unwrap_or(additional.direct_limit),
            checked_at,
        ));
    }
    Ok(OfficialQuotaSnapshot {
        profile_id: profile_id.to_owned(),
        email: display_email(raw.email.as_deref())
            .or_else(|| display_email(credentials.email.as_deref())),
        plan_type: nonempty(raw.plan_type.as_deref()).map(str::to_owned),
        limits,
        checked_at: checked_at.to_rfc3339(),
    })
}

fn credential_fingerprint(credentials: &OfficialProfileQuotaCredentials) -> [u8; 32] {
    let mut digest = Sha256::new();
    // Length-delimit components so two different identities cannot have the
    // same concatenated input. Only this digest is compared after the request.
    for value in [
        Some(credentials.access_token.as_str()),
        credentials.account_id.as_deref(),
        credentials.email.as_deref(),
    ] {
        let bytes = value.unwrap_or_default().as_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.finalize().into()
}

fn parse_reset_credits_response(
    bytes: &[u8],
    profile_id: &str,
    checked_at: DateTime<Utc>,
) -> Result<OfficialResetCreditsSnapshot> {
    let raw: RawResetCreditsResponse = serde_json::from_slice(bytes)
        .map_err(|_| config_error("官方重置次数响应格式无法识别，请稍后重试"))?;
    Ok(OfficialResetCreditsSnapshot {
        profile_id: profile_id.to_owned(),
        available_count: raw.available_count,
        checked_at: checked_at.to_rfc3339(),
    })
}

fn ensure_same_credentials(
    expected: [u8; 32],
    current: &OfficialProfileQuotaCredentials,
) -> Result<()> {
    if expected != credential_fingerprint(current) {
        return Err(config_error("此官方账号的登录信息已变化，请重新刷新"));
    }
    Ok(())
}

fn fetch_official_response(
    query: OfficialQuery,
    credentials: &OfficialProfileQuotaCredentials,
) -> Result<Vec<u8>> {
    let headers = quota_headers(credentials)?;
    let label = query.label();
    let response = quota_client()?
        .get(query.url())
        .headers(headers)
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                config_error(&format!("官方{label}查询超时，请检查网络后重试"))
            } else if error.is_connect() {
                config_error(&format!("无法连接官方{label}接口，请检查网络或代理设置"))
            } else {
                config_error(&format!("官方{label}网络请求失败，请稍后重试"))
            }
        })?;
    check_status(response.status(), query)?;
    let length = response.content_length();
    read_bounded_body(response, length)
}

fn read_query_credentials(
    codex_dir: &Path,
    profile_id: &str,
) -> Result<OfficialProfileQuotaCredentials> {
    let _query_guard = QUERY_CREDENTIAL_LOCK
        .lock()
        .map_err(|_| config_error("官方账号查询暂不可用，请重试"))?;
    let _file_guard = acquire_live_config_lock(codex_dir)
        .map_err(|_| config_error("官方配置正在修改，请稍后刷新"))?;
    official_profile_quota_credentials(codex_dir, profile_id)
}

fn read_official_snapshot<T>(
    codex_dir: &Path,
    profile_id: &str,
    fetch: impl FnOnce(&OfficialProfileQuotaCredentials) -> Result<Vec<u8>>,
    parse: impl FnOnce(&[u8], &str, &OfficialProfileQuotaCredentials, DateTime<Utc>) -> Result<T>,
) -> Result<T> {
    let credentials = read_query_credentials(codex_dir, profile_id)?;
    let fingerprint = credential_fingerprint(&credentials);
    let bytes = fetch(&credentials)?;
    let snapshot = parse(&bytes, profile_id, &credentials, Utc::now())?;
    let current = read_query_credentials(codex_dir, profile_id)
        .map_err(|_| config_error("此官方配置已变化或被删除，请重新选择账号后查询"))?;
    ensure_same_credentials(fingerprint, &current)?;
    Ok(snapshot)
}

pub(crate) fn get_official_profile_quota_inner(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<OfficialQuotaSnapshot> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    read_official_snapshot(
        &codex_dir,
        profile_id.trim(),
        |credentials| fetch_official_response(OfficialQuery::Quota, credentials),
        parse_quota_response,
    )
}

pub(crate) fn get_official_profile_reset_credits_inner(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<OfficialResetCreditsSnapshot> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    read_official_snapshot(
        &codex_dir,
        profile_id.trim(),
        |credentials| fetch_official_response(OfficialQuery::ResetCredits, credentials),
        |bytes, profile_id, _, checked_at| {
            parse_reset_credits_response(bytes, profile_id, checked_at)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn credentials(account: Option<&str>) -> OfficialProfileQuotaCredentials {
        OfficialProfileQuotaCredentials {
            access_token: "synthetic-access-token".to_owned(),
            account_id: account.map(str::to_owned),
            email: Some("saved@example.test".to_owned()),
        }
    }

    fn checked_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-09T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn parse(value: serde_json::Value, account: Option<&str>) -> OfficialQuotaSnapshot {
        parse_quota_response(
            &serde_json::to_vec(&value).unwrap(),
            "official-profile-a",
            &credentials(account),
            checked_at(),
        )
        .unwrap()
    }

    #[test]
    fn pro_primary_window_can_be_weekly_and_account_id_can_be_absent() {
        let snapshot = parse(
            json!({
                "plan_type":"pro", "email":"active@example.test", "account_id":"server-account",
                "rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"limit_window_seconds":604800,"used_percent":37.5,"reset_after_seconds":600}},
                "additional_rate_limits":null,"code_review_rate_limit":null
            }),
            None,
        );
        assert_eq!(snapshot.profile_id, "official-profile-a");
        assert_eq!(snapshot.email.as_deref(), Some("active@example.test"));
        assert_eq!(snapshot.plan_type.as_deref(), Some("pro"));
        assert_eq!(snapshot.limits.len(), 1);
        let limit = &snapshot.limits[0];
        assert_eq!(limit.id, "main");
        assert_eq!(limit.allowed, Some(true));
        assert_eq!(limit.windows[0].id, "primary");
        assert_eq!(limit.windows[0].window_seconds, Some(604800));
        assert_eq!(limit.windows[0].remaining_percent, Some(62.5));
        assert_eq!(
            limit.windows[0].resets_at.as_deref(),
            Some("2026-09-09T03:10:00+00:00")
        );
    }

    #[test]
    fn team_two_windows_preserve_real_durations_and_absolute_reset_priority() {
        let reset = checked_at().timestamp() + 3600;
        let snapshot = parse(
            json!({
                "account_id":"team-a","plan_type":"team",
                "rate_limit":{"allowed":false,"limit_reached":true,
                    "primary_window":{"limit_window_seconds":18000,"used_percent":100,"reset_at":reset,"reset_after_seconds":20},
                    "secondary_window":{"limit_window_seconds":604800,"used_percent":23.25}}
            }),
            Some("team-a"),
        );
        let limit = &snapshot.limits[0];
        assert_eq!(limit.windows.len(), 2);
        assert_eq!(limit.limit_reached, Some(true));
        assert_eq!(limit.windows[0].window_seconds, Some(18000));
        assert_eq!(limit.windows[0].remaining_percent, Some(0.0));
        assert_eq!(
            limit.windows[0].resets_at.as_deref(),
            Some("2026-09-09T04:00:00+00:00")
        );
        assert_eq!(limit.windows[1].window_seconds, Some(604800));
        assert_eq!(limit.windows[1].remaining_percent, Some(76.75));
        assert!(limit.windows[1].resets_at.is_none());
    }

    #[test]
    fn code_review_and_additional_features_remain_separate_limits() {
        let snapshot = parse(
            json!({
                "rate_limit":null,
                "code_review_rate_limit":{"allowed":true,"primary_window":{"used_percent":10,"limit_window_seconds":604800}},
                "additional_rate_limits":[null,
                    {"limit_name":"Codex Spark","metered_feature":"codex_bengal","rate_limit":{"allowed":true,"primary_window":{"used_percent":40,"limit_window_seconds":18000}}},
                    {"metered_feature":"future_feature","allowed":false,"limit_reached":true,"primary_window":{"used_percent":100,"limit_window_seconds":86400}}]
            }),
            None,
        );
        assert_eq!(snapshot.limits.len(), 3);
        assert_eq!(snapshot.limits[0].id, "code_review");
        assert_eq!(snapshot.limits[1].id, "additional-1");
        assert_eq!(snapshot.limits[1].name.as_deref(), Some("Codex Spark"));
        assert_eq!(snapshot.limits[2].name.as_deref(), Some("future_feature"));
        assert_eq!(snapshot.limits[2].allowed, Some(false));
        assert_eq!(snapshot.limits[2].windows[0].window_seconds, Some(86400));
    }

    #[test]
    fn missing_usage_is_unknown_instead_of_zero_or_full_remaining() {
        let snapshot = parse(
            json!({"rate_limit":{"primary_window":{"limit_window_seconds":18000}}}),
            None,
        );
        let window = &snapshot.limits[0].windows[0];
        assert!(window.used_percent.is_none());
        assert!(window.remaining_percent.is_none());
        assert!(snapshot.limits[0].allowed.is_none());
        assert!(snapshot.limits[0].limit_reached.is_none());
        let empty = parse(json!({}), None);
        assert!(empty.limits.is_empty());
    }

    #[test]
    fn finite_percentages_are_clamped_and_nonfinite_values_stay_unknown() {
        for (used, expected) in [
            (-20.0, Some(0.0)),
            (130.0, Some(100.0)),
            (f64::NAN, None),
            (f64::INFINITY, None),
        ] {
            let value = window(
                "primary",
                RawWindow {
                    used_percent: Some(used),
                    ..Default::default()
                },
                checked_at(),
            );
            assert_eq!(value.used_percent, expected);
            assert_eq!(value.remaining_percent, expected.map(|used| 100.0 - used));
        }
    }

    #[test]
    fn invalid_reset_and_window_numbers_do_not_panic_or_invent_dates() {
        let value = window(
            "primary",
            RawWindow {
                limit_window_seconds: Some(-1),
                reset_at: Some(i64::MAX),
                reset_after_seconds: Some(i64::MAX),
                ..Default::default()
            },
            checked_at(),
        );
        assert!(value.window_seconds.is_none());
        assert!(value.resets_at.is_none());
        let negative = window(
            "primary",
            RawWindow {
                reset_after_seconds: Some(-10),
                ..Default::default()
            },
            checked_at(),
        );
        assert!(negative.resets_at.is_none());
        let now = window(
            "primary",
            RawWindow {
                reset_after_seconds: Some(0),
                ..Default::default()
            },
            checked_at(),
        );
        assert_eq!(now.resets_at.as_deref(), Some("2026-09-09T03:00:00+00:00"));
    }

    #[test]
    fn different_nonempty_response_account_is_rejected_without_identifiers_in_error() {
        let error = parse_quota_response(
            br#"{"account_id":"other-private-account"}"#,
            "official-profile-a",
            &credentials(Some("selected-private-account")),
            checked_at(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("不一致"));
        assert!(!error.contains("private-account"));
        assert!(!error.contains("synthetic-access-token"));
        assert!(
            parse(json!({"account_id":null}), Some("selected-private-account"))
                .limits
                .is_empty()
        );
    }

    #[test]
    fn email_uses_response_first_then_safe_saved_display_metadata() {
        assert_eq!(
            parse(json!({"email":"response@example.test"}), None)
                .email
                .as_deref(),
            Some("response@example.test")
        );
        assert_eq!(
            parse(json!({"email":"bad\n@example.test"}), None)
                .email
                .as_deref(),
            Some("saved@example.test")
        );
        assert_eq!(
            parse(json!({}), None).email.as_deref(),
            Some("saved@example.test")
        );
    }

    #[test]
    fn request_has_only_fixed_https_endpoint_and_sensitive_auth_headers() {
        let request = quota_client()
            .unwrap()
            .get(QUOTA_URL)
            .headers(quota_headers(&credentials(Some("account-a"))).unwrap())
            .build()
            .unwrap();
        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(request.url().as_str(), QUOTA_URL);
        assert_eq!(request.url().host_str(), Some("chatgpt.com"));
        assert_eq!(request.url().scheme(), "https");
        assert!(request.headers()[AUTHORIZATION].is_sensitive());
        assert_eq!(
            request.headers()[AUTHORIZATION],
            "Bearer synthetic-access-token"
        );
        assert!(request.headers()["chatgpt-account-id"].is_sensitive());
        assert_eq!(request.headers()["chatgpt-account-id"], "account-a");
        assert_eq!(request.headers()[ACCEPT], "application/json");
        assert_eq!(request.headers()[USER_AGENT], "codex-cli");
        assert!(!format!("{request:?}").contains("synthetic-access-token"));
        assert!(!quota_headers(&credentials(None))
            .unwrap()
            .contains_key("chatgpt-account-id"));
        let credits_request = quota_client()
            .unwrap()
            .get(OfficialQuery::ResetCredits.url())
            .headers(quota_headers(&credentials(Some("account-a"))).unwrap())
            .build()
            .unwrap();
        assert_eq!(credits_request.method(), reqwest::Method::GET);
        assert_eq!(credits_request.url().as_str(), RESET_CREDITS_URL);
        assert_eq!(credits_request.url().scheme(), "https");
        assert_eq!(credits_request.url().host_str(), Some("chatgpt.com"));
        assert!(credits_request.headers()[AUTHORIZATION].is_sensitive());
        assert!(credits_request.headers()["chatgpt-account-id"].is_sensitive());
        assert!(!format!("{credits_request:?}").contains("synthetic-access-token"));
    }

    #[test]
    fn empty_or_injected_credentials_fail_without_revealing_content() {
        let mut empty = credentials(None);
        empty.access_token = "  ".to_owned();
        assert!(quota_headers(&empty)
            .unwrap_err()
            .to_string()
            .contains("尚未登录"));
        let mut invalid = credentials(None);
        invalid.access_token = "private-secret\r\nInjected: true".to_owned();
        let error = quota_headers(&invalid).unwrap_err().to_string();
        assert!(!error.contains("private-secret"));
        assert!(!error.contains("Injected"));
        invalid = credentials(Some("private-account\r\nInjected: true"));
        let error = quota_headers(&invalid).unwrap_err().to_string();
        assert!(!error.contains("private-account"));
        assert!(error.contains("账号标识无效"));
    }

    #[test]
    fn error_statuses_have_safe_actionable_messages() {
        for query in [OfficialQuery::Quota, OfficialQuery::ResetCredits] {
            assert!(check_status(StatusCode::OK, query).is_ok());
            for (status, expected) in [
                (401, "重新登录"),
                (403, "检查网络"),
                (429, "过于频繁"),
                (302, "重定向"),
                (503, "HTTP 503"),
            ] {
                let error = check_status(StatusCode::from_u16(status).unwrap(), query)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(expected));
                assert!(error.contains(query.label()));
            }
        }
    }

    #[test]
    fn body_limit_applies_even_without_or_with_false_content_length() {
        assert_eq!(read_bounded_body(&b"{}"[..], Some(2)).unwrap(), b"{}");
        let large = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        for content_length in [None, Some(1), Some(large.len() as u64)] {
            assert!(read_bounded_body(&large[..], content_length)
                .unwrap_err()
                .to_string()
                .contains("响应过大"));
        }
    }

    #[test]
    fn malformed_response_never_enters_error_text() {
        let error = parse_quota_response(
            b"server accidentally returned private-secret-token",
            "official-profile-a",
            &credentials(None),
            checked_at(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("格式无法识别"));
        assert!(!error.contains("private-secret-token"));
    }

    #[test]
    fn credential_change_rejects_stale_result_for_token_account_or_email() {
        let original = credentials(Some("account-a"));
        let fingerprint = credential_fingerprint(&original);
        assert!(ensure_same_credentials(fingerprint, &credentials(Some("account-a"))).is_ok());
        for field in ["token", "account", "email"] {
            let mut changed = credentials(Some("account-a"));
            match field {
                "token" => changed.access_token = "private-replacement-token".into(),
                "account" => changed.account_id = Some("account-b".into()),
                _ => changed.email = Some("changed@example.test".into()),
            }
            let error = ensure_same_credentials(fingerprint, &changed)
                .unwrap_err()
                .to_string();
            assert!(error.contains("登录信息已变化"));
            assert!(!error.contains("private-replacement-token"));
            assert!(!error.contains("account-b"));
            assert!(!error.contains("changed@example.test"));
        }
    }

    #[test]
    fn serialized_snapshot_contains_only_display_fields_and_no_credentials() {
        let snapshot = parse(
            json!({"plan_type":"team","rate_limit":{"primary_window":{"used_percent":40,"limit_window_seconds":18000}}}),
            Some("account-a"),
        );
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["profileId"], "official-profile-a");
        assert_eq!(value["planType"], "team");
        assert_eq!(value["limits"][0]["windows"][0]["remainingPercent"], 60.0);
        let serialized = value.to_string();
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("synthetic-access-token"));
        assert!(!serialized.contains("account-a"));
    }

    #[test]
    fn reset_credits_uses_available_count_and_returns_no_credit_details() {
        let raw = json!({
            "available_count": 3,
            "total_earned_count": 0,
            "credits": [{"id":"private-credit-id", "status":"available", "title":"private-credit-title"}],
            "immediate_reset_purchase_eligible": true
        });
        let snapshot = parse_reset_credits_response(
            &serde_json::to_vec(&raw).unwrap(),
            "official-profile-a",
            checked_at(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "profileId":"official-profile-a",
                "availableCount":3,
                "checkedAt":"2026-09-09T03:00:00+00:00"
            })
        );
        let zero = parse_reset_credits_response(
            br#"{"available_count":0,"credits":[{"status":"available"}]}"#,
            "official-profile-a",
            checked_at(),
        )
        .unwrap();
        assert_eq!(zero.available_count, 0);
    }

    #[test]
    fn reset_credits_unknown_or_invalid_counts_are_not_reported_as_zero() {
        for raw in [
            "{}",
            "null",
            r#"{"available_count":null}"#,
            r#"{"available_count":-1}"#,
            r#"{"available_count":1.5}"#,
            r#"{"available_count":"3"}"#,
            r#"{"available_count":true}"#,
            r#"{"available_count":4294967296}"#,
            "private-malformed-response",
        ] {
            let error =
                parse_reset_credits_response(raw.as_bytes(), "official-profile-a", checked_at())
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("重置次数响应格式无法识别"));
            assert!(!error.contains("private-malformed-response"));
        }
    }

    fn query_fixture(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codex-x-official-query-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::file_io::write_text(
            &crate::config_path(&dir),
            "model_provider = \"openai\"\nmodel = \"synthetic-model\"\n",
        )
        .unwrap();
        crate::file_io::write_json(
            &crate::auth_path(&dir),
            &json!({
                "auth_mode":"chatgpt", "tokens":{
                    "access_token":"synthetic-query-token", "account_id":"synthetic-account"
                }
            }),
        )
        .unwrap();
        dir
    }

    #[test]
    fn quota_and_reset_queries_overlap_without_holding_the_live_config_lock() {
        use super::super::official_profiles::DEFAULT_OFFICIAL_PROFILE_ID;
        use std::sync::mpsc;

        let _db_guard = crate::app_db::test_db_guard();
        let dir = query_fixture("parallel");
        // Initialize the test database before starting the two independent reads.
        read_query_credentials(&dir, DEFAULT_OFFICIAL_PROFILE_ID).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (quota_release_tx, quota_release_rx) = mpsc::channel();
        let (credits_release_tx, credits_release_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let quota_started = started_tx.clone();
            let quota_dir = &dir;
            let quota = scope.spawn(move || {
                read_official_snapshot(
                    quota_dir,
                    DEFAULT_OFFICIAL_PROFILE_ID,
                    |_| {
                        quota_started.send("quota").unwrap();
                        quota_release_rx
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                        Ok(b"{}".to_vec())
                    },
                    parse_quota_response,
                )
            });
            let credits_dir = &dir;
            let credits = scope.spawn(move || {
                read_official_snapshot(
                    credits_dir,
                    DEFAULT_OFFICIAL_PROFILE_ID,
                    |_| {
                        started_tx.send("credits").unwrap();
                        credits_release_rx
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                        Ok(br#"{"available_count":3}"#.to_vec())
                    },
                    |bytes, id, _, time| parse_reset_credits_response(bytes, id, time),
                )
            });
            // Both requests must start before either can finish. A lock held over
            // the simulated network wait would make this fail instead of hang.
            let first = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let second = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_ne!(first, second);
            drop(acquire_live_config_lock(&dir).unwrap());
            quota_release_tx.send(()).unwrap();
            credits_release_tx.send(()).unwrap();
            assert_eq!(
                quota.join().unwrap().unwrap().profile_id,
                DEFAULT_OFFICIAL_PROFILE_ID
            );
            assert_eq!(credits.join().unwrap().unwrap().available_count, 3);
        });
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reset_query_rejects_login_replaced_during_request() {
        use super::super::official_profiles::DEFAULT_OFFICIAL_PROFILE_ID;

        let _db_guard = crate::app_db::test_db_guard();
        let dir = query_fixture("changed-login");
        let error = read_official_snapshot(
            &dir, DEFAULT_OFFICIAL_PROFILE_ID,
            |_| {
                let _guard = acquire_live_config_lock(&dir).unwrap();
                crate::file_io::write_json(&crate::auth_path(&dir), &json!({
                    "auth_mode":"chatgpt", "tokens":{
                        "access_token":"synthetic-replacement-token", "account_id":"synthetic-account"
                    }
                })).unwrap();
                Ok(br#"{"available_count":3}"#.to_vec())
            },
            |bytes, id, _, time| parse_reset_credits_response(bytes, id, time),
        ).unwrap_err().to_string();
        assert!(error.contains("登录信息已变化"));
        assert!(!error.contains("synthetic-replacement-token"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
