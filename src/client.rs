use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;
use zeroize::Zeroizing;

use crate::config::ResolvedCredential;
use crate::error::{AppError, ErrorCode, ExitClass, OperationOutcome, RetrySafety};

pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteEndpoint {
    CreateIssue,
    CreateProject,
    UpdateIssue,
    AddComment,
    TransitionIssue,
    AssignIssue,
    AddIssueLink,
    DeleteIssue,
    RemoveIssueLink,
    AddRemoteLink,
    RemoveRemoteLink,
    AddWorklog,
    UpdateWorklog,
    RemoveWorklog,
    AddEpicIssues,
    RemoveEpicIssues,
    AddSprintIssues,
    CloseSprint,
    AddInternalComment,
    AddWatcher,
    RemoveWatcher,
}

impl WriteEndpoint {
    pub const fn method(self) -> HttpMethod {
        match self {
            Self::CreateIssue
            | Self::CreateProject
            | Self::AddComment
            | Self::TransitionIssue
            | Self::AddIssueLink
            | Self::AddRemoteLink
            | Self::AddWorklog
            | Self::AddEpicIssues
            | Self::RemoveEpicIssues
            | Self::AddSprintIssues
            | Self::AddInternalComment
            | Self::AddWatcher => HttpMethod::Post,
            Self::UpdateIssue | Self::AssignIssue | Self::UpdateWorklog | Self::CloseSprint => {
                HttpMethod::Put
            }
            Self::DeleteIssue
            | Self::RemoveIssueLink
            | Self::RemoveRemoteLink
            | Self::RemoveWorklog
            | Self::RemoveWatcher => HttpMethod::Delete,
        }
    }

    pub const fn success_status(self) -> u16 {
        match self {
            Self::CreateIssue
            | Self::CreateProject
            | Self::AddComment
            | Self::AddIssueLink
            | Self::AddRemoteLink
            | Self::AddWorklog
            | Self::AddInternalComment => 201,
            Self::UpdateWorklog | Self::CloseSprint => 200,
            Self::UpdateIssue
            | Self::TransitionIssue
            | Self::AssignIssue
            | Self::DeleteIssue
            | Self::RemoveIssueLink
            | Self::RemoveRemoteLink
            | Self::RemoveWorklog
            | Self::AddEpicIssues
            | Self::RemoveEpicIssues
            | Self::AddSprintIssues
            | Self::AddWatcher
            | Self::RemoveWatcher => 204,
        }
    }

    pub const fn accepts_success(self, status: u16) -> bool {
        match self {
            Self::RemoveIssueLink => matches!(status, 200 | 204),
            _ => status == self.success_status(),
        }
    }

