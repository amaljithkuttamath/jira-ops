use assert_cmd::Command;
use jira_ops::MutationRuntime;
use jira_ops::cli::{Cli, Command as CliCommand, ProjectCommand};
use jira_ops::client::{
    DispatchPhase, HttpMethod, HttpRequest, HttpResponse, JiraTransport, RequestEffect,
    TransportFailure, TransportFailureKind,
};
use jira_ops::commands::{MAX_MUTATION_INPUT_BYTES, read_json_input};
use jira_ops::config::{
    ConfigStore, CredentialKey, CredentialStore, EnvironmentSource, SavedIdentity, StoreError,
};
use jira_ops::error::{AppError, ErrorCode, RetrySafety};
use jira_ops::model::{CommentInput, CreateIssueInput, ProjectCreateInput};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::rc::Rc;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

struct ProcessRun {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Representation {
    DefaultJson,
    ExplicitJson,
    PrettyJson,
    Toon,
}

impl Representation {
    fn arguments<'a>(self, command: &[&'a str]) -> Vec<&'a str> {
        let mut args = match self {
            Self::DefaultJson => Vec::new(),
            Self::ExplicitJson => vec!["--output", "json"],
            Self::PrettyJson => vec!["--pretty"],
            Self::Toon => vec!["--output", "toon"],
        };
        args.extend_from_slice(command);
        args
    }

    fn decode(self, document: &str) -> Value {
        match self {
            Self::DefaultJson | Self::ExplicitJson | Self::PrettyJson => {
                serde_json::from_str(document).expect("JSON process document")
            }
            Self::Toon => toon_format::decode_default(
                document
                    .strip_suffix('\n')
                    .expect("TOON document has one terminal LF"),
            )
            .expect("TOON process document"),
        }
    }
}

fn assert_error_layout(representation: Representation, document: &str) {
    match representation {
        Representation::DefaultJson | Representation::ExplicitJson => {
            assert_eq!(document.matches('\n').count(), 1);
        }
        Representation::PrettyJson => {
            assert!(document.matches('\n').count() > 1);
        }
        Representation::Toon => {
            assert!(document.ends_with('\n'));
            assert!(!document.ends_with("\n\n"));
        }
    }
}

fn run_binary<const N: usize>(args: [&str; N], stdin: &str) -> ProcessRun {
    run_binary_bytes(args, stdin.as_bytes())
}

