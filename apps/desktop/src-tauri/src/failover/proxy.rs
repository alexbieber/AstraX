//! Local Responses forwarding. A request owns its route snapshot, including
//! credentials, and can only be retried before any response reaches Codex.

use super::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use super::config::RoutingTuning;
use super::native_official::{OfficialRequestAuth, OfficialRouteSpec};
use crate::error::{CodexxError, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_HEADERS_BYTES: usize = 32 * 1024;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_ROUTES: usize = 64;
const WORKER_COUNT: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProxyRoute {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) models: HashSet<String>,
    pub(crate) official: Option<OfficialRouteSpec>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProxySnapshot {
    pub(crate) request_count: u64,
    pub(crate) failover_count: u64,
    pub(crate) in_flight: u32,
    pub(crate) last_provider_id: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) providers: Vec<ProxyProviderHealth>,
    pub(crate) success_count: u64,
    pub(crate) failure_count: u64,
    pub(crate) uptime_seconds: u64,
    pub(crate) last_request_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProxyProviderHealth {
    pub(crate) id: String,
    pub(crate) state: String,
    pub(crate) cooldown_seconds: u64,
    pub(crate) last_status: Option<u16>,
    pub(crate) consecutive_failures: u32,
    pub(crate) consecutive_successes: u32,
    pub(crate) total_requests: u32,
    pub(crate) failed_requests: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProxyOptions {
    pub(crate) auto_failover_enabled: bool,
    pub(crate) tuning: RoutingTuning,
}

#[derive(Debug, Clone)]
pub(crate) struct ProxySelectionEvent {
    pub(crate) provider_id: String,
    pub(crate) revision: u64,
}

type SelectionCallback = Arc<dyn Fn(ProxySelectionEvent) + Send + Sync>;

struct Health {
    breaker: Arc<CircuitBreaker>,
    last_status: Option<u16>,
}

impl Health {
    fn new(tuning: &RoutingTuning) -> Self {
        Self {
            breaker: Arc::new(CircuitBreaker::new(CircuitBreakerConfig::from(tuning))),
            last_status: None,
        }
    }
}

#[derive(Default)]
struct Statistics {
    request_count: u64,
    failover_count: u64,
    in_flight: u32,
    last_provider_id: Option<String>,
    last_error: Option<String>,
    health: HashMap<String, Health>,
    success_count: u64,
    failure_count: u64,
    last_request_at: Option<String>,
}

#[derive(Clone)]
struct Configuration {
    routes: Vec<ProxyRoute>,
    options: ProxyOptions,
    revision: u64,
}

struct Shared {
    listener: Mutex<Option<TcpListener>>,
    configuration: RwLock<Configuration>,
    statistics: Mutex<Statistics>,
    selection_callback: Mutex<Option<SelectionCallback>>,
    stopped: AtomicBool,
    address: IpAddr,
    port: u16,
    token: String,
    started: Instant,
    #[cfg(test)]
    timing_unit: Mutex<Duration>,
    #[cfg(test)]
    timeout_override: Mutex<Option<RequestTimeouts>>,
}

pub(crate) struct ProxyHandle {
    shared: Arc<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ProxyHandle {
    pub(crate) fn start(
        address: IpAddr,
        port: u16,
        token: String,
        routes: Vec<ProxyRoute>,
        options: ProxyOptions,
    ) -> Result<Self> {
        validate_routes(&routes)?;
        options.tuning.validate()?;
        crate::remote::ensure_crypto_provider();
        if token.len() < 32 || token.len() > 512 || !token.bytes().all(|c| c.is_ascii_graphic()) {
            return Err(CodexxError::Config("Local routing credentials are invalid".into()));
        }
        let listener = TcpListener::bind(SocketAddr::new(address, port)).map_err(|_| {
            CodexxError::Config("Local routing failed to start; listen address is invalid or the port is in use".into())
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|_| CodexxError::Config("Unable to initialize local routing".into()))?;
        let port = listener
            .local_addr()
            .map_err(|_| CodexxError::Config("Unable to read local routing port".into()))?
            .port();
        reject_self_routes(&routes, address, port)?;
        let statistics = Statistics {
            health: routes
                .iter()
                .map(|route| (route.id.clone(), Health::new(&options.tuning)))
                .collect(),
            ..Statistics::default()
        };
        let shared = Arc::new(Shared {
            listener: Mutex::new(Some(listener)),
            configuration: RwLock::new(Configuration {
                routes,
                options,
                revision: 1,
            }),
            statistics: Mutex::new(statistics),
            selection_callback: Mutex::new(None),
            stopped: AtomicBool::new(false),
            address,
            port,
            token,
            started: Instant::now(),
            #[cfg(test)]
            timing_unit: Mutex::new(Duration::from_secs(1)),
            #[cfg(test)]
            timeout_override: Mutex::new(None),
        });
        let handle = Self { shared };
        for index in 0..WORKER_COUNT {
            let shared = Arc::clone(&handle.shared);
            if thread::Builder::new()
                .name(format!("codex-x-routing-{index}"))
                .spawn(move || worker(shared))
                .is_err()
            {
                handle.shutdown();
                return Err(CodexxError::Config("Local routing could not create a request thread".into()));
            }
        }
        Ok(handle)
    }

    pub(crate) fn port(&self) -> u16 {
        self.shared.port
    }
    pub(crate) fn listen_address(&self) -> IpAddr {
        self.shared.address
    }
    pub(crate) fn revision(&self) -> u64 {
        self.shared
            .configuration
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .revision
    }
    pub(crate) fn set_selection_callback(&self, callback: SelectionCallback) {
        *lock(&self.shared.selection_callback) = Some(callback);
    }

    pub(crate) fn configure(&self, routes: Vec<ProxyRoute>, options: ProxyOptions) -> Result<()> {
        self.validate_configuration(&routes, &options)?;
        let mut current = self
            .shared
            .configuration
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if current.routes == routes && current.options == options {
            return Ok(());
        }
        self.configure_locked(&mut current, routes, options)
    }

    pub(crate) fn validate_configuration(
        &self,
        routes: &[ProxyRoute],
        options: &ProxyOptions,
    ) -> Result<()> {
        validate_routes(routes)?;
        options.tuning.validate()?;
        reject_self_routes(routes, self.listen_address(), self.port())?;
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(CodexxError::Config("Local routing has stopped".into()));
        }
        Ok(())
    }

    fn configure_locked(
        &self,
        current: &mut Configuration,
        routes: Vec<ProxyRoute>,
        options: ProxyOptions,
    ) -> Result<()> {
        let ids: HashSet<&str> = routes.iter().map(|route| route.id.as_str()).collect();
        let mut statistics = lock(&self.shared.statistics);
        statistics.health.retain(|id, _| ids.contains(id.as_str()));
        for route in &routes {
            let transport_changed = current
                .routes
                .iter()
                .find(|old| old.id == route.id)
                .is_some_and(|old| !same_transport(old, route));
            if transport_changed || !statistics.health.contains_key(&route.id) {
                statistics
                    .health
                    .insert(route.id.clone(), Health::new(&options.tuning));
            } else if let Some(health) = statistics.health.get(&route.id) {
                health
                    .breaker
                    .update_config(CircuitBreakerConfig::from(&options.tuning));
            }
        }
        current.routes = routes;
        current.options = options;
        current.revision = current.revision.saturating_add(1);
        Ok(())
    }

    pub(crate) fn reset_breakers(&self, provider_id: Option<&str>) {
        let mut stats = lock(&self.shared.statistics);
        for (id, health) in &mut stats.health {
            if provider_id.is_none_or(|target| target == id) {
                health.breaker.reset();
                health.last_status = None;
            }
        }
        stats.last_error = None;
    }

    pub(crate) fn snapshot(&self) -> ProxySnapshot {
        let config = self
            .shared
            .configuration
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let stats = lock(&self.shared.statistics);
        ProxySnapshot {
            request_count: stats.request_count,
            failover_count: stats.failover_count,
            in_flight: stats.in_flight,
            last_provider_id: stats.last_provider_id.clone(),
            last_error: stats.last_error.clone(),
            success_count: stats.success_count,
            failure_count: stats.failure_count,
            uptime_seconds: self.shared.started.elapsed().as_secs(),
            last_request_at: stats.last_request_at.clone(),
            providers: config
                .routes
                .iter()
                .filter_map(|route| {
                    let health = stats.health.get(&route.id)?;
                    let breaker = health.breaker.get_stats();
                    Some(ProxyProviderHealth {
                        id: route.id.clone(),
                        state: breaker.state.to_string(),
                        cooldown_seconds: health.breaker.cooldown_seconds(),
                        last_status: health.last_status,
                        consecutive_failures: breaker.consecutive_failures,
                        consecutive_successes: breaker.consecutive_successes,
                        total_requests: breaker.total_requests,
                        failed_requests: breaker.failed_requests,
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn shutdown(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        drop(lock(&self.shared.listener).take());
    }

    #[cfg(test)]
    fn set_routes(&self, routes: Vec<ProxyRoute>) -> Result<()> {
        let options = self.shared.configuration.read().unwrap().options.clone();
        self.configure(routes, options)
    }

    #[cfg(test)]
    fn start_with_timeout(
        port: u16,
        token: String,
        routes: Vec<ProxyRoute>,
        timeout: Duration,
    ) -> Result<Self> {
        let options = ProxyOptions {
            auto_failover_enabled: true,
            tuning: RoutingTuning {
                circuit_failure_threshold: 1,
                ..Default::default()
            },
        };
        let handle = Self::start("127.0.0.1".parse().unwrap(), port, token, routes, options)?;
        *lock(&handle.shared.timeout_override) = Some(RequestTimeouts {
            first_header: Some(timeout),
            first_chunk: Some(timeout),
            idle: Some(timeout),
            non_streaming: Some(timeout),
        });
        Ok(handle)
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn validate_routes(routes: &[ProxyRoute]) -> Result<()> {
    if routes.len() > MAX_ROUTES {
        return Err(CodexxError::Config("Auto-failover supports at most 64 providers".into()));
    }
    if routes.iter().any(|route| route.official.is_some()) && routes.len() != 1 {
        return Err(CodexxError::Config("Official login can only use dedicated account routing".into()));
    }
    let mut ids = HashSet::new();
    for route in routes {
        if route.id.trim().is_empty()
            || route.name.trim().is_empty()
            || !ids.insert(route.id.as_str())
        {
            return Err(CodexxError::Config("Auto-failover providers cannot be empty or duplicated".into()));
        }
        let url = reqwest::Url::parse(&route.base_url)
            .map_err(|_| CodexxError::Config("Auto-failover provider URL is invalid".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(CodexxError::Config(
                "Auto-failover providers require a valid HTTP or HTTPS URL".into(),
            ));
        }
        if route
            .api_key
            .as_ref()
            .is_some_and(|key| key.bytes().any(|byte| byte.is_ascii_control()))
            || route.headers.iter().any(|(name, value)| {
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                    || reqwest::header::HeaderValue::from_str(value).is_err()
            })
        {
            return Err(CodexxError::Config(
                "Auto-failover provider headers or credentials are invalid".into(),
            ));
        }
    }
    Ok(())
}

fn reject_self_routes(routes: &[ProxyRoute], address: IpAddr, port: u16) -> Result<()> {
    for route in routes {
        if reqwest::Url::parse(&route.base_url).is_ok_and(|url| {
            url.port_or_known_default() == Some(port)
                && url.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("localhost")
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback() || ip == address)
                })
        }) {
            return Err(CodexxError::Config("Provider cannot point at the auto-failover service itself".into()));
        }
    }
    Ok(())
}

fn same_transport(left: &ProxyRoute, right: &ProxyRoute) -> bool {
    left.base_url == right.base_url
        && left.api_key == right.api_key
        && left.headers == right.headers
        && left.models == right.models
        && left.official == right.official
}

struct InFlight(Arc<Shared>);

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut statistics = lock(&self.0.statistics);
        statistics.in_flight = statistics.in_flight.saturating_sub(1);
    }
}

fn worker(shared: Arc<Shared>) {
    while !shared.stopped.load(Ordering::Acquire) {
        let connection = {
            let listener = lock(&shared.listener);
            let Some(listener) = listener.as_ref() else {
                return;
            };
            match listener.accept() {
                Ok((stream, _)) => {
                    lock(&shared.statistics).in_flight += 1;
                    Some(stream)
                }
                Err(_) => None,
            }
        };
        let Some(stream) = connection else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        let _in_flight = InFlight(Arc::clone(&shared));
        if stream.set_nonblocking(false).is_err()
            || stream.set_read_timeout(Some(CLIENT_IDLE_TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
        {
            continue;
        }
        let _ = stream.set_nodelay(true);
        let mut request = Request {
            reader: BufReader::with_capacity(16 * 1024, stream),
            method: String::new(),
            target: String::new(),
            headers: Vec::new(),
            started: Instant::now(),
        };
        if let Err((status, message)) = parse_request_headers(&mut request) {
            respond_error(request, status, message);
            continue;
        }
        handle_request(request, &shared);
    }
}

struct Request {
    reader: BufReader<TcpStream>,
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    started: Instant,
}

impl Request {
    fn into_writer(self) -> TcpStream {
        self.reader.into_inner()
    }
}

fn bounded_line(reader: &mut impl BufRead, limit: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline exceeded",
            ));
        }
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete line",
            ));
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + count > limit {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, "line limit"));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            if !line.ends_with(b"\r\n") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid line ending",
                ));
            }
            return Ok(line);
        }
    }
}