    pub const fn is_definitive_rejection(self, status: u16) -> bool {
        match self {
            Self::CreateIssue => matches!(status, 400 | 401 | 403 | 422 | 429),
            Self::CreateProject => matches!(status, 400 | 401 | 403 | 422 | 429),
            Self::UpdateIssue => matches!(status, 400 | 401 | 403 | 404 | 409 | 422 | 429),
            Self::AddComment => matches!(status, 400 | 401 | 404 | 413 | 429),
            Self::TransitionIssue => {
                matches!(status, 400 | 401 | 404 | 409 | 413 | 422 | 429)
            }
            Self::AssignIssue => matches!(status, 400 | 401 | 403 | 404 | 422 | 429),
            Self::AddIssueLink => matches!(status, 400 | 401 | 403 | 404 | 413 | 422 | 429),
            Self::DeleteIssue
            | Self::RemoveIssueLink
            | Self::AddRemoteLink
            | Self::RemoveRemoteLink
            | Self::AddWorklog
            | Self::UpdateWorklog
            | Self::RemoveWorklog
            | Self::AddEpicIssues
            | Self::RemoveEpicIssues
            | Self::AddSprintIssues
            | Self::CloseSprint
            | Self::AddInternalComment => {
                matches!(status, 400 | 401 | 403 | 404 | 409 | 413 | 422 | 429)
            }
            Self::AddWatcher | Self::RemoveWatcher => {
                matches!(status, 400 | 401 | 403 | 404 | 422 | 429)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestEffect {
    Read,
    JiraWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchPhase {
    BeforeDispatch,
    DispatchStarted,
    ResponseStarted,
    Complete,
}

pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: Url,
    pub headers: BTreeMap<String, SecretString>,
    pub body: Vec<u8>,
    pub effect: RequestEffect,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportFailureKind {
    Timeout,
    Connection,
    Protocol,
    ResponseTooLarge,
}

#[derive(Clone, Debug)]
pub struct TransportFailure {
    pub kind: TransportFailureKind,
    pub phase: DispatchPhase,
    pub status: Option<u16>,
    rate_limit: Option<RateLimitMetadata>,
}

impl TransportFailure {
    pub const fn new(
        kind: TransportFailureKind,
        phase: DispatchPhase,
        status: Option<u16>,
    ) -> Self {
        Self {
            kind,
            phase,
            status,
            rate_limit: None,
        }
    }

    pub fn response_started_with_headers(
        kind: TransportFailureKind,
        status: u16,
        headers: &BTreeMap<String, String>,
    ) -> Self {
        Self {
            kind,
            phase: DispatchPhase::ResponseStarted,
            status: Some(status),
            rate_limit: Some(RateLimitMetadata::from_headers(headers)),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RateLimitMetadata {
    retry_after_ms: Option<u64>,
    reason: Option<String>,
}

impl RateLimitMetadata {
    fn from_headers(headers: &BTreeMap<String, String>) -> Self {
        Self {
            retry_after_ms: parse_retry_after_ms(headers),
            reason: headers
                .get("ratelimit-reason")
                .or_else(|| headers.get("x-ratelimit-reason"))
                .cloned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Classification {
    pub code: ErrorCode,
    pub outcome: OperationOutcome,
    pub retry_safety: RetrySafety,
    pub exit_class: ExitClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteFailure {
    BeforeDispatch(ErrorCode),
    Transport(TransportFailureKind, DispatchPhase, Option<u16>),
    HttpStatus(u16),
    InvalidSuccessBody(u16),
}

pub fn classify_write_failure(endpoint: WriteEndpoint, failure: WriteFailure) -> Classification {
    let (code, outcome, retry_safety) = match failure {
        WriteFailure::BeforeDispatch(code) => {
            (code, OperationOutcome::NotApplied, RetrySafety::Safe)
        }
        WriteFailure::Transport(kind, DispatchPhase::BeforeDispatch, _) => (
            transport_error_code(kind),
            OperationOutcome::NotApplied,
            RetrySafety::Safe,
        ),
        WriteFailure::Transport(_, _, Some(status)) if endpoint.accepts_success(status) => (
            ErrorCode::MutationResponseInvalid,
            OperationOutcome::Applied,
            RetrySafety::Unsafe,
        ),
        WriteFailure::Transport(_, _, Some(status)) if endpoint.is_definitive_rejection(status) => {
            (
                rejection_error_code(status),
                OperationOutcome::NotApplied,
                RetrySafety::Safe,
            )
        }
        WriteFailure::Transport(_, _, _) => (
            ErrorCode::MutationOutcomeUnknown,
            OperationOutcome::Unknown,
            RetrySafety::Unknown,
        ),
        WriteFailure::HttpStatus(status) if endpoint.is_definitive_rejection(status) => (
            rejection_error_code(status),
            OperationOutcome::NotApplied,
            RetrySafety::Safe,
        ),
        WriteFailure::HttpStatus(_) => (
            ErrorCode::MutationOutcomeUnknown,
            OperationOutcome::Unknown,
            RetrySafety::Unknown,
        ),
        WriteFailure::InvalidSuccessBody(status) if endpoint.accepts_success(status) => (
            ErrorCode::MutationResponseInvalid,
            OperationOutcome::Applied,
            RetrySafety::Unsafe,
        ),
        WriteFailure::InvalidSuccessBody(_) => (
            ErrorCode::MutationOutcomeUnknown,
            OperationOutcome::Unknown,
            RetrySafety::Unknown,
        ),
    };
    Classification {
        code,
        outcome,
        retry_safety,
        exit_class: code.exit_class(),
    }
}

const fn transport_error_code(kind: TransportFailureKind) -> ErrorCode {
    match kind {
        TransportFailureKind::Timeout => ErrorCode::Timeout,
        TransportFailureKind::Connection => ErrorCode::ConnectionFailed,
        TransportFailureKind::Protocol => ErrorCode::ResponseInvalid,
        TransportFailureKind::ResponseTooLarge => ErrorCode::ResponseTooLarge,
    }
}

const fn rejection_error_code(status: u16) -> ErrorCode {
    match status {
        401 => ErrorCode::AuthInvalid,
        403 => ErrorCode::Forbidden,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::Conflict,
        429 => ErrorCode::RateLimited,
        _ => ErrorCode::RemoteRejected,
    }
}

pub trait JiraTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure>;
}

impl<T: JiraTransport + ?Sized> JiraTransport for &T {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        (*self).execute(request)
    }
}

pub struct UreqTransport;

impl JiraTransport for UreqTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        execute_ureq(request, true)
    }
}

#[cfg(jira_ops_hierarchy_test)]
pub(crate) struct HierarchyTestTransport {
    loopback_origin: Url,
}

#[cfg(jira_ops_hierarchy_test)]
impl HierarchyTestTransport {
    pub(crate) fn from_process() -> Self {
        const SERVER_ENV: &str = "JIRA_OPS_HIERARCHY_TEST_SERVER";
        let value = std::env::var_os(SERVER_ENV)
            .unwrap_or_else(|| panic!("{SERVER_ENV} is required under hierarchy test cfg"))
            .into_string()
            .unwrap_or_else(|_| panic!("{SERVER_ENV} must be UTF-8"));
        let origin =
            Url::parse(&value).unwrap_or_else(|_| panic!("{SERVER_ENV} must be a valid http URL"));
        let loopback = match origin.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(_)) | None => false,
        };
        assert!(
            origin.scheme() == "http"
                && loopback
                && origin.port().is_some_and(|port| port != 0)
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "{SERVER_ENV} must be an IP-literal loopback http origin with an explicit port"
        );
        Self {
            loopback_origin: origin,
        }
    }
}

#[cfg(jira_ops_hierarchy_test)]
impl JiraTransport for HierarchyTestTransport {
    fn execute(&self, mut request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        const GATEWAY_PREFIX: &str = "/ex/jira/00000000-0000-0000-0000-000000000000/";
        let valid_gateway = request.url.scheme() == "https"
            && request.url.host_str() == Some("api.atlassian.com")
            && request.url.port().is_none()
            && request.url.username().is_empty()
            && request.url.password().is_none()
            && request.url.path().starts_with(GATEWAY_PREFIX);
        if !valid_gateway {
            return Err(TransportFailure::new(
                TransportFailureKind::Protocol,
                DispatchPhase::BeforeDispatch,
                None,
            ));
        }
        request
            .url
            .set_scheme("http")
            .expect("validated gateway accepts http scheme");
        request
            .url
            .set_host(self.loopback_origin.host_str())
            .expect("validated IP-literal loopback host");
        request
            .url
            .set_port(self.loopback_origin.port())
            .expect("validated explicit loopback port");
        execute_ureq(request, false)
    }
}

fn execute_ureq(request: HttpRequest, https_only: bool) -> Result<HttpResponse, TransportFailure> {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let config = ureq::Agent::config_builder()
        .https_only(https_only)
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(request.timeout))
        .tls_config(tls)
        .build();
    let agent: ureq::Agent = config.into();

    let method = match request.method {
        HttpMethod::Get => ureq::http::Method::GET,
        HttpMethod::Post => ureq::http::Method::POST,
        HttpMethod::Put => ureq::http::Method::PUT,
        HttpMethod::Delete => ureq::http::Method::DELETE,
    };
    let mut builder = ureq::http::Request::builder()
        .method(method)
        .uri(request.url.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(name, value.expose_secret());
    }
    let no_body_success = request.effect == RequestEffect::JiraWrite;
    let request = builder.body(request.body).map_err(|_| {
        TransportFailure::new(
            TransportFailureKind::Protocol,
            DispatchPhase::BeforeDispatch,
            None,
        )
    })?;
    let mut response = agent
        .run(request)
        .map_err(|error| map_ureq_error(error, DispatchPhase::DispatchStarted, None))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_owned()))
        })
        .collect();
    if no_body_success && status == 204 {
        return Ok(HttpResponse {
            status,
            headers,
            body: Vec::new(),
        });
    }
    let body = response
        .body_mut()
        .with_config()
        .limit((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_vec()
        .map_err(|error| map_ureq_response_error(error, status, &headers))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(TransportFailure::response_started_with_headers(
            TransportFailureKind::ResponseTooLarge,
            status,
            &headers,
        ));
    }

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

pub struct JiraClient<T> {
    transport: T,
    credential: ResolvedCredential,
    timeout: Duration,
}

impl<T: JiraTransport> JiraClient<T> {
    pub fn new(transport: T, credential: ResolvedCredential, timeout: Duration) -> Self {
        Self {
            transport,
            credential,
            timeout,
        }
    }

    pub fn get(&self, path: &str) -> Result<HttpResponse, AppError> {
        self.request(HttpMethod::Get, path, Vec::new(), false)
    }

    pub fn verified_site(&self) -> &Url {
        &self.credential.site
    }

    pub fn jira_write<B: Serialize>(
        &self,
        endpoint: WriteEndpoint,
        path: &str,
        body: &B,
    ) -> Result<HttpResponse, AppError> {
        let body = serde_json::to_vec(body).map_err(|_| {
            write_before_dispatch_error(
                endpoint,
                ErrorCode::Internal,
                "failed to encode the Jira mutation request",
            )
        })?;
        let request = self
            .build_request(
                endpoint.method(),
                path,
                body,
                true,
                RequestEffect::JiraWrite,
            )
            .map_err(|error| write_before_dispatch_error(endpoint, error.code, error.message))?;
        let response = self.transport.execute(request).map_err(|failure| {
            let rate_limit = failure.rate_limit.clone();
            write_app_error(
                endpoint,
                WriteFailure::Transport(failure.kind, failure.phase, failure.status),
                rate_limit,
            )
        })?;
        if endpoint.accepts_success(response.status) {
            return Ok(response);
        }
        Err(write_app_error(
            endpoint,
            WriteFailure::HttpStatus(response.status),
            Some(RateLimitMetadata::from_headers(&response.headers)),
        ))
    }

    pub fn jira_write_empty(
        &self,
        endpoint: WriteEndpoint,
        path: &str,
    ) -> Result<HttpResponse, AppError> {
        let request = self
            .build_request(
                endpoint.method(),
                path,
                Vec::new(),
                false,
                RequestEffect::JiraWrite,
            )
            .map_err(|error| write_before_dispatch_error(endpoint, error.code, error.message))?;
        let response = self.transport.execute(request).map_err(|failure| {
            let rate_limit = failure.rate_limit.clone();
            write_app_error(
                endpoint,
                WriteFailure::Transport(failure.kind, failure.phase, failure.status),
                rate_limit,
            )
        })?;
        if endpoint.accepts_success(response.status) {
            return Ok(response);
        }
        Err(write_app_error(
            endpoint,
            WriteFailure::HttpStatus(response.status),
            Some(RateLimitMetadata::from_headers(&response.headers)),
        ))
    }

    pub fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R, AppError> {
        let response = self.get(path)?;
        decode_json_response(response)
    }

    pub fn get_json_exact<R: DeserializeOwned>(
        &self,
        path: &str,
        expected_status: u16,
    ) -> Result<R, AppError> {
        let response = self.get(path)?;
        if response.status != expected_status {
            return Err(invalid_response(
                "Jira returned an unexpected success status",
                Some(response.status),
            ));
        }
        decode_json_response(response)
    }

    pub fn post_json_read<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R, AppError> {
        let body = serde_json::to_vec(body).map_err(|_| {
            AppError::new(
                ErrorCode::Internal,
                "failed to encode the Jira request",
                RetrySafety::Safe,
            )
        })?;
        let response = self.request(HttpMethod::Post, path, body, true)?;
        decode_json_response(response)
    }

    fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Vec<u8>,
        json_body: bool,
    ) -> Result<HttpResponse, AppError> {
        let request = self.build_request(method, path, body, json_body, RequestEffect::Read)?;
        let response = self
            .transport
            .execute(request)
            .map_err(map_transport_error)?;
        classify_response(response)
    }

    fn build_request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Vec<u8>,
        json_body: bool,
        effect: RequestEffect,
    ) -> Result<HttpRequest, AppError> {
        let gateway = Url::parse(&format!(
            "https://api.atlassian.com/ex/jira/{}/",
            self.credential.cloud_id
        ))
        .map_err(|_| internal_error())?;
        let url = gateway
            .join(path.trim_start_matches('/'))
            .map_err(|_| invalid_response("the Jira request path is invalid", None))?;
        if url.origin() != gateway.origin() || !url.path().starts_with(gateway.path()) {
            return Err(invalid_response(
                "the Jira request path escaped the configured gateway",
                None,
            ));
        }

        let mut basic_material = Zeroizing::new(self.credential.email.clone());
        basic_material.push(':');
        basic_material.push_str(self.credential.token.expose_secret());
        let encoded = Zeroizing::new(STANDARD.encode(basic_material.as_bytes()));
        let mut authorization = String::from("Basic ");
        authorization.push_str(&encoded);

        let mut headers = BTreeMap::from([
            ("accept".to_owned(), SecretString::from("application/json")),
            (
                "authorization".to_owned(),
                SecretString::from(authorization),
            ),
            (
                "user-agent".to_owned(),
                SecretString::from(concat!("jira-ops/", env!("CARGO_PKG_VERSION"))),
            ),
        ]);
        if json_body {
            headers.insert(
                "content-type".to_owned(),
                SecretString::from("application/json"),
            );
        }

        Ok(HttpRequest {
            method,
            url,
            headers,
            body,
            effect,
            timeout: self.timeout,
        })
    }
}

fn write_before_dispatch_error(
    endpoint: WriteEndpoint,
    code: ErrorCode,
    message: impl Into<String>,
) -> AppError {
    let classification = classify_write_failure(endpoint, WriteFailure::BeforeDispatch(code));
    let mut error = AppError::new(classification.code, message, classification.retry_safety);
    error.operation_outcome = Some(classification.outcome);
    error
}

fn write_app_error(
    endpoint: WriteEndpoint,
    failure: WriteFailure,
    rate_limit: Option<RateLimitMetadata>,
) -> AppError {
    let classification = classify_write_failure(endpoint, failure);
    let message = match classification.code {
        ErrorCode::AuthInvalid => "Jira rejected the credential",
        ErrorCode::Forbidden => "Jira denied the requested mutation",
        ErrorCode::NotFound => "the Jira mutation target was not found",
        ErrorCode::Conflict => "Jira reported a mutation conflict",
        ErrorCode::RateLimited => "Jira rate limited the mutation",
        ErrorCode::RemoteRejected => "Jira rejected the mutation",
        ErrorCode::MutationResponseInvalid => {
            "Jira applied the mutation but returned an invalid success response"
        }
        ErrorCode::MutationOutcomeUnknown => {
            "Jira may have applied the mutation; do not retry automatically"
        }
        ErrorCode::Timeout => "the Jira mutation request timed out before dispatch",
        ErrorCode::ConnectionFailed => "the Jira connection failed before dispatch",
        ErrorCode::ResponseInvalid => "the Jira mutation request failed before dispatch",
        ErrorCode::ResponseTooLarge => "the Jira mutation response exceeded the 16 MiB limit",
        _ => "the Jira mutation failed before dispatch",
    };
    let mut error = AppError::new(classification.code, message, classification.retry_safety);
    error.operation_outcome = Some(classification.outcome);
    error.status = match failure {
        WriteFailure::Transport(_, _, status) => status,
        WriteFailure::HttpStatus(status) | WriteFailure::InvalidSuccessBody(status) => Some(status),
        WriteFailure::BeforeDispatch(_) => None,
    };
    if classification.code == ErrorCode::RateLimited
        && let Some(rate_limit) = rate_limit
    {
        error.retry_after_ms = rate_limit.retry_after_ms;
        error.rate_limit_reason = rate_limit.reason;
    }
    if let WriteFailure::Transport(kind, phase, _) = failure {
        error.details = Some(json!({
            "failure_kind": transport_failure_name(kind),
            "dispatch_phase": dispatch_phase_name(phase),
        }));
    }
    error
}

const fn transport_failure_name(kind: TransportFailureKind) -> &'static str {
    match kind {
        TransportFailureKind::Timeout => "timeout",
        TransportFailureKind::Connection => "connection",
        TransportFailureKind::Protocol => "protocol",
        TransportFailureKind::ResponseTooLarge => "response_too_large",
    }
}