fn run_binary_bytes<const N: usize>(args: [&str; N], stdin: &[u8]) -> ProcessRun {
    let output = Command::cargo_bin("jira-ops")
        .expect("jira-ops binary")
        .args(args)
        .write_stdin(stdin)
        .output()
        .expect("run jira-ops");

    ProcessRun {
        code: output.status.code().expect("process exit code"),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn run_binary_os(args: Vec<OsString>, stdin: &[u8]) -> ProcessRun {
    let output = Command::cargo_bin("jira-ops")
        .expect("jira-ops binary")
        .args(args)
        .write_stdin(stdin)
        .output()
        .expect("run jira-ops");

    ProcessRun {
        code: output.status.code().expect("process exit code"),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn run_binary_without_environment<const N: usize>(args: [&str; N]) -> ProcessRun {
    let output = Command::cargo_bin("jira-ops")
        .expect("jira-ops binary")
        .args(args)
        .env_clear()
        .output()
        .expect("run jira-ops without configuration or credentials");

    ProcessRun {
        code: output.status.code().expect("process exit code"),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn error_json(run: &ProcessRun) -> Value {
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.matches('\n').count(), 1);
    serde_json::from_str(&run.stderr).expect("one stderr error envelope")
}

#[test]
fn project_create_is_plan_by_default_and_apply_is_explicit() {
    let plan = Cli::parse_args(["project", "create", "--input", "-"].map(OsString::from))
        .expect("project create plan grammar");
    let CliCommand::Project(project) = plan.command else {
        panic!("project command");
    };
    let ProjectCommand::Create(plan) = project.command else {
        panic!("project create command");
    };
    assert!(!plan.mutation.apply);

    let apply =
        Cli::parse_args(["project", "create", "--input", "-", "--apply"].map(OsString::from))
            .expect("project create apply grammar");
    let CliCommand::Project(project) = apply.command else {
        panic!("project command");
    };
    let ProjectCommand::Create(apply) = project.command else {
        panic!("project create command");
    };
    assert!(apply.mutation.apply);
}

#[test]
fn project_templates_are_local_filterable_and_need_no_configuration() {
    let all = run_binary(["project", "templates"], "");
    assert_eq!(all.code, 0, "{}", all.stderr);
    assert_eq!(all.stderr, "");
    let all: Value = serde_json::from_str(&all.stdout).unwrap();
    assert_eq!(all["data"].as_array().unwrap().len(), 4);

    let software = run_binary(["project", "templates", "--type", "software"], "");
    assert_eq!(software.code, 0, "{}", software.stderr);
    let software: Value = serde_json::from_str(&software.stdout).unwrap();
    assert_eq!(software, all);

    let unknown = run_binary(["project", "templates", "--type", "service_management"], "");
    assert_eq!(unknown.code, 0, "{}", unknown.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&unknown.stdout).unwrap()["data"],
        serde_json::json!([])
    );
}

#[test]
fn project_plan_is_exact_and_performs_zero_service_calls() {
    let runtime = CountingRuntime {
        config_calls: Cell::new(0),
        credential_calls: Cell::new(0),
        transport_calls: Cell::new(0),
    };
    let input = r#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#;
    let run = run_injected(&["project", "create", "--input", "-"], input, &runtime);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        serde_json::from_str::<Value>(&run.stdout).unwrap(),
        serde_json::json!({"data":{
            "operation":"project.create",
            "method":"POST",
            "path":"/rest/api/3/project",
            "body":{
                "key":"OPSDEMO",
                "name":"Ops Demo",
                "projectTypeKey":"software",
                "projectTemplateKey":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic",
                "leadAccountId":"abc123",
                "assigneeType":"UNASSIGNED"
            }
        }})
    );
    assert_eq!(runtime.config_calls.get(), 0);
    assert_eq!(runtime.credential_calls.get(), 0);
    assert_eq!(runtime.transport_calls.get(), 0);
}

#[test]
fn project_plan_rejects_every_strict_input_class_before_services() {
    let invalid = [
        br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123","extra":true}"#.as_slice(),
        br#"{"key":"OPSDEMO","key":"OTHER","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#.as_slice(),
        br#"{"key":"bad","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#.as_slice(),
        br#"{"key":"OPSDEMO","name":"","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#.as_slice(),
        br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"business","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#.as_slice(),
        br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"unknown","lead_account_id":"abc123"}"#.as_slice(),
        br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":" "}"#.as_slice(),
        br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123","assignee_type":"AUTOMATIC"}"#.as_slice(),
        b"\xff".as_slice(),
    ];

    for mut input in invalid {
        let runtime = CountingRuntime {
            config_calls: Cell::new(0),
            credential_calls: Cell::new(0),
            transport_calls: Cell::new(0),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = jira_ops::run_mutation_with_runtime(
            ["project", "create", "--input", "-"].map(OsString::from),
            &mut input,
            &mut stdout,
            &mut stderr,
            &EmptyEnvironment,
            &runtime,
        );
        assert_eq!(code, std::process::ExitCode::from(2));
        assert!(stdout.is_empty());
        let error: Value = serde_json::from_slice(&stderr).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("invalid_json" | "schema_violation")
        ));
        assert_eq!(error["error"]["operation_outcome"], "not_applied");
        assert_eq!(runtime.config_calls.get(), 0);
        assert_eq!(runtime.credential_calls.get(), 0);
        assert_eq!(runtime.transport_calls.get(), 0);
    }

    let mut oversized = vec![b' '; jira_ops::commands::MAX_MUTATION_INPUT_BYTES + 1];
    let error =
        jira_ops::commands::read_json_input::<ProjectCreateInput>(&mut oversized.as_slice())
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    oversized.zeroize();
}

#[test]
fn version_success_is_one_compact_json_line_on_stdout() {
    let run = run_binary(["version"], "");

    assert_eq!(run.code, 0);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout.matches('\n').count(), 1);
    assert!(!run.stdout.contains("\n  "));

    let value: Value = serde_json::from_str(&run.stdout).expect("valid success JSON");
    assert_eq!(value["data"]["cli_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["data"]["contract_version"], "1");
    assert!(value.get("meta").is_none());
    assert!(value.get("warnings").is_none());
}

#[test]
fn root_help_is_one_successful_stdout_document_without_environment() {
    let run = run_binary_without_environment(["--help"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout.matches("Usage:").count(), 1);
    assert!(run.stdout.contains("Commands:"));
}

#[test]
fn nested_help_is_one_successful_stdout_document_without_environment() {
    let run = run_binary_without_environment(["issue", "create", "--help"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout.matches("Usage:").count(), 1);
    assert!(run.stdout.contains("jira-ops issue create"));
    assert!(run.stdout.contains("--input"));
}

#[test]
fn pretty_version_changes_whitespace_only() {
    let compact = run_binary(["version"], "");
    let pretty = run_binary(["--pretty", "version"], "");

    assert_eq!(pretty.code, 0);
    assert_eq!(pretty.stderr, "");
    assert!(pretty.stdout.matches('\n').count() > 1);
    assert_eq!(
        serde_json::from_str::<Value>(&compact.stdout).unwrap(),
        serde_json::from_str::<Value>(&pretty.stdout).unwrap()
    );
}

#[test]
fn explicit_json_output_is_byte_identical_and_global() {
    let default = run_binary(["version"], "");
    for run in [
        run_binary(["-o", "json", "version"], ""),
        run_binary(["version", "--output", "json"], ""),
    ] {
        assert_eq!(run.code, 0);
        assert_eq!(run.stdout, default.stdout);
        assert_eq!(run.stderr, default.stderr);
    }

    let default = run_binary(["schema", "issue", "get"], "");
    for run in [
        run_binary(["--output", "json", "schema", "issue", "get"], ""),
        run_binary(["schema", "issue", "get", "-o", "json"], ""),
    ] {
        assert_eq!(run.code, 0);
        assert_eq!(run.stdout, default.stdout);
        assert_eq!(run.stderr, default.stderr);
    }

    let input = r#"{"body":"JSON identity"}"#;
    let default = run_binary(["issue", "comment", "DEMO-1", "--input", "-"], input);
    let explicit = run_binary(
        [
            "issue", "comment", "DEMO-1", "--input", "-", "--output", "json",
        ],
        input,
    );
    assert_eq!(explicit.code, 0, "stderr: {}", explicit.stderr);
    assert_eq!(explicit.stdout, default.stdout);
    assert_eq!(explicit.stderr, default.stderr);
}

#[test]
fn toon_output_is_global_for_success_and_error_streams() {
    for run in [
        run_binary(["-o", "toon", "version"], ""),
        run_binary(["version", "--output", "toon"], ""),
    ] {
        assert_eq!(run.code, 0);
        assert_eq!(run.stderr, "");
        assert!(run.stdout.ends_with('\n'));
        assert!(!run.stdout.ends_with("\n\n"));
        assert!(
            run.stdout
                .contains(&format!("cli_version: \"{}\"", env!("CARGO_PKG_VERSION")))
        );
        assert!(run.stdout.contains("contract_version: \"1\""));
        assert!(!run.stdout.starts_with('{'));
    }

    for run in [
        run_binary(["-o", "toon", "schema", "issue", "erase"], ""),
        run_binary(["schema", "issue", "erase", "--output", "toon"], ""),
    ] {
        assert_eq!(run.code, 2);
        assert_eq!(run.stdout, "");
        assert!(run.stderr.ends_with('\n'));
        assert!(!run.stderr.ends_with("\n\n"));
        assert!(run.stderr.contains("code: invalid_input"));
        assert!(run.stderr.contains("retry_safety: safe"));
        assert!(!run.stderr.starts_with('{'));
    }
}

#[test]
fn toon_covers_parse_errors_and_local_mutation_plans() {
    let parse_error = run_binary(["-o", "toon", "unknown"], "");
    assert_eq!(parse_error.code, 2);
    assert_eq!(parse_error.stdout, "");
    assert!(parse_error.stderr.contains("code: invalid_input"));
    assert!(parse_error.stderr.ends_with('\n'));
    assert!(!parse_error.stderr.starts_with('{'));

    let mutation_error = run_binary(
        ["issue", "-o", "toon", "comment", "DEMO-1", "--body", "text"],
        "",
    );
    assert_eq!(mutation_error.code, 2);
    assert_eq!(mutation_error.stdout, "");
    assert!(mutation_error.stderr.contains("code: invalid_input"));
    assert!(
        mutation_error
            .stderr
            .contains("operation_outcome: not_applied")
    );

    let plan = run_binary(
        [
            "issue", "comment", "DEMO-1", "--input", "-", "--output", "toon",
        ],
        r#"{"body":"No Jira write"}"#,
    );
    assert_eq!(plan.code, 0, "stderr: {}", plan.stderr);
    assert_eq!(plan.stderr, "");
    assert!(plan.stdout.contains("operation: issue.comment"));
    assert!(plan.stdout.contains("applied: false"));
    assert!(plan.stdout.contains("issue: \"DEMO-1\""));
    assert!(plan.stdout.ends_with('\n'));
    assert!(!plan.stdout.ends_with("\n\n"));
}

#[test]
fn unsafe_content_controls_fail_locally_before_toon_output() {
    let run = run_binary(
        [
            "issue", "comment", "DEMO-1", "--input", "-", "--output", "toon",
        ],
        "{\"body\":\"before\\u001b[31mafter\"}",
    );

    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("code: schema_violation"));
    assert!(run.stderr.contains("operation_outcome: not_applied"));
    assert!(!run.stderr.contains('\u{001b}'));
    assert!(!run.stderr.contains("\u{001b}["));
    assert!(run.stderr.ends_with('\n'));
    assert!(!run.stderr.ends_with("\n\n"));
    assert!(
        run.stderr
            .trim_end_matches('\n')
            .chars()
            .all(|character| !character.is_control() || character == '\n')
    );
}

#[test]
fn unsafe_toon_runtime_error_emits_one_safe_internal_document() {
    let run = run_binary(["-o", "toon", "schema", "unsafe\u{001b}[31m"], "");

    assert_eq!(run.code, 70);
    assert_eq!(run.stdout, "");
    let document = run
        .stderr
        .strip_suffix('\n')
        .expect("one terminal LF on stderr");
    assert!(!document.ends_with('\n'));
    assert!(
        document
            .chars()
            .all(|character| !character.is_control() || character == '\n')
    );
    let value: Value = toon_format::decode_default(document).expect("safe TOON error envelope");
    assert_eq!(
        value,
        serde_json::json!({
            "error": {
                "code": "internal",
                "message": "failed to write process output",
                "retry_safety": "safe"
            }
        })
    );
}

#[test]
fn invalid_output_and_pretty_toon_conflict_are_compact_json_errors() {
    for run in [
        run_binary(["--output", "yaml", "version"], ""),
        run_binary(["--pretty", "-o", "toon", "version"], ""),
        run_binary(["version", "--output=toon", "--pretty"], ""),
    ] {
        assert_eq!(run.code, 2);
        assert_eq!(run.stdout, "");
        assert_eq!(run.stderr.matches('\n').count(), 1);
        assert!(!run.stderr.contains("\n  "));
        let value: Value = serde_json::from_str(&run.stderr).expect("compact JSON error");
        assert_eq!(value["error"]["code"], "invalid_input");
        assert_eq!(value["error"]["retry_safety"], "safe");
    }
}

#[test]
fn parse_error_format_respects_option_termination_and_rejects_duplicates() {
    for args in [
        vec!["version", "--", "--output", "toon"],
        vec!["--output", "toon", "--output", "toon", "version"],
        vec!["--output", "json", "--output", "toon", "version"],
    ] {
        let run = run_binary_os(args.into_iter().map(OsString::from).collect(), b"");
        assert_eq!(run.code, 2);
        let value = error_json(&run);
        assert_eq!(value["error"]["code"], "invalid_input");
    }
}

#[cfg(unix)]
#[test]
fn parse_error_format_finds_valid_toon_after_non_utf8_argument() {
    use std::os::unix::ffi::OsStringExt;

    let run = run_binary_os(
        vec![
            OsString::from("schema"),
            OsString::from_vec(b"bad\xffpath".to_vec()),
            OsString::from("-o"),
            OsString::from("toon"),
        ],
        b"",
    );

    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    let document = run
        .stderr
        .strip_suffix('\n')
        .expect("one terminal LF on stderr");
    let value: Value = toon_format::decode_default(document).expect("TOON parse error envelope");
    assert_eq!(value["error"]["code"], "invalid_input");
}

#[test]
fn parse_failure_is_one_compact_json_line_on_stderr() {
    let run = run_binary(["unknown"], "");

    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.matches('\n').count(), 1);

    let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
    assert_eq!(value["error"]["code"], "invalid_input");
    assert_eq!(value["error"]["retry_safety"], "safe");
    assert!(value["error"].get("operation_outcome").is_none());
    assert!(value["error"].get("status").is_none());
    assert!(value["error"].get("details").is_none());
}

#[test]
fn pretty_parse_failure_changes_whitespace_only() {
    let compact = run_binary(["unknown"], "");
    let pretty = run_binary(["--pretty", "unknown"], "");

    assert_eq!(pretty.code, 2);
    assert_eq!(pretty.stdout, "");
    assert!(pretty.stderr.matches('\n').count() > 1);
    assert_eq!(
        serde_json::from_str::<Value>(&compact.stderr).unwrap(),
        serde_json::from_str::<Value>(&pretty.stderr).unwrap()
    );
}

#[test]
fn mutation_rejects_unknown_top_level_property_for_every_input() {
    for (args, stdin) in [
        (
            &["issue", "create", "--input", "-"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"},"extra":true}"#,
        ),
        (
            &["issue", "update", "ACCL-1", "--input", "-"][..],
            r#"{"set":{"summary":"x"},"extra":true}"#,
        ),
        (
            &["issue", "comment", "ACCL-1", "--input", "-"][..],
            r#"{"body":"x","extra":true}"#,
        ),
        (
            &["issue", "transition", "ACCL-1", "--input", "-"][..],
            r#"{"transition_id":"31","extra":true}"#,
        ),
        (
            &["issue", "assign", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":null,"extra":true}"#,
        ),
        (
            &["issue", "link", "add", "--input", "-"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks","extra":true}"#,
        ),
        (
            &["issue", "watcher", "add", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc","extra":true}"#,
        ),
        (
            &["issue", "watcher", "remove", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc","extra":true}"#,
        ),
    ] {
        let output = Command::cargo_bin("jira-ops")
            .expect("jira-ops binary")
            .args(args)
            .write_stdin(stdin)
            .output()
            .expect("run jira-ops");
        let run = ProcessRun {
            code: output.status.code().expect("process exit code"),
            stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        };
        assert_eq!(run.code, 2);
        let error = error_json(&run);
        assert_eq!(error["error"]["code"], "schema_violation");
        assert_eq!(error["error"]["operation_outcome"], "not_applied");
        assert_eq!(error["error"]["retry_safety"], "safe");
    }
}

#[test]
fn mutation_input_is_byte_bounded_utf8_and_exactly_one_document() {
    let oversized = vec![b' '; 1024 * 1024 + 1];
    let cases: Vec<(&str, &[u8], &str)> = vec![
        ("empty", b"", "invalid_json"),
        (
            "multiple documents",
            br#"{"body":"one"} {"body":"two"}"#,
            "invalid_json",
        ),
        ("invalid UTF-8", &[0xff], "invalid_json"),
        ("more than one MiB", &oversized, "invalid_input"),
    ];

    for (name, stdin, expected_code) in cases {
        let run = run_binary_bytes(["issue", "comment", "ACCL-1", "--input", "-"], stdin);
        assert_eq!(run.code, 2, "case: {name}");
        let error = error_json(&run);
        assert_eq!(error["error"]["code"], expected_code, "case: {name}");
        assert_eq!(
            error["error"]["operation_outcome"], "not_applied",
            "case: {name}"
        );
    }
}

struct CountingReader {
    bytes: Vec<u8>,
    position: usize,
    maximum: usize,
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(
            self.position < self.maximum,
            "production attempted an underlying read after the sentinel"
        );
        let available = self.bytes.len().saturating_sub(self.position);
        let count = available
            .min(buffer.len())
            .min(self.maximum - self.position);
        if count == 0 {
            return Ok(0);
        }
        buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[test]
#[should_panic(expected = "production attempted an underlying read after the sentinel")]
fn revised_fake_rejects_a_deliberately_unbounded_reader_harness() {
    let mut reader = CountingReader {
        bytes: vec![b'x'; 4],
        position: 0,
        maximum: 3,
    };
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
}

#[test]
fn strict_reader_observes_exact_sentinel_and_error_precedence() {
    const OVERHEAD: usize = r#"{"body":""}"#.len();
    let exact = format!(
        r#"{{"body":"{}"}}"#,
        "x".repeat(MAX_MUTATION_INPUT_BYTES - OVERHEAD)
    );
    assert_eq!(exact.len(), MAX_MUTATION_INPUT_BYTES);
    let parsed: CommentInput = read_json_input(&mut exact.as_bytes()).unwrap();
    assert_eq!(parsed.body.len(), MAX_MUTATION_INPUT_BYTES - OVERHEAD);

    let plus_one = format!(
        r#"{{"body":"{}"}}"#,
        "x".repeat(MAX_MUTATION_INPUT_BYTES + 1 - OVERHEAD)
    );
    assert_eq!(plus_one.len(), MAX_MUTATION_INPUT_BYTES + 1);
    let error = read_json_input::<CommentInput>(&mut plus_one.as_bytes()).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);

    let mut oversized = vec![b' '; MAX_MUTATION_INPUT_BYTES + 64];
    oversized[MAX_MUTATION_INPUT_BYTES] = 0xff;
    let mut reader = CountingReader {
        bytes: oversized,
        position: 0,
        maximum: MAX_MUTATION_INPUT_BYTES + 1,
    };
    let error = read_json_input::<CommentInput>(&mut reader).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(reader.position, MAX_MUTATION_INPUT_BYTES + 1);

    let duplicate =
        br#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x","summary":"y"}}"#;
    let error = read_json_input::<CreateIssueInput>(&mut duplicate.as_slice()).unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);

    let distinct = br#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x","description":"y"}}"#;
    let parsed = read_json_input::<CreateIssueInput>(&mut distinct.as_slice()).unwrap();
    assert_eq!(parsed.fields.len(), 2);
}

#[test]
fn mutation_typed_inputs_reject_missing_duplicate_and_forbidden_values() {
    for (args, stdin) in [
        (&["issue", "comment", "ACCL-1", "--input", "-"][..], r#"[]"#),
        (
            &["issue", "comment", "ACCL-1", "--input", "-"][..],
            r#"null"#,
        ),
        (&["issue", "comment", "ACCL-1", "--input", "-"][..], r#"{}"#),
        (
            &["issue", "comment", "ACCL-1", "--input", "-"][..],
            r#"{"body":"one","body":"two"}"#,
        ),
        (
            &["issue", "comment", "ACCL-1", "--input", "-"][..],
            r#"{"body":""}"#,
        ),
        (
            &["issue", "comment", "ACCL-1", "--input", "-"][..],
            r#"{"body":7}"#,
        ),
        (
            &["issue", "update", "ACCL-1", "--input", "-"][..],
            r#"{"set":{"summary":"one","summary":"two"}}"#,
        ),
        (
            &["issue", "update", "ACCL-1", "--input", "-"][..],
            r#"{"set":{}}"#,
        ),
        (
            &["issue", "create", "--input", "-"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x","project":{"key":"OTHER"}}}"#,
        ),
        (
            &["issue", "create", "--input", "-"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x","issuetype":{"id":"9"}}}"#,
        ),
        (
            &["issue", "create", "--input", "-"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{}}"#,
        ),
        (
            &["issue", "transition", "ACCL-1", "--input", "-"][..],
            r#"{}"#,
        ),
        (
            &["issue", "transition", "ACCL-1", "--input", "-"][..],
            r#"{"transition_id":" "}"#,
        ),
        (
            &["issue", "assign", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1"}"#,
        ),
        (
            &["issue", "assign", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"one","account_id":"two"}"#,
        ),
        (
            &["issue", "assign", "--input", "-"][..],
            r#"{"issue_key":"accl-1","account_id":"abc"}"#,
        ),
        (
            &["issue", "link", "add", "--input", "-"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":""}"#,
        ),
        (
            &["issue", "link", "add", "--input", "-"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"bad","type_name":"Blocks"}"#,
        ),
        (
            &["issue", "watcher", "add", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"bad\naccount"}"#,
        ),
        (
            &["issue", "watcher", "remove", "--input", "-"][..],
            r#"{"issue_key":"ACCL-0","account_id":"abc"}"#,
        ),
    ] {
        let output = Command::cargo_bin("jira-ops")
            .expect("jira-ops binary")
            .args(args)
            .write_stdin(stdin)
            .output()
            .expect("run jira-ops");
        let run = ProcessRun {
            code: output.status.code().expect("process exit code"),
            stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        };
        assert_eq!(run.code, 2);
        let error = error_json(&run);
        assert_eq!(error["error"]["code"], "schema_violation");
        assert_eq!(error["error"]["operation_outcome"], "not_applied");
    }
}

#[test]
fn comment_dry_run_preserves_plain_intent_and_needs_no_credentials() {
    let run = run_binary(
        ["issue", "comment", "ACCL-1", "--input", "-"],
        r#"{"body":"line one\nline two"}"#,
    );

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let value: Value = serde_json::from_str(&run.stdout).expect("valid success JSON");
    assert_eq!(
        value,
        serde_json::json!({
            "data": {
                "operation": "issue.comment",
                "applied": false,
                "target": {"issue": "ACCL-1"},
                "changes": {"body": "line one\nline two"},
                "validation": {"local": "passed", "metadata": "not_applicable"}
            }
        })
    );
}

#[test]
fn daily_core_plans_are_zero_request_exact_and_representation_stable() {
    let cases = [
        (
            &["issue", "assign", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":null}"#,
            serde_json::json!({"data":{"operation":"issue.assign","applied":false,"target":{"issue":"ACCL-1"},"changes":{"account_id":null},"validation":{"local":"passed","metadata":"not_applicable"}}}),
        ),
        (
            &["issue", "link", "add", "--input", "-"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}"#,
            serde_json::json!({"data":{"operation":"issue.link.add","applied":false,"target":{"inward_issue":"ACCL-1","outward_issue":"OPS-2"},"changes":{"type_name":"Blocks"},"validation":{"local":"passed","metadata":"not_applicable"}}}),
        ),
        (
            &["issue", "watcher", "add", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc +/=?"}"#,
            serde_json::json!({"data":{"operation":"issue.watcher.add","applied":false,"target":{"issue":"ACCL-1","account_id":"abc +/=?"},"changes":{"action":"add"},"validation":{"local":"passed","metadata":"not_applicable"}}}),
        ),
        (
            &["issue", "watcher", "remove", "--input", "-"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc +/=?"}"#,
            serde_json::json!({"data":{"operation":"issue.watcher.remove","applied":false,"target":{"issue":"ACCL-1","account_id":"abc +/=?"},"changes":{"action":"remove"},"validation":{"local":"passed","metadata":"not_applicable"}}}),
        ),
    ];

    for (command, stdin, expected) in cases {
        let mut baseline = None;
        for representation in [
            Representation::DefaultJson,
            Representation::ExplicitJson,
            Representation::PrettyJson,
            Representation::Toon,
        ] {
            let (runtime, state) = injected_runtime([]);
            let args = representation.arguments(command);
            let run = run_injected(&args, stdin, &runtime);
            assert_eq!(
                run.code, 0,
                "{command:?} {representation:?}: {}",
                run.stderr
            );
            assert_eq!(run.stderr, "");
            let value = representation.decode(&run.stdout);
            assert_eq!(value, expected, "{command:?} {representation:?}");
            if let Some(baseline) = &baseline {
                assert_eq!(&value, baseline);
            } else {
                baseline = Some(value);
            }
            assert!(state.borrow().requests.is_empty(), "{command:?}");
            assert!(state.borrow().responses.is_empty(), "{command:?}");
        }
    }
}

#[test]
fn daily_core_apply_uses_one_exact_request_and_no_readback() {
    let cases = [
        (
            &["issue", "assign", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
            204,
            "Put",
            "/rest/api/3/issue/ACCL-1/assignee",
            serde_json::json!({"accountId":"abc"}),
            serde_json::json!({"data":{"operation":"issue.assign","applied":true,"issue":{"key":"ACCL-1"},"assignment":{"account_id":"abc"}}}),
        ),
        (
            &["issue", "link", "add", "--input", "-", "--apply"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}"#,
            201,
            "Post",
            "/rest/api/3/issueLink",
            serde_json::json!({"type":{"name":"Blocks"},"inwardIssue":{"key":"ACCL-1"},"outwardIssue":{"key":"OPS-2"}}),
            serde_json::json!({"data":{"operation":"issue.link.add","applied":true,"link":{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}}}),
        ),
        (
            &["issue", "watcher", "add", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
            204,
            "Post",
            "/rest/api/3/issue/ACCL-1/watchers",
            serde_json::json!("abc"),
            serde_json::json!({"data":{"operation":"issue.watcher.add","applied":true,"issue":{"key":"ACCL-1"},"watcher":{"account_id":"abc"}}}),
        ),
        (
            &["issue", "watcher", "remove", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc +/=?"}"#,
            204,
            "Delete",
            "/rest/api/3/issue/ACCL-1/watchers?accountId=abc+%2B%2F%3D%3F",
            Value::Null,
            serde_json::json!({"data":{"operation":"issue.watcher.remove","applied":true,"issue":{"key":"ACCL-1"},"watcher":{"account_id":"abc +/=?"}}}),
        ),
    ];

    for (command, stdin, status, method, path, body, expected) in cases {
        let (runtime, state) = injected_runtime([Ok(injected_json_response(status, ""))]);
        let run = run_injected(command, stdin, &runtime);
        assert_eq!(run.code, 0, "{command:?}: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert_eq!(
            serde_json::from_str::<Value>(&run.stdout).unwrap(),
            expected
        );
        let state = state.borrow();
        assert_eq!(state.requests.len(), 1, "{command:?}");
        assert!(state.responses.is_empty(), "{command:?}");
        let request = &state.requests[0];
        assert_eq!(format!("{:?}", request.method), method, "{command:?}");
        assert!(request.url.as_str().ends_with(path), "{}", request.url);
        assert_eq!(request.effect, RequestEffect::JiraWrite);
        let actual_body = if request.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&request.body).unwrap()
        };
        assert_eq!(actual_body, body, "{command:?}");
    }
}

#[test]
fn daily_core_write_failures_are_conservative_private_and_one_attempt() {
    let operations = [
        (
            &["issue", "assign", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
            204,
        ),
        (
            &["issue", "link", "add", "--input", "-", "--apply"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}"#,
            201,
        ),
        (
            &["issue", "watcher", "add", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
            204,
        ),
        (
            &["issue", "watcher", "remove", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
            204,
        ),
    ];

    for (command, stdin, success_status) in operations {
        let cases = [
            (
                Ok(injected_json_response(
                    success_status,
                    r#"{"secret":"upstream"}"#,
                )),
                "mutation_response_invalid",
                "applied",
                "unsafe",
                Some(success_status),
            ),
            (
                Ok(injected_json_response(429, r#"{"secret":"upstream"}"#)),
                "rate_limited",
                "not_applied",
                "safe",
                Some(429),
            ),
            (
                Ok(injected_json_response(503, r#"{"secret":"upstream"}"#)),
                "mutation_outcome_unknown",
                "unknown",
                "unknown",
                Some(503),
            ),
            (
                Err(TransportFailure::new(
                    TransportFailureKind::Timeout,
                    DispatchPhase::DispatchStarted,
                    None,
                )),
                "mutation_outcome_unknown",
                "unknown",
                "unknown",
                None,
            ),
        ];
        for (first, code, outcome, retry, status) in cases {
            let (runtime, state) =
                injected_runtime([first, Ok(injected_json_response(success_status, ""))]);
            let run = run_injected(command, stdin, &runtime);
            assert_eq!(run.code, if code == "rate_limited" { 6 } else { 8 });
            assert_eq!(run.stdout, "");
            assert!(!run.stderr.contains("upstream"));
            assert!(!run.stderr.contains("secret"));
            let error: Value = serde_json::from_str(&run.stderr).unwrap();
            assert_eq!(error["error"]["code"], code, "{command:?}");
            assert_eq!(error["error"]["operation_outcome"], outcome);
            assert_eq!(error["error"]["retry_safety"], retry);
            assert_eq!(
                error["error"].get("status").and_then(Value::as_u64),
                status.map(u64::from)
            );
            assert_eq!(state.borrow().requests.len(), 1, "{command:?}");
            assert_eq!(state.borrow().responses.len(), 1, "{command:?}");
        }
    }
}

#[test]
fn read_only_apply_rejects_all_mutations_before_credentials_or_network() {
    for (args, stdin) in [
        (
            &["issue", "create", "--input", "-", "--apply"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
        ),
        (
            &["issue", "update", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"set":{"summary":"x"}}"#,
        ),
        (
            &["issue", "comment", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"body":"x"}"#,
        ),
        (
            &["issue", "transition", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"transition_id":"31"}"#,
        ),
        (
            &["project", "create", "--input", "-", "--apply"][..],
            r#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#,
        ),
        (
            &["issue", "assign", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":null}"#,
        ),
        (
            &["issue", "link", "add", "--input", "-", "--apply"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}"#,
        ),
        (
            &["issue", "watcher", "add", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
        ),
        (
            &["issue", "watcher", "remove", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
        ),
    ] {
        let output = Command::cargo_bin("jira-ops")
            .expect("jira-ops binary")
            .env("JIRA_READ_ONLY", "1")
            .env_remove("JIRA_SITE")
            .env_remove("JIRA_CLOUD_ID")
            .env_remove("JIRA_EMAIL")
            .env_remove("JIRA_API_TOKEN")
            .args(args)
            .write_stdin(stdin)
            .output()
            .expect("run jira-ops");
        let run = ProcessRun {
            code: output.status.code().expect("process exit code"),
            stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        };
        assert_eq!(run.code, 3, "args: {args:?}, stderr: {}", run.stderr);
        assert_eq!(run.stdout, "");
        let error = error_json(&run);
        assert_eq!(error["error"]["code"], "config_conflict");
        assert_eq!(error["error"]["operation_outcome"], "not_applied");
        assert_eq!(error["error"]["retry_safety"], "safe");
    }
}

#[test]
fn project_create_apply_has_output_and_one_request_parity() {
    let input = r#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#;
    let expected = serde_json::json!({"data":{
        "operation":"project.create",
        "outcome":"applied",
        "project":{"id":"10001","key":"OPSDEMO"}
    }});
    let mut default_json = None;
    let mut baseline_requests = None;
    for representation in [
        Representation::DefaultJson,
        Representation::ExplicitJson,
        Representation::PrettyJson,
        Representation::Toon,
    ] {
        let (runtime, state) = injected_runtime([Ok(injected_json_response(
            201,
            r#"{"id":"10001","key":"OPSDEMO","extra":"ignored"}"#,
        ))]);
        let args = representation.arguments(&["project", "create", "--input", "-", "--apply"]);
        let run = run_injected(&args, input, &runtime);
        assert_eq!(run.code, 0, "{representation:?}: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert_eq!(representation.decode(&run.stdout), expected);
        let requests = request_snapshots(&state.borrow());
        assert_eq!(requests.len(), 1);
        if let Some(expected) = &baseline_requests {
            assert_eq!(&requests, expected);
        } else {
            baseline_requests = Some(requests);
        }
        match representation {
            Representation::DefaultJson => default_json = Some(run.stdout),
            Representation::ExplicitJson => assert_eq!(Some(run.stdout), default_json),
            Representation::PrettyJson | Representation::Toon => {}
        }
    }
}

#[test]
fn project_create_plan_has_output_and_zero_request_parity() {
    let input = r#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123","description":"Local plan"}"#;
    let expected = serde_json::json!({"data":{
        "operation":"project.create",
        "method":"POST",
        "path":"/rest/api/3/project",
        "body":{
            "key":"OPSDEMO",
            "name":"Ops Demo",
            "projectTypeKey":"software",
            "projectTemplateKey":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic",
            "leadAccountId":"abc123",
            "assigneeType":"UNASSIGNED",
            "description":"Local plan"
        }
    }});
    let mut default_json = None;
    for representation in [
        Representation::DefaultJson,
        Representation::ExplicitJson,
        Representation::PrettyJson,
        Representation::Toon,
    ] {
        let (runtime, state) = injected_runtime([]);
        let args = representation.arguments(&["project", "create", "--input", "-"]);
        let run = run_injected(&args, input, &runtime);
        assert_eq!(run.code, 0, "{representation:?}: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert_eq!(representation.decode(&run.stdout), expected);
        assert!(state.borrow().requests.is_empty());
        match representation {
            Representation::DefaultJson => default_json = Some(run.stdout),
            Representation::ExplicitJson => assert_eq!(Some(run.stdout), default_json),
            Representation::PrettyJson | Representation::Toon => {}
        }
    }
}

#[test]
fn project_create_stdout_failure_is_applied_and_unsafe() {
    let (runtime, state) = injected_runtime([Ok(injected_json_response(
        201,
        r#"{"id":"10001","key":"OPSDEMO"}"#,
    ))]);
    let mut stderr = Vec::new();
    let code = jira_ops::run_mutation_with_runtime(
        ["project", "create", "--input", "-", "--apply"].map(OsString::from),
        &mut br#"{"key":"OPSDEMO","name":"Ops Demo","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"abc123"}"#.as_slice(),
        &mut FailingWriter,
        &mut stderr,
        &EmptyEnvironment,
        &runtime,
    );
    assert_eq!(code, std::process::ExitCode::from(8));
    let error: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error"]["code"], "mutation_response_invalid");
    assert_eq!(error["error"]["operation_outcome"], "applied");
    assert_eq!(error["error"]["retry_safety"], "unsafe");
    assert_eq!(state.borrow().requests.len(), 1);
}

struct ReadOnlyEnvironment {
    calls: Cell<usize>,
}

impl EnvironmentSource for ReadOnlyEnvironment {
    fn value(&self, key: &str) -> Option<OsString> {
        self.calls.set(self.calls.get() + 1);
        (key == "JIRA_READ_ONLY").then(|| OsString::from("1"))
    }
}

struct ForbiddenConfig;

impl ConfigStore for ForbiddenConfig {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
        panic!("config load must not occur")
    }

    fn atomic_replace(&self, _value: &SavedIdentity) -> Result<(), StoreError> {
        panic!("config write must not occur")
    }

    fn remove(&self) -> Result<(), StoreError> {
        panic!("config removal must not occur")
    }
}

struct ForbiddenCredentials;

impl CredentialStore for ForbiddenCredentials {
    fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
        panic!("credential read must not occur")
    }

    fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        panic!("credential write must not occur")
    }

    fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
        panic!("credential delete must not occur")
    }
}

struct ForbiddenTransport;

impl JiraTransport for ForbiddenTransport {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        panic!("transport must not occur")
    }
}

struct CountingRuntime {
    config_calls: Cell<usize>,
    credential_calls: Cell<usize>,
    transport_calls: Cell<usize>,
}

struct ConfigFailureRuntime {
    credential_calls: Cell<usize>,
    transport_calls: Cell<usize>,
}

impl MutationRuntime for ConfigFailureRuntime {
    type Config = ForbiddenConfig;
    type Credentials = ForbiddenCredentials;
    type Transport = ForbiddenTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        Err(AppError::new(
            ErrorCode::LocalStatePartial,
            "injected config failure",
            RetrySafety::Safe,
        ))
    }

    fn credentials(&self) -> Self::Credentials {
        self.credential_calls.set(self.credential_calls.get() + 1);
        ForbiddenCredentials
    }

    fn transport(&self) -> Self::Transport {
        self.transport_calls.set(self.transport_calls.get() + 1);
        ForbiddenTransport
    }
}

#[derive(Clone)]
struct MissingCredentials {
    gets: Rc<Cell<usize>>,
}

impl CredentialStore for MissingCredentials {
    fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
        self.gets.set(self.gets.get() + 1);
        Err(StoreError::NotFound)
    }

    fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        panic!("mutation runtime must not write credentials")
    }

    fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
        panic!("mutation runtime must not delete credentials")
    }
}

struct CredentialFailureRuntime {
    config: InjectedConfig,
    credentials: MissingCredentials,
}

impl MutationRuntime for CredentialFailureRuntime {
    type Config = InjectedConfig;
    type Credentials = MissingCredentials;
    type Transport = ForbiddenTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        Ok(self.config.clone())
    }

    fn credentials(&self) -> Self::Credentials {
        self.credentials.clone()
    }

    fn transport(&self) -> Self::Transport {
        ForbiddenTransport
    }
}

impl MutationRuntime for CountingRuntime {
    type Config = ForbiddenConfig;
    type Credentials = ForbiddenCredentials;
    type Transport = ForbiddenTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        self.config_calls.set(self.config_calls.get() + 1);
        Ok(ForbiddenConfig)
    }

    fn credentials(&self) -> Self::Credentials {
        self.credential_calls.set(self.credential_calls.get() + 1);
        ForbiddenCredentials
    }

    fn transport(&self) -> Self::Transport {
        self.transport_calls.set(self.transport_calls.get() + 1);
        ForbiddenTransport
    }
}

#[derive(Clone)]
struct IdentityValidationCredentials {
    gets: Rc<Cell<usize>>,
}

impl CredentialStore for IdentityValidationCredentials {
    fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
        self.gets.set(self.gets.get() + 1);
        panic!("invalid saved identity must not reach credential lookup")
    }

    fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        panic!("mutation runtime must not write credentials")
    }

    fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
        panic!("mutation runtime must not delete credentials")
    }
}

#[derive(Clone)]
struct IdentityValidationTransport {
    requests: Rc<Cell<usize>>,
}

impl JiraTransport for IdentityValidationTransport {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        self.requests.set(self.requests.get() + 1);
        panic!("invalid saved identity must not reach transport execution")
    }
}

struct IdentityValidationRuntime {
    config: InjectedConfig,
    credential_provider_calls: Cell<usize>,
    transport_provider_calls: Cell<usize>,
    credential_gets: Rc<Cell<usize>>,
    transport_requests: Rc<Cell<usize>>,
}

impl MutationRuntime for IdentityValidationRuntime {
    type Config = InjectedConfig;
    type Credentials = IdentityValidationCredentials;
    type Transport = IdentityValidationTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        Ok(self.config.clone())
    }

    fn credentials(&self) -> Self::Credentials {
        self.credential_provider_calls
            .set(self.credential_provider_calls.get() + 1);
        IdentityValidationCredentials {
            gets: Rc::clone(&self.credential_gets),
        }
    }

    fn transport(&self) -> Self::Transport {
        self.transport_provider_calls
            .set(self.transport_provider_calls.get() + 1);
        IdentityValidationTransport {
            requests: Rc::clone(&self.transport_requests),
        }
    }
}

#[test]
fn injected_read_only_guard_observes_zero_forbidden_service_calls() {
    for (args, stdin) in [
        (
            &["issue", "create", "--input", "-", "--apply"][..],
            r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
        ),
        (
            &["issue", "update", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"set":{"summary":"x"}}"#,
        ),
        (
            &["issue", "comment", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"body":"x"}"#,
        ),
        (
            &["issue", "transition", "ACCL-1", "--input", "-", "--apply"][..],
            r#"{"transition_id":"31"}"#,
        ),
        (
            &["issue", "assign", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":null}"#,
        ),
        (
            &["issue", "link", "add", "--input", "-", "--apply"][..],
            r#"{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}"#,
        ),
        (
            &["issue", "watcher", "add", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
        ),
        (
            &["issue", "watcher", "remove", "--input", "-", "--apply"][..],
            r#"{"issue_key":"ACCL-1","account_id":"abc"}"#,
        ),
    ] {
        let mut default_json = None;
        for representation in [
            Representation::DefaultJson,
            Representation::ExplicitJson,
            Representation::PrettyJson,
            Representation::Toon,
        ] {
            let environment = ReadOnlyEnvironment {
                calls: Cell::new(0),
            };
            let runtime = CountingRuntime {
                config_calls: Cell::new(0),
                credential_calls: Cell::new(0),
                transport_calls: Cell::new(0),
            };
            let formatted_args = representation.arguments(args);
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let code = jira_ops::run_mutation_with_runtime(
                formatted_args.iter().map(OsString::from),
                &mut stdin.as_bytes(),
                &mut stdout,
                &mut stderr,
                &environment,
                &runtime,
            );
            assert_eq!(code, std::process::ExitCode::from(3), "args: {args:?}");
            assert!(stdout.is_empty(), "args: {args:?}");
            let stderr = String::from_utf8(stderr).unwrap();
            assert_error_layout(representation, &stderr);
            let error = representation.decode(&stderr);
            assert_eq!(error["error"]["code"], "config_conflict");
            assert_eq!(error["error"]["operation_outcome"], "not_applied");
            assert_eq!(error["error"]["retry_safety"], "safe");
            assert_eq!(environment.calls.get(), 1);
            assert_eq!(runtime.config_calls.get(), 0);
            assert_eq!(runtime.credential_calls.get(), 0);
            assert_eq!(runtime.transport_calls.get(), 0);
            match representation {
                Representation::DefaultJson => default_json = Some(stderr),
                Representation::ExplicitJson => assert_eq!(Some(stderr), default_json),
                Representation::PrettyJson | Representation::Toon => {}
            }
        }
    }
}

#[test]
fn invalid_saved_identity_fails_before_provider_construction_or_write_dispatch() {
    let mut invalid_site = saved_identity();
    invalid_site.site = Url::parse("https://attacker.invalid/").unwrap();
    let mut invalid_email = saved_identity();
    invalid_email.email = "agent@example.com\nforged".to_owned();
    let mut invalid_account_id = saved_identity();
    invalid_account_id.account_id = String::new();

    for (field, identity) in [
        ("site", invalid_site),
        ("email", invalid_email),
        ("account ID", invalid_account_id),
    ] {
        for (args, stdin) in [
            (
                &["issue", "create", "--input", "-", "--apply"][..],
                r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
            ),
            (
                &["issue", "update", "ACCL-1", "--input", "-", "--apply"][..],
                r#"{"set":{"summary":"x"}}"#,
            ),
            (
                &["issue", "comment", "ACCL-1", "--input", "-", "--apply"][..],
                r#"{"body":"hello"}"#,
            ),
            (
                &["issue", "transition", "ACCL-1", "--input", "-", "--apply"][..],
                r#"{"transition_id":"31"}"#,
            ),
        ] {
            let runtime = IdentityValidationRuntime {
                config: InjectedConfig(Some(identity.clone())),
                credential_provider_calls: Cell::new(0),
                transport_provider_calls: Cell::new(0),
                credential_gets: Rc::new(Cell::new(0)),
                transport_requests: Rc::new(Cell::new(0)),
            };

            let run = run_injected(args, stdin, &runtime);

            assert_eq!(run.code, 3, "{field} {args:?}: {}", run.stderr);
            assert_eq!(run.stdout, "", "{field} {args:?}");
            assert_eq!(run.stderr.matches('\n').count(), 1, "{field} {args:?}");
            let error: Value = serde_json::from_str(&run.stderr).unwrap();
            assert_eq!(error["error"]["code"], "local_state_partial");
            assert_eq!(error["error"]["operation_outcome"], "not_applied");
            assert_eq!(error["error"]["retry_safety"], "safe");
            assert_eq!(
                runtime.credential_provider_calls.get(),
                0,
                "{field} {args:?}"
            );
            assert_eq!(
                runtime.transport_provider_calls.get(),
                0,
                "{field} {args:?}"
            );
            assert_eq!(runtime.credential_gets.get(), 0, "{field} {args:?}");
            assert_eq!(runtime.transport_requests.get(), 0, "{field} {args:?}");
        }
    }
}

#[test]
fn injected_config_and_credential_failures_are_stderr_only_and_not_applied() {
    let command = ["issue", "comment", "ACCL-1", "--input", "-", "--apply"];
    let mut default_config_json = None;
    let mut default_credential_json = None;
    for representation in [
        Representation::DefaultJson,
        Representation::ExplicitJson,
        Representation::PrettyJson,
        Representation::Toon,
    ] {
        let args = representation.arguments(&command);
        let config_runtime = ConfigFailureRuntime {
            credential_calls: Cell::new(0),
            transport_calls: Cell::new(0),
        };
        let config_run = run_injected(&args, r#"{"body":"hello"}"#, &config_runtime);
        assert_pre_dispatch_failure(&config_run, representation, 3, "local_state_partial");
        assert_eq!(config_runtime.credential_calls.get(), 0);
        assert_eq!(config_runtime.transport_calls.get(), 0);
        match representation {
            Representation::DefaultJson => default_config_json = Some(config_run.stderr),
            Representation::ExplicitJson => {
                assert_eq!(Some(config_run.stderr), default_config_json)
            }
            Representation::PrettyJson | Representation::Toon => {}
        }

        let credential_gets = Rc::new(Cell::new(0));
        let credential_runtime = CredentialFailureRuntime {
            config: InjectedConfig(Some(saved_identity())),
            credentials: MissingCredentials {
                gets: Rc::clone(&credential_gets),
            },
        };
        let credential_run = run_injected(&args, r#"{"body":"hello"}"#, &credential_runtime);
        assert_pre_dispatch_failure(&credential_run, representation, 4, "auth_missing");
        assert_eq!(credential_gets.get(), 1);
        match representation {
            Representation::DefaultJson => default_credential_json = Some(credential_run.stderr),
            Representation::ExplicitJson => {
                assert_eq!(Some(credential_run.stderr), default_credential_json)
            }
            Representation::PrettyJson | Representation::Toon => {}
        }
    }
}

fn assert_pre_dispatch_failure(
    run: &ProcessRun,
    representation: Representation,
    exit: i32,
    code: &str,
) {
    assert_eq!(run.code, exit, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_error_layout(representation, &run.stderr);
    let error = representation.decode(&run.stderr);
    assert_eq!(error["error"]["code"], code);
    assert_eq!(error["error"]["operation_outcome"], "not_applied");
    assert_eq!(error["error"]["retry_safety"], "safe");
}

#[derive(Clone)]
struct InjectedConfig(Option<SavedIdentity>);

impl ConfigStore for InjectedConfig {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
        Ok(self.0.clone())
    }

    fn atomic_replace(&self, _value: &SavedIdentity) -> Result<(), StoreError> {
        panic!("mutation runtime must not write config")
    }

    fn remove(&self) -> Result<(), StoreError> {
        panic!("mutation runtime must not remove config")
    }
}

#[derive(Clone)]
struct InjectedCredentials {
    gets: Rc<Cell<usize>>,
}

impl CredentialStore for InjectedCredentials {
    fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
        self.gets.set(self.gets.get() + 1);
        Ok(SecretString::from("scoped-test-token"))
    }

    fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        panic!("mutation runtime must not write credentials")
    }

    fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
        panic!("mutation runtime must not delete credentials")
    }
}

struct InjectedTransportState {
    requests: Vec<HttpRequest>,
    responses: VecDeque<Result<HttpResponse, TransportFailure>>,
}

#[derive(Debug, Eq, PartialEq)]
struct RequestSnapshot {
    method: HttpMethod,
    url: String,
    headers: Vec<(String, String)>,
    body: Value,
    effect: RequestEffect,
}

fn request_snapshots(state: &InjectedTransportState) -> Vec<RequestSnapshot> {
    state
        .requests
        .iter()
        .map(|request| RequestSnapshot {
            method: request.method,
            url: request.url.as_str().to_owned(),
            headers: request
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), value.expose_secret().to_owned()))
                .collect(),
            body: serde_json::from_slice(&request.body).unwrap_or(Value::Null),
            effect: request.effect,
        })
        .collect()
}

#[derive(Clone)]
struct InjectedTransport(Rc<RefCell<InjectedTransportState>>);

impl JiraTransport for InjectedTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        let mut state = self.0.borrow_mut();
        state.requests.push(request);
        state.responses.pop_front().expect("injected response")
    }
}

#[derive(Clone)]
struct InjectedRuntime {
    config: InjectedConfig,
    credentials: InjectedCredentials,
    transport: InjectedTransport,
}

impl MutationRuntime for InjectedRuntime {
    type Config = InjectedConfig;
    type Credentials = InjectedCredentials;
    type Transport = InjectedTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        Ok(self.config.clone())
    }

    fn credentials(&self) -> Self::Credentials {
        self.credentials.clone()
    }

    fn transport(&self) -> Self::Transport {
        self.transport.clone()
    }
}

#[derive(Default)]
struct EmptyEnvironment;

impl EnvironmentSource for EmptyEnvironment {
    fn value(&self, _key: &str) -> Option<OsString> {
        None
    }
}

fn injected_runtime(
    responses: impl IntoIterator<Item = Result<HttpResponse, TransportFailure>>,
) -> (InjectedRuntime, Rc<RefCell<InjectedTransportState>>) {
    let state = Rc::new(RefCell::new(InjectedTransportState {
        requests: Vec::new(),
        responses: responses.into_iter().collect(),
    }));
    (
        InjectedRuntime {
            config: InjectedConfig(Some(saved_identity())),
            credentials: InjectedCredentials {
                gets: Rc::new(Cell::new(0)),
            },
            transport: InjectedTransport(Rc::clone(&state)),
        },
        state,
    )
}

fn saved_identity() -> SavedIdentity {
    SavedIdentity {
        site: Url::parse("https://example.atlassian.net/").unwrap(),
        cloud_id: Uuid::nil(),
        email: "agent@example.com".to_owned(),
        account_id: "abc123".to_owned(),
        default_project: None,
        default_board: None,
    }
}

fn injected_json_response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: body.as_bytes().to_vec(),
    }
}

struct MutationProcessCase {
    command: Vec<&'static str>,
    stdin: &'static str,
    responses: Vec<Result<HttpResponse, TransportFailure>>,
    expected: Value,
}

fn mutation_process_case(operation: &str, apply: bool) -> MutationProcessCase {
    match (operation, apply) {
        ("create", true) => MutationProcessCase {
            command: vec!["issue", "create", "--input", "-", "--apply"],
            stdin: r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
            responses: vec![
                Ok(injected_json_response(
                    200,
                    r#"{"startAt":0,"total":1,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}] }"#,
                )),
                Ok(injected_json_response(
                    201,
                    r#"{"id":"10001","key":"ACCL-1","extra":true}"#,
                )),
            ],
            expected: serde_json::json!({"data":{"operation":"issue.create","applied":true,"issue":{"id":"10001","key":"ACCL-1","url":"https://example.atlassian.net/browse/ACCL-1"}}}),
        },
        ("create", false) => MutationProcessCase {
            command: vec!["issue", "create", "--input", "-"],
            stdin: r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
            responses: vec![Ok(injected_json_response(
                200,
                r#"{"startAt":0,"total":1,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}] }"#,
            ))],
            expected: serde_json::json!({"data":{"operation":"issue.create","applied":false,"target":{"project_key":"ACCL","issue_type_id":"10001"},"changes":{"fields":{"summary":"x"}},"validation":{"local":"passed","metadata":"passed"}}}),
        },
        ("update", true) => MutationProcessCase {
            command: vec!["issue", "update", "ACCL-1", "--input", "-", "--apply"],
            stdin: r#"{"set":{"summary":"x"}}"#,
            responses: vec![
                Ok(injected_json_response(
                    200,
                    r#"{"fields":{"summary":{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}}}"#,
                )),
                Ok(injected_json_response(204, "")),
            ],
            expected: serde_json::json!({"data":{"operation":"issue.update","applied":true,"issue":{"key":"ACCL-1"}}}),
        },
        ("update", false) => MutationProcessCase {
            command: vec!["issue", "update", "ACCL-1", "--input", "-"],
            stdin: r#"{"set":{"summary":"x"}}"#,
            responses: vec![Ok(injected_json_response(
                200,
                r#"{"fields":{"summary":{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}}}"#,
            ))],
            expected: serde_json::json!({"data":{"operation":"issue.update","applied":false,"target":{"issue":"ACCL-1"},"changes":{"set":{"summary":"x"}},"validation":{"local":"passed","metadata":"passed"}}}),
        },
        ("comment", true) => MutationProcessCase {
            command: vec!["issue", "comment", "ACCL-1", "--input", "-", "--apply"],
            stdin: r#"{"body":"hello"}"#,
            responses: vec![Ok(injected_json_response(201, r#"{"id":"20001"}"#))],
            expected: serde_json::json!({"data":{"operation":"issue.comment","applied":true,"issue":{"key":"ACCL-1"},"comment":{"id":"20001"}}}),
        },
        ("comment", false) => MutationProcessCase {
            command: vec!["issue", "comment", "ACCL-1", "--input", "-"],
            stdin: r#"{"body":"hello"}"#,
            responses: Vec::new(),
            expected: serde_json::json!({"data":{"operation":"issue.comment","applied":false,"target":{"issue":"ACCL-1"},"changes":{"body":"hello"},"validation":{"local":"passed","metadata":"not_applicable"}}}),
        },
        ("transition", true) => MutationProcessCase {
            command: vec!["issue", "transition", "ACCL-1", "--input", "-", "--apply"],
            stdin: r#"{"transition_id":"31"}"#,
            responses: vec![
                Ok(injected_json_response(
                    200,
                    r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"3","name":"Done"},"fields":{}}]}"#,
                )),
                Ok(injected_json_response(204, "")),
            ],
            expected: serde_json::json!({"data":{"operation":"issue.transition","applied":true,"issue":{"key":"ACCL-1"}}}),
        },
        ("transition", false) => MutationProcessCase {
            command: vec!["issue", "transition", "ACCL-1", "--input", "-"],
            stdin: r#"{"transition_id":"31"}"#,
            responses: vec![Ok(injected_json_response(
                200,
                r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"3","name":"Done"},"fields":{}}]}"#,
            ))],
            expected: serde_json::json!({"data":{"operation":"issue.transition","applied":false,"target":{"issue":"ACCL-1"},"changes":{"transition_id":"31","fields":{}},"validation":{"local":"passed","metadata":"passed"}}}),
        },
        _ => unreachable!(),
    }
}

fn run_injected<R: MutationRuntime>(args: &[&str], stdin: &str, runtime: &R) -> ProcessRun {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = jira_ops::run_mutation_with_runtime(
        args.iter().map(OsString::from),
        &mut stdin.as_bytes(),
        &mut stdout,
        &mut stderr,
        &EmptyEnvironment,
        runtime,
    );
    ProcessRun {
        code: [0_u8, 2, 3, 4, 5, 6, 7, 8, 70]
            .into_iter()
            .find(|candidate| code == std::process::ExitCode::from(*candidate))
            .map(i32::from)
            .expect("stable process exit"),
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

#[test]
fn all_mutation_successes_and_dry_runs_have_json_toon_request_parity() {
    for operation in ["create", "update", "comment", "transition"] {
        for apply in [false, true] {
            let case = mutation_process_case(operation, apply);
            let mut default_json = None;
            let mut baseline_requests = None;
            for representation in [
                Representation::DefaultJson,
                Representation::ExplicitJson,
                Representation::PrettyJson,
                Representation::Toon,
            ] {
                let (runtime, state) = injected_runtime(case.responses.clone());
                let args = representation.arguments(&case.command);
                let run = run_injected(&args, case.stdin, &runtime);
                assert_eq!(
                    run.code, 0,
                    "{operation} apply={apply} {representation:?}: {}",
                    run.stderr
                );
                assert_eq!(run.stderr, "");
                assert_eq!(representation.decode(&run.stdout), case.expected);
                if representation == Representation::Toon {
                    assert!(!run.stdout.starts_with('{'));
                    assert!(run.stdout.ends_with('\n'));
                    assert!(!run.stdout.ends_with("\n\n"));
                }
                let requests = request_snapshots(&state.borrow());
                let expected_request_count = match (operation, apply) {
                    ("comment", false) => 0,
                    ("comment", true) | (_, false) => 1,
                    (_, true) => 2,
                };
                assert_eq!(requests.len(), expected_request_count);
                if apply {
                    assert_eq!(
                        requests.last().map(|request| request.effect),
                        Some(RequestEffect::JiraWrite)
                    );
                } else {
                    assert!(
                        requests
                            .iter()
                            .all(|request| request.effect != RequestEffect::JiraWrite)
                    );
                }
                if let Some(expected) = &baseline_requests {
                    assert_eq!(&requests, expected, "{operation} apply={apply}");
                } else {
                    baseline_requests = Some(requests);
                }
                assert!(state.borrow().responses.is_empty());
                match representation {
                    Representation::DefaultJson => default_json = Some(run.stdout),
                    Representation::ExplicitJson => {
                        assert_eq!(Some(run.stdout), default_json, "{operation} apply={apply}")
                    }
                    Representation::PrettyJson | Representation::Toon => {}
                }
            }
        }
    }
}

#[test]
fn applied_mutations_emit_exact_json_and_preserve_call_order() {
    for pretty in [false, true] {
        let pretty_arg = pretty.then_some("--pretty");
        for operation in ["create", "update", "comment", "transition"] {
            let (args, stdin, responses, expected, methods, effects, path_suffixes) =
                match operation {
                    "create" => (
                        vec!["issue", "create", "--input", "-", "--apply"],
                        r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
                        vec![
                            Ok(injected_json_response(
                                200,
                                r#"{"startAt":0,"total":1,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}] }"#,
                            )),
                            Ok(injected_json_response(
                                201,
                                r#"{"id":"10001","key":"ACCL-1","extra":true}"#,
                            )),
                        ],
                        serde_json::json!({"data":{"operation":"issue.create","applied":true,"issue":{"id":"10001","key":"ACCL-1","url":"https://example.atlassian.net/browse/ACCL-1"}}}),
                        vec![HttpMethod::Get, HttpMethod::Post],
                        vec![RequestEffect::Read, RequestEffect::JiraWrite],
                        vec![
                            "/rest/api/3/issue/createmeta/ACCL/issuetypes/10001?maxResults=100",
                            "/rest/api/3/issue",
                        ],
                    ),
                    "update" => (
                        vec!["issue", "update", "ACCL-1", "--input", "-", "--apply"],
                        r#"{"set":{"summary":"x"}}"#,
                        vec![
                            Ok(injected_json_response(
                                200,
                                r#"{"fields":{"summary":{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}}}"#,
                            )),
                            Ok(injected_json_response(204, "")),
                        ],
                        serde_json::json!({"data":{"operation":"issue.update","applied":true,"issue":{"key":"ACCL-1"}}}),
                        vec![HttpMethod::Get, HttpMethod::Put],
                        vec![RequestEffect::Read, RequestEffect::JiraWrite],
                        vec![
                            "/rest/api/3/issue/ACCL-1/editmeta",
                            "/rest/api/3/issue/ACCL-1",
                        ],
                    ),
                    "comment" => (
                        vec!["issue", "comment", "ACCL-1", "--input", "-", "--apply"],
                        r#"{"body":"hello"}"#,
                        vec![Ok(injected_json_response(201, r#"{"id":"20001"}"#))],
                        serde_json::json!({"data":{"operation":"issue.comment","applied":true,"issue":{"key":"ACCL-1"},"comment":{"id":"20001"}}}),
                        vec![HttpMethod::Post],
                        vec![RequestEffect::JiraWrite],
                        vec!["/rest/api/3/issue/ACCL-1/comment"],
                    ),
                    "transition" => (
                        vec!["issue", "transition", "ACCL-1", "--input", "-", "--apply"],
                        r#"{"transition_id":"31"}"#,
                        vec![
                            Ok(injected_json_response(
                                200,
                                r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"3","name":"Done"},"fields":{}}]}"#,
                            )),
                            Ok(injected_json_response(204, "")),
                        ],
                        serde_json::json!({"data":{"operation":"issue.transition","applied":true,"issue":{"key":"ACCL-1"}}}),
                        vec![HttpMethod::Get, HttpMethod::Post],
                        vec![RequestEffect::Read, RequestEffect::JiraWrite],
                        vec![
                            "/rest/api/3/issue/ACCL-1/transitions?expand=transitions.fields",
                            "/rest/api/3/issue/ACCL-1/transitions",
                        ],
                    ),
                    _ => unreachable!(),
                };
            let mut args = args;
            if let Some(pretty_arg) = pretty_arg {
                args.insert(0, pretty_arg);
            }
            let (runtime, state) = injected_runtime(responses);
            let run = run_injected(&args, stdin, &runtime);
            assert_eq!(run.code, 0, "{operation} pretty={pretty}: {}", run.stderr);
            assert_eq!(run.stderr, "");
            assert_eq!(
                serde_json::from_str::<Value>(&run.stdout).unwrap(),
                expected
            );
            if pretty {
                assert!(run.stdout.matches('\n').count() > 1);
            } else {
                assert_eq!(run.stdout.matches('\n').count(), 1);
            }
            let state = state.borrow();
            assert_eq!(state.requests.len(), methods.len());
            assert_eq!(
                state
                    .requests
                    .iter()
                    .map(|request| request.method)
                    .collect::<Vec<_>>(),
                methods
            );
            assert_eq!(
                state
                    .requests
                    .iter()
                    .map(|request| request.effect)
                    .collect::<Vec<_>>(),
                effects
            );
            for (request, suffix) in state.requests.iter().zip(path_suffixes) {
                assert!(request.url.as_str().ends_with(suffix), "{}", request.url);
            }
            assert!(state.responses.is_empty());
        }
    }
}

#[test]
fn classified_write_failures_survive_process_dispatch() {
    let cases = [
        (
            Err(TransportFailure::new(
                TransportFailureKind::Timeout,
                DispatchPhase::DispatchStarted,
                None,
            )),
            8,
            "mutation_outcome_unknown",
            "unknown",
            "unknown",
            None::<u16>,
        ),
        (
            Ok(HttpResponse {
                status: 429,
                headers: BTreeMap::from([
                    ("retry-after".to_owned(), "2".to_owned()),
                    ("ratelimit-reason".to_owned(), "current".to_owned()),
                ]),
                body: br#"{"secret":"must-not-leak"}"#.to_vec(),
            }),
            6,
            "rate_limited",
            "not_applied",
            "safe",
            Some(429_u16),
        ),
        (
            Ok(injected_json_response(503, r#"{"secret":"must-not-leak"}"#)),
            8,
            "mutation_outcome_unknown",
            "unknown",
            "unknown",
            Some(503_u16),
        ),
        (
            Ok(injected_json_response(201, r#"{"id":"truncated""#)),
            8,
            "mutation_response_invalid",
            "applied",
            "unsafe",
            Some(201_u16),
        ),
    ];

    for (response, exit, code, outcome, retry, status) in cases {
        let (runtime, state) = injected_runtime([response]);
        let run = run_injected(
            &["issue", "comment", "ACCL-1", "--input", "-", "--apply"],
            r#"{"body":"request-secret"}"#,
            &runtime,
        );
        assert_eq!(run.code, exit, "{}", run.stderr);
        assert_eq!(run.stdout, "");
        assert_eq!(run.stderr.matches('\n').count(), 1);
        assert!(!run.stderr.contains("request-secret"));
        assert!(!run.stderr.contains("must-not-leak"));
        let value: Value = serde_json::from_str(&run.stderr).unwrap();
        assert_eq!(value["error"]["code"], code);
        assert_eq!(value["error"]["operation_outcome"], outcome);
        assert_eq!(value["error"]["retry_safety"], retry);
        assert_eq!(
            value["error"].get("status").and_then(Value::as_u64),
            status.map(u64::from)
        );
        assert_eq!(state.borrow().requests.len(), 1);
        assert_eq!(state.borrow().requests[0].effect, RequestEffect::JiraWrite);
        assert!(state.borrow().responses.is_empty());
    }
}

#[test]
fn classified_write_failures_have_json_toon_stream_and_request_parity() {
    let cases = vec![
        (
            "rate limit",
            Ok(HttpResponse {
                status: 429,
                headers: BTreeMap::from([
                    ("retry-after".to_owned(), "2".to_owned()),
                    ("ratelimit-reason".to_owned(), "current".to_owned()),
                ]),
                body: br#"{"secret":"must-not-leak"}"#.to_vec(),
            }),
            6,
            "rate_limited",
            "not_applied",
            "safe",
            Some(429_u64),
        ),
        (
            "timeout",
            Err(TransportFailure::new(
                TransportFailureKind::Timeout,
                DispatchPhase::DispatchStarted,
                None,
            )),
            8,
            "mutation_outcome_unknown",
            "unknown",
            "unknown",
            None,
        ),
        (
            "503",
            Ok(injected_json_response(503, r#"{"secret":"must-not-leak"}"#)),
            8,
            "mutation_outcome_unknown",
            "unknown",
            "unknown",
            Some(503_u64),
        ),
        (
            "malformed success",
            Ok(injected_json_response(201, r#"{"id":"truncated""#)),
            8,
            "mutation_response_invalid",
            "applied",
            "unsafe",
            Some(201_u64),
        ),
    ];

    for (name, first, exit, code, outcome, retry, status) in cases {
        let mut default_json = None;
        let mut baseline_value = None;
        let mut baseline_requests = None;
        for representation in [
            Representation::DefaultJson,
            Representation::ExplicitJson,
            Representation::PrettyJson,
            Representation::Toon,
        ] {
            let (runtime, state) = injected_runtime([
                first.clone(),
                Ok(injected_json_response(201, r#"{"id":"second"}"#)),
            ]);
            let args = representation
                .arguments(&["issue", "comment", "ACCL-1", "--input", "-", "--apply"]);
            let run = run_injected(&args, r#"{"body":"request-secret"}"#, &runtime);
            assert_eq!(run.code, exit, "{name} {representation:?}: {}", run.stderr);
            assert_eq!(run.stdout, "");
            assert!(!run.stderr.contains("request-secret"));
            assert!(!run.stderr.contains("must-not-leak"));
            assert_error_layout(representation, &run.stderr);
            let value = representation.decode(&run.stderr);
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["error"]["operation_outcome"], outcome);
            assert_eq!(value["error"]["retry_safety"], retry);
            assert_eq!(value["error"].get("status").and_then(Value::as_u64), status);
            if name == "rate limit" {
                assert_eq!(value["error"]["retry_after_ms"], 2_000);
                assert_eq!(value["error"]["rate_limit_reason"], "current");
            }
            if let Some(expected) = &baseline_value {
                assert_eq!(&value, expected, "{name}");
            } else {
                baseline_value = Some(value);
            }
            let requests = request_snapshots(&state.borrow());
            if let Some(expected) = &baseline_requests {
                assert_eq!(&requests, expected, "{name}");
            } else {
                baseline_requests = Some(requests);
            }
            assert_eq!(state.borrow().requests.len(), 1);
            assert_eq!(state.borrow().responses.len(), 1);
            match representation {
                Representation::DefaultJson => default_json = Some(run.stderr),
                Representation::ExplicitJson => assert_eq!(Some(run.stderr), default_json),
                Representation::PrettyJson | Representation::Toon => {}
            }
        }
    }
}

#[test]
fn invalid_create_and_comment_successes_have_json_toon_no_retry_parity() {
    for (operation, body) in [
        ("create", "not-json"),
        ("create", r#"{"id":"10001","key":"ACCL-1""#),
        ("comment", "not-json"),
        ("comment", r#"{"id":"20001""#),
    ] {
        let (command, stdin, prefix, expected_requests) = if operation == "create" {
            (
                vec!["issue", "create", "--input", "-", "--apply"],
                r#"{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"x"}}"#,
                vec![Ok(injected_json_response(
                    200,
                    r#"{"startAt":0,"total":1,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}] }"#,
                ))],
                2,
            )
        } else {
            (
                vec!["issue", "comment", "ACCL-1", "--input", "-", "--apply"],
                r#"{"body":"request-secret"}"#,
                Vec::new(),
                1,
            )
        };
        let mut baseline_value = None;
        let mut baseline_requests = None;
        let mut default_json = None;
        for representation in [
            Representation::DefaultJson,
            Representation::ExplicitJson,
            Representation::PrettyJson,
            Representation::Toon,
        ] {
            let mut responses = prefix.clone();
            responses.push(Ok(injected_json_response(201, body)));
            responses.push(Ok(injected_json_response(
                201,
                if operation == "create" {
                    r#"{"id":"second","key":"ACCL-2"}"#
                } else {
                    r#"{"id":"second"}"#
                },
            )));
            let (runtime, state) = injected_runtime(responses);
            let args = representation.arguments(&command);
            let run = run_injected(&args, stdin, &runtime);
            assert_eq!(run.code, 8, "{operation} {body:?}: {}", run.stderr);
            assert_eq!(run.stdout, "");
            assert!(!run.stderr.contains("request-secret"));
            assert_error_layout(representation, &run.stderr);
            let value = representation.decode(&run.stderr);
            assert_eq!(value["error"]["code"], "mutation_response_invalid");
            assert_eq!(value["error"]["status"], 201);
            assert_eq!(value["error"]["operation_outcome"], "applied");
            assert_eq!(value["error"]["retry_safety"], "unsafe");
            if let Some(expected) = &baseline_value {
                assert_eq!(&value, expected);
            } else {
                baseline_value = Some(value);
            }
            let requests = request_snapshots(&state.borrow());
            if let Some(expected) = &baseline_requests {
                assert_eq!(&requests, expected);
            } else {
                baseline_requests = Some(requests);
            }
            assert_eq!(state.borrow().requests.len(), expected_requests);
            assert_eq!(state.borrow().responses.len(), 1);
            match representation {
                Representation::DefaultJson => default_json = Some(run.stderr),
                Representation::ExplicitJson => assert_eq!(Some(run.stderr), default_json),
                Representation::PrettyJson | Representation::Toon => {}
            }
        }
    }
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected stdout failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_failure_preserves_whether_the_mutation_was_applied() {
    let mut default_applied_json = None;
    let mut default_dry_json = None;
    let mut baseline_applied = None;
    let mut baseline_dry = None;
    for representation in [
        Representation::DefaultJson,
        Representation::ExplicitJson,
        Representation::PrettyJson,
        Representation::Toon,
    ] {
        let (runtime, state) =
            injected_runtime([Ok(injected_json_response(201, r#"{"id":"20001"}"#))]);
        let args =
            representation.arguments(&["issue", "comment", "ACCL-1", "--input", "-", "--apply"]);
        let mut stderr = Vec::new();
        let code = jira_ops::run_mutation_with_runtime(
            args.iter().map(OsString::from),
            &mut br#"{"body":"hello"}"#.as_slice(),
            &mut FailingWriter,
            &mut stderr,
            &EmptyEnvironment,
            &runtime,
        );
        assert_eq!(code, std::process::ExitCode::from(8));
        let stderr = String::from_utf8(stderr).unwrap();
        assert_error_layout(representation, &stderr);
        let error = representation.decode(&stderr);
        assert_eq!(error["error"]["code"], "mutation_response_invalid");
        assert_eq!(error["error"]["operation_outcome"], "applied");
        assert_eq!(error["error"]["retry_safety"], "unsafe");
        assert_eq!(
            error["error"]["message"],
            "Jira applied the mutation but the CLI could not write the success output"
        );
        if let Some(expected) = &baseline_applied {
            assert_eq!(&error, expected);
        } else {
            baseline_applied = Some(error);
        }
        assert_eq!(state.borrow().requests.len(), 1);
        assert!(state.borrow().responses.is_empty());
        match representation {
            Representation::DefaultJson => default_applied_json = Some(stderr),
            Representation::ExplicitJson => assert_eq!(Some(stderr), default_applied_json),
            Representation::PrettyJson | Representation::Toon => {}
        }

        let (runtime, state) = injected_runtime([]);
        let args = representation.arguments(&["issue", "comment", "ACCL-1", "--input", "-"]);
        let mut stderr = Vec::new();
        let code = jira_ops::run_mutation_with_runtime(
            args.iter().map(OsString::from),
            &mut br#"{"body":"hello"}"#.as_slice(),
            &mut FailingWriter,
            &mut stderr,
            &EmptyEnvironment,
            &runtime,
        );
        assert_eq!(code, std::process::ExitCode::from(70));
        let stderr = String::from_utf8(stderr).unwrap();
        assert_error_layout(representation, &stderr);
        let error = representation.decode(&stderr);
        assert_eq!(error["error"]["code"], "internal");
        assert_eq!(error["error"]["operation_outcome"], "not_applied");
        assert_eq!(error["error"]["retry_safety"], "safe");
        if let Some(expected) = &baseline_dry {
            assert_eq!(&error, expected);
        } else {
            baseline_dry = Some(error);
        }
        assert!(state.borrow().requests.is_empty());
        match representation {
            Representation::DefaultJson => default_dry_json = Some(stderr),
            Representation::ExplicitJson => assert_eq!(Some(stderr), default_dry_json),
            Representation::PrettyJson | Representation::Toon => {}
        }
    }
}

#[test]
fn create_hierarchy_required_parent_fails_at_the_public_process_boundary_before_post() {
    let (runtime, state) = injected_runtime([Ok(injected_json_response(
        200,
        include_str!("fixtures/create_meta_subtask_parent.json"),
    ))]);
    let run = run_injected(
        &["issue", "create", "--input", "-", "--apply"],
        r#"{"project_key":"ACCL","issue_type_id":"10004","fields":{"summary":"Missing parent"}}"#,
        &runtime,
    );

    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    let error: Value = serde_json::from_str(&run.stderr).expect("public JSON error");
    assert_eq!(error["error"]["code"], "schema_violation");
    assert_eq!(error["error"]["operation_outcome"], "not_applied");
    let requests = request_snapshots(&state.borrow());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[0].effect, RequestEffect::Read);
    assert!(state.borrow().responses.is_empty());
}