fn parse_request_headers(request: &mut Request) -> std::result::Result<(), (u16, &'static str)> {
    let mut bytes = Vec::new();
    loop {
        if request.started.elapsed() > CLIENT_REQUEST_TIMEOUT {
            return Err((408, "Read request timed out"));
        }
        let line = bounded_line(
            &mut request.reader,
            MAX_HEADERS_BYTES.saturating_sub(bytes.len()),
            request.started + CLIENT_REQUEST_TIMEOUT,
        )
        .map_err(|error| {
            if error.kind() == io::ErrorKind::FileTooLarge {
                (431, "Request headers too large")
            } else {
                (400, "Invalid HTTP request")
            }
        })?;
        let complete = line == b"\r\n";
        bytes.extend_from_slice(&line);
        if complete {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut parsed = httparse::Request::new(&mut headers);
    if !parsed
        .parse(&bytes)
        .is_ok_and(|status| status.is_complete())
        || !matches!(parsed.version, Some(0 | 1))
    {
        return Err((400, "Invalid HTTP request"));
    }
    request.method = parsed.method.ok_or((400, "Invalid HTTP method"))?.into();
    request.target = parsed.path.ok_or((400, "Invalid HTTP URL"))?.into();
    for header in parsed.headers {
        let value = std::str::from_utf8(header.value).map_err(|_| (400, "Invalid HTTP headers"))?;
        request
            .headers
            .push((header.name.to_owned(), value.to_owned()));
    }
    Ok(())
}

fn header_values<'a>(request: &'a Request, name: &str) -> Vec<&'a str> {
    request
        .headers
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

fn fixed_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn authorize(
    request: &Request,
    shared: &Shared,
    config: &Configuration,
) -> std::result::Result<Option<OfficialRequestAuth>, (u16, &'static str)> {
    if !header_values(request, "origin").is_empty()
        || !header_values(request, "sec-fetch-site").is_empty()
    {
        return Err((403, "This endpoint does not accept browser requests"));
    }
    let hosts = header_values(request, "host");
    let advertised =
        SocketAddr::new(super::config::client_ip(shared.address), shared.port).to_string();
    let local = request
        .reader
        .get_ref()
        .local_addr()
        .ok()
        .map(|address| address.to_string());
    if hosts.len() != 1 || (hosts[0] != advertised && local.as_deref() != Some(hosts[0])) {
        return Err((403, "Invalid local request URL"));
    }
    if !header_values(request, "upgrade").is_empty() {
        return Err((400, "This endpoint uses HTTP and does not support protocol upgrades"));
    }
    let authorization = header_values(request, "authorization");
    if let Some(spec) = config
        .routes
        .first()
        .and_then(|route| route.official.as_ref())
    {
        let route_token = header_values(request, super::config::ROUTE_TOKEN_HEADER);
        if route_token.len() != 1
            || !fixed_time_equal(route_token[0].as_bytes(), shared.token.as_bytes())
        {
            return Err((401, "Local routing authentication failed"));
        }
        let account_id = header_values(request, "chatgpt-account-id");
        if authorization.len() != 1 || account_id.len() > 1 {
            return Err((401, "Official account authentication is incomplete"));
        }
        return super::native_official::verify_request(
            spec,
            authorization[0],
            account_id.first().copied(),
        )
        .map(Some)
        .map_err(|_| (401, "Official account changed; sign in again or reopen Codex"));
    }
    let expected = format!("Bearer {}", shared.token);
    if authorization.len() != 1
        || !fixed_time_equal(authorization[0].as_bytes(), expected.as_bytes())
    {
        return Err((401, "Local request authentication failed"));
    }
    Ok(None)
}

struct RequestData {
    suffix: &'static str,
    query: Option<String>,
    model: Option<String>,
    primary_only: bool,
    streaming: bool,
    client_headers: Vec<(String, String)>,
    bytes: Vec<u8>,
}

fn request_data(request: &mut Request) -> std::result::Result<RequestData, (u16, &'static str)> {
    let client_headers = request
        .headers
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "user-agent"
                    | "openai-beta"
                    | "originator"
                    | "version"
                    | "x-client-request-id"
                    | "x-codex-turn-state"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let has_turn_state = client_headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("x-codex-turn-state") && !value.is_empty());
    let (path, query) = request
        .target
        .split_once('?')
        .map_or((request.target.as_str(), None), |(path, query)| {
            (path, Some(query))
        });
    if query.is_some_and(|query| {
        query.len() > MAX_QUERY_BYTES
            || query.contains('#')
            || query.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err((400, "Invalid query parameters"));
    }
    let query = query.map(str::to_owned);
    let (suffix, is_models) = match (request.method.as_str(), path) {
        ("POST", "/v1/responses") => ("responses", false),
        ("POST", "/v1/responses/compact") => ("responses/compact", false),
        ("GET", "/v1/models") => ("models", true),
        (_, "/v1/responses" | "/v1/responses/compact" | "/v1/models") => {
            return Err((405, "Request method not supported"))
        }
        _ => return Err((404, "Endpoint not found")),
    };
    let encoding = header_values(request, "content-encoding")
        .join(",")
        .to_ascii_lowercase();
    let codings = encoding
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "identity")
        .collect::<Vec<_>>();
    if codings.len() > 8
        || codings
            .iter()
            .any(|value| !matches!(*value, "gzip" | "x-gzip" | "deflate" | "zstd" | "zst"))
    {
        return Err((415, "Request compression format not supported"));
    }
    if !is_models
        && header_values(request, "content-type").iter().any(|value| {
            !value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err((415, "Request must use JSON"));
    }
    let bytes = read_request_body(request, is_models)?;
    let bytes = decode_content_encoding(&encoding, bytes, MAX_BODY_BYTES).map_err(|error| {
        if error.kind() == io::ErrorKind::FileTooLarge {
            (413, "Decompressed request exceeds 64 MiB; please reduce this input")
        } else {
            (400, "Compressed request body is corrupt or invalid")
        }
    })?;
    if is_models {
        if !bytes.is_empty() {
            return Err((400, "Model list queries must not include a request body"));
        }
        return Ok(RequestData {
            suffix,
            query,
            model: None,
            primary_only: has_turn_state,
            streaming: false,
            client_headers,
            bytes,
        });
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| (400, "Request is not valid JSON"))?;
    if !body.is_object() {
        return Err((400, "Request must be a JSON object"));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty() && model.len() <= 1024)
        .ok_or((400, "Request is missing a valid model name"))?
        .to_owned();
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    Ok(RequestData {
        suffix,
        query,
        model: Some(model),
        primary_only: contains_server_state(&body) || has_turn_state,
        streaming,
        client_headers,
        bytes,
    })
}

fn read_request_part(
    request: &mut Request,
    bytes: &mut Vec<u8>,
    length: usize,
) -> std::result::Result<(), (u16, &'static str)> {
    if length > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
        return Err((413, "Request exceeds 64 MiB; please reduce this input"));
    }
    let mut remaining = length;
    let mut buffer = [0; 16 * 1024];
    while remaining > 0 {
        if request.started.elapsed() >= CLIENT_REQUEST_TIMEOUT {
            return Err((408, "Read request timed out"));
        }
        let capacity = remaining.min(buffer.len());
        let count = request
            .reader
            .read(&mut buffer[..capacity])
            .map_err(|_| (408, "Request body read timed out or was interrupted"))?;
        if count == 0 {
            return Err((400, "Request body is incomplete"));
        }
        bytes.extend_from_slice(&buffer[..count]);
        remaining -= count;
    }
    Ok(())
}

