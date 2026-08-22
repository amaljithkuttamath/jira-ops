use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

const CLOUD_PREFIX: &str = "/ex/jira/00000000-0000-0000-0000-000000000000";
const TEST_SERVER_ENV: &str = "JIRA_OPS_HIERARCHY_TEST_SERVER";

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequestTrace {
    method: String,
    path_and_query: String,
    headers: BTreeMap<String, Vec<String>>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct ExpectedRequest {
    method: &'static str,
    path_and_query: String,
    body: Vec<u8>,
    response_status: u16,
    response_body: &'static str,
}

#[derive(Default)]
struct ServerState {
    script: VecDeque<ExpectedRequest>,
    trace: Vec<RequestTrace>,
    error: Option<String>,
}

struct FakeJira {
    address: SocketAddr,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeJira {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback fake Jira");
        let address = listener.local_addr().expect("fake Jira address");
        let state = Arc::new(Mutex::new(ServerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let server_state = Arc::clone(&state);
        let server_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            for connection in listener.incoming() {
                if server_stop.load(Ordering::SeqCst) {
                    break;
                }
                match connection {
                    Ok(mut stream) => serve_one(&mut stream, &server_state),
                    Err(error) => {
                        server_state.lock().expect("server state").error =
                            Some(format!("fake Jira accept failed: {error}"));
                        break;
                    }
                }
            }
        });
        Self {
            address,
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn origin(&self) -> String {
        format!("http://{}/", self.address)
    }

    fn reset(&self) {
        let mut state = self.state.lock().expect("server state");
        assert!(
            state.script.is_empty(),
            "previous fake Jira script remained"
        );
        assert!(state.error.is_none(), "previous fake Jira error remained");
        state.trace.clear();
        state.script = hierarchy_script();
    }

    fn finish_run(&self) -> Vec<RequestTrace> {
        let mut state = self.state.lock().expect("server state");
        assert!(state.script.is_empty(), "scripted responses remain");
        assert_eq!(state.error, None, "fake Jira rejected a request");
        std::mem::take(&mut state.trace)
    }
}

impl Drop for FakeJira {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join fake Jira");
        }
    }
}

fn serve_one(stream: &mut TcpStream, state: &Arc<Mutex<ServerState>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set fake read timeout");
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => {
            state.lock().expect("server state").error = Some(error);
            write_response(stream, 500, r#"{"error":"invalid request"}"#);
            return;
        }
    };

    let response = {
        let mut state = state.lock().expect("server state");
        let Some(expected) = state.script.pop_front() else {
            state.error = Some(format!(
                "unexpected extra request: {} {}",
                request.method, request.path_and_query
            ));
            return write_response(stream, 500, r#"{"error":"extra request"}"#);
        };
        let mismatch = (request.method != expected.method)
            .then(|| {
                format!(
                    "method: expected {}, got {}",
                    expected.method, request.method
                )
            })
            .or_else(|| {
                (request.path_and_query != expected.path_and_query).then(|| {
                    format!(
                        "path: expected {}, got {}",
                        expected.path_and_query, request.path_and_query
                    )
                })
            })
            .or_else(|| {
                (request.body != expected.body).then(|| {
                    format!(
                        "body: expected {}, got {}",
                        String::from_utf8_lossy(&expected.body),
                        String::from_utf8_lossy(&request.body)
                    )
                })
            });
        state.trace.push(request);
        if let Some(mismatch) = mismatch {
            state.error = Some(mismatch);
            (500, r#"{"error":"request mismatch"}"#)
        } else {
            (expected.response_status, expected.response_body)
        }
    };
    write_response(stream, response.0, response.1);
}

fn read_request(stream: &mut TcpStream) -> Result<RequestTrace, String> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() > 64 * 1024 {
            return Err("request headers exceeded 64 KiB".to_owned());
        }
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("failed to read request: {error}"))?;
        if read == 0 {
            return Err("request ended before headers completed".to_owned());
        }
        bytes.extend_from_slice(&chunk[..read]);
    };

    let headers_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| "request headers were not UTF-8".to_owned())?;
    let mut lines = headers_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| "request line missing".to_owned())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "request method missing".to_owned())?
        .to_owned();
    let path_and_query = request_parts
        .next()
        .ok_or_else(|| "request target missing".to_owned())?
        .to_owned();
    if request_parts.next() != Some("HTTP/1.1") || request_parts.next().is_some() {
        return Err("request line was not canonical HTTP/1.1".to_owned());
    }

    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed request header: {line}"))?;
        headers
            .entry(name.trim().to_ascii_lowercase())
            .or_default()
            .push(value.trim().to_owned());
    }
    for values in headers.values_mut() {
        values.sort();
    }
    let content_length = headers
        .get("content-length")
        .and_then(|values| values.first())
        .map_or(Ok(0_usize), |value| value.parse::<usize>())
        .map_err(|_| "invalid Content-Length".to_owned())?;
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("failed to read request body: {error}"))?;
        if read == 0 {
            return Err("request body ended before Content-Length".to_owned());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() - header_end != content_length {
        return Err("request body exceeded Content-Length".to_owned());
    }

    Ok(RequestTrace {
        method,
        path_and_query,
        headers,
        body: bytes[header_end..].to_vec(),
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 || status == 201 {
        "OK"
    } else {
        "Internal Server Error"
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write fake Jira response");
}

fn hierarchy_script() -> VecDeque<ExpectedRequest> {
    let types = r#"{"startAt":0,"total":5,"issueTypes":[{"id":"10001","name":"Epic","subtask":false},{"id":"10002","name":"Story","subtask":false},{"id":"10003","name":"Task","subtask":false},{"id":"10004","name":"Subtask","subtask":true},{"id":"10005","name":"Bug","subtask":false}]}"#;
    let epic_fields = r#"{"startAt":0,"total":1,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string","system":"summary"},"allowedValues":[] }]}"#;
    let story_fields = include_str!("fixtures/create_meta_story_parent.json");
    let task_fields = r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string","system":"summary"},"allowedValues":[]},{"fieldId":"parent","name":"Parent","required":false,"operations":["set"],"schema":{"type":"issuelink","system":"parent"}}]}"#;
    let subtask_fields = include_str!("fixtures/create_meta_subtask_parent.json");
    let bug_fields = r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string","system":"summary"},"allowedValues":[]},{"fieldId":"parent","name":"Parent","required":false,"operations":["set"],"schema":{"type":"issuelink","system":"parent"}}]}"#;

    VecDeque::from([
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes?maxResults=100", b"", 200, types),
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes/10001?maxResults=100", b"", 200, epic_fields),
        expected("POST", "/rest/api/3/issue", br#"{"fields":{"issuetype":{"id":"10001"},"project":{"key":"ACCL"},"summary":"Epic root"}}"#, 201, r#"{"id":"20000","key":"ACCL-100"}"#),
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes/10002?maxResults=100", b"", 200, story_fields),
        expected("POST", "/rest/api/3/issue", br#"{"fields":{"issuetype":{"id":"10002"},"parent":{"key":"ACCL-100"},"project":{"key":"ACCL"},"summary":"Story child"}}"#, 201, r#"{"id":"20001","key":"ACCL-101"}"#),
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes/10003?maxResults=100", b"", 200, task_fields),
        expected("POST", "/rest/api/3/issue", br#"{"fields":{"issuetype":{"id":"10003"},"parent":{"key":"ACCL-100"},"project":{"key":"ACCL"},"summary":"Task child"}}"#, 201, r#"{"id":"20002","key":"ACCL-102"}"#),
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes/10004?maxResults=100", b"", 200, subtask_fields),
        expected("POST", "/rest/api/3/issue", br#"{"fields":{"issuetype":{"id":"10004"},"parent":{"key":"ACCL-102"},"project":{"key":"ACCL"},"summary":"Subtask child"}}"#, 201, r#"{"id":"20003","key":"ACCL-103"}"#),
        expected("GET", "/rest/api/3/issue/createmeta/ACCL/issuetypes/10005?maxResults=100", b"", 200, bug_fields),
        expected("POST", "/rest/api/3/issue", br#"{"fields":{"issuetype":{"id":"10005"},"parent":{"key":"ACCL-100"},"project":{"key":"ACCL"},"summary":"Bug child"}}"#, 201, r#"{"id":"20004","key":"ACCL-104"}"#),
    ])
}

fn expected(
    method: &'static str,
    path: &str,
    body: &[u8],
    response_status: u16,
    response_body: &'static str,
) -> ExpectedRequest {
    ExpectedRequest {
        method,
        path_and_query: format!("{CLOUD_PREFIX}{path}"),
        body: body.to_vec(),
        response_status,
        response_body,
    }
}

#[derive(Clone, Copy)]
enum OutputMode {
    Json,
    Toon,
}

struct ScenarioRun {
    documents: Vec<Value>,
    trace: Vec<RequestTrace>,
}

fn build_cfg_binary(root: &TempDir) -> PathBuf {
    let target = root.path().join("target");
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "build",
            "--offline",
            "--bin",
            "jira-ops",
            "--all-features",
            "--target-dir",
        ])
        .arg(&target)
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTFLAGS", "--cfg jira_ops_hierarchy_test")
        .output()
        .expect("nested offline Cargo build");
    assert!(
        output.status.success(),
        "nested build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    target
        .join("debug")
        .join(format!("jira-ops{}", std::env::consts::EXE_SUFFIX))
}

fn run_scenario(
    binary: &Path,
    fake: &FakeJira,
    isolation: &TempDir,
    mode: OutputMode,
) -> ScenarioRun {
    fake.reset();
    let types = run_child(
        binary,
        fake,
        isolation,
        mode,
        &[
            "issue",
            "create-meta",
            "--project",
            "ACCL",
            "--limit",
            "100",
        ],
        None,
    );
    let type_ids: BTreeMap<&str, &str> = types["data"]
        .as_array()
        .expect("issue type data")
        .iter()
        .map(|item| {
            (
                item["name"].as_str().expect("issue type name"),
                item["id"].as_str().expect("issue type id"),
            )
        })
        .collect();
    assert_eq!(type_ids.len(), 5);

    let epic = create_issue(
        binary,
        fake,
        isolation,
        mode,
        type_ids["Epic"],
        json!({"summary":"Epic root"}),
    );
    let epic_key = created_key(&epic).to_owned();
    let story = create_issue(
        binary,
        fake,
        isolation,
        mode,
        type_ids["Story"],
        json!({"summary":"Story child","parent":{"key":epic_key}}),
    );
    let task = create_issue(
        binary,
        fake,
        isolation,
        mode,
        type_ids["Task"],
        json!({"summary":"Task child","parent":{"key":epic_key}}),
    );
    let task_key = created_key(&task).to_owned();
    let subtask = create_issue(
        binary,
        fake,
        isolation,
        mode,
        type_ids["Subtask"],
        json!({"summary":"Subtask child","parent":{"key":task_key}}),
    );
    let bug = create_issue(
        binary,
        fake,
        isolation,
        mode,
        type_ids["Bug"],
        json!({"summary":"Bug child","parent":{"key":epic_key}}),
    );
    let documents = vec![types, epic, story, task, subtask, bug];
    let expected_keys = ["ACCL-100", "ACCL-101", "ACCL-102", "ACCL-103", "ACCL-104"];
    for (document, expected_key) in documents[1..].iter().zip(expected_keys) {
        assert_eq!(created_key(document), expected_key);
    }
    ScenarioRun {
        documents,
        trace: fake.finish_run(),
    }
}

fn create_issue(
    binary: &Path,
    fake: &FakeJira,
    isolation: &TempDir,
    mode: OutputMode,
    issue_type_id: &str,
    fields: Value,
) -> Value {
    run_child(
        binary,
        fake,
        isolation,
        mode,
        &["issue", "create", "--input", "-", "--apply"],
        Some(
            &json!({
                "project_key":"ACCL",
                "issue_type_id":issue_type_id,
                "fields":fields
            })
            .to_string(),
        ),
    )
}

fn created_key(document: &Value) -> &str {
    let key = document["data"]["issue"]["key"]
        .as_str()
        .expect("created issue key");
    assert!(!key.trim().is_empty());
    key
}

fn run_child(
    binary: &Path,
    fake: &FakeJira,
    isolation: &TempDir,
    mode: OutputMode,
    args: &[&str],
    stdin: Option<&str>,
) -> Value {
    let home = isolation.path().join("empty-home");
    let config = isolation.path().join("empty-config");
    fs::create_dir_all(&home).expect("create empty HOME");
    fs::create_dir_all(&config).expect("create empty XDG_CONFIG_HOME");
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env(TEST_SERVER_ENV, fake.origin())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if matches!(mode, OutputMode::Toon) {
        command.args(["--output", "toon"]);
    }
    command.args(args);
    let mut child = command.spawn().expect("spawn cfg-only jira-ops binary");
    if let Some(stdin) = stdin {
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(stdin.as_bytes())
            .expect("write child stdin");
    }
    let output = child.wait_with_output().expect("wait for jira-ops child");
    assert!(
        output.status.success(),
        "child failed: args={args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "successful child wrote stderr");
    decode_output(mode, &output.stdout)
}

fn decode_output(mode: OutputMode, output: &[u8]) -> Value {
    assert!(output.ends_with(b"\n"), "output omitted terminal LF");
    let document = &output[..output.len() - 1];
    assert!(
        !document.ends_with(b"\n") && !document.ends_with(b"\r"),
        "output had more than one line ending"
    );
    match mode {
        OutputMode::Json => serde_json::from_slice(document).expect("public JSON output"),
        OutputMode::Toon => toon_format::decode_default(
            std::str::from_utf8(document).expect("public TOON output is UTF-8"),
        )
        .expect("public TOON output"),
    }
}

fn assert_test_origin_rejected(binary: &Path, isolation: &TempDir, origin: &str) {
    let home = isolation.path().join("invalid-origin-home");
    let config = isolation.path().join("invalid-origin-config");
    fs::create_dir_all(&home).expect("create invalid-origin HOME");
    fs::create_dir_all(&config).expect("create invalid-origin config");
    let output = Command::new(binary)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env(TEST_SERVER_ENV, origin)
        .args([
            "issue",
            "create-meta",
            "--project",
            "ACCL",
            "--limit",
            "100",
        ])
        .output()
        .expect("run cfg binary with invalid origin");
    assert!(
        !output.status.success(),
        "invalid origin was accepted: {origin}"
    );
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("must be an IP-literal loopback http origin with an explicit port"),
        "unexpected invalid-origin failure for {origin}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn create_hierarchy_real_binary_has_json_toon_and_complete_http_trace_parity() {
    let build = TempDir::new().expect("nested build tempdir");
    let binary = build_cfg_binary(&build);
    let isolation = TempDir::new().expect("child isolation tempdir");
    for invalid_origin in [
        "https://127.0.0.1:49152/",
        "http://localhost:49152/",
        "http://127.0.0.1/",
        "http://127.0.0.1:49152/not-root",
        "http://user@127.0.0.1:49152/",
        "http://127.0.0.1:49152/?query=forbidden",
    ] {
        assert_test_origin_rejected(&binary, &isolation, invalid_origin);
    }

    let fake = FakeJira::start();

    let json = run_scenario(&binary, &fake, &isolation, OutputMode::Json);
    let toon = run_scenario(&binary, &fake, &isolation, OutputMode::Toon);

    assert_eq!(toon.documents, json.documents, "JSON/TOON semantic drift");
    assert_eq!(toon.trace, json.trace, "JSON/TOON HTTP trace drift");
    assert_eq!(json.trace.len(), 11);
    assert_eq!(
        json.trace
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        5
    );
    assert_eq!(
        json.trace
            .iter()
            .map(|request| (request.method.as_str(), request.path_and_query.as_str()))
            .collect::<Vec<_>>(),
        hierarchy_script()
            .iter()
            .map(|request| (request.method, request.path_and_query.as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        fs::read_dir(isolation.path().join("empty-home"))
            .expect("isolated HOME remains")
            .count(),
        0,
        "cfg-only child wrote into HOME"
    );
    assert_eq!(
        fs::read_dir(isolation.path().join("empty-config"))
            .expect("isolated config remains")
            .count(),
        0,
        "cfg-only child wrote into XDG_CONFIG_HOME"
    );
}