const fn dispatch_phase_name(phase: DispatchPhase) -> &'static str {
    match phase {
        DispatchPhase::BeforeDispatch => "before_dispatch",
        DispatchPhase::DispatchStarted => "dispatch_started",
        DispatchPhase::ResponseStarted => "response_started",
        DispatchPhase::Complete => "complete",
    }
}

pub fn unauthenticated_get_json<T: JiraTransport, R: DeserializeOwned>(
    transport: &T,
    url: Url,
    timeout: Duration,
) -> Result<R, AppError> {
    if url.scheme() != "https" {
        return Err(invalid_response(
            "the unauthenticated Jira URL must use HTTPS",
            None,
        ));
    }
    let request = HttpRequest {
        method: HttpMethod::Get,
        url,
        headers: BTreeMap::from([
            ("accept".to_owned(), SecretString::from("application/json")),
            (
                "user-agent".to_owned(),
                SecretString::from(concat!("jira-ops/", env!("CARGO_PKG_VERSION"))),
            ),
        ]),
        body: Vec::new(),
        effect: RequestEffect::Read,
        timeout,
    };
    let response = transport.execute(request).map_err(map_transport_error)?;
    decode_json_response(classify_response(response)?)
}

pub(crate) fn decode_write_response<R: DeserializeOwned>(
    endpoint: WriteEndpoint,
    response: HttpResponse,
) -> Result<R, AppError> {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_write_success(endpoint, response.status));
    }
    let content_type = response
        .headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase());
    if content_type
        .as_deref()
        .is_some_and(|value| value.starts_with("text/html"))
        || response
            .body
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'<')
    {
        return Err(invalid_write_success(endpoint, response.status));
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| invalid_write_success(endpoint, response.status))
}