fn read_request_body(
    request: &mut Request,
    is_models: bool,
) -> std::result::Result<Vec<u8>, (u16, &'static str)> {
    let lengths = header_values(request, "content-length");
    let transfers = header_values(request, "transfer-encoding");
    if (!lengths.is_empty() && !transfers.is_empty())
        || transfers.len() > 1
        || lengths.windows(2).any(|pair| pair[0] != pair[1])
    {
        return Err((400, "Request length is unclear"));
    }
    let length = lengths
        .first()
        .map(|value| {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err((400, "Invalid request length"));
            }
            value.parse::<usize>().map_err(|_| (413, "Request length too large"))
        })
        .transpose()?;
    if length.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err((413, "Request exceeds 64 MiB; please reduce this input"));
    }
    let chunked = transfers
        .first()
        .map(|value| value.eq_ignore_ascii_case("chunked"));
    if chunked == Some(false) {
        return Err((400, "Transfer encoding not supported"));
    }
    if length.is_none() && chunked.is_none() && !is_models {
        return Err((411, "Request must declare Content-Length"));
    }
    let expectations = header_values(request, "expect");
    if expectations.len() > 1
        || expectations
            .first()
            .is_some_and(|value| !value.eq_ignore_ascii_case("100-continue"))
    {
        return Err((417, "Request condition not supported"));
    }
    if !expectations.is_empty() {
        request
            .reader
            .get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| request.reader.get_mut().flush())
            .map_err(|_| (400, "Client disconnected"))?;
    }
    let mut bytes = Vec::new();
    if chunked.is_none() {
        read_request_part(request, &mut bytes, length.unwrap_or(0))?;
        return Ok(bytes);
    }
    loop {
        if request.started.elapsed() >= CLIENT_REQUEST_TIMEOUT {
            return Err((408, "Read request timed out"));
        }
        let line = bounded_line(
            &mut request.reader,
            256,
            request.started + CLIENT_REQUEST_TIMEOUT,
        )
        .map_err(|_| (400, "Invalid chunked request format"))?;
        let raw_size = line[..line.len() - 2]
            .split(|byte| *byte == b';')
            .next()
            .unwrap_or_default();
        if raw_size.is_empty() || !raw_size.iter().all(u8::is_ascii_hexdigit) {
            return Err((400, "Invalid chunked request length"));
        }
        let size = std::str::from_utf8(raw_size)
            .ok()
            .and_then(|text| usize::from_str_radix(text, 16).ok())
            .ok_or((413, "Chunked request length too large"))?;
        if size == 0 {
            let mut trailer_bytes = 0;
            loop {
                if request.started.elapsed() >= CLIENT_REQUEST_TIMEOUT {
                    return Err((408, "Read request timed out"));
                }
                let line = bounded_line(
                    &mut request.reader,
                    MAX_HEADERS_BYTES - trailer_bytes,
                    request.started + CLIENT_REQUEST_TIMEOUT,
                )
                .map_err(|_| (400, "Invalid chunked request ending"))?;
                trailer_bytes += line.len();
                if line == b"\r\n" {
                    return Ok(bytes);
                }
                // Trailer values never become authorization or forwarding
                // headers. They are framing only and have their own bound.
            }
        }
        read_request_part(request, &mut bytes, size)?;
        let mut ending = [0; 2];
        request
            .reader
            .read_exact(&mut ending)
            .map_err(|_| (400, "Chunked request body is incomplete"))?;
        if ending != *b"\r\n" {
            return Err((400, "Invalid chunked request ending"));
        }
    }
}

fn contains_server_state(body: &Value) -> bool {
    fn populated(value: Option<&Value>) -> bool {
        value.is_some_and(|value| {
            !value.is_null() && value.as_str().is_none_or(|text| !text.is_empty())
        })
    }
    fn stateful_input(value: &Value) -> bool {
        match value {
            Value::Array(items) => items.iter().any(stateful_input),
            Value::Object(item) => {
                item.get("type").and_then(Value::as_str) == Some("item_reference")
                    || populated(item.get("file_id"))
                    || populated(item.get("encrypted_content"))
                    || item.get("content").is_some_and(stateful_input)
            }
            _ => false,
        }
    }
    populated(body.get("previous_response_id"))
        || populated(body.get("conversation"))
        || body.get("background").and_then(Value::as_bool) == Some(true)
        || body.get("input").is_some_and(stateful_input)
}

fn read_bounded(reader: &mut dyn Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "body limit exceeded",
        ));
    }
    Ok(bytes)
}

fn retryable_status(status: u16) -> bool {
    (400..=599).contains(&status)
        && !matches!(status, 400 | 405 | 406 | 413 | 414 | 415 | 422 | 501)
}

// Adapted from CC Switch 06082e1 proxy/content_encoding.rs (MIT, Copyright
// 2025 Jason Young; full notice in THIRD_PARTY_NOTICES.md). Each decoding
// stage is bounded, including Codex Desktop's gzip/zstd request bodies.
fn decode_content_encoding(
    encoding: &str,
    mut bytes: Vec<u8>,
    limit: usize,
) -> io::Result<Vec<u8>> {
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "encoded body limit",
        ));
    }
    let codings = encoding
        .split(',')
        .map(str::trim)
        .filter(|coding| !coding.is_empty() && *coding != "identity")
        .collect::<Vec<_>>();
    if codings.len() > 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many content encodings",
        ));
    }
    for coding in codings.into_iter().rev() {
        bytes = match coding {
            "gzip" | "x-gzip" => {
                read_bounded(&mut flate2::read::GzDecoder::new(bytes.as_slice()), limit)?
            }
            "deflate" => {
                match read_bounded(&mut flate2::read::ZlibDecoder::new(bytes.as_slice()), limit) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == io::ErrorKind::FileTooLarge => return Err(error),
                    Err(_) => read_bounded(
                        &mut flate2::read::DeflateDecoder::new(bytes.as_slice()),
                        limit,
                    )?,
                }
            }
            "zstd" | "zst" => {
                let mut decoder =
                    zstd::stream::read::Decoder::new(io::Cursor::new(bytes.as_slice()))?;
                decoder.window_log_max(26)?;
                read_bounded(&mut decoder, limit)?
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported content encoding",
                ))
            }
        };
    }
    Ok(bytes)
}

fn upstream_url(route: &ProxyRoute, data: &RequestData) -> Option<String> {
    let mut url = reqwest::Url::parse(&route.base_url).ok()?;
    let base_path = url.path().trim_end_matches('/');
    // Saved base_url is the API prefix, matching Codex's own /responses join.
    // Do not invent /v1 for vendor endpoints that deliberately omit it.
    url.set_path(&format!("{base_path}/{}", data.suffix));
    let query = match (url.query(), data.query.as_deref()) {
        (Some(base), Some(request)) if !base.is_empty() && !request.is_empty() => {
            Some(format!("{base}&{request}"))
        }
        (Some(base), _) => Some(base.to_owned()),
        (None, Some(request)) => Some(request.to_owned()),
        (None, None) => None,
    };
    url.set_query(query.as_deref());
    Some(url.into())
}

fn forbidden_upstream_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "cookie"
            | "origin"
            | "referer"
            | "accept-encoding"
            | "content-encoding"
    )
}

#[derive(Clone, Copy)]
struct RequestTimeouts {
    first_header: Option<Duration>,
    first_chunk: Option<Duration>,
    idle: Option<Duration>,
    non_streaming: Option<Duration>,
}

fn seconds(shared: &Shared, value: u64) -> Duration {
    #[cfg(test)]
    {
        lock(&shared.timing_unit).saturating_mul(value.try_into().unwrap_or(u32::MAX))
    }
    #[cfg(not(test))]
    {
        let _ = shared;
        Duration::from_secs(value)
    }
}

fn timeouts(shared: &Shared, options: &ProxyOptions, official: bool) -> RequestTimeouts {
    #[cfg(test)]
    if let Some(overridden) = *lock(&shared.timeout_override) {
        return overridden;
    }
    if !options.auto_failover_enabled || official {
        return RequestTimeouts {
            first_header: Some(seconds(shared, 600)),
            first_chunk: None,
            idle: None,
            non_streaming: Some(seconds(shared, 600)),
        };
    }
    RequestTimeouts {
        first_header: Some(seconds(shared, options.tuning.streaming_first_byte_timeout)),
        first_chunk: Some(seconds(shared, options.tuning.streaming_first_byte_timeout)),
        idle: (options.tuning.streaming_idle_timeout > 0)
            .then(|| seconds(shared, options.tuning.streaming_idle_timeout)),
        non_streaming: Some(seconds(shared, options.tuning.non_streaming_timeout)),
    }
}

struct UpstreamResponse {
    response: reqwest::Response,
    status: u16,
    headers: reqwest::header::HeaderMap,
    deadline: Option<Instant>,
    next_timeout: Option<Duration>,
    pending: Vec<u8>,
    offset: usize,
}

impl Read for UpstreamResponse {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let idle_deadline = self.next_timeout.map(|duration| Instant::now() + duration);
        loop {
            if self.offset < self.pending.len() {
                let count = bytes.len().min(self.pending.len() - self.offset);
                bytes[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
                self.offset += count;
                return Ok(count);
            }
            let deadline = match (self.deadline, idle_deadline) {
                (Some(total), Some(idle)) => Some(total.min(idle)),
                (total, idle) => total.or(idle),
            };
            let timeout = deadline
                .map(|deadline| {
                    deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::TimedOut, "upstream deadline exceeded")
                        })
                })
                .transpose()?;
            let chunk = tauri::async_runtime::block_on(async {
                if let Some(timeout) = timeout {
                    tokio::time::timeout(timeout, self.response.chunk())
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "upstream response timeout")
                        })?
                } else {
                    self.response.chunk().await
                }
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "upstream response interrupted",
                    )
                })
            })?;
            let Some(chunk) = chunk else { return Ok(0) };
            self.pending = chunk.to_vec();
            self.offset = 0;
        }
    }
}

