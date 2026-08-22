use std::collections::{BTreeMap, BTreeSet};

use assert_cmd::Command;
use jira_ops::commands::assignment::validate_assignment_input;
use jira_ops::commands::link::validate_link_input;
use jira_ops::commands::read_json_input;
use jira_ops::commands::watcher::validate_watcher_input;
use jira_ops::model::{
    AssignmentInput, CommentInput, CountMeta, CreateIssueInput, LinkInput, LinkItem, LinkTypeItem,
    LinkedIssue, ProjectCreateInput, TransitionInput, UpdateIssueInput, WatcherInput, WatcherItem,
};
use jira_ops::output::{SuccessEnvelope, Warning};
use jira_ops::schema::command_specs;
use serde_json::{Value, json};

const STRICT_ISSUE_KEY_PATTERN: &str = r"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$";
const NONBLANK_NO_CONTROL_PATTERN: &str = r"^(?=.*\S)[^\x00-\x1F\x7F-\x9F]+$";

struct ProcessRun {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> ProcessRun {
    let output = Command::cargo_bin("jira-ops")
        .expect("jira-ops binary")
        .args(args)
        .output()
        .expect("run jira-ops");

    ProcessRun {
        code: output.status.code().expect("process exit code"),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

fn success_json(args: &[&str]) -> (String, Value) {
    let run = run(args);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let value = serde_json::from_str(&run.stdout).expect("valid success JSON");
    (run.stdout, value)
}

fn assert_bounded_identifier_schema(schema: &Value, max_bytes: usize) {
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], NONBLANK_NO_CONTROL_PATTERN);
    assert_eq!(schema["maxLength"], max_bytes);
    assert_eq!(schema["x-maxBytes"], max_bytes);
}

fn assert_schema_runtime_parity(
    schema: &Value,
    value: &Value,
    runtime_accepts: impl FnOnce(&Value) -> bool,
    expected: bool,
    label: &str,
) {
    assert_eq!(
        runtime_accepts(value),
        expected,
        "runtime: {label}: {value}"
    );
    assert_eq!(
        schema_accepts(schema, value),
        expected,
        "schema: {label}: {value}"
    );
}

#[test]
fn root_schema_contains_every_beta_leaf_exactly_once() {
    let (raw, value) = success_json(&["schema"]);
    assert!(raw.len() <= 8 * 1024, "root schema is {} bytes", raw.len());
    assert_eq!(value["data"]["contract_version"], "1");
    assert_eq!(
        value["data"]["output"],
        serde_json::json!({
            "default": "json",
            "formats": [
                {"name": "json"},
                {"name": "toon", "spec_version": "3.0"}
            ],
            "flags": ["-o", "--output"]
        })
    );

    let actual: Vec<&str> = value["data"]["commands"]
        .as_array()
        .expect("commands array")
        .iter()
        .map(|command| command["name"].as_str().expect("command name"))
        .collect();
    let unique: BTreeSet<&str> = actual.iter().copied().collect();
    assert_eq!(actual.len(), unique.len(), "duplicate schema command");
    assert_eq!(
        unique,
        BTreeSet::from([
            "auth.login",
            "auth.logout",
            "auth.status",
            "board.list",
            "completion",
            "config.get",
            "config.set",
            "config.unset",
            "field.list",
            "epic.add",
            "epic.create",
            "epic.list",
            "epic.remove",
            "issue.comment",
            "issue.comments",
            "issue.clone",
            "issue.delete",
            "issue.create",
            "issue.create-meta",
            "issue.assign",
            "issue.get",
            "issue.link.add",
            "issue.link.get",
            "issue.link.remove",
            "issue.link.types",
            "issue.remote-link.add",
            "issue.remote-link.get",
            "issue.remote-link.list",
            "issue.remote-link.remove",
            "issue.worklog.add",
            "issue.worklog.delete",
            "issue.worklog.list",
            "issue.worklog.update",
            "issue.search",
            "issue.transition",
            "issue.transitions",
            "issue.update",
            "issue.watcher.add",
            "issue.watcher.list",
            "issue.watcher.remove",
            "man",
            "me",
            "project.list",
            "project.get",
            "project.templates",
            "project.create",
            "release.list",
            "schema",
            "server.info",
            "sprint.add",
            "sprint.close",
            "sprint.issues",
            "sprint.list",
            "url.issue",
            "url.project",
            "user.search",
            "version",
        ])
    );
}

fn expected_global_flags() -> Value {
    json!({
        "pretty": {
            "flags": ["--pretty"],
            "type": "boolean",
            "default": false,
            "conflicts_with": "output=toon"
        },
        "output": {
            "flags": ["-o", "--output"],
            "type": "string",
            "enum": ["json", "toon"],
            "default": "json",
            "conflicts_with": "pretty=true"
        },
        "timeout_ms": {
            "flags": ["--timeout-ms"],
            "type": "integer",
            "minimum": 1000,
            "maximum": 120000,
            "default": 30000
        }
    })
}

#[test]
fn global_flag_contract_is_exposed_everywhere_and_matches_runtime_grammar() {
    let (_, index) = success_json(&["schema"]);
    assert_eq!(index["data"]["global_flags"], expected_global_flags());

    let (all_raw, all) = success_json(&["schema", "--all"]);
    assert!(
        all_raw.len() <= 128 * 1024,
        "full schema is {} bytes",
        all_raw.len()
    );
    assert_eq!(all["data"]["global_flags"], expected_global_flags());

    let mut largest = (0, "");
    for spec in command_specs() {
        let args: Vec<&str> = std::iter::once("schema")
            .chain(spec.name.split('.'))
            .collect();
        let (raw, scoped) = success_json(&args);
        assert_eq!(
            scoped["data"]["global_flags"],
            expected_global_flags(),
            "{}",
            spec.name
        );
        if raw.len() > largest.0 {
            largest = (raw.len(), spec.name);
        }
    }
    assert_eq!(largest.1, "issue.worklog.update");
    assert!(
        largest.0 <= 4 * 1024,
        "largest scoped schema is {} bytes ({})",
        largest.0,
        largest.1
    );
    assert!(
        largest.0 <= 3067 + 1024,
        "schema repair grew the df29788 baseline max by more than 1 KiB: {largest:?}"
    );

    for args in [
        &["--pretty", "version"][..],
        &["-o", "json", "version"][..],
        &["--output", "toon", "version"][..],
        &["--timeout-ms", "1000", "version"][..],
        &["--timeout-ms", "120000", "version"][..],
    ] {
        let result = run(args);
        assert_eq!(result.code, 0, "{args:?}: {}", result.stderr);
    }
    for args in [
        &["--pretty", "--output", "toon", "version"][..],
        &["--pretty", "-o", "toon", "version"][..],
        &["--output", "yaml", "version"][..],
        &["--timeout-ms", "999", "version"][..],
        &["--timeout-ms", "120001", "version"][..],
    ] {
        let result = run(args);
        assert_eq!(result.code, 2, "{args:?}: {}", result.stderr);
    }
}

fn plain_envelope(data: Value) -> Value {
    serde_json::to_value(SuccessEnvelope::new(data)).unwrap()
}

fn page_envelope(data: Value, warnings: bool) -> Value {
    let mut envelope = SuccessEnvelope::with_meta(data, json!({"count": 1, "next_cursor": null}));
    if warnings {
        envelope.warnings.push(Warning {
            code: "jira_warning".to_owned(),
            message: "representative warning".to_owned(),
        });
    }
    serde_json::to_value(envelope).unwrap()
}

fn count_envelope(data: Value) -> Value {
    serde_json::to_value(SuccessEnvelope::with_meta(data, CountMeta { count: 1 })).unwrap()
}

fn mutation_envelope(operation: &str) -> Value {
    plain_envelope(json!({
        "operation": operation,
        "applied": false,
        "target": {},
        "changes": {},
        "validation": {"local": "passed", "metadata": "not_applicable"}
    }))
}

fn representative_success(command: &str) -> Value {
    let account = json!({
        "account_id": "abc",
        "display_name": "Agent",
        "active": true,
        "email": "agent@example.com"
    });
    let field_schema = json!({
        "type": "string",
        "items": null,
        "custom": null,
        "system": "summary"
    });
    let field_metadata = json!({
        "id": "summary",
        "name": "Summary",
        "required": true,
        "operations": ["set"],
        "schema": field_schema,
        "input_kind": "string",
        "supported_selector_members": [],
        "allowed_values": [],
        "allowed_values_complete": true
    });
    let issue = json!({
        "key": "ACCL-1",
        "summary": "Example",
        "status": null,
        "assignee": {"account_id": "abc", "display_name": "Agent"},
        "updated": "2026-08-22T00:00:00.000+0000",
        "description": null,
        "fields": {"customfield_10000": 7}
    });

    match command {
        "version" => plain_envelope(json!({
            "cli_version": env!("CARGO_PKG_VERSION"),
            "contract_version": "1"
        })),
        "schema" => success_json(&["schema"]).1,
        "config.get" | "config.set" | "config.unset" => plain_envelope(json!({
            "default_project": null,
            "default_board": 1
        })),
        "url.issue" | "url.project" => {
            plain_envelope(json!({"url": "https://example.atlassian.net/browse/ACCL-1"}))
        }
        "completion" => plain_envelope(json!("complete -c jira-ops")),
        "man" => plain_envelope(json!({"files": ["jira-ops.1"]})),
        "server.info" => plain_envelope(json!({
            "version": "1001.0.0",
            "deployment_type": "Cloud",
            "build_number": 1001,
            "build_date": "2026-08-22",
            "server_time": "2026-08-22T00:00:00.000+0000"
        })),
        "user.search" => page_envelope(
            json!([{"account_id":"abc","display_name":"Agent","active":true,"account_type":"atlassian"}]),
            false,
        ),
        "board.list" => page_envelope(
            json!([{"id":1,"name":"Board","type":"scrum","project_key":null}]),
            false,
        ),
        "release.list" => page_envelope(
            json!([{"id":"1","name":"v1","archived":false,"released":false,"start_date":null,"release_date":null}]),
            false,
        ),
        "auth.login" => {
            let mut envelope = SuccessEnvelope::new(json!({
                "site": "https://example.atlassian.net/",
                "cloud_id": "550e8400-e29b-41d4-a716-446655440000",
                "email": "agent@example.com",
                "account_id": "abc",
                "display_name": "Agent",
                "credential_source": "keyring"
            }));
            envelope.warnings.push(Warning {
                code: "local_state_partial".to_owned(),
                message: "representative warning".to_owned(),
            });
            serde_json::to_value(envelope).unwrap()
        }
        "auth.status" => plain_envelope(json!({
            "configured": false,
            "identity_source": null,
            "credential_source": "none"
        })),
        "auth.logout" => plain_envelope(json!({
            "removed_config": true,
            "removed_keyring": true,
            "environment_credentials_active": false
        })),
        "me" => plain_envelope(account),
        "project.list" => page_envelope(
            json!([{"id":"1","key":"ACCL","name":"Example","project_type":"software","simplified":true}]),
            true,
        ),
        "project.get" => plain_envelope(json!({
            "id": "1", "key": "ACCL", "name": "Example", "type": "software", "style": "next-gen"
        })),
        "project.templates" => plain_envelope(json!([{
            "name": "Team-managed Scrum",
            "project_type_key": "software",
            "project_template_key": "com.pyxis.greenhopper.jira:gh-simplified-agility-scrum"
        }])),
        "field.list" => page_envelope(
            json!([{"id":"summary","name":"Summary","custom":false,"schema":field_schema}]),
            true,
        ),
        "issue.get" => plain_envelope(issue.clone()),
        "issue.search" | "epic.list" | "sprint.issues" => page_envelope(json!([issue]), true),
        "issue.create-meta" => {
            let mut envelope = SuccessEnvelope::with_meta(
                json!([field_metadata]),
                json!({
                    "kind": "fields",
                    "project": "ACCL",
                    "issue_type_id": "10001",
                    "count": 1,
                    "next_cursor": null
                }),
            );
            envelope.warnings.push(Warning {
                code: "jira_warning".to_owned(),
                message: "representative warning".to_owned(),
            });
            serde_json::to_value(envelope).unwrap()
        }
        "issue.comments" => page_envelope(
            json!([{
                "id":"1",
                "author":{"account_id":"abc","display_name":"Agent"},
                "body":"Comment",
                "created":"2026-08-22T00:00:00.000+0000",
                "updated":"2026-08-22T00:00:00.000+0000"
            }]),
            true,
        ),
        "issue.transitions" => count_envelope(json!([{
            "id":"1",
            "name":"Done",
            "to":{"id":"10001","name":"Done"},
            "fields":[field_metadata]
        }])),
        "issue.link.types" => count_envelope(json!([{
            "id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks"
        }])),
        "issue.link.get" => plain_envelope(json!({
            "id":"10000",
            "type":{"id":"10001","name":"Blocks","inward":"is blocked by","outward":"blocks"},
            "inward_issue":{"key":"ACCL-1"},
            "outward_issue":{"key":"OPS-2"}
        })),
        "issue.remote-link.list" => count_envelope(json!([{
            "id":1,"global_id":null,"title":"Ticket","url":"https://tracker.example/1","relationship":null
        }])),
        "issue.remote-link.get" => plain_envelope(json!({
            "id":1,"global_id":null,"title":"Ticket","url":"https://tracker.example/1","relationship":null
        })),
        "issue.worklog.list" => page_envelope(
            json!([{
                "id":"1",
                "author":account,
                "started":"2026-08-22T00:00:00.000+0000",
                "time_spent":"1h",
                "time_spent_seconds":3600,
                "comment":null,
                "updated":null
            }]),
            false,
        ),
        "issue.watcher.list" => count_envelope(json!([{
            "account_id":"abc","display_name":"Agent","active":true
        }])),
        "sprint.list" => page_envelope(
            json!([{
                "id":1,"name":"Sprint 1","state":"active","start_date":null,"end_date":null,
                "complete_date":null,"goal":null
            }]),
            false,
        ),
        "project.create" => plain_envelope(json!({
            "operation":"project.create",
            "method":"POST",
            "path":"/rest/api/3/project",
            "body":{"key":"ACCL"}
        })),
        "issue.create"
        | "issue.clone"
        | "issue.delete"
        | "issue.update"
        | "issue.comment"
        | "issue.transition"
        | "issue.assign"
        | "issue.link.add"
        | "issue.link.remove"
        | "issue.remote-link.add"
        | "issue.remote-link.remove"
        | "issue.worklog.add"
        | "issue.worklog.update"
        | "issue.worklog.delete"
        | "epic.create"
        | "epic.add"
        | "epic.remove"
        | "sprint.add"
        | "sprint.close"
        | "issue.watcher.add"
        | "issue.watcher.remove" => mutation_envelope(command),
        other => panic!("missing representative success for {other}"),
    }
}

#[test]
fn every_public_leaf_has_a_typed_exact_success_envelope() {
    for spec in command_specs() {
        let args: Vec<&str> = std::iter::once("schema")
            .chain(spec.name.split('.'))
            .collect();
        let (_, scoped) = success_json(&args);
        let schema = &scoped["data"]["success_schema"];
        let data_schema = &schema["properties"]["data"];
        assert!(schema.is_object(), "{} success schema", spec.name);
        assert_ne!(data_schema, &json!({}), "{} unconstrained data", spec.name);
        assert!(
            data_schema.get("type").is_some() || data_schema.get("oneOf").is_some(),
            "{} untyped data: {data_schema}",
            spec.name
        );

        let representative = representative_success(spec.name);
        assert!(
            schema_accepts(schema, &representative),
            "{} rejects representative {representative}",
            spec.name
        );

        let mut missing_data = representative.clone();
        missing_data.as_object_mut().unwrap().remove("data");
        assert!(
            !schema_accepts(schema, &missing_data),
            "{} missing data",
            spec.name
        );
        if representative.get("meta").is_some() {
            let mut missing_meta = representative.clone();
            missing_meta.as_object_mut().unwrap().remove("meta");
            assert!(
                !schema_accepts(schema, &missing_meta),
                "{} accepts missing runtime metadata",
                spec.name
            );
        }
        assert!(
            !schema_accepts(schema, &json!({"data": 7})),
            "{} accepts wrong data type",
            spec.name
        );
        let mut extra_envelope = representative;
        extra_envelope["unexpected"] = json!(true);
        assert!(
            !schema_accepts(schema, &extra_envelope),
            "{} accepts extra envelope member",
            spec.name
        );
    }
}

#[test]
fn schema_command_success_contract_accepts_index_scoped_and_full_runtime_variants() {
    let (_, scoped_schema) = success_json(&["schema", "schema"]);
    let success_schema = &scoped_schema["data"]["success_schema"];

    for args in [
        &["schema"][..],
        &["schema", "version"][..],
        &["schema", "--all"][..],
    ] {
        let (_, runtime_envelope) = success_json(args);
        assert!(
            schema_accepts(success_schema, &runtime_envelope),
            "schema success contract rejects {args:?}"
        );
    }
}

#[test]
fn every_reference_capability_has_a_runtime_command() {
    let expected = [
        "config.get",
        "config.set",
        "config.unset",
        "server.info",
        "user.search",
        "board.list",
        "release.list",
        "issue.clone",
        "issue.delete",
        "issue.link.remove",
        "issue.remote-link.list",
        "issue.remote-link.get",
        "issue.remote-link.add",
        "issue.remote-link.remove",
        "issue.worklog.list",
        "issue.worklog.add",
        "issue.worklog.update",
        "issue.worklog.delete",
        "epic.list",
        "epic.create",
        "epic.add",
        "epic.remove",
        "sprint.list",
        "sprint.issues",
        "sprint.add",
        "sprint.close",
        "url.issue",
        "url.project",
        "completion",
        "man",
    ];
    let actual: BTreeSet<_> = jira_ops::schema::command_specs()
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert!(expected.into_iter().all(|name| actual.contains(name)));
}

#[test]
fn project_create_schema_requires_exact_input_members() {
    let (raw, value) = success_json(&["schema", "project", "create"]);
    assert!(
        raw.len() <= 3 * 1024,
        "project.create schema is {} bytes",
        raw.len()
    );
    assert_eq!(value["data"]["command"], "project.create");
    assert_eq!(value["data"]["effect"], "jira_write");
    assert_eq!(
        value["data"]["stdin_schema"]["required"],
        serde_json::json!([
            "key",
            "name",
            "project_type_key",
            "project_template_key",
            "lead_account_id"
        ])
    );
    assert_eq!(value["data"]["stdin_schema"]["additionalProperties"], false);
    assert_eq!(
        value["data"]["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|flag| flag["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["--input", "--apply"]
    );
    let derived = serde_json::to_value(schemars::schema_for!(ProjectCreateInput)).unwrap();
    assert_eq!(
        value["data"]["stdin_schema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<BTreeSet<_>>(),
        derived["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn daily_core_schemas_are_compact_typed_and_exact() {
    for (path, command, effect, properties, required) in [
        (
            &["issue", "assign"][..],
            "issue.assign",
            "jira_write",
            BTreeSet::from(["account_id", "issue_key"]),
            BTreeSet::from(["account_id", "issue_key"]),
        ),
        (
            &["issue", "link", "add"][..],
            "issue.link.add",
            "jira_write",
            BTreeSet::from(["inward_issue", "outward_issue", "type_name"]),
            BTreeSet::from(["inward_issue", "outward_issue", "type_name"]),
        ),
        (
            &["issue", "watcher", "add"][..],
            "issue.watcher.add",
            "jira_write",
            BTreeSet::from(["account_id", "issue_key"]),
            BTreeSet::from(["account_id", "issue_key"]),
        ),
        (
            &["issue", "watcher", "remove"][..],
            "issue.watcher.remove",
            "jira_write",
            BTreeSet::from(["account_id", "issue_key"]),
            BTreeSet::from(["account_id", "issue_key"]),
        ),
    ] {
        let mut args = vec!["schema"];
        args.extend_from_slice(path);
        let (raw, value) = success_json(&args);
        assert!(raw.len() <= 3 * 1024, "{command}: {} bytes", raw.len());
        assert_eq!(value["data"]["command"], command);
        assert_eq!(value["data"]["effect"], effect);
        assert_eq!(value["data"]["idempotency"], "non_idempotent");
        assert_eq!(value["data"]["stdin_schema"]["additionalProperties"], false);
        assert_eq!(
            value["data"]["stdin_schema"]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            properties
        );
        assert_eq!(
            value["data"]["stdin_schema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            required
        );
        assert_eq!(value["data"]["errors"]["mutation_outcome_unknown"], 8);
        assert_eq!(value["data"]["errors"]["mutation_response_invalid"], 8);
    }

    for (path, command) in [
        (&["issue", "link", "types"][..], "issue.link.types"),
        (&["issue", "link", "get"][..], "issue.link.get"),
        (&["issue", "watcher", "list"][..], "issue.watcher.list"),
    ] {
        let mut args = vec!["schema"];
        args.extend_from_slice(path);
        let (raw, value) = success_json(&args);
        assert!(raw.len() <= 2 * 1024, "{command}: {} bytes", raw.len());
        assert_eq!(value["data"]["command"], command);
        assert_eq!(value["data"]["effect"], "read");
        assert_eq!(value["data"]["idempotency"], "idempotent");
        assert!(value["data"]["stdin_schema"].is_null());
    }
}

#[test]
fn daily_core_input_schemas_match_runtime_validation_and_examples() {
    let (_, assignment) = success_json(&["schema", "issue", "assign"]);
    let assignment_schema = &assignment["data"]["stdin_schema"];
    assert_eq!(
        assignment_schema["properties"]["issue_key"]["pattern"],
        STRICT_ISSUE_KEY_PATTERN
    );
    assert_eq!(
        assignment_schema["properties"]["account_id"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(
        assignment_schema["properties"]["account_id"]["pattern"],
        NONBLANK_NO_CONTROL_PATTERN
    );
    assert_eq!(
        assignment_schema["properties"]["account_id"]["maxLength"],
        1024
    );
    assert_eq!(
        assignment_schema["properties"]["account_id"]["x-maxBytes"],
        1024
    );
    assert_schema_runtime_parity(
        assignment_schema,
        &assignment["data"]["example"]["stdin"],
        |value| {
            serde_json::from_value::<AssignmentInput>(value.clone())
                .is_ok_and(|input| validate_assignment_input(&input).is_ok())
        },
        true,
        "assignment example",
    );
    for (value, accepted) in [
        (json!({"issue_key":"ACCL-1","account_id":null}), true),
        (json!({"issue_key":"ACCL_2-9","account_id":"abc"}), true),
        (json!({"issue_key":"accl-1","account_id":"abc"}), false),
        (json!({"issue_key":"ACCL-1","account_id":" \t"}), false),
        (
            json!({"issue_key":"ACCL-1","account_id":"abc\u{0007}"}),
            false,
        ),
        (
            json!({"issue_key":"ACCL-1","account_id":"é".repeat(513)}),
            false,
        ),
    ] {
        assert_schema_runtime_parity(
            assignment_schema,
            &value,
            |value| {
                serde_json::from_value::<AssignmentInput>(value.clone())
                    .is_ok_and(|input| validate_assignment_input(&input).is_ok())
            },
            accepted,
            "assignment input",
        );
    }

    let (_, link) = success_json(&["schema", "issue", "link", "add"]);
    let link_schema = &link["data"]["stdin_schema"];
    for property in ["inward_issue", "outward_issue"] {
        assert_eq!(
            link_schema["properties"][property]["pattern"],
            STRICT_ISSUE_KEY_PATTERN
        );
    }
    assert_bounded_identifier_schema(&link_schema["properties"]["type_name"], 255);
    assert_schema_runtime_parity(
        link_schema,
        &link["data"]["example"]["stdin"],
        |value| {
            serde_json::from_value::<LinkInput>(value.clone())
                .is_ok_and(|input| validate_link_input(&input).is_ok())
        },
        true,
        "link example",
    );
    for (value, accepted) in [
        (
            json!({"inward_issue":"ACCL-1","outward_issue":"OPS_2-3","type_name":"Blocks"}),
            true,
        ),
        (
            json!({"inward_issue":"ACCL-0","outward_issue":"OPS-2","type_name":"Blocks"}),
            false,
        ),
        (
            json!({"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"\n"}),
            false,
        ),
        (
            json!({"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"é".repeat(128)}),
            false,
        ),
    ] {
        assert_schema_runtime_parity(
            link_schema,
            &value,
            |value| {
                serde_json::from_value::<LinkInput>(value.clone())
                    .is_ok_and(|input| validate_link_input(&input).is_ok())
            },
            accepted,
            "link input",
        );
    }

    for command in ["add", "remove"] {
        let (_, watcher) = success_json(&["schema", "issue", "watcher", command]);
        let watcher_schema = &watcher["data"]["stdin_schema"];
        assert_eq!(
            watcher_schema["properties"]["issue_key"]["pattern"],
            STRICT_ISSUE_KEY_PATTERN
        );
        assert_bounded_identifier_schema(&watcher_schema["properties"]["account_id"], 1024);
        assert_schema_runtime_parity(
            watcher_schema,
            &watcher["data"]["example"]["stdin"],
            |value| {
                serde_json::from_value::<WatcherInput>(value.clone())
                    .is_ok_and(|input| validate_watcher_input(&input).is_ok())
            },
            true,
            "watcher example",
        );
        for (value, accepted) in [
            (json!({"issue_key":"ACCL-1","account_id":"abc"}), true),
            (json!({"issue_key":"ACCL-1","account_id":""}), false),
            (
                json!({"issue_key":"ACCL-1","account_id":"abc\u{0085}"}),
                false,
            ),
            (
                json!({"issue_key":"ACCL-1","account_id":"é".repeat(513)}),
                false,
            ),
        ] {
            assert_schema_runtime_parity(
                watcher_schema,
                &value,
                |value| {
                    serde_json::from_value::<WatcherInput>(value.clone())
                        .is_ok_and(|input| validate_watcher_input(&input).is_ok())
                },
                accepted,
                "watcher input",
            );
        }
    }
}

#[test]
fn daily_core_read_success_schemas_match_runtime_projections() {
    let cases = [
        (
            &["issue", "link", "types"][..],
            serde_json::to_value(SuccessEnvelope::with_meta(
                vec![LinkTypeItem {
                    id: "10000".to_owned(),
                    name: "Blocks".to_owned(),
                    inward: "is blocked by".to_owned(),
                    outward: "blocks".to_owned(),
                }],
                CountMeta { count: 1 },
            ))
            .unwrap(),
            vec![
                json!({"data":[{"id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks","email":"secret@example.com"}],"meta":{"count":1}}),
                json!({"data":[{"id":"10000","name":"Blocks","inward":"is blocked by"}],"meta":{"count":1}}),
                json!({"data":[],"meta":{"count":"0"}}),
            ],
        ),
        (
            &["issue", "link", "get"][..],
            serde_json::to_value(SuccessEnvelope::new(LinkItem {
                id: "10000".to_owned(),
                link_type: LinkTypeItem {
                    id: "10001".to_owned(),
                    name: "Blocks".to_owned(),
                    inward: "is blocked by".to_owned(),
                    outward: "blocks".to_owned(),
                },
                inward_issue: LinkedIssue {
                    key: "ACCL-1".to_owned(),
                },
                outward_issue: LinkedIssue {
                    key: "OPS-2".to_owned(),
                },
            }))
            .unwrap(),
            vec![
                json!({"data":{"id":"10000","type":{"id":"10001","name":"Blocks","inward":"is blocked by","outward":"blocks"},"inward_issue":{"key":"ACCL-1"},"outward_issue":{"key":"OPS-2"},"self":"https://example.invalid"}}),
                json!({"data":{"id":"10000","type":{"id":"10001","name":"Blocks","inward":"is blocked by","outward":"blocks"},"inward_issue":{"key":1},"outward_issue":{"key":"OPS-2"}}}),
            ],
        ),
        (
            &["issue", "watcher", "list"][..],
            serde_json::to_value(SuccessEnvelope::with_meta(
                vec![WatcherItem {
                    account_id: "abc".to_owned(),
                    display_name: "Agent".to_owned(),
                    active: true,
                }],
                CountMeta { count: 1 },
            ))
            .unwrap(),
            vec![
                json!({"data":[{"account_id":"abc","display_name":"Agent","active":true,"email":"secret@example.com"}],"meta":{"count":1}}),
                json!({"data":[{"account_id":"abc","display_name":"Agent","active":"true"}],"meta":{"count":1}}),
                json!({"data":[]}),
            ],
        ),
    ];

    for (path, runtime_value, impossible_values) in cases {
        let mut default_args = vec!["schema"];
        default_args.extend_from_slice(path);
        let mut explicit_args = vec!["--output", "json", "schema"];
        explicit_args.extend_from_slice(path);
        let mut pretty_args = vec!["--pretty", "schema"];
        pretty_args.extend_from_slice(path);
        let mut toon_args = vec!["--output", "toon", "schema"];
        toon_args.extend_from_slice(path);

        let default = run(&default_args);
        let explicit = run(&explicit_args);
        let pretty = run(&pretty_args);
        let toon = run(&toon_args);
        assert_eq!(default.code, 0, "{path:?}: {}", default.stderr);
        assert_eq!(explicit.code, 0, "{path:?}: {}", explicit.stderr);
        assert_eq!(pretty.code, 0, "{path:?}: {}", pretty.stderr);
        assert_eq!(toon.code, 0, "{path:?}: {}", toon.stderr);
        assert_eq!(default.stdout, explicit.stdout, "{path:?}");
        let default_value: Value = serde_json::from_str(&default.stdout).unwrap();
        let pretty_value: Value = serde_json::from_str(&pretty.stdout).unwrap();
        let toon_value: Value = toon_format::decode_default(
            toon.stdout
                .strip_suffix('\n')
                .expect("TOON schema terminal LF"),
        )
        .unwrap();
        assert_eq!(pretty_value, default_value, "{path:?}");
        assert_eq!(toon_value, default_value, "{path:?}");
        assert!(
            default.stdout.len() <= 2 * 1024,
            "{path:?}: {} bytes",
            default.stdout.len()
        );

        let success_schema = &default_value["data"]["success_schema"];
        assert!(
            schema_accepts(success_schema, &runtime_value),
            "{path:?} rejects its runtime projection: {runtime_value}"
        );
        for impossible in impossible_values {
            assert!(
                !schema_accepts(success_schema, &impossible),
                "{path:?} accepts impossible projection: {impossible}"
            );
        }
    }
}

#[test]
fn daily_core_mutation_schemas_preserve_output_and_success_parity() {
    for (path, operation, applied_required) in [
        (
            &["issue", "assign"][..],
            "issue.assign",
            serde_json::json!(["issue", "assignment"]),
        ),
        (
            &["issue", "link", "add"][..],
            "issue.link.add",
            serde_json::json!(["link"]),
        ),
        (
            &["issue", "watcher", "add"][..],
            "issue.watcher.add",
            serde_json::json!(["issue", "watcher"]),
        ),
        (
            &["issue", "watcher", "remove"][..],
            "issue.watcher.remove",
            serde_json::json!(["issue", "watcher"]),
        ),
    ] {
        let mut default_args = vec!["schema"];
        default_args.extend_from_slice(path);
        let mut explicit_args = vec!["--output", "json", "schema"];
        explicit_args.extend_from_slice(path);
        let mut toon_args = vec!["--output", "toon", "schema"];
        toon_args.extend_from_slice(path);

        let default = run(&default_args);
        let explicit = run(&explicit_args);
        let toon = run(&toon_args);
        assert_eq!(default.code, 0, "{operation}: {}", default.stderr);
        assert_eq!(explicit.code, 0, "{operation}: {}", explicit.stderr);
        assert_eq!(toon.code, 0, "{operation}: {}", toon.stderr);
        assert_eq!(default.stdout, explicit.stdout, "{operation}");
        let json_value: Value = serde_json::from_str(&default.stdout).unwrap();
        let toon_value: Value = toon_format::decode_default(
            toon.stdout
                .strip_suffix('\n')
                .expect("TOON schema terminal LF"),
        )
        .unwrap();
        assert_eq!(toon_value, json_value, "{operation}");
        let success = &json_value["data"]["success_schema"]["properties"]["data"];
        assert_eq!(success["oneOf"].as_array().unwrap().len(), 2);
        assert_eq!(
            success["oneOf"][0]["properties"]["operation"]["const"],
            operation
        );
        assert_eq!(
            success["oneOf"][1]["properties"]["operation"]["const"],
            operation
        );
        assert_eq!(success["oneOf"][0]["properties"]["applied"]["const"], false);
        assert_eq!(success["oneOf"][1]["properties"]["applied"]["const"], true);
        let required = success["oneOf"][1]["required"].as_array().unwrap();
        for member in applied_required.as_array().unwrap() {
            assert!(required.contains(member), "{operation}: missing {member}");
        }
    }
}

#[test]
fn every_scoped_schema_uses_the_stable_error_exit_map() {
    for spec in command_specs() {
        let command = spec.name;
        let args: Vec<&str> = std::iter::once("schema")
            .chain(command.split('.'))
            .collect();
        let (_, value) = success_json(&args);
        let errors = value["data"]["errors"]
            .as_object()
            .expect("compact error exit map");
        assert!(!errors.is_empty(), "{command}");
        let actual: BTreeMap<String, u64> = errors
            .iter()
            .map(|(code, exit)| (code.clone(), exit.as_u64().expect("numeric exit class")))
            .collect();
        let expected: BTreeMap<String, u64> = spec
            .errors
            .iter()
            .map(|code| {
                (
                    serde_json::to_value(code)
                        .expect("serializable error code")
                        .as_str()
                        .expect("string error code")
                        .to_owned(),
                    u64::from(code.exit_class().code()),
                )
            })
            .collect();
        assert_eq!(actual, expected, "{command}");
    }
}

#[test]
fn schema_honors_toon_output_before_and_after_nested_path() {
    for args in [
        &["-o", "toon", "schema", "issue", "create"][..],
        &["schema", "issue", "create", "--output", "toon"][..],
    ] {
        let run = run(args);
        assert_eq!(run.code, 0, "stderr: {}", run.stderr);
        assert_eq!(run.stderr, "");
        assert!(run.stdout.ends_with('\n'));
        assert!(!run.stdout.ends_with("\n\n"));
        assert!(run.stdout.contains("command: issue.create"));
        assert!(run.stdout.contains("effect: jira_write"));
        assert!(!run.stdout.starts_with('{'));
    }
}

#[test]
fn mutation_schema_is_representation_independent_and_json_default_is_identical() {
    for (family, command) in [
        ("issue", "create"),
        ("issue", "update"),
        ("issue", "comment"),
        ("issue", "transition"),
        ("project", "create"),
    ] {
        let default = run(&["schema", family, command]);
        let explicit = run(&["--output", "json", "schema", family, command]);
        let toon = run(&["--output", "toon", "schema", family, command]);

        assert_eq!(default.code, 0, "{command}: {}", default.stderr);
        assert_eq!(explicit.code, 0, "{command}: {}", explicit.stderr);
        assert_eq!(toon.code, 0, "{command}: {}", toon.stderr);
        assert_eq!(default.stderr, "");
        assert_eq!(explicit.stderr, "");
        assert_eq!(toon.stderr, "");
        assert_eq!(default.stdout, explicit.stdout, "{command}");
        assert!(
            default.stdout.len() <= 3 * 1024,
            "{command}: {}",
            default.stdout.len()
        );

        let json_value: Value = serde_json::from_str(&default.stdout).unwrap();
        let toon_value: Value = toon_format::decode_default(
            toon.stdout
                .strip_suffix('\n')
                .expect("TOON schema terminal LF"),
        )
        .unwrap();
        assert_eq!(toon_value, json_value, "{command}");
        assert!(json_value["data"]["errors"].is_object());
        assert_eq!(json_value["data"]["errors"]["mutation_outcome_unknown"], 8);
        assert_eq!(json_value["data"]["errors"]["mutation_response_invalid"], 8);
    }
}

#[test]
fn scoped_operation_schema_is_small_and_actionable() {
    let (raw, value) = success_json(&["schema", "issue", "get"]);
    assert!(
        raw.len() <= 2 * 1024,
        "operation schema is {} bytes",
        raw.len()
    );
    assert_eq!(value["data"]["command"], "issue.get");
    assert_eq!(value["data"]["effect"], "read");
    assert_eq!(value["data"]["idempotency"], "idempotent");
    assert_eq!(value["data"]["positionals"][0]["name"], "ISSUE");
    assert_eq!(value["data"]["flags"][0]["name"], "--fields");
    assert_eq!(value["data"]["example"]["argv"][0], "issue");
    assert!(value["data"]["success_schema"].is_object());
    assert!(value["data"]["errors"].is_object());
}

#[test]
fn gate_eight_scoped_schemas_describe_both_metadata_modes_and_read_contracts() {
    for (args, command, positionals, flags, paginated, example) in [
        (
            &["schema", "issue", "create-meta"][..],
            "issue.create-meta",
            serde_json::json!([]),
            serde_json::json!(["--project", "--issue-type", "--limit", "--cursor"]),
            true,
            serde_json::json!(["issue", "create-meta", "--project", "ACCL"]),
        ),
        (
            &["schema", "issue", "comments"][..],
            "issue.comments",
            serde_json::json!([{"name":"ISSUE","type":"string","required":true}]),
            serde_json::json!(["--limit", "--cursor"]),
            true,
            serde_json::json!(["issue", "comments", "ACCL-1"]),
        ),
        (
            &["schema", "issue", "transitions"][..],
            "issue.transitions",
            serde_json::json!([{"name":"ISSUE","type":"string","required":true}]),
            serde_json::json!([]),
            false,
            serde_json::json!(["issue", "transitions", "ACCL-1"]),
        ),
    ] {
        let (_, value) = success_json(args);
        assert_eq!(value["data"]["command"], command);
        assert_eq!(value["data"]["effect"], "read");
        assert_eq!(value["data"]["idempotency"], "idempotent");
        assert_eq!(value["data"]["positionals"], positionals);
        assert_eq!(
            value["data"]["flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|flag| flag["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            flags
                .as_array()
                .unwrap()
                .iter()
                .map(|flag| flag.as_str().unwrap())
                .collect::<Vec<_>>()
        );
        assert_eq!(value["data"]["pagination"].is_object(), paginated);
        assert_eq!(value["data"]["example"]["argv"], example);
    }
}

#[test]
fn all_schema_conflicts_with_a_command_path() {
    let run = run(&["schema", "--all", "issue", "get"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
    assert_eq!(value["error"]["code"], "invalid_input");
}

#[test]
fn mutation_grammar_requires_exact_stdin_marker_and_rejects_payload_flags() {
    for args in [
        &["issue", "create", "--input", "payload.json"][..],
        &["issue", "update", "ACCL-1"][..],
        &["issue", "comment", "ACCL-1", "--body", "text"][..],
        &["issue", "--pretty", "comment", "ACCL-1", "--body", "text"][..],
        &[
            "issue",
            "transition",
            "ACCL-1",
            "--input",
            "-",
            "--fields",
            "{}",
        ][..],
        &["project", "create", "--input", "payload.json"][..],
    ] {
        let run = run(args);
        assert_eq!(run.code, 2, "args: {args:?}");
        assert_eq!(run.stdout, "", "args: {args:?}");
        let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
        assert_eq!(value["error"]["code"], "invalid_input", "args: {args:?}");
        assert_eq!(
            value["error"]["operation_outcome"], "not_applied",
            "args: {args:?}"
        );
    }
}

#[test]
fn mutation_scoped_schemas_are_jira_write_non_idempotent_and_typed() {
    for (command, positionals, properties, required) in [
        (
            "create",
            serde_json::json!([]),
            BTreeSet::from(["fields", "issue_type_id", "project_key"]),
            BTreeSet::from(["fields", "issue_type_id", "project_key"]),
        ),
        (
            "update",
            serde_json::json!([{"name":"ISSUE","type":"string","required":true,"pattern":"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"}]),
            BTreeSet::from(["notify_users", "set"]),
            BTreeSet::from(["set"]),
        ),
        (
            "comment",
            serde_json::json!([{"name":"ISSUE","type":"string","required":true,"pattern":"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"}]),
            BTreeSet::from(["body", "internal"]),
            BTreeSet::from(["body"]),
        ),
        (
            "transition",
            serde_json::json!([{"name":"ISSUE","type":"string","required":true,"pattern":"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"}]),
            BTreeSet::from(["comment", "fields", "notify_users", "transition_id"]),
            BTreeSet::from(["transition_id"]),
        ),
    ] {
        let (raw, value) = success_json(&["schema", "issue", command]);
        assert!(raw.len() <= 3 * 1024, "schema {command} is too large");
        assert_eq!(value["data"]["effect"], "jira_write");
        assert_eq!(value["data"]["idempotency"], "non_idempotent");
        assert!(
            value["data"]["summary"]
                .as_str()
                .unwrap()
                .starts_with("Plan ")
        );
        assert!(
            value["data"]["summary"]
                .as_str()
                .unwrap()
                .contains("--apply writes")
        );
        assert_eq!(value["data"]["positionals"], positionals);
        assert_eq!(
            value["data"]["flags"],
            serde_json::json!([
                {"name":"--input","type":"stdin","required":true},
                {"name":"--apply","type":"boolean","required":false,"default":false}
            ])
        );
        assert_eq!(value["data"]["stdin_schema"]["additionalProperties"], false);
        let stdin_schema = &value["data"]["stdin_schema"];
        assert_eq!(
            stdin_schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            properties,
            "properties: {command}"
        );
        assert_eq!(
            stdin_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            required,
            "required: {command}"
        );
        assert_eq!(value["data"]["errors"]["schema_violation"], 2);
        assert_eq!(value["data"]["errors"]["scope_missing"], 4);
        let errors: BTreeSet<&str> = value["data"]["errors"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for required in [
            "local_state_partial",
            "conflict",
            "mutation_outcome_unknown",
            "mutation_response_invalid",
        ] {
            assert!(errors.contains(required), "{command} omits {required}");
        }
    }

    let (_, create) = success_json(&["schema", "issue", "create"]);
    let create_fields = &create["data"]["stdin_schema"]["properties"]["fields"];
    assert!(
        create_fields["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("summary"))
    );
    assert!(create_fields["not"].is_object());
    assert_eq!(
        create["data"]["stdin_schema"]["properties"]["project_key"]["pattern"],
        r"\S"
    );
    assert_eq!(
        create["data"]["stdin_schema"]["properties"]["issue_type_id"]["pattern"],
        r"\S"
    );

    let (_, update) = success_json(&["schema", "issue", "update"]);
    assert_eq!(
        update["data"]["stdin_schema"]["properties"]["set"]["minProperties"],
        1
    );
    let (_, comment) = success_json(&["schema", "issue", "comment"]);
    assert_eq!(
        comment["data"]["stdin_schema"]["properties"]["body"]["minLength"],
        1
    );
    let (_, transition) = success_json(&["schema", "issue", "transition"]);
    assert_eq!(
        transition["data"]["stdin_schema"]["properties"]["transition_id"]["pattern"],
        r"\S"
    );

    let create_example = create["data"]["example"]["stdin"].to_string().into_bytes();
    read_json_input::<CreateIssueInput>(&mut create_example.as_slice()).unwrap();
    let update_example = update["data"]["example"]["stdin"].to_string().into_bytes();
    read_json_input::<UpdateIssueInput>(&mut update_example.as_slice()).unwrap();
    let comment_example = comment["data"]["example"]["stdin"].to_string().into_bytes();
    read_json_input::<CommentInput>(&mut comment_example.as_slice()).unwrap();
    let transition_example = transition["data"]["example"]["stdin"]
        .to_string()
        .into_bytes();
    read_json_input::<TransitionInput>(&mut transition_example.as_slice()).unwrap();
}

#[test]
fn mutation_success_schema_has_exact_dry_run_and_applied_alternatives() {
    for (command, operation, applied_required) in [
        ("create", "issue.create", vec!["issue"]),
        ("update", "issue.update", vec!["issue"]),
        ("comment", "issue.comment", vec!["issue", "comment"]),
        ("transition", "issue.transition", vec!["issue"]),
    ] {
        let (_, value) = success_json(&["schema", "issue", command]);
        let success = &value["data"]["success_schema"];
        assert_eq!(success["type"], "object");
        assert_eq!(success["required"], serde_json::json!(["data"]));
        assert_eq!(success["properties"]["data"]["type"], "object");
        let alternatives = success["properties"]["data"]["oneOf"]
            .as_array()
            .expect("dry-run and applied alternatives");
        assert_eq!(alternatives.len(), 2);
        let dry_run = &alternatives[0];
        assert_eq!(dry_run["properties"]["operation"]["const"], operation);
        assert_eq!(dry_run["properties"]["applied"]["const"], false);
        assert_eq!(
            dry_run["required"],
            serde_json::json!(["operation", "applied", "target", "changes", "validation"])
        );
        assert_eq!(dry_run["properties"]["validation"]["type"], "object");
        assert_eq!(
            dry_run["properties"]["validation"]["properties"]["local"]["const"],
            "passed"
        );
        assert_eq!(
            dry_run["properties"]["validation"]["properties"]["metadata"]["enum"],
            serde_json::json!(["passed", "partial", "not_applicable"])
        );

        let applied = &alternatives[1];
        assert_eq!(applied["properties"]["operation"]["const"], operation);
        assert_eq!(applied["properties"]["applied"]["const"], true);
        let required = applied["required"].as_array().unwrap();
        assert!(required.contains(&json!("operation")));
        assert!(required.contains(&json!("applied")));
        for member in applied_required {
            assert!(required.contains(&json!(member)));
        }
        let issue_required = applied["properties"]["issue"]["required"]
            .as_array()
            .expect("applied issue fields");
        assert_eq!(applied["properties"]["issue"]["type"], "object");
        assert_eq!(
            applied["properties"]["issue"]["properties"]["key"]["type"],
            "string"
        );
        assert!(issue_required.contains(&serde_json::json!("key")));
        if command == "create" {
            assert!(issue_required.contains(&serde_json::json!("id")));
            assert!(issue_required.contains(&serde_json::json!("url")));
            assert_eq!(
                applied["properties"]["issue"]["properties"]["id"]["type"],
                "string"
            );
            assert_eq!(
                applied["properties"]["issue"]["properties"]["url"]["type"],
                "string"
            );
        }
        if command == "comment" {
            assert_eq!(applied["properties"]["comment"]["type"], "object");
            assert_eq!(
                applied["properties"]["comment"]["required"],
                serde_json::json!(["id"])
            );
            assert_eq!(
                applied["properties"]["comment"]["properties"]["id"]["type"],
                "string"
            );
        }

        let errors: BTreeSet<&str> = value["data"]["errors"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(errors.contains("mutation_outcome_unknown"));
        assert!(errors.contains("mutation_response_invalid"));
    }
}

#[test]
fn mutation_success_schema_rejects_impossible_runtime_projections() {
    for (command, operation) in [
        ("create", "issue.create"),
        ("update", "issue.update"),
        ("comment", "issue.comment"),
        ("transition", "issue.transition"),
    ] {
        let (_, value) = success_json(&["schema", "issue", command]);
        let schema = &value["data"]["success_schema"];
        let dry_run = serde_json::json!({
            "data": {
                "operation": operation,
                "applied": false,
                "target": {},
                "changes": {},
                "validation": {"local": "passed", "metadata": "passed"}
            }
        });
        assert!(schema_accepts(schema, &dry_run), "{command} valid dry run");
        for impossible in [
            serde_json::json!({"data": null}),
            serde_json::json!({"data": 7}),
            serde_json::json!({"data": {
                "operation": operation, "applied": false, "target": {}, "changes": {},
                "validation": null
            }}),
            serde_json::json!({"data": {
                "operation": operation, "applied": false, "target": {}, "changes": {},
                "validation": {"local": "partial", "metadata": "passed"}
            }}),
            serde_json::json!({"data": {
                "operation": operation, "applied": false, "target": {}, "changes": {},
                "validation": {"local": "passed", "metadata": "unknown"}
            }}),
        ] {
            assert!(
                !schema_accepts(schema, &impossible),
                "{command}: {impossible}"
            );
        }

        let mut applied = serde_json::json!({
            "data": {
                "operation": operation,
                "applied": true,
                "issue": {"key": "ACCL-1"}
            }
        });
        if command == "create" {
            applied["data"]["issue"]["id"] = serde_json::json!("10001");
            applied["data"]["issue"]["url"] =
                serde_json::json!("https://example.atlassian.net/browse/ACCL-1");
        }
        if command == "comment" {
            applied["data"]["comment"] = serde_json::json!({"id": "20001"});
        }
        assert!(schema_accepts(schema, &applied), "{command} valid applied");

        for mut extra in [dry_run.clone(), applied.clone()] {
            extra["unexpected"] = json!(true);
            assert!(!schema_accepts(schema, &extra), "{command} extra envelope");
        }

        for impossible_issue in [
            Value::Null,
            serde_json::json!(7),
            serde_json::json!({"key": 7}),
        ] {
            let mut impossible = applied.clone();
            impossible["data"]["issue"] = impossible_issue;
            assert!(
                !schema_accepts(schema, &impossible),
                "{command}: {impossible}"
            );
        }
        if command == "create" {
            for field in ["id", "url"] {
                let mut impossible = applied.clone();
                impossible["data"]["issue"][field] = Value::Null;
                assert!(
                    !schema_accepts(schema, &impossible),
                    "{field}: {impossible}"
                );
            }
        }
        if command == "comment" {
            for impossible_comment in [
                Value::Null,
                serde_json::json!(7),
                serde_json::json!({"id": 7}),
            ] {
                let mut impossible = applied.clone();
                impossible["data"]["comment"] = impossible_comment;
                assert!(!schema_accepts(schema, &impossible), "{impossible}");
            }
        }
    }
}

#[test]
fn create_schema_keeps_hierarchy_fields_metadata_driven() {
    let (_, schema) = success_json(&["schema", "issue", "create"]);
    let fields = &schema["data"]["stdin_schema"]["properties"]["fields"];

    assert_eq!(fields["type"], "object");
    assert_eq!(fields["required"], serde_json::json!(["summary"]));
    assert!(fields.get("properties").is_none());
    assert!(fields.get("patternProperties").is_none());
    assert!(!schema.to_string().contains("parent"));
    assert!(!schema.to_string().contains("customfield_"));
}

#[test]
fn project_create_schema_rejects_non_object_runtime_shapes() {
    let (raw, value) = success_json(&["schema", "project", "create"]);
    assert!(raw.len() <= 3 * 1024, "schema is {} bytes", raw.len());
    let schema = &value["data"]["success_schema"];
    let data_schema = &schema["properties"]["data"];
    let alternatives = data_schema["oneOf"].as_array().unwrap();
    let planned_schema = &alternatives[0];
    let applied_schema = &alternatives[1];

    assert_eq!(schema["type"], "object");
    assert_eq!(data_schema["type"], "object");
    assert_eq!(planned_schema["properties"]["body"]["type"], "object");
    assert_eq!(applied_schema["properties"]["project"]["type"], "object");
    assert_eq!(
        applied_schema["properties"]["project"]["properties"]["id"]["type"],
        "string"
    );
    assert_eq!(
        applied_schema["properties"]["project"]["properties"]["key"]["type"],
        "string"
    );

    let planned = serde_json::json!({
        "data": {
            "operation": "project.create",
            "method": "POST",
            "path": "/rest/api/3/project",
            "body": {"key": "OPSDEMO", "future": true}
        }
    });
    let applied = serde_json::json!({
        "data": {
            "operation": "project.create",
            "outcome": "applied",
            "project": {"id": "10001", "key": "OPSDEMO"}
        }
    });
    assert!(schema_accepts(schema, &planned));
    assert!(schema_accepts(schema, &applied));

    for impossible in [Value::Null, serde_json::json!(7), serde_json::json!([])] {
        assert!(!schema_accepts(schema, &impossible), "root: {impossible}");

        let mut invalid = planned.clone();
        invalid["data"] = impossible.clone();
        assert!(!schema_accepts(schema, &invalid), "data: {impossible}");

        let mut invalid = planned.clone();
        invalid["data"]["body"] = impossible.clone();
        assert!(!schema_accepts(schema, &invalid), "body: {impossible}");

        let mut invalid = applied.clone();
        invalid["data"]["project"] = impossible.clone();
        assert!(!schema_accepts(schema, &invalid), "project: {impossible}");
    }

    for field in ["id", "key"] {
        for impossible in [Value::Null, serde_json::json!(7), serde_json::json!([])] {
            let mut invalid = applied.clone();
            invalid["data"]["project"][field] = impossible.clone();
            assert!(
                !schema_accepts(schema, &invalid),
                "project.{field}: {impossible}"
            );
        }
    }
}

fn schema_accepts(schema: &Value, instance: &Value) -> bool {
    if let Some(expected) = schema.get("const")
        && instance != expected
    {
        return false;
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(instance)
    {
        return false;
    }
    if let Some(types) = schema.get("type") {
        let matches = match types {
            Value::String(value_type) => value_matches_type(instance, value_type),
            Value::Array(value_types) => value_types.iter().any(|value_type| {
                value_type
                    .as_str()
                    .is_some_and(|value_type| value_matches_type(instance, value_type))
            }),
            _ => false,
        };
        if !matches {
            return false;
        }
    }
    if let Some(string) = instance.as_str() {
        let character_count = string.chars().count();
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| character_count > maximum as usize)
            || schema
                .get("x-maxBytes")
                .and_then(Value::as_u64)
                .is_some_and(|maximum| string.len() > maximum as usize)
        {
            return false;
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
            let matches = match pattern {
                STRICT_ISSUE_KEY_PATTERN => strict_issue_key(string),
                NONBLANK_NO_CONTROL_PATTERN => {
                    !string.trim().is_empty() && !string.chars().any(char::is_control)
                }
                "^https://" => string.starts_with("https://"),
                other => panic!("test schema matcher does not implement pattern {other}"),
            };
            if !matches {
                return false;
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array)
        && let Some(object) = instance.as_object()
        && required
            .iter()
            .filter_map(Value::as_str)
            .any(|name| !object.contains_key(name))
    {
        return false;
    }
    if let (Some(properties), Some(object)) = (
        schema.get("properties").and_then(Value::as_object),
        instance.as_object(),
    ) {
        if schema.get("additionalProperties") == Some(&Value::Bool(false))
            && object.keys().any(|name| !properties.contains_key(name))
        {
            return false;
        }
        for (name, property_schema) in properties {
            if let Some(value) = object.get(name)
                && !schema_accepts(property_schema, value)
            {
                return false;
            }
        }
    }
    if let (Some(item_schema), Some(items)) = (schema.get("items"), instance.as_array())
        && items.iter().any(|item| !schema_accepts(item_schema, item))
    {
        return false;
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array)
        && one_of
            .iter()
            .filter(|alternative| schema_accepts(alternative, instance))
            .count()
            != 1
    {
        return false;
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array)
        && !any_of
            .iter()
            .any(|alternative| schema_accepts(alternative, instance))
    {
        return false;
    }
    true
}

fn value_matches_type(value: &Value, value_type: &str) -> bool {
    match value_type {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        other => panic!("test schema matcher does not implement type {other}"),
    }
}

fn strict_issue_key(issue: &str) -> bool {
    issue == issue.trim()
        && issue.split_once('-').is_some_and(|(project, number)| {
            !project.is_empty()
                && !number.is_empty()
                && !number.contains('-')
                && project
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_uppercase)
                && project
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
                && number
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[test]
fn schema_rejects_unknown_operation() {
    let run = run(&["schema", "issue", "erase"]);
    assert_eq!(run.code, 2);
    let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
    assert_eq!(value["error"]["code"], "invalid_input");
}

#[test]
fn every_declared_beta_argv_shape_is_accepted_by_the_parser() {
    for args in [
        &["version"][..],
        &["schema"][..],
        &[
            "auth",
            "login",
            "--site",
            "https://example.atlassian.net",
            "--email",
            "agent@example.com",
            "--token-stdin",
        ][..],
        &["auth", "status"][..],
        &["auth", "logout"][..],
        &["me"][..],
        &["project", "list", "--limit", "1"][..],
        &["project", "get", "ACCL"][..],
        &["project", "templates", "--type", "software"][..],
        &["project", "create", "--input", "-"][..],
        &["field", "list", "--query", "story", "--limit", "100"][..],
        &["issue", "get", "ACCL-1", "--fields", "summary,status"][..],
        &[
            "issue",
            "search",
            "--jql",
            "project = ACCL",
            "--fields",
            "summary",
            "--limit",
            "20",
        ][..],
        &[
            "issue",
            "create-meta",
            "--project",
            "ACCL",
            "--issue-type",
            "10001",
        ][..],
        &["issue", "create-meta", "--project", "ACCL"][..],
        &["issue", "comments", "ACCL-1", "--limit", "20"][..],
        &["issue", "transitions", "ACCL-1"][..],
        &["issue", "create", "--input", "-"][..],
        &["issue", "update", "ACCL-1", "--input", "-"][..],
        &["issue", "comment", "ACCL-1", "--input", "-"][..],
        &["issue", "transition", "ACCL-1", "--input", "-", "--apply"][..],
    ] {
        let run = run(args);
        if run.code == 2 {
            let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
            assert_ne!(
                value["error"]["code"], "invalid_input",
                "declared syntax rejected: {args:?}"
            );
        }
    }
}

#[test]
fn parser_rejects_out_of_range_and_ambiguous_values() {
    for args in [
        &["--timeout-ms", "999", "version"][..],
        &["project", "list", "--limit", "0"][..],
        &["field", "list", "--limit", "101"][..],
        &["issue", "get", "ACCL-1", "--fields", "summary, summary"][..],
        &["issue", "get", "ACCL-1", "--fields", "summary,"][..],
    ] {
        let run = run(args);
        assert_eq!(run.code, 2, "args: {args:?}");
        let value: Value = serde_json::from_str(&run.stderr).expect("valid error JSON");
        assert_eq!(value["error"]["code"], "invalid_input", "args: {args:?}");
    }
}