pub(crate) fn invalid_write_success(endpoint: WriteEndpoint, status: u16) -> AppError {
    write_app_error(endpoint, WriteFailure::InvalidSuccessBody(status), None)
}

pub(crate) fn ensure_empty_write_response(
    endpoint: WriteEndpoint,
    response: HttpResponse,
) -> Result<(), AppError> {
    if response.body.is_empty() {
        Ok(())
    } else {
        Err(invalid_write_success(endpoint, response.status))
    }
}

fn decode_json_response<R: DeserializeOwned>(response: HttpResponse) -> Result<R, AppError> {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(response_too_large(Some(response.status)));
    }
    let content_type = response
        .headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase());
    if content_type
        .as_deref()
        .is_some_and(|value| value.starts_with("text/html"))
        || response
            .body
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'<')
    {
        return Err(invalid_response(
            "Jira returned a non-JSON response",
            Some(response.status),
        ));
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("Jira returned malformed JSON", Some(response.status)))
}

fn classify_response(response: HttpResponse) -> Result<HttpResponse, AppError> {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(response_too_large(Some(response.status)));
    }
    if (200..=299).contains(&response.status) {
        return Ok(response);
    }

    let (code, message) = match response.status {
        300..=399 => (
            ErrorCode::ResponseInvalid,
            "Jira returned an unexpected redirect",
        ),
        401 => (ErrorCode::AuthInvalid, "Jira rejected the credential"),
        403 => (ErrorCode::Forbidden, "Jira denied the requested operation"),
        404 => (ErrorCode::NotFound, "the Jira resource was not found"),
        409 => (ErrorCode::Conflict, "Jira reported a conflict"),
        429 => (ErrorCode::RateLimited, "Jira rate limited the request"),
        500..=599 => (
            ErrorCode::RemoteUnavailable,
            "Jira is temporarily unavailable",
        ),
        400..=499 => (ErrorCode::RemoteRejected, "Jira rejected the request"),
        _ => (
            ErrorCode::ResponseInvalid,
            "Jira returned an unexpected status",
        ),
    };
    let mut error = AppError::new(code, message, RetrySafety::Safe);
    error.status = Some(response.status);
    if response.status == 429 {
        error.retry_after_ms = parse_retry_after_ms(&response.headers);
        error.rate_limit_reason = response
            .headers
            .get("ratelimit-reason")
            .or_else(|| response.headers.get("x-ratelimit-reason"))
            .cloned();
    }
    Err(error)
}