fn request_upstream(
    route: &ProxyRoute,
    data: &RequestData,
    timing: RequestTimeouts,
    official: Option<&OfficialRequestAuth>,
) -> std::result::Result<UpstreamResponse, ()> {
    let mut effective = route.clone();
    if let Some(auth) = official {
        effective.base_url = auth.base_url.clone();
    }
    let url = upstream_url(&effective, data).ok_or(())?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent("Astra/auto-failover");
    if reqwest::Url::parse(&url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|_| ())?;
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in data.client_headers.iter().chain(route.headers.iter()) {
        if !forbidden_upstream_header(name)
            && !name.eq_ignore_ascii_case(super::config::ROUTE_TOKEN_HEADER)
            && !name.eq_ignore_ascii_case(super::config::ROUTE_GENERATION_HEADER)
        {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| ())?,
                reqwest::header::HeaderValue::from_str(value).map_err(|_| ())?,
            );
        }
    }
    if let Some(auth) = official {
        headers.remove("authorization");
        headers.remove("chatgpt-account-id");
        let mut value =
            reqwest::header::HeaderValue::from_str(&auth.authorization).map_err(|_| ())?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        if let Some(account) = &auth.account_id {
            headers.insert(
                "chatgpt-account-id",
                reqwest::header::HeaderValue::from_str(account).map_err(|_| ())?,
            );
        }
    } else if let Some(key) = route.api_key.as_deref().filter(|key| !key.is_empty()) {
        let mut value =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| ())?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static(if data.streaming {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    let request = if data.model.is_none() {
        client.get(&url)
    } else {
        client.post(&url).body(data.bytes.clone())
    }
    .headers(headers);
    let started = Instant::now();
    let header_timeout = if data.streaming {
        timing.first_header
    } else {
        timing.non_streaming
    };
    let response = tauri::async_runtime::block_on(async {
        match header_timeout {
            Some(timeout) => tokio::time::timeout(timeout, request.send())
                .await
                .map_err(|_| ())?
                .map_err(|_| ()),
            None => request.send().await.map_err(|_| ()),
        }
    })?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Ok(UpstreamResponse {
        response,
        status,
        headers,
        deadline: if data.streaming {
            None
        } else {
            timing.non_streaming.map(|duration| started + duration)
        },
        next_timeout: if data.streaming {
            timing.first_chunk
        } else {
            None
        },
        pending: Vec::new(),
        offset: 0,
    })
}

type ResponseHeaders = Vec<(String, String)>;
fn downstream_headers(response: &UpstreamResponse) -> ResponseHeaders {
    [
        "content-type",
        "retry-after",
        "x-request-id",
        "request-id",
        "openai-processing-ms",
    ]
    .iter()
    .filter_map(|name| {
        let value = response.headers.get(*name)?.to_str().ok()?;
        (!value.bytes().any(|byte| byte.is_ascii_control()) && value.len() <= MAX_QUERY_BYTES)
            .then(|| ((*name).to_string(), value.to_owned()))
    })
    .collect()
}

struct BufferedResponse {
    status: u16,
    headers: ResponseHeaders,
    bytes: Vec<u8>,
}
fn buffered_response(mut response: UpstreamResponse) -> io::Result<BufferedResponse> {
    let status = response.status;
    let headers = downstream_headers(&response);
    let limit = if status >= 300 {
        MAX_ERROR_BYTES
    } else {
        MAX_BODY_BYTES
    };
    if response
        .response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "body limit exceeded",
        ));
    }
    Ok(BufferedResponse {
        status,
        headers,
        bytes: read_bounded(&mut response, limit)?,
    })
}

struct Permit {
    breaker: Option<Arc<CircuitBreaker>>,
    half_open: bool,
    completed: bool,
}
impl Permit {
    fn acquire(
        breakers: &HashMap<String, Arc<CircuitBreaker>>,
        route: &ProxyRoute,
        enabled: bool,
    ) -> Option<Self> {
        if !enabled || route.official.is_some() {
            return Some(Self {
                breaker: None,
                half_open: false,
                completed: false,
            });
        }
        let breaker = breakers.get(&route.id)?.clone();
        if !breaker.is_available() {
            return None;
        }
        let allowance = breaker.allow_request();
        allowance.allowed.then_some(Self {
            breaker: Some(breaker),
            half_open: allowance.used_half_open_permit,
            completed: false,
        })
    }
    fn success(&mut self) {
        if let Some(breaker) = &self.breaker {
            breaker.record_success(self.half_open);
        }
        self.completed = true;
    }
    fn failure(&mut self) {
        if let Some(breaker) = &self.breaker {
            breaker.record_failure(self.half_open);
        }
        self.completed = true;
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if !self.completed && self.half_open {
            if let Some(breaker) = &self.breaker {
                breaker.release_half_open_permit();
            }
        }
    }
}

fn set_attempt_failure(
    shared: &Shared,
    config: &Configuration,
    route: &ProxyRoute,
    permit: &mut Permit,
    status: Option<u16>,
    message: &'static str,
) {
    permit.failure();
    let current = shared
        .configuration
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let same = current
        .routes
        .iter()
        .any(|saved| saved.id == route.id && same_transport(saved, route));
    let mut stats = lock(&shared.statistics);
    if same {
        if let Some(health) = stats.health.get_mut(&route.id) {
            health.last_status = status;
        }
    }
    if current.revision == config.revision {
        stats.last_error = Some(status.map_or_else(
            || message.into(),
            |status| format!("{message}（HTTP {status}）"),
        ));
    }
}

fn set_success(
    shared: &Shared,
    config: &Configuration,
    route: &ProxyRoute,
    permit: &mut Permit,
    index: usize,
    status: u16,
) {
    permit.success();
    let current = shared
        .configuration
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let mut stats = lock(&shared.statistics);
    stats.success_count = stats.success_count.saturating_add(1);
    if current.revision != config.revision || shared.stopped.load(Ordering::Acquire) {
        return;
    }
    if let Some(health) = stats.health.get_mut(&route.id) {
        health.last_status = Some(status);
    }
    if index > 0 {
        stats.failover_count = stats.failover_count.saturating_add(1);
    }
    stats.last_provider_id = Some(route.id.clone());
    stats.last_error = None;
    drop(stats);
    drop(current);
    if route.official.is_none() {
        let callback = lock(&shared.selection_callback).clone();
        if let Some(callback) = callback {
            callback(ProxySelectionEvent {
                provider_id: route.id.clone(),
                revision: config.revision,
            });
        }
    }
}

fn fail_request(shared: &Shared) {
    let mut stats = lock(&shared.statistics);
    stats.failure_count = stats.failure_count.saturating_add(1);
}

fn handle_request(mut request: Request, shared: &Arc<Shared>) {
    let (config, breakers) = {
        let config = shared
            .configuration
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let stats = lock(&shared.statistics);
        (
            config.clone(),
            stats
                .health
                .iter()
                .map(|(id, health)| (id.clone(), health.breaker.clone()))
                .collect::<HashMap<_, _>>(),
        )
    };
    let official_auth = match authorize(&request, shared, &config) {
        Ok(auth) => auth,
        Err((status, message)) => {
            respond_error(request, status, message);
            return;
        }
    };
    let data = match request_data(&mut request) {
        Ok(data) => data,
        Err((status, message)) => {
            respond_error(request, status, message);
            return;
        }
    };
    {
        let mut stats = lock(&shared.statistics);
        stats.request_count = stats.request_count.saturating_add(1);
        stats.last_request_at = Some(chrono::Utc::now().to_rfc3339());
    }
    let official = config
        .routes
        .first()
        .is_some_and(|route| route.official.is_some());
    if official && data.model.is_none() {
        let route = &config.routes[0];
        match super::native_official::native_models(route.official.as_ref().unwrap()) {
            Ok(models) => {
                let mut permit = Permit {
                    breaker: None,
                    half_open: false,
                    completed: false,
                };
                set_success(shared, &config, route, &mut permit, 0, 200);
                respond_buffered(
                    request,
                    BufferedResponse {
                        status: 200,
                        headers: vec![("content-type".into(), "application/json".into())],
                        bytes: serde_json::to_vec(&models).unwrap_or_default(),
                    },
                );
            }
            Err(_) => {
                fail_request(shared);
                respond_error(request, 502, "Official model catalog is temporarily unavailable");
            }
        }
        return;
    }
    let automatic = config.options.auto_failover_enabled && !official;
    let max_attempts = if automatic {
        config.options.tuning.max_retries.saturating_add(1) as usize
    } else {
        1
    };
    let timing = timeouts(shared, &config.options, official);
    let mut attempted = 0;
    let mut last_response = None;
    for (index, route) in config.routes.iter().enumerate() {
        if shared.stopped.load(Ordering::Acquire) {
            fail_request(shared);
            respond_error(request, 503, "Local routing has stopped; please resend this request");
            return;
        }
        if attempted >= max_attempts {
            break;
        }
        if index > 0 && (!automatic || data.primary_only) {
            break;
        }
        let Some(mut permit) = Permit::acquire(&breakers, route, automatic) else {
            continue;
        };
        attempted += 1;
        let mut response = match request_upstream(route, &data, timing, official_auth.as_ref()) {
            Ok(response) => response,
            Err(()) => {
                set_attempt_failure(
                    shared,
                    &config,
                    route,
                    &mut permit,
                    None,
                    "Provider connection failed or timed out",
                );
                if official {
                    break;
                }
                continue;
            }
        };
        let status = response.status;
        if retryable_status(status) && !official {
            set_attempt_failure(
                shared,
                &config,
                route,
                &mut permit,
                Some(status),
                "Provider temporarily unavailable",
            );
            last_response = Some(response);
            continue;
        }
        if (300..=399).contains(&status) {
            set_attempt_failure(
                shared,
                &config,
                route,
                &mut permit,
                Some(status),
                "Provider returned an unsupported redirect",
            );
            fail_request(shared);
            respond_error(request, 502, "Provider returned a redirect; please check the API URL");
            return;
        }
        if !(200..=299).contains(&status) {
            // Client errors are neutral: the permit guard releases the probe,
            // but no provider-failure counters are incremented.
            fail_request(shared);
            if let Some(health) = lock(&shared.statistics).health.get_mut(&route.id) {
                health.last_status = Some(status);
            }
            match buffered_response(response) {
                Ok(response) => respond_buffered(request, response),
                Err(_) => respond_error(request, 502, "Unable to read provider response"),
            };
            return;
        }
        let streaming = data.streaming
            || response
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .eq_ignore_ascii_case("text/event-stream")
                });
        if streaming {
            let headers = downstream_headers(&response);
            let mut first = [0; 16 * 1024];
            let count = match response.read(&mut first) {
                Ok(0) | Err(_) => {
                    set_attempt_failure(
                        shared,
                        &config,
                        route,
                        &mut permit,
                        Some(status),
                        "Failed to read provider first byte or timed out",
                    );
                    if official {
                        break;
                    }
                    continue;
                }
                Ok(count) => count,
            };
            response.next_timeout = timing.idle;
            set_success(shared, &config, route, &mut permit, index, status);
            if matches!(
                stream_response(
                    request,
                    status,
                    headers,
                    &first[..count],
                    Box::new(response)
                ),
                StreamOutcome::UpstreamFailure
            ) {
                // Like CC Switch, after the committed first chunk this is a
                // downstream stream failure, never a fresh provider attempt.
                let mut stats = lock(&shared.statistics);
                stats.last_error = Some("Response interrupted or idle timeout exceeded; please retry this request".into());
            }
            return;
        }
        match buffered_response(response) {
            Ok(response) => {
                set_success(shared, &config, route, &mut permit, index, status);
                respond_buffered(request, response);
                return;
            }
            Err(_) => {
                set_attempt_failure(
                    shared,
                    &config,
                    route,
                    &mut permit,
                    Some(status),
                    "Failed to read full provider response or timed out",
                );
                if official {
                    break;
                }
            }
        }
    }
    fail_request(shared);
    if let Some(response) = last_response {
        match buffered_response(response) {
            Ok(response) => respond_buffered(request, response),
            Err(_) => respond_error(request, 502, "Unable to reach an available provider right now; please try again later"),
        }
    } else if attempted > 0 {
        respond_error(request, 502, "Unable to reach an available provider right now; please try again later");
    } else {
        respond_error(request, 503, "Queue is empty or providers are recovering from circuit break; please try again later");
    }
}

fn respond_error(request: Request, status: u16, message: &str) {
    respond_buffered(
        request,
        BufferedResponse {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
            bytes: serde_json::to_vec(
                &json!({"error": {"message": message, "type": "codex_x_failover_error"}}),
            )
            .unwrap_or_default(),
        },
    );
}

fn write_headers(
    writer: &mut dyn Write,
    status: u16,
    headers: ResponseHeaders,
    length: Option<usize>,
) -> io::Result<()> {
    // Do not forward hop-by-hop headers, Set-Cookie, compression, CORS or
    // Location. Every response is private, uncached, and a single connection.
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Response");
    write!(writer, "HTTP/1.1 {status} {reason}\r\n")?;
    for (name, value) in headers {
        write!(writer, "{name}: {value}\r\n")?;
    }
    write!(writer, "Cache-Control: no-store\r\nConnection: close\r\n")?;
    match length {
        Some(length) => write!(writer, "Content-Length: {length}\r\n")?,
        None => write!(
            writer,
            "Transfer-Encoding: chunked\r\nX-Accel-Buffering: no\r\n"
        )?,
    }
    write!(writer, "\r\n")
}

fn respond_buffered(request: Request, response: BufferedResponse) {
    let mut writer = request.into_writer();
    let _ = (|| -> io::Result<()> {
        write_headers(
            &mut writer,
            response.status,
            response.headers,
            Some(response.bytes.len()),
        )?;
        writer.write_all(&response.bytes)?;
        writer.flush()
    })();
}

enum StreamOutcome {
    Complete,
    UpstreamFailure,
    ClientClosed,
}

fn write_chunk(writer: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    write!(writer, "{:x}\r\n", bytes.len())?;
    writer.write_all(bytes)?;
    writer.write_all(b"\r\n")?;
    writer.flush()
}