fn parse_retry_after_ms(headers: &BTreeMap<String, String>) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
}

fn map_transport_error(failure: TransportFailure) -> AppError {
    let (code, message) = match failure.kind {
        TransportFailureKind::Timeout => (ErrorCode::Timeout, "the Jira request timed out"),
        TransportFailureKind::Connection => {
            (ErrorCode::ConnectionFailed, "the Jira connection failed")
        }
        TransportFailureKind::Protocol => (
            ErrorCode::ResponseInvalid,
            "the Jira transport returned an invalid response",
        ),
        TransportFailureKind::ResponseTooLarge => {
            return response_too_large(None);
        }
    };
    AppError::new(code, message, RetrySafety::Safe)
}

fn map_ureq_error(
    error: ureq::Error,
    phase: DispatchPhase,
    status: Option<u16>,
) -> TransportFailure {
    let kind = match error {
        ureq::Error::Timeout(_) => TransportFailureKind::Timeout,
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Io(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => TransportFailureKind::Connection,
        ureq::Error::BodyExceedsLimit(_) => TransportFailureKind::ResponseTooLarge,
        _ => TransportFailureKind::Protocol,
    };
    TransportFailure::new(kind, phase, status)
}

fn map_ureq_response_error(
    error: ureq::Error,
    status: u16,
    headers: &BTreeMap<String, String>,
) -> TransportFailure {
    let kind = match error {
        ureq::Error::Timeout(_) => TransportFailureKind::Timeout,
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Io(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => TransportFailureKind::Connection,
        ureq::Error::BodyExceedsLimit(_) => TransportFailureKind::ResponseTooLarge,
        _ => TransportFailureKind::Protocol,
    };
    TransportFailure::response_started_with_headers(kind, status, headers)
}

fn response_too_large(status: Option<u16>) -> AppError {
    let mut error = AppError::new(
        ErrorCode::ResponseTooLarge,
        "the Jira response exceeded the 16 MiB limit",
        RetrySafety::Safe,
    );
    error.status = status;
    error
}

fn invalid_response(message: &str, status: Option<u16>) -> AppError {
    let mut error = AppError::new(ErrorCode::ResponseInvalid, message, RetrySafety::Safe);
    error.status = status;
    error
}

fn internal_error() -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "failed to construct the Jira gateway",
        RetrySafety::Safe,
    )
}