fn stream_response(
    request: Request,
    status: u16,
    headers: ResponseHeaders,
    first: &[u8],
    mut reader: Box<dyn Read + Send + Sync>,
) -> StreamOutcome {
    let mut writer = request.into_writer();
    if write_headers(&mut writer, status, headers, None)
        .and_then(|()| write_chunk(&mut writer, first))
        .is_err()
    {
        return StreamOutcome::ClientClosed;
    }
    let mut buffer = [0; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return if writer
                    .write_all(b"0\r\n\r\n")
                    .and_then(|()| writer.flush())
                    .is_ok()
                {
                    StreamOutcome::Complete
                } else {
                    StreamOutcome::ClientClosed
                };
            }
            Ok(count) => {
                if write_chunk(&mut writer, &buffer[..count]).is_err() {
                    return StreamOutcome::ClientClosed;
                }
            }
            Err(_) => return StreamOutcome::UpstreamFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const TOKEN: &str = "test-only-local-token-0123456789abcdef";
    const MODEL: &str = "fixture-model";

    #[derive(Clone)]
    struct Observed {
        target: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    struct Upstream {
        url: String,
        calls: Arc<Mutex<Vec<Observed>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Upstream {
        fn serve(reply: impl Fn(&mut TcpStream) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&calls);
            let stopping = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let target = line.split_whitespace().nth(1).unwrap().to_owned();
                    let mut headers = HashMap::new();
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        let (name, value) = line.split_once(':').unwrap();
                        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                    }
                    let length = headers
                        .get("content-length")
                        .map(|value| value.parse::<usize>().unwrap())
                        .unwrap_or(0);
                    assert!(length <= MAX_BODY_BYTES);
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    lock(&observed).push(Observed {
                        target,
                        headers,
                        body,
                    });
                    reply(&mut stream);
                }
            });
            Self {
                url,
                calls,
                stop,
                thread: Some(thread),
            }
        }

        fn json(status: u16, body: &'static str) -> Self {
            Self::serve(move |stream| {
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: upstream-private-cookie\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            })
        }

        fn observed(&self) -> Vec<Observed> {
            lock(&self.calls).clone()
        }

        fn wait_for_calls(&self, count: usize) {
            eventually(|| self.observed().len() >= count);
        }
    }

    impl Drop for Upstream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let result = thread.join();
                if !thread::panicking() {
                    result.unwrap();
                }
            }
        }
    }

    fn eventually(mut condition: impl FnMut() -> bool) {
        let started = Instant::now();
        while !condition() {
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "condition did not become true"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn route(id: &str, upstream: &Upstream, key: &str) -> ProxyRoute {
        ProxyRoute {
            id: id.into(),
            name: id.into(),
            base_url: upstream.url.clone(),
            api_key: Some(key.into()),
            headers: Vec::new(),
            models: HashSet::from([MODEL.to_string()]),
            official: None,
        }
    }

    fn start(routes: Vec<ProxyRoute>) -> ProxyHandle {
        ProxyHandle::start_with_timeout(0, TOKEN.into(), routes, Duration::from_secs(2)).unwrap()
    }

    fn client() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs(5))
            .redirects(0)
            .build()
    }

    fn request(proxy: &ProxyHandle, body: &[u8]) -> ureq::Response {
        any_status(
            client()
                .post(&format!("http://127.0.0.1:{}/v1/responses", proxy.port()))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .set("Content-Type", "application/json")
                .send_bytes(body),
        )
    }

    fn any_status(result: std::result::Result<ureq::Response, ureq::Error>) -> ureq::Response {
        match result {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("local request failed: {error}"),
        }
    }

    fn payload() -> Vec<u8> {
        format!("{{\"model\":\"{MODEL}\",\"input\":\"fixture prompt\",\"stream\":false}}")
            .into_bytes()
    }

    fn stream_payload() -> Vec<u8> {
        serde_json::to_vec(&json!({"model":MODEL,"input":"fixture prompt","stream":true})).unwrap()
    }

    fn raw(proxy: &ProxyHandle, headers_and_body: &str) -> String {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(headers_and_body.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn upstream_failure_retries_with_unchanged_body_and_isolated_credentials() {
        let first = Upstream::json(503, r#"{"error":{"message":"fixture outage"}}"#);
        let second = Upstream::json(200, r#"{"id":"response-from-backup"}"#);
        let mut primary = route("primary", &first, "primary-secret");
        primary.base_url.push_str("/api/v1?tenant=one");
        primary.headers = vec![("x-provider-header".into(), "primary-only".into())];
        primary.headers.push((
            "X-Codex-X-Route-Token".into(),
            "saved-local-route-token".into(),
        ));
        primary.headers.push((
            "X-Codex-X-Route-Generation".into(),
            "saved-local-generation".into(),
        ));
        let mut backup = route("backup", &second, "backup-secret");
        backup.headers = vec![("x-provider-header".into(), "backup-only".into())];
        let proxy = start(vec![primary, backup]);
        let body = payload();
        let response = any_status(
            client()
                .post(&format!(
                    "http://127.0.0.1:{}/v1/responses?api-version=fixture",
                    proxy.port()
                ))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .set("Content-Type", "application/json")
                .set("Cookie", "private-client-cookie")
                .set("X-API-Key", "private-client-api-key")
                .set("ChatGPT-Account-Id", "private-client-account")
                .set("User-Agent", "codex_cli_fixture/1.0")
                .set("OpenAI-Beta", "responses=fixture")
                .set("X-Codex-X-Route-Generation", "incoming-local-generation")
                .send_bytes(&body),
        );
        assert_eq!(response.status(), 200);
        assert!(response.header("set-cookie").is_none());
        assert!(response
            .into_string()
            .unwrap()
            .contains("response-from-backup"));
        let first_call = &first.observed()[0];
        let second_call = &second.observed()[0];
        assert_eq!(
            first_call.target,
            "/api/v1/responses?tenant=one&api-version=fixture"
        );
        assert_eq!(second_call.target, "/responses?api-version=fixture");
        for (call, key, marker) in [
            (first_call, "primary-secret", "primary-only"),
            (second_call, "backup-secret", "backup-only"),
        ] {
            assert_eq!(call.body, body);
            assert_eq!(
                call.headers.get("user-agent").map(String::as_str),
                Some("codex_cli_fixture/1.0")
            );
            assert_eq!(
                call.headers.get("openai-beta").map(String::as_str),
                Some("responses=fixture")
            );
            assert_eq!(
                call.headers.get("authorization"),
                Some(&format!("Bearer {key}"))
            );
            assert_eq!(
                call.headers.get("x-provider-header").map(String::as_str),
                Some(marker)
            );
            for forbidden in [
                "cookie",
                "x-api-key",
                "chatgpt-account-id",
                "origin",
                "x-codex-x-route-token",
                "x-codex-x-route-generation",
            ] {
                assert!(!call.headers.contains_key(forbidden));
            }
            assert!(!format!("{:?}", call.headers).contains(TOKEN));
        }
        eventually(|| proxy.snapshot().in_flight == 0);
        let snapshot = proxy.snapshot();
        assert_eq!(snapshot.request_count, 1);
        assert_eq!(snapshot.failover_count, 1);
        assert_eq!(snapshot.last_provider_id.as_deref(), Some("backup"));
        assert!(snapshot.last_error.is_none());
        assert_eq!(snapshot.providers[0].state, "open");
        assert_eq!(snapshot.providers[1].state, "closed");
        assert!(!serde_json::to_string(&snapshot).unwrap().contains("secret"));
    }

    #[test]
    fn transport_failure_uses_backup_and_only_retries_failure_statuses() {
        let unavailable = TcpListener::bind("127.0.0.1:0").unwrap();
        let unavailable_url = format!("http://{}", unavailable.local_addr().unwrap());
        drop(unavailable);
        let backup = Upstream::json(200, "{}");
        let mut primary = route("primary", &backup, "first-key");
        primary.base_url = unavailable_url;
        let proxy = start(vec![primary, route("backup", &backup, "backup-key")]);
        assert_eq!(request(&proxy, &payload()).status(), 200);
        assert_eq!(backup.observed().len(), 1);
        for status in [401, 403, 404, 408, 409, 429, 451, 500, 502, 503, 504, 599] {
            assert!(retryable_status(status), "{status}");
        }
        for status in [200, 301, 400, 405, 406, 413, 414, 415, 422, 501] {
            assert!(!retryable_status(status), "{status}");
        }
    }

    #[test]
    fn request_errors_are_returned_without_replaying_or_cooling_provider() {
        let primary = Upstream::json(400, r#"{"error":{"message":"unsupported parameter"}}"#);
        let backup = Upstream::json(200, "{}");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let response = request(&proxy, &payload());
        assert_eq!(response.status(), 400);
        assert!(response
            .into_string()
            .unwrap()
            .contains("unsupported parameter"));
        assert_eq!(primary.observed().len(), 1);
        assert!(backup.observed().is_empty());
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
    }

    #[test]
    fn model_catalog_does_not_restrict_responses_routing() {
        let primary = Upstream::json(503, "primary unavailable");
        let backup = Upstream::json(200, "backup accepted original model");
        let mut different = route("backup", &backup, "two");
        different.models = HashSet::from(["another-model".into()]);
        let proxy = start(vec![route("primary", &primary, "one"), different]);
        assert_eq!(request(&proxy, &payload()).status(), 200);
        assert_eq!(backup.observed()[0].body, payload());
        assert_eq!(request(&proxy, &payload()).status(), 200);
        assert_eq!(primary.observed().len(), 1);
        assert_eq!(backup.observed().len(), 2);
    }

    #[test]
    fn all_failed_routes_are_skipped_during_cooldown() {
        let primary = Upstream::json(401, "primary unavailable");
        let backup = Upstream::json(429, "backup unavailable");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        assert_eq!(request(&proxy, &payload()).status(), 429);
        assert_eq!(request(&proxy, &payload()).status(), 503);
        assert_eq!(primary.observed().len(), 1);
        assert_eq!(backup.observed().len(), 1);
        assert_eq!(proxy.snapshot().failover_count, 0);
    }

    #[test]
    fn requests_referencing_server_state_never_cross_provider_boundaries() {
        for value in [
            json!({"previous_response_id":"resp-private"}),
            json!({"conversation":"conv-private"}),
            json!({"background":true}),
            json!({"input":[{"type":"item_reference","id":"private"}]}),
            json!({"input":[{"role":"user","content":[{"type":"input_file","file_id":"private"}]}]}),
            json!({"input":[{"type":"reasoning","encrypted_content":"private"}]}),
        ] {
            assert!(contains_server_state(&value), "{value}");
        }
        assert!(!contains_server_state(
            &json!({"previous_response_id":null,"conversation":null,"background":false,"input":[{"role":"user","content":[{"type":"input_text","text":"ordinary history"}]}]})
        ));
        let primary = Upstream::json(503, "unavailable");
        let backup = Upstream::json(200, "{}");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let body = serde_json::to_vec(
            &json!({"model": MODEL,"input":"next","previous_response_id":"resp-private"}),
        )
        .unwrap();
        assert_eq!(request(&proxy, &body).status(), 503);
        assert!(backup.observed().is_empty());
    }

    #[test]
    fn malformed_browser_or_unauthenticated_requests_never_reach_upstream() {
        let upstream = Upstream::json(200, "{}");
        let proxy = start(vec![route("primary", &upstream, "secret")]);
        let host = format!("Host: 127.0.0.1:{}\r\n", proxy.port());
        let auth = format!("Authorization: Bearer {TOKEN}\r\n");
        let cases = [
            (format!("POST /v1/responses HTTP/1.1\r\n{host}Content-Length: 2\r\n\r\n{{}}"), 401),
            (format!("POST /v1/responses HTTP/1.1\r\nHost: attacker.invalid\r\n{auth}Content-Length: 2\r\n\r\n{{}}"), 403),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Origin: https://attacker.invalid\r\nContent-Length: 2\r\n\r\n{{}}"), 403),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}{auth}Content-Length: 2\r\n\r\n{{}}"), 401),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 1\r\n\r\n!"), 400),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n{{}}"), 400),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 2\r\nContent-Length: 3\r\n\r\n{{}}"), 400),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 18446744073709551615\r\n\r\n"), 413),
            (format!("POST /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 67108865\r\n\r\n"), 413),
            (format!("POST /admin HTTP/1.1\r\n{host}{auth}Content-Length: 0\r\n\r\n"), 404),
            (format!("DELETE /v1/responses HTTP/1.1\r\n{host}{auth}Content-Length: 0\r\n\r\n"), 405),
        ];
        for (message, expected) in cases {
            let response = raw(&proxy, &message);
            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected} ")),
                "{response}"
            );
        }
        let oversized = format!(
            "GET /v1/models HTTP/1.1\r\n{host}{auth}X-Padding: {}\r\n\r\n",
            "x".repeat(MAX_HEADERS_BYTES)
        );
        assert!(raw(&proxy, &oversized).starts_with("HTTP/1.1 431 "));
        assert!(upstream.observed().is_empty());
        assert_eq!(proxy.snapshot().request_count, 0);
    }

    #[test]
    fn chunked_requests_and_compact_route_preserve_original_json() {
        let upstream = Upstream::json(200, "{}");
        let mut provider = route("primary", &upstream, "secret");
        provider.base_url.push_str("/v1/");
        let proxy = start(vec![provider]);
        let body = String::from_utf8(payload()).unwrap();
        let message = format!("POST /v1/responses/compact HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", proxy.port(), body.len());
        assert!(raw(&proxy, &message).starts_with("HTTP/1.1 200 "));
        assert_eq!(upstream.observed()[0].body, body.as_bytes());
        assert_eq!(upstream.observed()[0].target, "/v1/responses/compact");
        let response = any_status(
            client()
                .get(&format!("http://127.0.0.1:{}/v1/models", proxy.port()))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .call(),
        );
        assert_eq!(response.status(), 200);
        assert_eq!(upstream.observed()[1].target, "/v1/models");
    }

    #[test]
    fn expect_continue_is_sent_only_after_valid_local_authentication() {
        let upstream = Upstream::json(200, "{}");
        let proxy = start(vec![route("primary", &upstream, "secret")]);
        let body = payload();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(stream, "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\nExpect: 100-continue\r\n\r\n", proxy.port(), body.len()).unwrap();
        let mut interim = [0; 25];
        stream.read_exact(&mut interim).unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        stream.write_all(&body).unwrap();
        let mut result = String::new();
        let read = stream.read_to_string(&mut result);
        assert!(
            read.is_ok(),
            "{read:?}, response: {result}, calls: {}",
            upstream.observed().len()
        );
        assert!(result.starts_with("HTTP/1.1 200 "));
        assert_eq!(upstream.observed()[0].body, body);
    }

    #[test]
    fn empty_stream_can_fall_back_before_any_output() {
        let primary = Upstream::serve(|stream| {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let backup = Upstream::serve(|stream| {
            let data = "data: {\"type\":\"response.completed\"}\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).unwrap();
        });
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let response = request(&proxy, &stream_payload());
        assert_eq!(response.status(), 200);
        assert!(response
            .into_string()
            .unwrap()
            .contains("response.completed"));
        assert_eq!(backup.observed().len(), 1);
        assert_eq!(proxy.snapshot().failover_count, 1);
    }

    #[test]
    fn interrupted_stream_is_never_replayed_after_output_started() {
        let primary = Upstream::serve(|stream| {
            let data = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"first\"}\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{data}\r\n", data.len()).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(80));
            // Missing terminal chunk: connection fails after visible content.
        });
        let backup = Upstream::json(200, "unexpected replay");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let response = request(&proxy, &stream_payload());
        assert_eq!(response.status(), 200);
        let mut partial = String::new();
        assert!(response.into_reader().read_to_string(&mut partial).is_err());
        assert!(partial.contains("first"));
        assert!(backup.observed().is_empty());
        eventually(|| proxy.snapshot().in_flight == 0);
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
        assert_eq!(proxy.snapshot().failover_count, 0);
    }

    #[test]
    fn route_updates_keep_in_flight_queue_snapshot() {
        let (release, wait) = mpsc::channel();
        let primary = Upstream::serve(move |stream| {
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let old_backup = Upstream::json(200, "old queue");
        let new_backup = Upstream::json(200, "new queue");
        let primary_route = route("primary", &primary, "one");
        let proxy = Arc::new(start(vec![
            primary_route.clone(),
            route("old", &old_backup, "old-key"),
        ]));
        let pending_proxy = Arc::clone(&proxy);
        let pending =
            thread::spawn(move || request(&pending_proxy, &payload()).into_string().unwrap());
        primary.wait_for_calls(1);
        proxy
            .set_routes(vec![primary_route, route("new", &new_backup, "new-key")])
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(pending.join().unwrap(), "old queue");
        assert_eq!(
            request(&proxy, &payload()).into_string().unwrap(),
            "new queue"
        );
        assert_eq!(old_backup.observed().len(), 1);
        assert_eq!(new_backup.observed().len(), 1);
        assert!(!proxy
            .snapshot()
            .providers
            .iter()
            .any(|provider| provider.id == "old"));
    }

    #[test]
    fn stopping_releases_port_without_truncating_existing_stream() {
        let (release, wait) = mpsc::channel();
        let primary = Upstream::serve(move |stream| {
            let first = "data: first\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{first}\r\n", first.len()).unwrap();
            stream.flush().unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            let last = "data: last\n\n";
            write!(stream, "{:x}\r\n{last}\r\n0\r\n\r\n", last.len()).unwrap();
        });
        let proxy = start(vec![route("primary", &primary, "one")]);
        let response = request(&proxy, &stream_payload());
        let mut reader = response.into_reader();
        let mut first = [0; 13];
        reader.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"data: first\n\n");
        let started = Instant::now();
        proxy.shutdown();
        assert!(started.elapsed() < Duration::from_millis(500));
        // Windows can delay a refused connect beyond this fixture's 2-second
        // stream-idle timeout. Bound the probe while the upstream is paused so
        // it tests listener shutdown rather than causing an unrelated timeout.
        let stopped_address = SocketAddr::new(proxy.listen_address(), proxy.port());
        assert!(TcpStream::connect_timeout(&stopped_address, Duration::from_millis(100)).is_err());
        release.send(()).unwrap();
        let mut last = String::new();
        reader.read_to_string(&mut last).unwrap();
        assert_eq!(last, "data: last\n\n");
        eventually(|| proxy.snapshot().in_flight == 0);
    }

    #[test]
    fn redirects_do_not_receive_provider_credentials_at_another_origin() {
        let target = Upstream::json(200, "should not be called");
        let target_url = target.url.clone();
        let primary = Upstream::serve(move |stream| {
            write!(stream, "HTTP/1.1 307 Redirect\r\nLocation: {target_url}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let proxy = start(vec![route("primary", &primary, "private-token")]);
        assert_eq!(request(&proxy, &payload()).status(), 502);
        assert!(target.observed().is_empty());
    }

    #[test]
    fn invalid_and_recursive_routes_are_rejected_and_buffers_are_bounded() {
        let upstream = Upstream::json(200, "{}");
        let primary = route("primary", &upstream, "one");
        let proxy = start(vec![primary.clone()]);
        let mut recursive = route("backup", &upstream, "two");
        recursive.base_url = format!("http://127.0.0.1:{}/v1", proxy.port());
        assert!(proxy.set_routes(vec![primary.clone(), recursive]).is_err());
        let mut credential_url = primary.clone();
        credential_url.base_url = "http://private:password@example.invalid/v1".into();
        assert!(validate_routes(&[credential_url]).is_err());
        assert!(proxy
            .set_routes(vec![route("other-primary", &upstream, "two")])
            .is_ok());
        let mut bytes = io::Cursor::new(vec![0; 9]);
        assert_eq!(
            read_bounded(&mut bytes, 8).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        proxy.shutdown();
        assert!(proxy.set_routes(vec![primary]).is_err());
    }

    #[test]
    fn sse_timeout_is_per_read_not_the_whole_stream() {
        let primary = Upstream::serve(|stream| {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            for _ in 0..8 {
                let data = b"data: still working\n\n";
                write!(stream, "{:x}\r\n", data.len()).unwrap();
                stream.write_all(data).unwrap();
                stream.write_all(b"\r\n").unwrap();
                stream.flush().unwrap();
                thread::sleep(Duration::from_millis(150));
            }
            stream.write_all(b"0\r\n\r\n").unwrap();
        });
        let idle_timeout = Duration::from_millis(500);
        let proxy = ProxyHandle::start_with_timeout(
            0,
            TOKEN.into(),
            vec![route("primary", &primary, "one")],
            idle_timeout,
        )
        .unwrap();
        let started = Instant::now();
        let body = request(&proxy, &stream_payload()).into_string().unwrap();
        assert!(started.elapsed() > idle_timeout * 2);
        assert_eq!(body, "data: still working\n\n".repeat(8));
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
    }

    #[test]
    fn empty_idle_stream_times_out_before_falling_back() {
        let primary = Upstream::serve(|stream| {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(350));
        });
        let backup = Upstream::json(200, "backup recovered");
        let proxy = ProxyHandle::start_with_timeout(
            0,
            TOKEN.into(),
            vec![
                route("primary", &primary, "one"),
                route("backup", &backup, "two"),
            ],
            Duration::from_millis(150),
        )
        .unwrap();
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "backup recovered"
        );
        assert_eq!(proxy.snapshot().failover_count, 1);
    }

    #[test]
    fn failed_status_does_not_wait_for_error_body_before_fallback() {
        let primary = Upstream::serve(|stream| {
            stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Type: application/json\r\nContent-Length: 999999\r\nConnection: close\r\n\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(600));
        });
        let backup = Upstream::json(200, "backup recovered");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let started = Instant::now();
        assert_eq!(
            request(&proxy, &payload()).into_string().unwrap(),
            "backup recovered"
        );
        assert!(started.elapsed() < Duration::from_millis(450));
    }

    #[test]
    fn changing_credentials_resets_cooldown_but_refreshing_same_routes_does_not() {
        let upstream = Upstream::json(401, "bad fixture key");
        let original = route("primary", &upstream, "old-key");
        let proxy = start(vec![original.clone()]);
        assert_eq!(request(&proxy, &payload()).status(), 401);
        assert_eq!(proxy.snapshot().providers[0].state, "open");
        proxy.set_routes(vec![original.clone()]).unwrap();
        assert_eq!(proxy.snapshot().providers[0].state, "open");
        let old_breaker = lock(&proxy.shared.statistics).health["primary"]
            .breaker
            .clone();
        let mut changed = original.clone();
        changed.api_key = Some("corrected-key".into());
        proxy.set_routes(vec![changed]).unwrap();
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
        assert_eq!(proxy.snapshot().providers[0].last_status, None);
        // An old in-flight request must not put the corrected key back on cooldown.
        old_breaker.record_failure(false);
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
    }

    #[test]
    fn shutdown_prevents_new_backup_attempts_for_already_pending_requests() {
        let (release, wait) = mpsc::channel();
        let primary = Upstream::serve(move |stream| {
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let backup = Upstream::json(200, "must not start");
        let proxy = Arc::new(start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]));
        let pending_proxy = Arc::clone(&proxy);
        let pending =
            thread::spawn(move || request(&pending_proxy, &payload()).into_string().unwrap());
        primary.wait_for_calls(1);
        proxy.shutdown();
        release.send(()).unwrap();
        assert!(pending.join().unwrap().contains("Stopped"));
        assert!(backup.observed().is_empty());
    }

    #[test]
    fn opaque_turn_state_remains_on_primary_and_provider_headers_override_client_metadata() {
        let primary = Upstream::json(503, "temporarily unavailable");
        let backup = Upstream::json(200, "must not cross account state");
        let mut provider = route("primary", &primary, "one");
        provider
            .headers
            .push(("originator".into(), "provider-required-client".into()));
        let proxy = start(vec![provider, route("backup", &backup, "two")]);
        let response = any_status(
            client()
                .post(&format!("http://127.0.0.1:{}/v1/responses", proxy.port()))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .set("X-Codex-Turn-State", "opaque-primary-state")
                .set("originator", "original-codex")
                .send_bytes(&payload()),
        );
        assert_eq!(response.status(), 503);
        let observed = &primary.observed()[0];
        assert_eq!(
            observed
                .headers
                .get("x-codex-turn-state")
                .map(String::as_str),
            Some("opaque-primary-state")
        );
        assert_eq!(
            observed.headers.get("originator").map(String::as_str),
            Some("provider-required-client")
        );
        assert!(backup.observed().is_empty());
    }

    fn configurable(
        routes: Vec<ProxyRoute>,
        tuning: RoutingTuning,
        automatic: bool,
        unit: Duration,
    ) -> ProxyHandle {
        let proxy = ProxyHandle::start(
            "127.0.0.1".parse().unwrap(),
            0,
            TOKEN.into(),
            routes,
            ProxyOptions {
                auto_failover_enabled: automatic,
                tuning,
            },
        )
        .unwrap();
        *lock(&proxy.shared.timing_unit) = unit;
        proxy
    }

    #[test]
    fn retry_limit_controls_attempted_providers_and_disabled_auto_uses_only_p1() {
        let a = Upstream::json(503, "a unavailable");
        let b = Upstream::json(502, "b unavailable");
        let c = Upstream::json(200, "c works");
        let routes = vec![
            route("a", &a, "a-key"),
            route("b", &b, "b-key"),
            route("c", &c, "c-key"),
        ];
        let tuning = RoutingTuning {
            max_retries: 0,
            circuit_failure_threshold: 20,
            ..Default::default()
        };
        let proxy = configurable(routes.clone(), tuning, true, Duration::from_millis(50));
        assert_eq!(request(&proxy, &payload()).status(), 503);
        assert_eq!(
            (a.observed().len(), b.observed().len(), c.observed().len()),
            (1, 0, 0)
        );
        proxy
            .configure(
                routes.clone(),
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        max_retries: 1,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(request(&proxy, &payload()).status(), 502);
        assert_eq!(
            (a.observed().len(), b.observed().len(), c.observed().len()),
            (2, 1, 0)
        );
        proxy
            .configure(
                routes.clone(),
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        max_retries: 2,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(request(&proxy, &payload()).status(), 200);
        assert_eq!(
            (a.observed().len(), b.observed().len(), c.observed().len()),
            (3, 2, 1)
        );
        proxy
            .configure(
                routes,
                ProxyOptions {
                    auto_failover_enabled: false,
                    tuning: RoutingTuning {
                        max_retries: 10,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(request(&proxy, &payload()).status(), 503);
        assert_eq!(
            (a.observed().len(), b.observed().len(), c.observed().len()),
            (4, 2, 1)
        );
        let stats = proxy.snapshot();
        assert_eq!(
            (
                stats.request_count,
                stats.success_count,
                stats.failure_count
            ),
            (4, 1, 3)
        );
        assert!(stats.last_request_at.is_some());
    }

    #[test]
    fn open_routes_do_not_consume_retry_budget_and_single_route_respects_breaker() {
        let a = Upstream::json(503, "a unavailable");
        let b = Upstream::json(503, "b unavailable");
        let c = Upstream::json(200, "c works");
        let routes = vec![
            route("a", &a, "a-key"),
            route("b", &b, "b-key"),
            route("c", &c, "c-key"),
        ];
        let tuning = RoutingTuning {
            max_retries: 1,
            circuit_failure_threshold: 1,
            ..Default::default()
        };
        let proxy = configurable(routes.clone(), tuning, true, Duration::from_secs(1));
        assert_eq!(request(&proxy, &payload()).status(), 503);
        proxy
            .configure(
                routes,
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        max_retries: 0,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(request(&proxy, &payload()).status(), 200);
        assert_eq!(
            (a.observed().len(), b.observed().len(), c.observed().len()),
            (1, 1, 1)
        );
        let single = configurable(
            vec![route("only", &a, "a-key")],
            tuning,
            true,
            Duration::from_secs(1),
        );
        assert_eq!(request(&single, &payload()).status(), 503);
        assert_eq!(request(&single, &payload()).status(), 503);
        assert_eq!(a.observed().len(), 2);
        assert_eq!(single.snapshot().providers[0].state, "open");
        single.reset_breakers(Some("only"));
        assert_eq!(single.snapshot().providers[0].state, "closed");
        assert_eq!(request(&single, &payload()).status(), 503);
        assert_eq!(a.observed().len(), 3);
        proxy
            .configure(
                Vec::new(),
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning,
                },
            )
            .unwrap();
        assert_eq!(request(&proxy, &payload()).status(), 503);
    }

    #[test]
    fn queue_order_and_options_change_revision_and_stale_requests_cannot_select_provider() {
        let (release, wait) = mpsc::channel();
        let a = Upstream::serve(move |stream| {
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let b = Upstream::json(200, "b selected");
        let routes = vec![route("a", &a, "a-key"), route("b", &b, "b-key")];
        let proxy = Arc::new(configurable(
            routes.clone(),
            RoutingTuning::default(),
            true,
            Duration::from_secs(1),
        ));
        let selected = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&selected);
        proxy.set_selection_callback(Arc::new(move |event| lock(&observed).push(event)));
        let first_revision = proxy.revision();
        proxy
            .configure(
                routes.clone(),
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning::default(),
                },
            )
            .unwrap();
        assert_eq!(proxy.revision(), first_revision);
        let pending_proxy = Arc::clone(&proxy);
        let pending =
            thread::spawn(move || request(&pending_proxy, &payload()).into_string().unwrap());
        a.wait_for_calls(1);
        proxy
            .configure(
                vec![routes[1].clone(), routes[0].clone()],
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning::default(),
                },
            )
            .unwrap();
        assert!(proxy.revision() > first_revision);
        release.send(()).unwrap();
        assert_eq!(pending.join().unwrap(), "{}");
        assert!(lock(&selected).is_empty());
        assert_eq!(
            request(&proxy, &payload()).into_string().unwrap(),
            "b selected"
        );
        assert_eq!(lock(&selected)[0].provider_id, "b");
        assert_eq!(lock(&selected)[0].revision, proxy.revision());
    }

    #[test]
    fn first_header_and_first_chunk_timeouts_have_separate_budgets() {
        let primary = Upstream::serve(|stream| {
            thread::sleep(Duration::from_millis(130));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 12\r\nConnection: close\r\n\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(130));
            let _ = stream.write_all(b"data: okay\n\n");
        });
        let tuning = RoutingTuning {
            streaming_first_byte_timeout: 1,
            ..Default::default()
        };
        let proxy = configurable(
            vec![route("primary", &primary, "one")],
            tuning,
            true,
            Duration::from_millis(220),
        );
        let started = Instant::now();
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "data: okay\n\n"
        );
        assert!(started.elapsed() > Duration::from_millis(220));
    }

    #[test]
    fn first_header_timeout_falls_back_and_larger_value_allows_slow_headers() {
        let slow = Upstream::serve(|stream| {
            thread::sleep(Duration::from_millis(200));
            let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 12\r\nConnection: close\r\n\r\ndata: okay\n\n");
        });
        let backup = Upstream::json(200, "backup");
        let tuning = RoutingTuning {
            streaming_first_byte_timeout: 1,
            ..Default::default()
        };
        let routes = vec![route("slow", &slow, "one"), route("backup", &backup, "two")];
        let proxy = configurable(routes.clone(), tuning, true, Duration::from_millis(120));
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "backup"
        );
        proxy
            .configure(
                routes,
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        streaming_first_byte_timeout: 4,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "data: okay\n\n"
        );
    }

    #[test]
    fn idle_timeout_zero_allows_gaps_and_nonzero_stops_without_replay() {
        let slow = Upstream::serve(|stream| {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nc\r\ndata: first\n\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(240));
            let _ = stream.write_all(b"b\r\ndata: last\n\r\n0\r\n\r\n");
        });
        let backup = Upstream::json(200, "must not replay");
        let tuning = RoutingTuning {
            streaming_first_byte_timeout: 5,
            streaming_idle_timeout: 1,
            ..Default::default()
        };
        let routes = vec![route("slow", &slow, "one"), route("backup", &backup, "two")];
        let proxy = configurable(routes.clone(), tuning, true, Duration::from_millis(100));
        let mut body = String::new();
        assert!(request(&proxy, &stream_payload())
            .into_reader()
            .read_to_string(&mut body)
            .is_err());
        assert!(body.contains("first"));
        assert!(backup.observed().is_empty());
        proxy
            .configure(
                routes,
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        streaming_idle_timeout: 0,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "data: first\ndata: last\n"
        );
        assert!(backup.observed().is_empty());
    }

    #[test]
    fn nonstream_timeout_covers_headers_and_entire_body_instead_of_resetting_per_chunk() {
        let slow = Upstream::serve(|stream| {
            thread::sleep(Duration::from_millis(140));
            if stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\npart")
                .is_err()
            {
                return;
            }
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(240));
            let _ = stream.write_all(b"done");
        });
        let backup = Upstream::json(200, "backup");
        let tuning = RoutingTuning {
            non_streaming_timeout: 60,
            ..Default::default()
        };
        let routes = vec![route("slow", &slow, "one"), route("backup", &backup, "two")];
        let proxy = configurable(routes.clone(), tuning, true, Duration::from_millis(5));
        assert_eq!(request(&proxy, &payload()).into_string().unwrap(), "backup");
        proxy
            .configure(
                routes,
                ProxyOptions {
                    auto_failover_enabled: true,
                    tuning: RoutingTuning {
                        non_streaming_timeout: 120,
                        ..tuning
                    },
                },
            )
            .unwrap();
        assert_eq!(
            request(&proxy, &payload()).into_string().unwrap(),
            "partdone"
        );
    }

    #[test]
    fn wildcard_listener_requires_valid_host_and_token_and_ipv6_urls_work() {
        let upstream = Upstream::json(200, "{}");
        let options = ProxyOptions {
            auto_failover_enabled: false,
            tuning: RoutingTuning::default(),
        };
        let proxy = ProxyHandle::start(
            "0.0.0.0".parse().unwrap(),
            0,
            TOKEN.into(),
            vec![route("one", &upstream, "one")],
            options.clone(),
        )
        .unwrap();
        assert_eq!(proxy.listen_address().to_string(), "0.0.0.0");
        assert_eq!(request(&proxy, &payload()).status(), 200);
        let bad=format!("GET /v1/models HTTP/1.1\r\nHost: evil.example\r\nAuthorization: Bearer {TOKEN}\r\n\r\n");
        assert!(raw(&proxy, &bad).starts_with("HTTP/1.1 403"));
        let ipv6 = match ProxyHandle::start(
            "::1".parse().unwrap(),
            0,
            TOKEN.into(),
            vec![route("one", &upstream, "one")],
            options,
        ) {
            Ok(proxy) => proxy,
            Err(_) => return,
        };
        let response = any_status(
            client()
                .post(&format!("http://[::1]:{}/v1/responses", ipv6.port()))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .send_bytes(&payload()),
        );
        assert_eq!(response.status(), 200);
    }

    #[test]
    fn half_open_allows_one_probe_and_needs_the_configured_success_count() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = Arc::clone(&calls);
        let (release, wait) = mpsc::channel();
        let primary = Upstream::serve(move |stream| {
            let call = served.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                return;
            }
            if call == 1 {
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let backup = Upstream::json(200, "backup");
        let tuning = RoutingTuning {
            circuit_failure_threshold: 1,
            circuit_success_threshold: 2,
            circuit_timeout_seconds: 0,
            ..Default::default()
        };
        let proxy = Arc::new(configurable(
            vec![
                route("primary", &primary, "one"),
                route("backup", &backup, "two"),
            ],
            tuning,
            true,
            Duration::from_secs(1),
        ));
        assert_eq!(request(&proxy, &payload()).into_string().unwrap(), "backup");
        let probing = Arc::clone(&proxy);
        let pending = thread::spawn(move || request(&probing, &payload()).into_string().unwrap());
        primary.wait_for_calls(2);
        assert_eq!(proxy.snapshot().providers[0].state, "half_open");
        assert_eq!(request(&proxy, &payload()).into_string().unwrap(), "backup");
        assert_eq!(primary.observed().len(), 2);
        release.send(()).unwrap();
        assert_eq!(pending.join().unwrap(), "{}");
        assert_eq!(proxy.snapshot().providers[0].state, "half_open");
        assert_eq!(proxy.snapshot().providers[0].consecutive_successes, 1);
        assert_eq!(request(&proxy, &payload()).into_string().unwrap(), "{}");
        assert_eq!(proxy.snapshot().providers[0].state, "closed");
        assert_eq!(proxy.snapshot().providers[0].total_requests, 0);
    }

    #[test]
    fn official_route_rejects_api_credentials_and_cannot_form_a_fallback_queue() {
        let backup = Upstream::json(200, "must never receive official authentication");
        let mut official = route("official:fixture", &backup, "unused");
        official.official = Some(OfficialRouteSpec {
            codex_dir: std::env::temp_dir().join(format!(
                "codex-x-nonexistent-official-{}",
                std::process::id()
            )),
            profile_id: "fixture".into(),
        });
        assert!(validate_routes(&[official.clone(), route("backup", &backup, "two")]).is_err());
        let proxy = configurable(
            vec![official],
            RoutingTuning::default(),
            true,
            Duration::from_secs(1),
        );
        assert_eq!(request(&proxy, &payload()).status(), 401);
        let response = any_status(
            client()
                .post(&format!("http://127.0.0.1:{}/v1/responses", proxy.port()))
                .set("x-codex-x-route-token", TOKEN)
                .set("Authorization", "Bearer fake-stale-official-token")
                .send_bytes(&payload()),
        );
        assert_eq!(response.status(), 401);
        assert!(backup.observed().is_empty());
        assert_eq!(proxy.snapshot().failover_count, 0);
    }

    #[test]
    fn compressed_codex_requests_are_bounded_and_keep_uncompressed_json_bytes() {
        let primary = Upstream::json(503, "unavailable");
        let backup = Upstream::json(200, "{}");
        let proxy = start(vec![
            route("primary", &primary, "one"),
            route("backup", &backup, "two"),
        ]);
        let body =
            format!("{{ \"model\" : \"{MODEL}\", \"input\":\"a compressed Codex request\" }}")
                .into_bytes();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&body).unwrap();
        let gzip = gzip.finish().unwrap();
        let zstd = zstd::stream::encode_all(io::Cursor::new(&body), 0).unwrap();
        let stacked = zstd::stream::encode_all(io::Cursor::new(&gzip), 0).unwrap();
        for (encoding, encoded) in [("gzip", gzip), ("zstd", zstd), ("gzip, zstd", stacked)] {
            proxy.reset_breakers(None);
            let response = any_status(
                client()
                    .post(&format!("http://127.0.0.1:{}/v1/responses", proxy.port()))
                    .set("Authorization", &format!("Bearer {TOKEN}"))
                    .set("Content-Type", "application/json")
                    .set("Content-Encoding", encoding)
                    .send_bytes(&encoded),
            );
            assert_eq!(response.status(), 200);
            assert_eq!(primary.observed().last().unwrap().body, body);
            assert_eq!(backup.observed().last().unwrap().body, body);
            assert!(!backup
                .observed()
                .last()
                .unwrap()
                .headers
                .contains_key("content-encoding"));
        }
        let expanded = vec![b'x'; 1024];
        let compressed = zstd::stream::encode_all(io::Cursor::new(expanded), 0).unwrap();
        assert!(compressed.len() < 64);
        assert_eq!(
            decode_content_encoding("zstd", compressed, 64)
                .unwrap_err()
                .kind(),
            io::ErrorKind::FileTooLarge
        );
        let malformed = any_status(
            client()
                .post(&format!("http://127.0.0.1:{}/v1/responses", proxy.port()))
                .set("Authorization", &format!("Bearer {TOKEN}"))
                .set("Content-Encoding", "gzip")
                .send_bytes(b"invalid gzip"),
        );
        assert_eq!(malformed.status(), 400);
        assert_eq!(backup.observed().len(), 3);
    }

    #[test]
    fn routing_without_auto_failover_bypasses_configured_stream_timeouts() {
        let upstream = Upstream::serve(|stream| {
            thread::sleep(Duration::from_millis(180));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nc\r\ndata: first\n\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(180));
            let _ = stream.write_all(b"b\r\ndata: last\n\r\n0\r\n\r\n");
        });
        let tuning = RoutingTuning {
            streaming_first_byte_timeout: 1,
            streaming_idle_timeout: 1,
            ..Default::default()
        };
        let proxy = configurable(
            vec![route("current", &upstream, "key")],
            tuning,
            false,
            Duration::from_millis(100),
        );
        assert_eq!(
            request(&proxy, &stream_payload()).into_string().unwrap(),
            "data: first\ndata: last\n"
        );
    }
}
