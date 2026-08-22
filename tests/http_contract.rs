use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::time::Duration;

use jira_ops::adf::text_to_adf;
use jira_ops::client::{
    DispatchPhase, HttpRequest, HttpResponse, JiraClient, JiraTransport, RequestEffect,
    TransportFailure, TransportFailureKind, WriteEndpoint, WriteFailure, classify_write_failure,
};
use jira_ops::commands::assignment::{apply_assignment, plan_assignment};
use jira_ops::commands::auth::{auth_login, me_command, myself, tenant_info};
use jira_ops::commands::comment::{apply_comment, issue_comments, plan_comment};
use jira_ops::commands::field::field_list;
use jira_ops::commands::issue::{
    apply_create_issue, apply_update_issue, issue_create_meta, issue_get, issue_search,
    plan_create_issue, plan_update_issue, validate_update_input,
};
use jira_ops::commands::link::{apply_link, issue_link_get, issue_link_types, plan_link};
use jira_ops::commands::project::{
    apply_project_create, plan_project_create, project_get, project_list, project_templates,
};
use jira_ops::commands::transition::{
    apply_transition_issue, issue_transitions, plan_transition_issue, validate_transition_input,
};
use jira_ops::commands::watcher::{
    apply_watcher_add, apply_watcher_remove, issue_watchers, plan_watcher_add, plan_watcher_remove,
};
use jira_ops::commands::{validate_confirmation, validate_confirmed_set};
use jira_ops::config::{
    ConfigStore, CredentialKey, CredentialSource, CredentialStore, EnvironmentSource,
    ResolvedCredential, SavedIdentity, StoreError,
};
use jira_ops::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use jira_ops::error::{ErrorCode, ExitClass, OperationOutcome, RetrySafety};
use jira_ops::model::{
    AssignmentInput, CommentInput, CreateIssueInput, LinkInput, MutationPlan, ProjectCreateInput,
    TransitionInput, UpdateIssueInput, ValidationLevel, WatcherInput,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

struct CaptureTransport {
    requests: RefCell<Vec<HttpRequest>>,
    response: HttpResponse,
}

impl CaptureTransport {
    fn new(response: HttpResponse) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            response,
        }
    }

    fn call_count(&self) -> usize {
        self.requests.borrow().len()
    }
}

impl JiraTransport for CaptureTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        assert_eq!(
            request.effect,
            RequestEffect::Read,
            "planner dispatched a Jira write"
        );
        self.requests.borrow_mut().push(request);
        Ok(self.response.clone())
    }
}

struct ScriptedTransport {
    requests: RefCell<Vec<HttpRequest>>,
    responses: RefCell<VecDeque<HttpResponse>>,
}

impl ScriptedTransport {
    fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into_iter().collect()),
        }
    }
}

impl JiraTransport for ScriptedTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        assert_eq!(
            request.effect,
            RequestEffect::Read,
            "planner dispatched a Jira write"
        );
        self.requests.borrow_mut().push(request);
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("scripted response"))
    }
}

struct FailingTransport(TransportFailureKind);

impl JiraTransport for FailingTransport {
    fn execute(&self, _request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        Err(TransportFailure::new(
            self.0,
            DispatchPhase::BeforeDispatch,
            None,
        ))
    }
}

struct WriteScriptedTransport {
    requests: RefCell<Vec<HttpRequest>>,
    responses: RefCell<VecDeque<Result<HttpResponse, TransportFailure>>>,
}

impl WriteScriptedTransport {
    fn new(responses: impl IntoIterator<Item = Result<HttpResponse, TransportFailure>>) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into_iter().collect()),
        }
    }
}

impl JiraTransport for WriteScriptedTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        self.requests.borrow_mut().push(request);
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("scripted write response")
    }
}

fn assert_exact_write_request(
    request: &HttpRequest,
    method: jira_ops::client::HttpMethod,
    path: &str,
    body: Value,
) {
    assert_eq!(request.method, method);
    assert_eq!(
        request.url.as_str(),
        format!("https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000{path}")
    );
    assert!(request.url.query().is_none());
    assert_eq!(request.effect, RequestEffect::JiraWrite);
    assert_eq!(
        request
            .headers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["accept", "authorization", "content-type", "user-agent"]
    );
    assert_eq!(
        request.headers["accept"].expose_secret(),
        "application/json"
    );
    assert_eq!(
        request.headers["content-type"].expose_secret(),
        "application/json"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        body
    );
}

#[test]
fn create_apply_uses_exact_request_contract() {
    let transport =
        WriteScriptedTransport::new([Ok(json_response(201, r#"{"id":"10001","key":"ACCL-1"}"#))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let response = client
        .jira_write(
            WriteEndpoint::CreateIssue,
            "/rest/api/3/issue",
            &json!({"fields":{"project":{"key":"ACCL"},"issuetype":{"id":"10001"},"summary":"x"}}),
        )
        .unwrap();

    assert_eq!(response.status, 201);
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/issue",
        json!({"fields":{"project":{"key":"ACCL"},"issuetype":{"id":"10001"},"summary":"x"}}),
    );
    assert_eq!(
        client.verified_site().as_str(),
        "https://example.atlassian.net/"
    );
}

#[test]
fn update_apply_uses_exact_request_contract() {
    let transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    client
        .jira_write(
            WriteEndpoint::UpdateIssue,
            "/rest/api/3/issue/ACCL-1",
            &json!({"fields":{"summary":"updated"}}),
        )
        .unwrap();

    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Put,
        "/rest/api/3/issue/ACCL-1",
        json!({"fields":{"summary":"updated"}}),
    );
}

#[test]
fn assignment_plan_and_apply_preserve_exact_contract() {
    let unassign = plan_assignment(AssignmentInput {
        issue_key: "ACCL-1".to_owned(),
        account_id: None,
    })
    .unwrap();
    assert_eq!(unassign.wire_payload(), &json!({"accountId":null}));
    assert_eq!(
        serde_json::to_value(&unassign).unwrap(),
        json!({"operation":"issue.assign","applied":false,"target":{"issue":"ACCL-1"},"changes":{"account_id":null},"validation":{"local":"passed","metadata":"not_applicable"}})
    );

    let transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let plan = plan_assignment(AssignmentInput {
        issue_key: "ACCL-1".to_owned(),
        account_id: Some("abc".to_owned()),
    })
    .unwrap();
    let applied = apply_assignment(&client, plan).unwrap();
    assert_eq!(
        serde_json::to_value(applied).unwrap(),
        json!({"operation":"issue.assign","applied":true,"issue":{"key":"ACCL-1"},"assignment":{"account_id":"abc"}})
    );
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Put,
        "/rest/api/3/issue/ACCL-1/assignee",
        json!({"accountId":"abc"}),
    );
}

#[test]
fn assignment_rejects_bad_identifiers_and_nonempty_204_without_retry() {
    for input in [
        AssignmentInput {
            issue_key: "accl-1".to_owned(),
            account_id: Some("abc".to_owned()),
        },
        AssignmentInput {
            issue_key: "ACCL-1".to_owned(),
            account_id: Some("bad\naccount".to_owned()),
        },
    ] {
        assert_eq!(
            plan_assignment(input).unwrap_err().code,
            ErrorCode::SchemaViolation
        );
    }

    let transport = WriteScriptedTransport::new([
        Ok(json_response(204, r#"{"secret":"must-not-leak"}"#)),
        Ok(json_response(204, "")),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let error = apply_assignment(
        &client,
        plan_assignment(AssignmentInput {
            issue_key: "ACCL-1".to_owned(),
            account_id: Some("abc".to_owned()),
        })
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::MutationResponseInvalid);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
    assert_eq!(error.retry_safety, RetrySafety::Unsafe);
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(transport.responses.borrow().len(), 1);
}

#[test]
fn issue_link_reads_use_documented_paths_and_project_only_safe_fields() {
    let transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"issueLinkTypes":[{"id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks","self":"https://secret.invalid/type"}]}"#,
        ),
        json_response(
            200,
            r#"{"id":"20000","self":"https://secret.invalid/link","type":{"id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks","self":"https://secret.invalid/type"},"inwardIssue":{"key":"ACCL-1","self":"https://secret.invalid/issue","fields":{"summary":"secret"}},"outwardIssue":{"key":"OPS-2","fields":{"reporter":{"emailAddress":"secret@example.com"}}}}"#,
        ),
    ]);
    let client = client_with_scripted(&transport);
    let types = issue_link_types(&client).unwrap();
    assert_eq!(
        serde_json::to_value(types).unwrap(),
        json!({"data":[{"id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks"}],"meta":{"count":1}})
    );
    let link = issue_link_get(&client, "20000").unwrap();
    let value = serde_json::to_value(link).unwrap();
    assert_eq!(
        value,
        json!({"data":{"id":"20000","type":{"id":"10000","name":"Blocks","inward":"is blocked by","outward":"blocks"},"inward_issue":{"key":"ACCL-1"},"outward_issue":{"key":"OPS-2"}}})
    );
    let serialized = value.to_string();
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("email"));
    let requests = transport.requests.borrow();
    assert_get_request(
        &requests[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issueLinkType",
        &[],
    );
    assert_get_request(
        &requests[1],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issueLink/20000",
        &[],
    );
}

#[test]
fn issue_link_add_preserves_direction_and_requires_empty_201() {
    let plan = plan_link(LinkInput {
        inward_issue: "ACCL-1".to_owned(),
        outward_issue: "OPS-2".to_owned(),
        type_name: "Blocks".to_owned(),
    })
    .unwrap();
    assert_eq!(
        plan.wire_payload(),
        &json!({"type":{"name":"Blocks"},"inwardIssue":{"key":"ACCL-1"},"outwardIssue":{"key":"OPS-2"}})
    );

    let transport = WriteScriptedTransport::new([Ok(json_response(201, ""))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let applied = apply_link(&client, plan).unwrap();
    assert_eq!(
        serde_json::to_value(applied).unwrap(),
        json!({"operation":"issue.link.add","applied":true,"link":{"inward_issue":"ACCL-1","outward_issue":"OPS-2","type_name":"Blocks"}})
    );
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/issueLink",
        json!({"type":{"name":"Blocks"},"inwardIssue":{"key":"ACCL-1"},"outwardIssue":{"key":"OPS-2"}}),
    );

    let transport = WriteScriptedTransport::new([
        Ok(json_response(201, r#"{"id":"must-not-be-invented"}"#)),
        Ok(json_response(201, "")),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let error = apply_link(
        &client,
        plan_link(LinkInput {
            inward_issue: "ACCL-1".to_owned(),
            outward_issue: "OPS-2".to_owned(),
            type_name: "Blocks".to_owned(),
        })
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::MutationResponseInvalid);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(transport.responses.borrow().len(), 1);
}

#[test]
fn watcher_list_projects_only_account_display_and_active() {
    let transport = ScriptedTransport::new([json_response(
        200,
        r#"{"self":"https://secret.invalid/watchers","isWatching":true,"watchCount":1,"watchers":[{"accountId":"abc","displayName":"Agent","active":true,"emailAddress":"secret@example.com","self":"https://secret.invalid/user","avatarUrls":{"48x48":"https://secret.invalid/avatar"}}]}"#,
    )]);
    let client = client_with_scripted(&transport);
    let envelope = issue_watchers(&client, "ACCL-1").unwrap();
    let value = serde_json::to_value(envelope).unwrap();
    assert_eq!(
        value,
        json!({"data":[{"account_id":"abc","display_name":"Agent","active":true}],"meta":{"count":1}})
    );
    let serialized = value.to_string();
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("email"));
    assert!(!serialized.contains("self"));
    assert_get_request(
        &transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/watchers",
        &[],
    );
}

#[test]
fn watcher_add_and_remove_use_exact_body_and_encoded_query() {
    let add_transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let add_client = JiraClient::new(&add_transport, test_credential(), Duration::from_secs(30));
    let add = apply_watcher_add(
        &add_client,
        plan_watcher_add(WatcherInput {
            issue_key: "ACCL-1".to_owned(),
            account_id: "abc".to_owned(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(add).unwrap(),
        json!({"operation":"issue.watcher.add","applied":true,"issue":{"key":"ACCL-1"},"watcher":{"account_id":"abc"}})
    );
    assert_exact_write_request(
        &add_transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/issue/ACCL-1/watchers",
        json!("abc"),
    );

    let remove_transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let remove_client = JiraClient::new(
        &remove_transport,
        test_credential(),
        Duration::from_secs(30),
    );
    let remove = apply_watcher_remove(
        &remove_client,
        plan_watcher_remove(WatcherInput {
            issue_key: "ACCL-1".to_owned(),
            account_id: "abc +/=?".to_owned(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(remove).unwrap(),
        json!({"operation":"issue.watcher.remove","applied":true,"issue":{"key":"ACCL-1"},"watcher":{"account_id":"abc +/=?"}})
    );
    let requests = remove_transport.requests.borrow();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(format!("{:?}", request.method), "Delete");
    assert_eq!(request.effect, RequestEffect::JiraWrite);
    assert!(request.body.is_empty());
    assert_eq!(
        request.url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/watchers?accountId=abc+%2B%2F%3D%3F"
    );
    assert_eq!(
        request
            .headers
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["accept", "authorization", "user-agent"]
    );
}

#[test]
fn daily_core_reads_require_exact_200_and_reject_ambiguous_projection() {
    let transport = ScriptedTransport::new([json_response(201, r#"{"issueLinkTypes":[]}"#)]);
    let error = issue_link_types(&client_with_scripted(&transport)).unwrap_err();
    assert_eq!(error.code, ErrorCode::ResponseInvalid);
    assert_eq!(error.status, Some(201));
    assert_eq!(transport.requests.borrow().len(), 1);

    let transport = ScriptedTransport::new([json_response(
        200,
        r#"{"watchers":[{"accountId":"abc","displayName":"One","active":true},{"accountId":"abc","displayName":"Two","active":true}]}"#,
    )]);
    let error = issue_watchers(&client_with_scripted(&transport), "ACCL-1").unwrap_err();
    assert_eq!(error.code, ErrorCode::ResponseInvalid);
    assert_eq!(transport.requests.borrow().len(), 1);

    let transport = ScriptedTransport::new([]);
    let error = issue_link_get(&client_with_scripted(&transport), "../1").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(transport.requests.borrow().is_empty());
}

#[test]
fn comment_apply_uses_exact_request_contract() {
    let transport = WriteScriptedTransport::new([Ok(json_response(201, r#"{"id":"20001"}"#))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let adf = text_to_adf("hello");

    client
        .jira_write(
            WriteEndpoint::AddComment,
            "/rest/api/3/issue/ACCL-1/comment",
            &json!({"body":adf}),
        )
        .unwrap();

    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/issue/ACCL-1/comment",
        json!({"body":text_to_adf("hello")}),
    );
}

#[test]
fn internal_comment_preflights_issue_uses_jsm_and_never_falls_back() {
    let plan = plan_comment(
        "OPS-1",
        CommentInput {
            body: jira_ops::content::ContentInput::Explicit {
                format: jira_ops::content::ContentFormat::Markdown,
                value: json!("**private**"),
            },
            internal: true,
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload(),
        &json!({"body":"**private**","public":false})
    );

    let transport = WriteScriptedTransport::new([
        Ok(json_response(200, r#"{"id":"10001"}"#)),
        Ok(json_response(201, r#"{"id":"20001"}"#)),
    ]);
    let applied = apply_comment(
        &JiraClient::new(&transport, test_credential(), Duration::from_secs(30)),
        "OPS-1",
        plan,
    )
    .unwrap();
    assert_eq!(applied.comment.id, "20001");
    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_get_request(
        &requests[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/OPS-1",
        &[("fields", "id")],
    );
    assert_exact_write_request(
        &requests[1],
        jira_ops::client::HttpMethod::Post,
        "/rest/servicedeskapi/request/OPS-1/comment",
        json!({"body":"**private**","public":false}),
    );
    drop(requests);

    let rejected_plan = plan_comment(
        "OPS-1",
        CommentInput {
            body: "private".into(),
            internal: true,
        },
    )
    .unwrap();
    let rejected = WriteScriptedTransport::new([
        Ok(json_response(200, r#"{"id":"10001"}"#)),
        Ok(json_response(404, r#"{"errorMessages":["not available"]}"#)),
    ]);
    let error = apply_comment(
        &JiraClient::new(&rejected, test_credential(), Duration::from_secs(30)),
        "OPS-1",
        rejected_plan,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedJiraCapability);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
    assert_eq!(rejected.requests.borrow().len(), 2);
}

#[test]
fn notification_query_is_exact_for_true_false_and_omission() {
    for (notify_users, expected_query) in [
        (Some(true), Some("notifyUsers=true")),
        (Some(false), Some("notifyUsers=false")),
        (None, None),
    ] {
        let transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
        let mut changes = json!({"set":{"summary":"x"}});
        if let Some(notify_users) = notify_users {
            changes
                .as_object_mut()
                .unwrap()
                .insert("notify_users".to_owned(), json!(notify_users));
        }
        let plan = MutationPlan::dry_run(
            "issue.update",
            json!({"issue":"OPS-1"}),
            changes,
            ValidationLevel::Passed,
            json!({"fields":{"summary":"x"}}),
        );
        apply_update_issue(
            &JiraClient::new(&transport, test_credential(), Duration::from_secs(30)),
            "OPS-1",
            plan,
        )
        .unwrap();
        let requests = transport.requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.query(), expected_query);
    }
}

#[test]
fn transition_comment_compiles_to_update_operation_and_notification_query() {
    let planning = CaptureTransport::new(json_response(
        200,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"3","name":"Done"},"fields":{}}]}"#,
    ));
    let plan = plan_transition_issue(
        &client_with(&planning),
        "OPS-1",
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::new(),
            comment: Some("finished".into()),
            notify_users: Some(true),
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload(),
        &json!({
            "transition":{"id":"31"},
            "fields":{},
            "update":{"comment":[{"add":{"body":{
                "type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[{"type":"text","text":"finished"}]}
                ]
            }}}]}
        })
    );

    let transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    apply_transition_issue(
        &JiraClient::new(&transport, test_credential(), Duration::from_secs(30)),
        "OPS-1",
        plan,
    )
    .unwrap();
    assert_eq!(
        transport.requests.borrow()[0].url.query(),
        Some("notifyUsers=true")
    );
}

#[test]
fn tagged_description_compiles_before_metadata_wire_projection() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}]}"#,
    ));
    let plan = plan_create_issue(
        &client_with(&transport),
        CreateIssueInput {
            project_key: "OPS".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([
                ("summary".to_owned(), json!("Rich issue")),
                (
                    "description".to_owned(),
                    json!({"format":"markdown","value":"## Context"}),
                ),
            ]),
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload()["fields"]["description"],
        json!({
            "type":"doc",
            "version":1,
            "content":[{
                "type":"heading",
                "attrs":{"level":2},
                "content":[{"type":"text","text":"Context"}]
            }]
        })
    );
}

#[test]
fn transition_apply_uses_exact_request_contract() {
    let transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    client
        .jira_write(
            WriteEndpoint::TransitionIssue,
            "/rest/api/3/issue/ACCL-1/transitions",
            &json!({"transition":{"id":"31"},"fields":{}}),
        )
        .unwrap();

    assert_eq!(transport.requests.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/issue/ACCL-1/transitions",
        json!({"transition":{"id":"31"},"fields":{}}),
    );
}

#[test]
fn jira_writes_are_never_retried() {
    let cases = [
        Err(TransportFailure::new(
            TransportFailureKind::Timeout,
            DispatchPhase::DispatchStarted,
            None,
        )),
        Ok(json_response(429, r#"{"errorMessages":["slow down"]}"#)),
        Ok(json_response(503, r#"{"errorMessages":["unavailable"]}"#)),
        Ok(json_response(201, "not-json")),
    ];

    for first in cases {
        let transport = WriteScriptedTransport::new([
            first,
            Ok(json_response(201, r#"{"id":"second","key":"ACCL-2"}"#)),
        ]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let _ = client.jira_write(
            WriteEndpoint::CreateIssue,
            "/rest/api/3/issue",
            &json!({"fields":{"summary":"x"}}),
        );
        assert_eq!(transport.requests.borrow().len(), 1);
        assert_eq!(transport.responses.borrow().len(), 1);
    }
}

#[test]
fn rate_limited_body_failure_preserves_bounded_headers_and_does_not_retry() {
    let failure = TransportFailure::response_started_with_headers(
        TransportFailureKind::Protocol,
        429,
        &BTreeMap::from([
            ("retry-after".to_owned(), "12".to_owned()),
            (
                "ratelimit-reason".to_owned(),
                "jira-current-limit".to_owned(),
            ),
            (
                "x-ratelimit-reason".to_owned(),
                "jira-legacy-limit".to_owned(),
            ),
            ("response-prose".to_owned(), "must-not-escape".to_owned()),
        ]),
    );
    let transport = WriteScriptedTransport::new([
        Err(failure),
        Ok(json_response(201, r#"{"id":"second","key":"ACCL-2"}"#)),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let error = client
        .jira_write(
            WriteEndpoint::CreateIssue,
            "/rest/api/3/issue",
            &json!({"fields":{"summary":"x"}}),
        )
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::RateLimited);
    assert_eq!(error.status, Some(429));
    assert_eq!(error.retry_after_ms, Some(12_000));
    assert_eq!(
        error.rate_limit_reason.as_deref(),
        Some("jira-current-limit")
    );
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
    assert_eq!(error.retry_safety, RetrySafety::Safe);
    assert!(
        !serde_json::to_string(&error)
            .unwrap()
            .contains("must-not-escape")
    );
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(transport.responses.borrow().len(), 1);
}

#[test]
fn rate_limited_body_failure_checks_retry_after_overflow() {
    let failure = TransportFailure::response_started_with_headers(
        TransportFailureKind::ResponseTooLarge,
        429,
        &BTreeMap::from([("retry-after".to_owned(), u64::MAX.to_string())]),
    );
    let transport = WriteScriptedTransport::new([Err(failure)]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let error = client
        .jira_write(
            WriteEndpoint::CreateIssue,
            "/rest/api/3/issue",
            &json!({"fields":{"summary":"x"}}),
        )
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::RateLimited);
    assert_eq!(error.retry_after_ms, None);
    assert_eq!(transport.requests.borrow().len(), 1);
}

fn create_write_plan() -> MutationPlan {
    MutationPlan::dry_run(
        "issue.create",
        json!({"project_key":"ACCL","issue_type_id":"10001"}),
        json!({"fields":{"summary":"x"}}),
        ValidationLevel::Passed,
        json!({"fields":{"project":{"key":"ACCL"},"issuetype":{"id":"10001"},"summary":"x"}}),
    )
}

fn issue_write_plan(operation: &'static str, body: Value) -> MutationPlan {
    MutationPlan::dry_run(
        operation,
        json!({"issue":"ACCL-1"}),
        json!({}),
        ValidationLevel::Passed,
        body,
    )
}

#[test]
fn mutation_issue_keys_use_the_strict_v1_contract() {
    let nonempty_update = UpdateIssueInput {
        set: BTreeMap::from([("summary".to_owned(), json!("x"))]),
        notify_users: None,
    };
    let transition = TransitionInput {
        transition_id: "31".to_owned(),
        fields: BTreeMap::new(),
        comment: None,
        notify_users: None,
    };
    for key in ["ACCL-1", "A1_B-42"] {
        validate_update_input(key, &nonempty_update).unwrap();
        plan_comment(
            key,
            CommentInput {
                body: "x".into(),
                internal: false,
            },
        )
        .unwrap();
        validate_transition_input(key, &transition).unwrap();
    }

    for key in [
        "10001", "accl-1", "", " ", " ACCL-1", "ACCL-1 ", "ACCL-0", "ACCL", "ACCL-1-2", "-1",
        "1ACCL-1", "AC.CL-1", "ACCL-1?x", "ACCL-1#x", "ACCL/1",
    ] {
        let error = validate_update_input(key, &nonempty_update).unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaViolation, "{key:?}");
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
        assert_eq!(error.retry_safety, RetrySafety::Safe);
    }
}

#[test]
fn create_success_response_requires_nonblank_string_id_and_key() {
    let invalid_bodies = [
        r#"{"key":"ACCL-1"}"#,
        r#"{"id":null,"key":"ACCL-1"}"#,
        r#"{"id":10001,"key":"ACCL-1"}"#,
        r#"{"id":" ","key":"ACCL-1"}"#,
        r#"{"id":"10001"}"#,
        r#"{"id":"10001","key":null}"#,
        r#"{"id":"10001","key":1}"#,
        r#"{"id":"10001","key":" "}"#,
        "<html>applied</html>",
        "not-json",
        r#"{"id":"10001","key":"ACCL-1""#,
    ];
    for body in invalid_bodies {
        let transport = WriteScriptedTransport::new([Ok(json_response(201, body))]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let error = apply_create_issue(&client, create_write_plan()).unwrap_err();
        assert_eq!(error.code, ErrorCode::MutationResponseInvalid, "{body:?}");
        assert_eq!(error.status, Some(201));
        assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
        assert_eq!(error.retry_safety, RetrySafety::Unsafe);
        assert_eq!(error.code.exit_class(), ExitClass::MutationOutcome);
        assert_eq!(transport.requests.borrow().len(), 1);
    }

    let oversized = vec![b'x'; jira_ops::client::MAX_RESPONSE_BYTES + 1];
    let transport = WriteScriptedTransport::new([Ok(HttpResponse {
        status: 201,
        headers: BTreeMap::new(),
        body: oversized,
    })]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
    let error = apply_create_issue(&client, create_write_plan()).unwrap_err();
    assert_eq!(error.code, ErrorCode::MutationResponseInvalid);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
    assert_eq!(error.retry_safety, RetrySafety::Unsafe);
}

#[test]
fn comment_success_response_requires_a_nonblank_string_id() {
    for body in [
        "{}",
        r#"{"id":null}"#,
        r#"{"id":20001}"#,
        r#"{"id":" "}"#,
        "<html>applied</html>",
        "not-json",
        r#"{"id":"20001""#,
    ] {
        let transport = WriteScriptedTransport::new([Ok(json_response(201, body))]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let error = apply_comment(
            &client,
            "ACCL-1",
            issue_write_plan("issue.comment", json!({"body":text_to_adf("hello")})),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::MutationResponseInvalid, "{body:?}");
        assert_eq!(error.status, Some(201));
        assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
        assert_eq!(error.retry_safety, RetrySafety::Unsafe);
    }
}

#[test]
fn create_and_comment_handlers_do_not_retry_malformed_or_truncated_201() {
    for body in ["not-json", r#"{"id":"10001","key":"ACCL-1""#] {
        let transport = WriteScriptedTransport::new([
            Ok(json_response(201, body)),
            Ok(json_response(201, r#"{"id":"second","key":"ACCL-2"}"#)),
        ]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let error = apply_create_issue(&client, create_write_plan()).unwrap_err();
        assert_eq!(error.code, ErrorCode::MutationResponseInvalid, "{body:?}");
        assert_eq!(error.status, Some(201));
        assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
        assert_eq!(error.retry_safety, RetrySafety::Unsafe);
        assert_eq!(transport.requests.borrow().len(), 1);
        assert_eq!(transport.responses.borrow().len(), 1);
    }

    for body in ["not-json", r#"{"id":"20001""#] {
        let transport = WriteScriptedTransport::new([
            Ok(json_response(201, body)),
            Ok(json_response(201, r#"{"id":"second"}"#)),
        ]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let error = apply_comment(
            &client,
            "ACCL-1",
            issue_write_plan("issue.comment", json!({"body":text_to_adf("hello")})),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::MutationResponseInvalid, "{body:?}");
        assert_eq!(error.status, Some(201));
        assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
        assert_eq!(error.retry_safety, RetrySafety::Unsafe);
        assert_eq!(transport.requests.borrow().len(), 1);
        assert_eq!(transport.responses.borrow().len(), 1);
    }
}

#[test]
fn oversized_comment_201_is_applied_unsafe_and_not_retried() {
    let transport = WriteScriptedTransport::new([
        Ok(HttpResponse {
            status: 201,
            headers: BTreeMap::new(),
            body: vec![b'x'; jira_ops::client::MAX_RESPONSE_BYTES + 1],
        }),
        Ok(json_response(201, r#"{"id":"second"}"#)),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let error = apply_comment(
        &client,
        "ACCL-1",
        issue_write_plan("issue.comment", json!({"body":text_to_adf("hello")})),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::MutationResponseInvalid);
    assert_eq!(error.status, Some(201));
    assert_eq!(error.operation_outcome, Some(OperationOutcome::Applied));
    assert_eq!(error.retry_safety, RetrySafety::Unsafe);
    assert_eq!(error.code.exit_class(), ExitClass::MutationOutcome);
    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(transport.responses.borrow().len(), 1);
}

#[test]
fn applied_outputs_are_exact_and_create_url_stays_on_the_verified_site() {
    let create_transport = WriteScriptedTransport::new([Ok(json_response(
        201,
        r#"{"id":"10001","key":"ACCL/1?x#y","self":"https://evil.invalid/leak","extra":true}"#,
    ))]);
    let create_client = JiraClient::new(
        &create_transport,
        test_credential(),
        Duration::from_secs(30),
    );
    let created = apply_create_issue(&create_client, create_write_plan()).unwrap();
    assert_eq!(
        serde_json::to_value(created).unwrap(),
        json!({
            "operation":"issue.create",
            "applied":true,
            "issue":{
                "id":"10001",
                "key":"ACCL/1?x#y",
                "url":"https://example.atlassian.net/browse/ACCL%2F1%3Fx%23y"
            }
        })
    );

    let update_transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let updated = apply_update_issue(
        &JiraClient::new(
            &update_transport,
            test_credential(),
            Duration::from_secs(30),
        ),
        "ACCL-1",
        issue_write_plan("issue.update", json!({"fields":{"summary":"x"}})),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(updated).unwrap(),
        json!({"operation":"issue.update","applied":true,"issue":{"key":"ACCL-1"}})
    );

    let comment_transport =
        WriteScriptedTransport::new([Ok(json_response(201, r#"{"id":"20001","self":"ignored"}"#))]);
    let commented = apply_comment(
        &JiraClient::new(
            &comment_transport,
            test_credential(),
            Duration::from_secs(30),
        ),
        "ACCL-1",
        issue_write_plan("issue.comment", json!({"body":text_to_adf("hello")})),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(commented).unwrap(),
        json!({
            "operation":"issue.comment",
            "applied":true,
            "issue":{"key":"ACCL-1"},
            "comment":{"id":"20001"}
        })
    );

    let transition_transport = WriteScriptedTransport::new([Ok(json_response(204, ""))]);
    let transitioned = apply_transition_issue(
        &JiraClient::new(
            &transition_transport,
            test_credential(),
            Duration::from_secs(30),
        ),
        "ACCL-1",
        issue_write_plan(
            "issue.transition",
            json!({"transition":{"id":"31"},"fields":{}}),
        ),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(transitioned).unwrap(),
        json!({"operation":"issue.transition","applied":true,"issue":{"key":"ACCL-1"}})
    );
}

#[test]
fn write_classifier_preserves_conservative_outcome() {
    let definitive = [
        (WriteEndpoint::CreateIssue, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::CreateIssue, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::CreateIssue, 403, ErrorCode::Forbidden),
        (WriteEndpoint::CreateIssue, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::CreateIssue, 429, ErrorCode::RateLimited),
        (WriteEndpoint::CreateProject, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::CreateProject, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::CreateProject, 403, ErrorCode::Forbidden),
        (WriteEndpoint::CreateProject, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::CreateProject, 429, ErrorCode::RateLimited),
        (WriteEndpoint::UpdateIssue, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::UpdateIssue, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::UpdateIssue, 403, ErrorCode::Forbidden),
        (WriteEndpoint::UpdateIssue, 404, ErrorCode::NotFound),
        (WriteEndpoint::UpdateIssue, 409, ErrorCode::Conflict),
        (WriteEndpoint::UpdateIssue, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::UpdateIssue, 429, ErrorCode::RateLimited),
        (WriteEndpoint::AddComment, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddComment, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::AddComment, 404, ErrorCode::NotFound),
        (WriteEndpoint::AddComment, 413, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddComment, 429, ErrorCode::RateLimited),
        (
            WriteEndpoint::TransitionIssue,
            400,
            ErrorCode::RemoteRejected,
        ),
        (WriteEndpoint::TransitionIssue, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::TransitionIssue, 404, ErrorCode::NotFound),
        (WriteEndpoint::TransitionIssue, 409, ErrorCode::Conflict),
        (
            WriteEndpoint::TransitionIssue,
            413,
            ErrorCode::RemoteRejected,
        ),
        (
            WriteEndpoint::TransitionIssue,
            422,
            ErrorCode::RemoteRejected,
        ),
        (WriteEndpoint::TransitionIssue, 429, ErrorCode::RateLimited),
        (WriteEndpoint::AssignIssue, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::AssignIssue, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::AssignIssue, 403, ErrorCode::Forbidden),
        (WriteEndpoint::AssignIssue, 404, ErrorCode::NotFound),
        (WriteEndpoint::AssignIssue, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::AssignIssue, 429, ErrorCode::RateLimited),
        (WriteEndpoint::AddIssueLink, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddIssueLink, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::AddIssueLink, 403, ErrorCode::Forbidden),
        (WriteEndpoint::AddIssueLink, 404, ErrorCode::NotFound),
        (WriteEndpoint::AddIssueLink, 413, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddIssueLink, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddIssueLink, 429, ErrorCode::RateLimited),
        (WriteEndpoint::AddWatcher, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddWatcher, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::AddWatcher, 403, ErrorCode::Forbidden),
        (WriteEndpoint::AddWatcher, 404, ErrorCode::NotFound),
        (WriteEndpoint::AddWatcher, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::AddWatcher, 429, ErrorCode::RateLimited),
        (WriteEndpoint::RemoveWatcher, 400, ErrorCode::RemoteRejected),
        (WriteEndpoint::RemoveWatcher, 401, ErrorCode::AuthInvalid),
        (WriteEndpoint::RemoveWatcher, 403, ErrorCode::Forbidden),
        (WriteEndpoint::RemoveWatcher, 404, ErrorCode::NotFound),
        (WriteEndpoint::RemoveWatcher, 422, ErrorCode::RemoteRejected),
        (WriteEndpoint::RemoveWatcher, 429, ErrorCode::RateLimited),
    ];
    for (endpoint, status, code) in definitive {
        let classification = classify_write_failure(endpoint, WriteFailure::HttpStatus(status));
        assert_eq!(classification.code, code, "{endpoint:?} {status}");
        assert_eq!(classification.outcome, OperationOutcome::NotApplied);
        assert_eq!(classification.retry_safety, RetrySafety::Safe);
        assert_eq!(classification.exit_class, code.exit_class());
    }

    for (endpoint, status) in [
        (WriteEndpoint::CreateIssue, 200),
        (WriteEndpoint::CreateProject, 200),
        (WriteEndpoint::CreateProject, 404),
        (WriteEndpoint::CreateProject, 409),
        (WriteEndpoint::CreateProject, 413),
        (WriteEndpoint::CreateProject, 500),
        (WriteEndpoint::UpdateIssue, 200),
        (WriteEndpoint::AddComment, 202),
        (WriteEndpoint::TransitionIssue, 201),
        (WriteEndpoint::CreateIssue, 300),
        (WriteEndpoint::CreateIssue, 307),
        (WriteEndpoint::CreateIssue, 308),
        (WriteEndpoint::CreateIssue, 408),
        (WriteEndpoint::CreateIssue, 418),
        (WriteEndpoint::CreateIssue, 500),
        (WriteEndpoint::CreateIssue, 502),
        (WriteEndpoint::CreateIssue, 503),
        (WriteEndpoint::CreateIssue, 504),
        (WriteEndpoint::AssignIssue, 200),
        (WriteEndpoint::AssignIssue, 201),
        (WriteEndpoint::AssignIssue, 413),
        (WriteEndpoint::AssignIssue, 500),
        (WriteEndpoint::AddIssueLink, 200),
        (WriteEndpoint::AddIssueLink, 204),
        (WriteEndpoint::AddIssueLink, 409),
        (WriteEndpoint::AddIssueLink, 500),
        (WriteEndpoint::AddWatcher, 200),
        (WriteEndpoint::AddWatcher, 201),
        (WriteEndpoint::AddWatcher, 409),
        (WriteEndpoint::AddWatcher, 413),
        (WriteEndpoint::AddWatcher, 500),
        (WriteEndpoint::RemoveWatcher, 200),
        (WriteEndpoint::RemoveWatcher, 201),
        (WriteEndpoint::RemoveWatcher, 409),
        (WriteEndpoint::RemoveWatcher, 413),
        (WriteEndpoint::RemoveWatcher, 500),
    ] {
        let classification = classify_write_failure(endpoint, WriteFailure::HttpStatus(status));
        assert_eq!(classification.code, ErrorCode::MutationOutcomeUnknown);
        assert_eq!(classification.outcome, OperationOutcome::Unknown);
        assert_eq!(classification.retry_safety, RetrySafety::Unknown);
        assert_eq!(classification.exit_class, ExitClass::MutationOutcome);
        assert_eq!(classification.exit_class, classification.code.exit_class());
    }

    for endpoint in [
        WriteEndpoint::CreateIssue,
        WriteEndpoint::CreateProject,
        WriteEndpoint::UpdateIssue,
        WriteEndpoint::AddComment,
        WriteEndpoint::TransitionIssue,
        WriteEndpoint::AssignIssue,
        WriteEndpoint::AddIssueLink,
        WriteEndpoint::AddWatcher,
        WriteEndpoint::RemoveWatcher,
    ] {
        let status = endpoint.success_status();
        let classification =
            classify_write_failure(endpoint, WriteFailure::InvalidSuccessBody(status));
        assert_eq!(classification.code, ErrorCode::MutationResponseInvalid);
        assert_eq!(classification.outcome, OperationOutcome::Applied);
        assert_eq!(classification.retry_safety, RetrySafety::Unsafe);
        assert_eq!(classification.exit_class, classification.code.exit_class());
    }

    for (failure, code) in [
        (
            WriteFailure::BeforeDispatch(ErrorCode::InvalidInput),
            ErrorCode::InvalidInput,
        ),
        (
            WriteFailure::BeforeDispatch(ErrorCode::ConfigMissing),
            ErrorCode::ConfigMissing,
        ),
        (
            WriteFailure::Transport(
                TransportFailureKind::Connection,
                DispatchPhase::BeforeDispatch,
                None,
            ),
            ErrorCode::ConnectionFailed,
        ),
    ] {
        let classification = classify_write_failure(WriteEndpoint::CreateIssue, failure);
        assert_eq!(classification.code, code);
        assert_eq!(classification.outcome, OperationOutcome::NotApplied);
        assert_eq!(classification.retry_safety, RetrySafety::Safe);
        assert_eq!(classification.exit_class, classification.code.exit_class());
    }

    for failure in [
        WriteFailure::Transport(
            TransportFailureKind::Timeout,
            DispatchPhase::DispatchStarted,
            None,
        ),
        WriteFailure::Transport(
            TransportFailureKind::Connection,
            DispatchPhase::ResponseStarted,
            None,
        ),
        WriteFailure::Transport(
            TransportFailureKind::Protocol,
            DispatchPhase::DispatchStarted,
            None,
        ),
    ] {
        let classification = classify_write_failure(WriteEndpoint::CreateIssue, failure);
        assert_eq!(classification.code, ErrorCode::MutationOutcomeUnknown);
        assert_eq!(classification.outcome, OperationOutcome::Unknown);
        assert_eq!(classification.retry_safety, RetrySafety::Unknown);
        assert_eq!(classification.exit_class, classification.code.exit_class());
    }

    for (endpoint, success_status) in [
        (WriteEndpoint::AssignIssue, 204),
        (WriteEndpoint::AddIssueLink, 201),
        (WriteEndpoint::AddWatcher, 204),
        (WriteEndpoint::RemoveWatcher, 204),
    ] {
        assert_eq!(endpoint.success_status(), success_status, "{endpoint:?}");

        let before_dispatch = classify_write_failure(
            endpoint,
            WriteFailure::Transport(
                TransportFailureKind::Connection,
                DispatchPhase::BeforeDispatch,
                None,
            ),
        );
        assert_eq!(
            before_dispatch.code,
            ErrorCode::ConnectionFailed,
            "{endpoint:?}"
        );
        assert_eq!(
            before_dispatch.outcome,
            OperationOutcome::NotApplied,
            "{endpoint:?}"
        );
        assert_eq!(
            before_dispatch.retry_safety,
            RetrySafety::Safe,
            "{endpoint:?}"
        );

        for phase in [
            DispatchPhase::DispatchStarted,
            DispatchPhase::ResponseStarted,
            DispatchPhase::Complete,
        ] {
            let unknown = classify_write_failure(
                endpoint,
                WriteFailure::Transport(TransportFailureKind::Timeout, phase, None),
            );
            assert_eq!(
                unknown.code,
                ErrorCode::MutationOutcomeUnknown,
                "{endpoint:?} {phase:?}"
            );
            assert_eq!(
                unknown.outcome,
                OperationOutcome::Unknown,
                "{endpoint:?} {phase:?}"
            );
            assert_eq!(
                unknown.retry_safety,
                RetrySafety::Unknown,
                "{endpoint:?} {phase:?}"
            );

            let applied = classify_write_failure(
                endpoint,
                WriteFailure::Transport(
                    TransportFailureKind::Protocol,
                    phase,
                    Some(success_status),
                ),
            );
            assert_eq!(
                applied.code,
                ErrorCode::MutationResponseInvalid,
                "{endpoint:?} {phase:?}"
            );
            assert_eq!(
                applied.outcome,
                OperationOutcome::Applied,
                "{endpoint:?} {phase:?}"
            );
            assert_eq!(
                applied.retry_safety,
                RetrySafety::Unsafe,
                "{endpoint:?} {phase:?}"
            );
        }
    }

    for (endpoint, status) in [
        (WriteEndpoint::CreateIssue, 201),
        (WriteEndpoint::CreateProject, 201),
        (WriteEndpoint::UpdateIssue, 204),
        (WriteEndpoint::AddComment, 201),
        (WriteEndpoint::TransitionIssue, 204),
        (WriteEndpoint::AssignIssue, 204),
        (WriteEndpoint::AddIssueLink, 201),
        (WriteEndpoint::AddWatcher, 204),
        (WriteEndpoint::RemoveWatcher, 204),
    ] {
        let classification = classify_write_failure(
            endpoint,
            WriteFailure::Transport(
                TransportFailureKind::ResponseTooLarge,
                DispatchPhase::ResponseStarted,
                Some(status),
            ),
        );
        assert_eq!(classification.code, ErrorCode::MutationResponseInvalid);
        assert_eq!(classification.outcome, OperationOutcome::Applied);
        assert_eq!(classification.retry_safety, RetrySafety::Unsafe);
        assert_eq!(classification.exit_class, classification.code.exit_class());
    }
}

#[test]
fn destructive_confirmations_are_target_bound_and_not_applied() {
    let error = validate_confirmation("OPS-2", "OPS-1", "confirm_issue").unwrap_err();
    assert_eq!(error.code, ErrorCode::DestructiveConfirmationRequired);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));

    let error = validate_confirmed_set(
        &["OPS-2".to_owned(), "OPS-9".to_owned()],
        &["OPS-1".to_owned(), "OPS-2".to_owned()],
        "confirm_issues",
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DestructiveConfirmationRequired);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
}

#[test]
fn delete_issue_accepts_only_empty_204_as_success() {
    assert_eq!(
        WriteEndpoint::DeleteIssue.method(),
        jira_ops::client::HttpMethod::Delete
    );
    assert!(WriteEndpoint::DeleteIssue.accepts_success(204));
    assert!(!WriteEndpoint::DeleteIssue.accepts_success(200));
}

#[test]
fn transport_failure_retains_known_response_status() {
    for (phase, status) in [
        (DispatchPhase::BeforeDispatch, None),
        (DispatchPhase::DispatchStarted, None),
        (DispatchPhase::ResponseStarted, Some(201)),
        (DispatchPhase::ResponseStarted, Some(204)),
        (DispatchPhase::ResponseStarted, Some(429)),
    ] {
        let failure = TransportFailure::new(TransportFailureKind::Protocol, phase, status);
        assert_eq!(failure.status, status);
    }
}

#[derive(Default)]
struct MemoryEnvironment(BTreeMap<String, OsString>);

impl EnvironmentSource for MemoryEnvironment {
    fn value(&self, key: &str) -> Option<OsString> {
        self.0.get(key).cloned()
    }
}

#[derive(Default)]
struct MemoryConfig(RefCell<Option<SavedIdentity>>);

impl ConfigStore for MemoryConfig {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
        Ok(self.0.borrow().clone())
    }

    fn atomic_replace(&self, value: &SavedIdentity) -> Result<(), StoreError> {
        self.0.replace(Some(value.clone()));
        Ok(())
    }

    fn remove(&self) -> Result<(), StoreError> {
        self.0.replace(None);
        Ok(())
    }
}

#[derive(Default)]
struct MemoryCredentials(RefCell<BTreeMap<String, String>>);

impl CredentialStore for MemoryCredentials {
    fn get(&self, key: &CredentialKey) -> Result<SecretString, StoreError> {
        self.0
            .borrow()
            .get(&key.account)
            .cloned()
            .map(SecretString::from)
            .ok_or(StoreError::NotFound)
    }

    fn set(&self, key: &CredentialKey, value: &SecretString) -> Result<(), StoreError> {
        self.0
            .borrow_mut()
            .insert(key.account.clone(), value.expose_secret().to_owned());
        Ok(())
    }

    fn delete(&self, key: &CredentialKey) -> Result<(), StoreError> {
        self.0.borrow_mut().remove(&key.account);
        Ok(())
    }
}

#[test]
fn scoped_request_uses_gateway_and_preemptive_basic_auth() {
    let transport = CaptureTransport::new(json_response(200, r#"{"accountId":"abc123"}"#));
    let client = client_with(&transport);

    client.get("/rest/api/3/myself").unwrap();

    let requests = transport.requests.borrow();
    let request = requests.first().expect("one request");
    assert_eq!(requests.len(), 1);
    assert_eq!(request.method, jira_ops::client::HttpMethod::Get);
    assert_eq!(
        request.url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/myself"
    );
    assert_eq!(
        request.headers["accept"].expose_secret(),
        "application/json"
    );
    assert!(
        request.headers["authorization"]
            .expose_secret()
            .starts_with("Basic ")
    );
    assert!(
        !request.headers["authorization"]
            .expose_secret()
            .contains("secret-token")
    );
}

#[test]
fn redirect_is_protocol_error_and_never_replays_credentials() {
    let transport = CaptureTransport::new(HttpResponse {
        status: 302,
        headers: BTreeMap::from([("location".to_owned(), "https://example.org/".to_owned())]),
        body: Vec::new(),
    });
    let error = client_with(&transport)
        .get("/rest/api/3/myself")
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::ResponseInvalid);
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn oversized_response_is_rejected_before_json_parsing() {
    let transport = CaptureTransport::new(HttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: vec![b'x'; jira_ops::client::MAX_RESPONSE_BYTES + 1],
    });
    let error = client_with(&transport)
        .get_json::<serde_json::Value>("/rest/api/3/myself")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResponseTooLarge);
}

#[test]
fn malformed_json_and_html_are_stable_response_errors() {
    for body in ["not-json", "<html>proxy error</html>"] {
        let transport = CaptureTransport::new(HttpResponse {
            status: 200,
            headers: BTreeMap::from([("content-type".to_owned(), "text/html".to_owned())]),
            body: body.as_bytes().to_vec(),
        });
        let error = client_with(&transport)
            .get_json::<serde_json::Value>("/rest/api/3/myself")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid);
    }
}

#[test]
fn unauthorized_response_maps_without_echoing_response_secrets() {
    let transport = CaptureTransport::new(json_response(
        401,
        r#"{"errorMessages":["token secret-token rejected"]}"#,
    ));
    let error = client_with(&transport)
        .get("/rest/api/3/myself")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AuthInvalid);
    assert_eq!(error.status, Some(401));
    assert!(
        !serde_json::to_string(&error)
            .unwrap()
            .contains("secret-token")
    );
}

#[test]
fn auth_and_permission_statuses_ignore_response_prose_and_headers() {
    for (status, headers, expected) in [
        (
            401,
            BTreeMap::from([
                (
                    "www-authenticate".to_owned(),
                    "Bearer error=\"insufficient_scope\"".to_owned(),
                ),
                ("x-aaccountid".to_owned(), "account-id".to_owned()),
            ]),
            ErrorCode::AuthInvalid,
        ),
        (
            403,
            BTreeMap::from([("x-unrelated".to_owned(), "scope grant missing".to_owned())]),
            ErrorCode::Forbidden,
        ),
    ] {
        let transport = CaptureTransport::new(HttpResponse {
            status,
            headers,
            body: br#"{"errorMessages":["scope does not match; missing scope grant"]}"#.to_vec(),
        });

        assert_eq!(
            client_with(&transport)
                .get("/rest/api/3/project/search")
                .unwrap_err()
                .code,
            expected
        );
    }
}

#[test]
fn read_transport_failures_remain_safe_and_use_stable_codes() {
    for (kind, code) in [
        (TransportFailureKind::Timeout, ErrorCode::Timeout),
        (
            TransportFailureKind::Connection,
            ErrorCode::ConnectionFailed,
        ),
        (TransportFailureKind::Protocol, ErrorCode::ResponseInvalid),
    ] {
        let client = JiraClient::new(
            FailingTransport(kind),
            test_credential(),
            Duration::from_secs(30),
        );
        let error = client.get("/rest/api/3/myself").unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.retry_safety, RetrySafety::Safe);
        assert!(error.operation_outcome.is_none());
    }
}

#[test]
fn rate_limit_headers_are_normalized_without_copying_response_body() {
    let transport = CaptureTransport::new(HttpResponse {
        status: 429,
        headers: BTreeMap::from([
            ("retry-after".to_owned(), "5".to_owned()),
            (
                "x-ratelimit-reason".to_owned(),
                "jira-burst-based".to_owned(),
            ),
        ]),
        body: br#"{"secret":"must-not-escape"}"#.to_vec(),
    });
    let error = client_with(&transport)
        .get("/rest/api/3/project/search")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RateLimited);
    assert_eq!(error.retry_after_ms, Some(5_000));
    assert_eq!(error.rate_limit_reason.as_deref(), Some("jira-burst-based"));
    assert!(
        !serde_json::to_string(&error)
            .unwrap()
            .contains("must-not-escape")
    );
}

#[test]
fn rate_limit_reason_prefers_current_header() {
    let cases = [
        (
            BTreeMap::from([
                ("retry-after".to_owned(), "5".to_owned()),
                ("ratelimit-reason".to_owned(), "jira-quota-based".to_owned()),
            ]),
            Some(5_000),
            Some("jira-quota-based"),
        ),
        (
            BTreeMap::from([(
                "x-ratelimit-reason".to_owned(),
                "jira-burst-based".to_owned(),
            )]),
            None,
            Some("jira-burst-based"),
        ),
        (
            BTreeMap::from([
                ("ratelimit-reason".to_owned(), "current".to_owned()),
                ("x-ratelimit-reason".to_owned(), "legacy".to_owned()),
            ]),
            None,
            Some("current"),
        ),
        (BTreeMap::new(), None, None),
        (
            BTreeMap::from([("retry-after".to_owned(), "not-seconds".to_owned())]),
            None,
            None,
        ),
        (
            BTreeMap::from([("retry-after".to_owned(), u64::MAX.to_string())]),
            None,
            None,
        ),
    ];

    for (headers, retry_after_ms, reason) in cases {
        let transport = CaptureTransport::new(HttpResponse {
            status: 429,
            headers,
            body: Vec::new(),
        });
        let error = client_with(&transport)
            .get("/rest/api/3/project/search")
            .unwrap_err();
        assert_eq!(error.retry_after_ms, retry_after_ms);
        assert_eq!(error.rate_limit_reason.as_deref(), reason);
    }
}

#[test]
fn tenant_info_uses_the_exact_site_origin_without_credentials() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"cloudId":"00000000-0000-0000-0000-000000000000"}"#,
    ));

    let cloud_id = tenant_info(
        &transport,
        &Url::parse("https://example.atlassian.net/").unwrap(),
        Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(cloud_id, Uuid::nil());
    let requests = transport.requests.borrow();
    let request = requests.first().expect("one tenant request");
    assert_eq!(
        request.url.as_str(),
        "https://example.atlassian.net/_edge/tenant_info"
    );
    assert!(!request.headers.contains_key("authorization"));
}

#[test]
fn myself_projects_only_the_stable_account_fields() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"accountId":"abc123","displayName":"Agent User","active":true,"emailAddress":"agent@example.com","avatarUrls":{"48x48":"ignored"}}"#,
    ));

    let account = myself(&client_with(&transport)).unwrap();

    assert_eq!(account.account_id, "abc123");
    assert_eq!(account.display_name, "Agent User");
    assert!(account.active);
    assert_eq!(account.email.as_deref(), Some("agent@example.com"));
}

#[test]
fn login_validates_tenant_then_identity_before_committing_secret() {
    let transport = ScriptedTransport::new([
        json_response(200, r#"{"cloudId":"00000000-0000-0000-0000-000000000000"}"#),
        json_response(
            200,
            r#"{"accountId":"abc123","displayName":"Agent User","active":true}"#,
        ),
    ]);
    let environment = MemoryEnvironment::default();
    let config = MemoryConfig::default();
    let credentials = MemoryCredentials::default();

    let result = auth_login(
        &environment,
        &config,
        &credentials,
        &transport,
        "https://example.atlassian.net",
        "agent@example.com",
        &mut b"scoped-token\n".as_slice(),
        Duration::from_secs(30),
    )
    .unwrap();

    assert_eq!(result.data.account_id, "abc123");
    assert_eq!(result.data.credential_source, "keyring");
    assert!(result.warnings.is_empty());
    let identity = config.0.borrow().clone().expect("saved identity");
    assert_eq!(identity.account_id, "abc123");
    assert_eq!(
        credentials
            .0
            .borrow()
            .get(&CredentialKey::for_identity(&identity).account)
            .map(String::as_str),
        Some("scoped-token")
    );
    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(requests[1].headers.contains_key("authorization"));
}

#[test]
fn login_rejects_any_environment_override_before_network_or_stdin() {
    let transport = ScriptedTransport::new([]);
    let environment = MemoryEnvironment(BTreeMap::from([(
        "JIRA_SITE".to_owned(),
        "https://other.atlassian.net".into(),
    )]));
    let error = auth_login(
        &environment,
        &MemoryConfig::default(),
        &MemoryCredentials::default(),
        &transport,
        "https://example.atlassian.net",
        "agent@example.com",
        &mut b"must-not-be-read\n".as_slice(),
        Duration::from_secs(30),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::ConfigConflict);
    assert!(transport.requests.borrow().is_empty());
}

#[test]
fn environment_cloud_binding_mismatch_stops_before_authenticated_request() {
    let transport = ScriptedTransport::new([json_response(
        200,
        r#"{"cloudId":"11111111-1111-1111-1111-111111111111"}"#,
    )]);
    let environment = MemoryEnvironment(BTreeMap::from([
        (
            "JIRA_SITE".to_owned(),
            "https://example.atlassian.net".into(),
        ),
        ("JIRA_CLOUD_ID".to_owned(), Uuid::nil().to_string().into()),
        ("JIRA_EMAIL".to_owned(), "agent@example.com".into()),
        ("JIRA_API_TOKEN".to_owned(), "scoped-token".into()),
    ]));

    let error = me_command(
        &environment,
        &MemoryConfig::default(),
        &MemoryCredentials::default(),
        &transport,
        Duration::from_secs(30),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::ConfigConflict);
    assert_eq!(transport.requests.borrow().len(), 1);
    assert!(
        !transport.requests.borrow()[0]
            .headers
            .contains_key("authorization")
    );
}

#[test]
fn cursor_is_opaque_and_bound_to_command_and_exact_query() {
    let fingerprint = QueryFingerprint::new("query=story&limit=20");
    let cursor = encode_cursor("field.list", &fingerprint, PageState::Offset(20)).unwrap();

    assert!(!cursor.contains("story"));
    assert_eq!(
        decode_cursor(&cursor, "field.list", &fingerprint).unwrap(),
        PageState::Offset(20)
    );
    assert_eq!(
        decode_cursor(
            &cursor,
            "field.list",
            &QueryFingerprint::new("query=points&limit=20")
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidCursor
    );
    assert_eq!(
        decode_cursor(&cursor, "project.list", &fingerprint)
            .unwrap_err()
            .code,
        ErrorCode::InvalidCursor
    );
}

#[test]
fn cursor_rejects_malformed_and_oversized_input() {
    let fingerprint = QueryFingerprint::new("limit=20");
    for cursor in ["not base64!".to_owned(), "x".repeat(4 * 1024 + 1)] {
        assert_eq!(
            decode_cursor(&cursor, "project.list", &fingerprint)
                .unwrap_err()
                .code,
            ErrorCode::InvalidCursor
        );
    }
}

#[test]
fn project_list_sends_exact_request_and_projects_compact_items() {
    let transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/project_page.json"),
    ));

    let output = project_list(&client_with(&transport), 1, None).unwrap();

    let request = &transport.requests.borrow()[0];
    assert_eq!(
        request.url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/project/search?maxResults=1"
    );
    assert_eq!(output.data.len(), 1);
    assert_eq!(output.data[0].key, "ACCL");
    assert_eq!(output.data[0].project_type, "software");
    assert_eq!(output.meta.as_ref().unwrap().count, 1);
    assert!(output.meta.as_ref().unwrap().next_cursor.is_some());
    let value = serde_json::to_value(&output.data[0]).unwrap();
    assert!(value.get("avatarUrls").is_none());
}

#[test]
fn project_get_encodes_one_path_segment_and_projects_only_stable_fields() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"id":"10001","key":"ACCL","name":"Agent CLI Company Lab","projectTypeKey":"software","style":"classic","simplified":false,"avatarUrls":{"48x48":"ignored"},"lead":{"accountId":"ignored"}}"#,
    ));

    let output = project_get(&client_with(&transport), "ACCL/needs encoding").unwrap();

    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(
        transport.requests.borrow()[0].url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/project/ACCL%2Fneeds%20encoding"
    );
    assert_eq!(
        serde_json::to_value(output).unwrap(),
        serde_json::json!({"data":{
            "id":"10001",
            "key":"ACCL",
            "name":"Agent CLI Company Lab",
            "type":"software",
            "style":"classic"
        }})
    );
}

#[test]
fn project_templates_are_versioned_local_data_with_zero_jira_requests() {
    let templates = project_templates(Some("software"));
    assert_eq!(templates.data.len(), 4);
    assert!(
        templates
            .data
            .iter()
            .all(|template| template.project_type_key == "software")
    );
    assert_eq!(project_templates(Some("business")).data, Vec::new());
    let value = serde_json::to_value(templates).unwrap();
    assert!(value["data"][0].get("registry_version").is_none());
}

fn valid_project_create_input() -> ProjectCreateInput {
    ProjectCreateInput {
        key: "OPSDEMO".to_owned(),
        name: "Ops Demo".to_owned(),
        project_type_key: "software".to_owned(),
        project_template_key: "com.pyxis.greenhopper.jira:gh-simplified-kanban-classic".to_owned(),
        lead_account_id: "abc123".to_owned(),
        description: None,
        assignee_type: None,
    }
}

#[test]
fn project_create_plan_compiles_the_exact_atlassian_wire_contract() {
    let plan = plan_project_create(valid_project_create_input()).unwrap();
    assert_eq!(plan.operation, "project.create");
    assert_eq!(plan.method, "POST");
    assert_eq!(plan.path, "/rest/api/3/project");
    assert_eq!(
        plan.body,
        serde_json::json!({
            "key":"OPSDEMO",
            "name":"Ops Demo",
            "projectTypeKey":"software",
            "projectTemplateKey":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic",
            "leadAccountId":"abc123",
            "assigneeType":"UNASSIGNED"
        })
    );
}

#[test]
fn project_create_apply_sends_one_exact_post_and_never_reads_back() {
    let transport = WriteScriptedTransport::new([
        Ok(json_response(
            201,
            r#"{"id":"10001","key":"OPSDEMO","self":"ignored"}"#,
        )),
        Ok(json_response(200, r#"{"must":"remain unused"}"#)),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let applied = apply_project_create(
        &client,
        plan_project_create(valid_project_create_input()).unwrap(),
    )
    .unwrap();

    assert_eq!(transport.requests.borrow().len(), 1);
    assert_eq!(transport.responses.borrow().len(), 1);
    assert_exact_write_request(
        &transport.requests.borrow()[0],
        jira_ops::client::HttpMethod::Post,
        "/rest/api/3/project",
        serde_json::json!({
            "key":"OPSDEMO",
            "name":"Ops Demo",
            "projectTypeKey":"software",
            "projectTemplateKey":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic",
            "leadAccountId":"abc123",
            "assigneeType":"UNASSIGNED"
        }),
    );
    assert_eq!(
        serde_json::to_value(applied).unwrap(),
        serde_json::json!({
            "operation":"project.create",
            "outcome":"applied",
            "project":{"id":"10001","key":"OPSDEMO"}
        })
    );
}

#[test]
fn project_create_accepts_numeric_id_from_jira_success_response() {
    let transport = WriteScriptedTransport::new([Ok(json_response(
        201,
        r#"{"id":10002,"key":"JOE0821","self":"ignored"}"#,
    ))]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let applied = apply_project_create(
        &client,
        plan_project_create(valid_project_create_input()).unwrap(),
    )
    .unwrap();

    assert_eq!(applied.project.id, "10002");
    assert_eq!(applied.project.key, "JOE0821");
}

#[test]
fn project_create_uses_endpoint_specific_one_attempt_outcome_classification() {
    let oversized = "x".repeat(jira_ops::client::MAX_RESPONSE_BYTES + 1);
    let cases = vec![
        (
            Ok(json_response(400, r#"{"errorMessages":["bad"]}"#)),
            ErrorCode::RemoteRejected,
            Some(OperationOutcome::NotApplied),
            RetrySafety::Safe,
            Some(400),
        ),
        (
            Ok(json_response(401, r#"{"secret":"must-not-escape"}"#)),
            ErrorCode::AuthInvalid,
            Some(OperationOutcome::NotApplied),
            RetrySafety::Safe,
            Some(401),
        ),
        (
            Ok(json_response(403, r#"{"secret":"must-not-escape"}"#)),
            ErrorCode::Forbidden,
            Some(OperationOutcome::NotApplied),
            RetrySafety::Safe,
            Some(403),
        ),
        (
            Ok(json_response(422, r#"{"secret":"must-not-escape"}"#)),
            ErrorCode::RemoteRejected,
            Some(OperationOutcome::NotApplied),
            RetrySafety::Safe,
            Some(422),
        ),
        (
            Ok(json_response(429, r#"{"secret":"must-not-escape"}"#)),
            ErrorCode::RateLimited,
            Some(OperationOutcome::NotApplied),
            RetrySafety::Safe,
            Some(429),
        ),
        (
            Ok(json_response(503, r#"{"errorMessages":["unavailable"]}"#)),
            ErrorCode::MutationOutcomeUnknown,
            Some(OperationOutcome::Unknown),
            RetrySafety::Unknown,
            Some(503),
        ),
        (
            Ok(json_response(500, r#"{"errorMessages":["unavailable"]}"#)),
            ErrorCode::MutationOutcomeUnknown,
            Some(OperationOutcome::Unknown),
            RetrySafety::Unknown,
            Some(500),
        ),
        (
            Ok(json_response(201, r#"{"id":"10001"}"#)),
            ErrorCode::MutationResponseInvalid,
            Some(OperationOutcome::Applied),
            RetrySafety::Unsafe,
            Some(201),
        ),
        (
            Ok(json_response(201, r#"{"id":"10001"#)),
            ErrorCode::MutationResponseInvalid,
            Some(OperationOutcome::Applied),
            RetrySafety::Unsafe,
            Some(201),
        ),
        (
            Ok(HttpResponse {
                status: 201,
                headers: BTreeMap::from([("content-type".to_owned(), "text/html".to_owned())]),
                body: b"<html>not json</html>".to_vec(),
            }),
            ErrorCode::MutationResponseInvalid,
            Some(OperationOutcome::Applied),
            RetrySafety::Unsafe,
            Some(201),
        ),
        (
            Ok(json_response(201, &oversized)),
            ErrorCode::MutationResponseInvalid,
            Some(OperationOutcome::Applied),
            RetrySafety::Unsafe,
            Some(201),
        ),
        (
            Err(TransportFailure::response_started_with_headers(
                TransportFailureKind::ResponseTooLarge,
                201,
                &BTreeMap::new(),
            )),
            ErrorCode::MutationResponseInvalid,
            Some(OperationOutcome::Applied),
            RetrySafety::Unsafe,
            Some(201),
        ),
        (
            Err(TransportFailure::new(
                TransportFailureKind::Connection,
                DispatchPhase::DispatchStarted,
                None,
            )),
            ErrorCode::MutationOutcomeUnknown,
            Some(OperationOutcome::Unknown),
            RetrySafety::Unknown,
            None,
        ),
    ];

    for (first, code, outcome, retry, status) in cases {
        let transport = WriteScriptedTransport::new([
            first,
            Ok(json_response(201, r#"{"id":"second","key":"SECOND"}"#)),
        ]);
        let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));
        let error = apply_project_create(
            &client,
            plan_project_create(valid_project_create_input()).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.operation_outcome, outcome);
        assert_eq!(error.retry_safety, retry);
        assert_eq!(error.status, status);
        assert_eq!(transport.requests.borrow().len(), 1);
        assert_eq!(transport.responses.borrow().len(), 1);
    }
}

#[test]
fn project_cursor_continues_at_next_offset_and_rejects_changed_limit() {
    let first_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/project_page.json"),
    ));
    let first = project_list(&client_with(&first_transport), 1, None).unwrap();
    let cursor = first.meta.unwrap().next_cursor.unwrap();

    let changed_transport = CaptureTransport::new(json_response(200, "{}"));
    let error = project_list(&client_with(&changed_transport), 2, Some(&cursor)).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidCursor);
    assert_eq!(changed_transport.call_count(), 0);

    let next_transport = CaptureTransport::new(json_response(
        200,
        r#"{"maxResults":1,"startAt":1,"total":2,"isLast":true,"values":[{"id":"10001","key":"KAN","name":"Agent CLI Team Lab","projectTypeKey":"software","simplified":true}]}"#,
    ));
    let next = project_list(&client_with(&next_transport), 1, Some(&cursor)).unwrap();
    assert_eq!(next.data[0].key, "KAN");
    assert!(next.meta.unwrap().next_cursor.is_none());
    assert_eq!(
        next_transport.requests.borrow()[0].url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/project/search?maxResults=1&startAt=1"
    );
}

#[test]
fn field_list_encodes_query_and_projects_only_normative_schema() {
    let transport =
        CaptureTransport::new(json_response(200, include_str!("fixtures/field_page.json")));

    let output = field_list(&client_with(&transport), Some("story points"), 20, None).unwrap();

    let request = &transport.requests.borrow()[0];
    assert_eq!(
        request.url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/field/search?query=story+points&maxResults=20"
    );
    assert_eq!(output.data.len(), 1);
    assert_eq!(output.data[0].id, "customfield_10042");
    assert!(output.data[0].custom);
    assert_eq!(
        output.data[0]
            .schema
            .as_ref()
            .unwrap()
            .value_type
            .as_deref(),
        Some("number")
    );
    assert!(output.meta.as_ref().unwrap().next_cursor.is_none());
    let value = serde_json::to_value(&output.data[0]).unwrap();
    assert!(value.get("orderable").is_none());
    assert!(value["schema"].get("customId").is_none());
}

#[test]
fn issue_get_encodes_one_path_segment_and_projects_default_fields() {
    let transport = CaptureTransport::new(json_response(200, include_str!("fixtures/issue.json")));

    let output = issue_get(&client_with(&transport), "ACCL/1 needs encoding", None).unwrap();

    let request = &transport.requests.borrow()[0];
    assert_eq!(request.method, jira_ops::client::HttpMethod::Get);
    assert_eq!(
        request.url.as_str(),
        "https://api.atlassian.com/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL%2F1%20needs%20encoding?fields=summary%2Cstatus%2Cassignee%2Cupdated"
    );
    assert_eq!(output.data.key, "ACCL-1");
    assert_eq!(
        serde_json::to_value(&output).unwrap(),
        serde_json::json!({
            "data":{
                "key":"ACCL-1",
                "summary":"Create endpoint contract test",
                "status":"To Do",
                "assignee":{"account_id":"abc123","display_name":"Agent User"},
                "updated":"2026-08-18T22:00:00.000+0000"
            }
        })
    );
}

#[test]
fn issue_get_replaces_defaults_and_keeps_custom_fields_compact() {
    let transport = CaptureTransport::new(json_response(200, include_str!("fixtures/issue.json")));
    let fields = vec!["description".to_owned(), "customfield_10042".to_owned()];

    let output = issue_get(&client_with(&transport), "ACCL-1", Some(&fields)).unwrap();

    let request = &transport.requests.borrow()[0];
    assert!(
        request
            .url
            .as_str()
            .ends_with("/rest/api/3/issue/ACCL-1?fields=description%2Ccustomfield_10042")
    );
    assert_eq!(
        serde_json::to_value(&output).unwrap(),
        serde_json::json!({
            "data":{
                "key":"ACCL-1",
                "description":"First line\n- nested",
                "fields":{"customfield_10042":8}
            }
        })
    );
}

#[test]
fn issue_search_uses_enhanced_jql_and_query_bound_token_cursor() {
    let transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/search_page.json"),
    ));

    let output = issue_search(
        &client_with(&transport),
        "project = ACCL ORDER BY key",
        None,
        1,
        None,
    )
    .unwrap();

    let request = &transport.requests.borrow()[0];
    assert_eq!(request.method, jira_ops::client::HttpMethod::Post);
    assert_eq!(request.effect, jira_ops::client::RequestEffect::Read);
    assert_eq!(
        request.headers["content-type"].expose_secret(),
        "application/json"
    );
    assert_eq!(
        request.url.path(),
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/search/jql"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        serde_json::json!({
            "jql":"project = ACCL ORDER BY key",
            "fields":["summary","status","assignee","updated"],
            "maxResults":1
        })
    );
    assert_eq!(output.data.len(), 1);
    assert_eq!(output.meta.as_ref().unwrap().count, 1);
    assert!(output.meta.as_ref().unwrap().next_cursor.is_some());
    assert_eq!(output.warnings[0].code, "jira_warning");

    let cursor = output.meta.unwrap().next_cursor.unwrap();
    let next_transport =
        CaptureTransport::new(json_response(200, r#"{"isLast":true,"issues":[]}"#));
    let next = issue_search(
        &client_with(&next_transport),
        "project = ACCL ORDER BY key",
        None,
        1,
        Some(&cursor),
    )
    .unwrap();
    assert!(next.data.is_empty());
    assert!(next.meta.unwrap().next_cursor.is_none());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&next_transport.requests.borrow()[0].body)
            .unwrap(),
        serde_json::json!({
            "jql":"project = ACCL ORDER BY key",
            "fields":["summary","status","assignee","updated"],
            "maxResults":1,
            "nextPageToken":"jira-token-2"
        })
    );

    for (changed_jql, changed_fields, changed_limit) in [
        ("project = KAN ORDER BY key", None, 1),
        (
            "project = ACCL ORDER BY key",
            Some(vec!["summary".to_owned()]),
            1,
        ),
        ("project = ACCL ORDER BY key", None, 2),
    ] {
        let changed_transport = CaptureTransport::new(json_response(200, "{}"));
        let error = issue_search(
            &client_with(&changed_transport),
            changed_jql,
            changed_fields.as_deref(),
            changed_limit,
            Some(&cursor),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidCursor);
        assert_eq!(changed_transport.call_count(), 0);
    }
}

#[test]
fn requested_missing_custom_field_is_explicitly_null() {
    let transport = CaptureTransport::new(json_response(200, include_str!("fixtures/issue.json")));
    let fields = vec!["customfield_99999".to_owned()];

    let output = issue_get(&client_with(&transport), "ACCL-1", Some(&fields)).unwrap();

    assert_eq!(
        serde_json::to_value(&output).unwrap(),
        serde_json::json!({
            "data":{
                "key":"ACCL-1",
                "fields":{"customfield_99999":null}
            }
        })
    );
}

#[test]
fn issue_search_rejects_inconsistent_pagination() {
    for body in [
        r#"{"isLast":false,"issues":[]}"#,
        r#"{"isLast":true,"nextPageToken":"unexpected","issues":[]}"#,
        r#"{"isLast":false,"nextPageToken":"","issues":[]}"#,
    ] {
        let transport = CaptureTransport::new(json_response(200, body));
        let error =
            issue_search(&client_with(&transport), "project = ACCL", None, 20, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid);
    }
}

#[test]
fn issue_projection_rejects_malformed_core_fields() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"id":"10042","key":"ACCL-1","fields":{"summary":7,"status":null,"assignee":null,"updated":null}}"#,
    ));

    let error = issue_get(&client_with(&transport), "ACCL-1", None).unwrap_err();
    assert_eq!(error.code, ErrorCode::ResponseInvalid);
}

#[test]
fn create_meta_has_exact_issue_type_and_field_modes() {
    let types_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/create_meta_issue_types.json"),
    ));
    let types = issue_create_meta(&client_with(&types_transport), "ACCL", None, 1, None).unwrap();
    assert_get_request(
        &types_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes",
        &[("maxResults", "1")],
    );
    let types_cursor = types.meta.as_ref().unwrap().next_cursor.as_deref();

    let fields_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/create_meta_fields.json"),
    ));
    let fields = issue_create_meta(
        &client_with(&fields_transport),
        "ACCL",
        Some("10001"),
        20,
        None,
    )
    .unwrap();
    assert_get_request(
        &fields_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes/10001",
        &[("maxResults", "20")],
    );
    assert_eq!(
        serde_json::to_value(&fields).unwrap(),
        serde_json::json!({
            "data":[
                {"id":"description","name":"Description","required":true,"operations":["set"],"schema":{"type":"string","items":null,"custom":"com.atlassian.jira.plugin.system.customfieldtypes:textarea","system":null},"input_kind":"adf_text","supported_selector_members":[],"allowed_values":[{"id":"1","name":"Public","account_id":"abc","display_name":"Agent"}],"allowed_values_complete":true},
                {"id":"customfield_10999","name":"Dynamic","required":false,"operations":["set"],"schema":{"type":"mystery","items":null,"custom":null,"system":null},"input_kind":"passthrough","supported_selector_members":[],"allowed_values_complete":false}
            ],
            "meta":{"kind":"fields","project":"ACCL","issue_type_id":"10001","count":2,"next_cursor":null}
        })
    );
    assert_eq!(
        serde_json::to_value(&types).unwrap(),
        serde_json::json!({
            "data":[{"id":"10001","name":"Task","subtask":false}],
            "meta":{"kind":"issue_types","project":"ACCL","count":1,"next_cursor":types_cursor}
        })
    );
}

#[test]
fn comments_use_the_documented_page_shape_and_project_adf() {
    let comments_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/comments_page.json"),
    ));
    let comments = issue_comments(&client_with(&comments_transport), "ACCL-1", 20, None).unwrap();
    assert_get_request(
        &comments_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/comment",
        &[("maxResults", "20")],
    );
    assert_eq!(
        serde_json::to_value(&comments).unwrap(),
        serde_json::json!({
            "data":[{"id":"10011","author":{"account_id":"abc123","display_name":"Agent User"},"body":"Plain-text comment","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}],
            "meta":{"count":1,"next_cursor":null}
        })
    );
}

#[test]
fn comments_accept_legacy_and_adf_bodies_in_one_page() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"comments":[
            {"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":"Legacy body","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"},
            {"id":"10012","author":{"accountId":"abc124","displayName":"Another User"},"body":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"ADF body"}]}]},"created":"2026-08-18T22:01:00.000+0000","updated":"2026-08-18T22:01:00.000+0000"}
        ]}"#,
    ));

    let comments = issue_comments(&client_with(&transport), "ACCL-1", 20, None).unwrap();

    assert_eq!(
        comments
            .data
            .iter()
            .map(|comment| comment.body.as_str())
            .collect::<Vec<_>>(),
        ["Legacy body", "ADF body"]
    );
}

#[test]
fn legacy_comment_body_respects_the_adf_plain_text_limit() {
    let body = serde_json::json!({
        "startAt": 0,
        "total": 1,
        "comments": [{
            "id": "10011",
            "author": {"accountId": "abc123", "displayName": "Agent User"},
            "body": "x".repeat(1024 * 1024 + 1),
            "created": "2026-08-18T22:00:00.000+0000",
            "updated": "2026-08-18T22:00:00.000+0000"
        }]
    })
    .to_string();
    let transport = CaptureTransport::new(json_response(200, &body));

    assert_eq!(
        issue_comments(&client_with(&transport), "ACCL-1", 1, None)
            .expect_err("oversized legacy body must fail")
            .code,
        ErrorCode::ResponseTooLarge
    );
}

#[test]
fn comments_reject_pages_larger_than_requested_limit() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"comments":[{"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":"first","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"},{"id":"10012","author":{"accountId":"abc123","displayName":"Agent User"},"body":"second","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
    ));

    assert_eq!(
        issue_comments(&client_with(&transport), "ACCL-1", 1, None)
            .expect_err("oversized page must fail")
            .code,
        ErrorCode::ResponseInvalid
    );
}

#[test]
fn create_meta_issue_types_reject_pages_larger_than_requested_limit() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"issueTypes":[{"id":"10001","name":"Task","subtask":false},{"id":"10002","name":"Bug","subtask":false}]}"#,
    ));

    assert_eq!(
        issue_create_meta(&client_with(&transport), "ACCL", None, 1, None)
            .expect_err("oversized page must fail")
            .code,
        ErrorCode::ResponseInvalid
    );
}

#[test]
fn create_meta_fields_reject_pages_larger_than_requested_limit() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"}},{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"}}]}"#,
    ));

    assert_eq!(
        issue_create_meta(&client_with(&transport), "ACCL", Some("10001"), 1, None)
            .expect_err("oversized page must fail")
            .code,
        ErrorCode::ResponseInvalid
    );
}

#[test]
fn empty_contradictory_comment_page_is_terminal() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":7,"total":99,"comments":[]}"#,
    ));

    let comments = issue_comments(&client_with(&transport), "ACCL-1", 20, None).unwrap();

    assert!(comments.data.is_empty());
    assert_eq!(comments.meta.unwrap().next_cursor, None);
}

#[test]
fn comment_cursor_uses_returned_start_offset_for_short_pages() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":5,"total":7,"comments":[{"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":{"type":"doc","version":1,"content":[]},"created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
    ));

    let comments = issue_comments(&client_with(&transport), "ACCL-1", 20, None).unwrap();
    let cursor = comments
        .meta
        .unwrap()
        .next_cursor
        .expect("short page cursor");
    let fingerprint = QueryFingerprint::new("issue=\"ACCL-1\"&limit=20");

    assert_eq!(
        decode_cursor(&cursor, "issue.comments", &fingerprint).unwrap(),
        PageState::Offset(6)
    );
}

#[test]
fn comment_cursor_round_trips_to_the_exact_continuation_request() {
    let transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"startAt":3,"total":5,"comments":[{"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":"first","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
        ),
        json_response(
            200,
            r#"{"startAt":4,"total":5,"comments":[{"id":"10012","author":{"accountId":"abc123","displayName":"Agent User"},"body":"second","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
        ),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let first = issue_comments(&client, "ACCL-1", 1, None).unwrap();
    let cursor = first
        .meta
        .unwrap()
        .next_cursor
        .expect("continuation cursor");
    let second = issue_comments(&client, "ACCL-1", 1, Some(&cursor)).unwrap();

    let requests = transport.requests.borrow();
    assert_get_request(
        &requests[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/comment",
        &[("maxResults", "1")],
    );
    assert_get_request(
        &requests[1],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/comment",
        &[("maxResults", "1"), ("startAt", "4")],
    );
    assert_eq!(second.meta.unwrap().next_cursor, None);
}

#[test]
fn create_meta_issue_type_cursor_round_trips_to_the_exact_continuation_request() {
    let transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"startAt":0,"total":2,"issueTypes":[{"id":"10001","name":"Task","subtask":false}]}"#,
        ),
        json_response(
            200,
            r#"{"startAt":1,"total":2,"issueTypes":[{"id":"10002","name":"Bug","subtask":false}]}"#,
        ),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let first = issue_create_meta(&client, "ACCL", None, 1, None).unwrap();
    let cursor = first
        .meta
        .unwrap()
        .next_cursor
        .expect("continuation cursor");
    let second = issue_create_meta(&client, "ACCL", None, 1, Some(&cursor)).unwrap();

    let requests = transport.requests.borrow();
    let path =
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes";
    assert_get_request(&requests[0], path, &[("maxResults", "1")]);
    assert_get_request(&requests[1], path, &[("maxResults", "1"), ("startAt", "1")]);
    assert_eq!(second.meta.unwrap().next_cursor, None);
}

#[test]
fn create_meta_field_cursor_round_trips_to_the_exact_continuation_request() {
    let transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"}}]}"#,
        ),
        json_response(
            200,
            r#"{"startAt":1,"total":2,"fields":[{"fieldId":"environment","name":"Environment","required":false,"operations":["set"],"schema":{"type":"string"}}]}"#,
        ),
    ]);
    let client = JiraClient::new(&transport, test_credential(), Duration::from_secs(30));

    let first = issue_create_meta(&client, "ACCL", Some("10001"), 1, None).unwrap();
    let cursor = first
        .meta
        .unwrap()
        .next_cursor
        .expect("continuation cursor");
    let second = issue_create_meta(&client, "ACCL", Some("10001"), 1, Some(&cursor)).unwrap();

    let requests = transport.requests.borrow();
    let path = "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes/10001";
    assert_get_request(&requests[0], path, &[("maxResults", "1")]);
    assert_get_request(&requests[1], path, &[("maxResults", "1"), ("startAt", "1")]);
    assert_eq!(second.meta.unwrap().next_cursor, None);
}

#[test]
fn comment_cursor_rejects_changed_issue_or_limit_before_network() {
    let initial = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"comments":[{"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":"first","created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
    ));
    let cursor = issue_comments(&client_with(&initial), "ACCL-1", 1, None)
        .unwrap()
        .meta
        .unwrap()
        .next_cursor
        .unwrap();

    for (issue, limit) in [("ACCL-2", 1), ("ACCL-1", 2)] {
        let transport = CaptureTransport::new(json_response(200, "{}"));
        assert_eq!(
            issue_comments(&client_with(&transport), issue, limit, Some(&cursor))
                .unwrap_err()
                .code,
            ErrorCode::InvalidCursor
        );
        assert_eq!(transport.call_count(), 0);
    }
}

#[test]
fn create_meta_cursor_rejects_changed_project_issue_type_or_limit_before_network() {
    let initial = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"}}]}"#,
    ));
    let cursor = issue_create_meta(&client_with(&initial), "ACCL", Some("10001"), 1, None)
        .unwrap()
        .meta
        .unwrap()
        .next_cursor
        .unwrap();

    for (project, issue_type, limit) in [
        ("KAN", Some("10001"), 1),
        ("ACCL", Some("10002"), 1),
        ("ACCL", Some("10001"), 2),
    ] {
        let transport = CaptureTransport::new(json_response(200, "{}"));
        assert_eq!(
            issue_create_meta(
                &client_with(&transport),
                project,
                issue_type,
                limit,
                Some(&cursor),
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidCursor
        );
        assert_eq!(transport.call_count(), 0);
    }
}

#[test]
fn create_meta_rejects_contradictory_empty_pages() {
    for (issue_type, body) in [
        (None, r#"{"startAt":0,"total":1,"issueTypes":[]}"#),
        (Some("10001"), r#"{"startAt":0,"total":1,"fields":[]}"#),
    ] {
        let transport = CaptureTransport::new(json_response(200, body));
        assert_eq!(
            issue_create_meta(&client_with(&transport), "ACCL", issue_type, 1, None)
                .unwrap_err()
                .code,
            ErrorCode::ResponseInvalid
        );
    }
}

#[test]
fn comment_handler_rejects_malformed_bodies_and_preserves_unknown_node_whitespace() {
    for body in ["null", "7", "[]", r#"{"type":"paragraph","content":[]}"#] {
        let page = format!(
            r#"{{"startAt":0,"total":1,"comments":[{{"id":"10011","author":{{"accountId":"abc123","displayName":"Agent User"}},"body":{body},"created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}}]}}"#
        );
        let transport = CaptureTransport::new(json_response(200, &page));
        assert_eq!(
            issue_comments(&client_with(&transport), "ACCL-1", 1, None)
                .unwrap_err()
                .code,
            ErrorCode::ResponseInvalid
        );
    }

    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":1,"comments":[{"id":"10011","author":{"accountId":"abc123","displayName":"Agent User"},"body":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":" leading "}]},{"type":"unknownNode"}]},"created":"2026-08-18T22:00:00.000+0000","updated":"2026-08-18T22:00:00.000+0000"}]}"#,
    ));
    assert_eq!(
        issue_comments(&client_with(&transport), "ACCL-1", 1, None)
            .unwrap()
            .data[0]
            .body,
        " leading \n[unsupported:unknownNode]"
    );
}

#[test]
fn transitions_request_expanded_screen_fields() {
    let transitions_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/transitions.json"),
    ));
    let transitions = issue_transitions(&client_with(&transitions_transport), "ACCL-1").unwrap();
    assert_get_request(
        &transitions_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/transitions",
        &[("expand", "transitions.fields")],
    );
    assert_eq!(
        serde_json::to_value(&transitions).unwrap(),
        serde_json::json!({
            "data":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":[{"id":"resolution","name":"Resolution","required":true,"operations":["set"],"schema":{"type":"string","items":null,"custom":null,"system":null},"input_kind":"string","supported_selector_members":[],"allowed_values":[{"id":"1","name":"Done"}],"allowed_values_complete":true}]}],
            "meta":{"count":1}
        })
    );
}

#[test]
fn empty_transition_discovery_is_a_successful_empty_envelope() {
    let transport = CaptureTransport::new(json_response(200, r#"{"transitions":[]}"#));

    let transitions = issue_transitions(&client_with(&transport), "ACCL-1").unwrap();

    assert_eq!(
        serde_json::to_value(&transitions).unwrap(),
        serde_json::json!({"data":[],"meta":{"count":0}})
    );
}

#[test]
fn transitions_normalize_empty_or_absent_field_maps() {
    for body in [
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{}}]}"#,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"}}]}"#,
    ] {
        let transport = CaptureTransport::new(json_response(200, body));
        let transitions = issue_transitions(&client_with(&transport), "ACCL-1").unwrap();

        assert_eq!(transitions.data[0].fields.len(), 0);
    }
}

#[test]
fn transition_fields_prefer_response_identifiers_and_mark_absent_allowed_values_partial() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{"fallback":{"fieldId":"response-id","key":"response-key","name":"Preferred","required":false,"operations":[],"schema":{"type":"string"}},"only-fallback":{"name":"Fallback","required":false,"operations":[],"schema":null}}}]}"#,
    ));

    let transitions = issue_transitions(&client_with(&transport), "ACCL-1").unwrap();
    let fields = &transitions.data[0].fields;

    let fallback = fields
        .iter()
        .find(|field| field.name == "Fallback")
        .expect("fallback field");
    assert_eq!(fallback.id, "only-fallback");
    assert_eq!(fallback.allowed_values, None);
    assert!(!fallback.allowed_values_complete);
    assert_eq!(
        fields
            .iter()
            .find(|field| field.name == "Preferred")
            .expect("response identifier field")
            .id,
        "response-id"
    );
}

#[test]
fn field_metadata_uses_the_first_nonblank_identifier_or_rejects_the_response() {
    for (field_key, field, expected_id) in [
        (
            "map-fallback",
            r#"{"fieldId":"","key":"resolution","name":"Key","required":false,"operations":[],"schema":{"type":"string"}}"#,
            Some("resolution"),
        ),
        (
            "map-fallback",
            r#"{"fieldId":"","key":"","name":"Map key","required":false,"operations":[],"schema":{"type":"string"}}"#,
            Some("map-fallback"),
        ),
        (
            "  ",
            r#"{"fieldId":" ","key":"\t","name":"Missing","required":false,"operations":[],"schema":{"type":"string"}}"#,
            None,
        ),
    ] {
        let body = format!(
            r#"{{"transitions":[{{"id":"31","name":"Done","to":{{"id":"10002","name":"Done"}},"fields":{{"{field_key}":{field}}}}}]}}"#
        );
        let transport = CaptureTransport::new(json_response(200, &body));
        let result = issue_transitions(&client_with(&transport), "ACCL-1");

        match expected_id {
            Some(expected_id) => assert_eq!(result.unwrap().data[0].fields[0].id, expected_id),
            None => assert_eq!(result.unwrap_err().code, ErrorCode::ResponseInvalid),
        }
    }
}

#[test]
fn field_metadata_normalization_matrix_is_compact_complete_and_type_safe() {
    let transport = CaptureTransport::new(json_response(
        200,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{"map-id":{"name":"Map","required":false,"operations":[],"schema":{"type":"string"}},"key-id":{"key":"response-key","name":"Key","required":false,"operations":[],"schema":{"type":"string"}},"field-id":{"fieldId":"response-field","key":"response-key","name":"Field","required":false,"operations":[],"schema":{"type":"string"}},"environment":{"name":"Environment","required":false,"operations":["set"],"schema":{"type":"string"}},"textarea":{"fieldId":"customfield_10001","name":"Textarea","required":false,"operations":["set"],"schema":{"type":"string","custom":"com.atlassian.jira.plugin.system.customfieldtypes:textarea"}},"summary":{"name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"}},"mystery":{"name":"Mystery","required":false,"operations":[],"schema":{"type":"mystery"}},"allowed":{"name":"Allowed","required":false,"operations":["set"],"schema":{"type":"array","items":"option"},"allowedValues":[{"id":"1","name":"One","avatarUrls":{"48x48":"ignored"}},{"value":"two","accountId":"abc","displayName":"Agent","extra":"ignored"}]},"empty-allowed":{"name":"Empty allowed","required":false,"operations":[],"schema":null,"allowedValues":[]},"absent-allowed":{"name":"Absent allowed","required":false,"operations":[],"schema":null}}}]}"#,
    ));
    let transitions = issue_transitions(&client_with(&transport), "ACCL-1").unwrap();
    let fields: BTreeMap<&str, _> = transitions.data[0]
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect();

    for (name, id, input_kind) in [
        ("Map", "map-id", "string"),
        ("Key", "response-key", "string"),
        ("Field", "response-field", "string"),
        ("Environment", "environment", "adf_text"),
        ("Textarea", "customfield_10001", "adf_text"),
        ("Summary", "summary", "string"),
        ("Mystery", "mystery", "passthrough"),
    ] {
        let field = fields[name];
        assert_eq!(field.id, id, "{name}");
        assert_eq!(
            serde_json::to_value(field).unwrap()["input_kind"],
            input_kind
        );
    }

    let allowed = fields["Allowed"];
    assert!(allowed.allowed_values_complete);
    assert_eq!(
        serde_json::to_value(&allowed.allowed_values).unwrap(),
        serde_json::json!([
            {"id":"1","name":"One"},
            {"value":"two","account_id":"abc","display_name":"Agent"}
        ])
    );
    assert_eq!(fields["Empty allowed"].allowed_values, Some(Vec::new()));
    assert!(fields["Empty allowed"].allowed_values_complete);
    assert_eq!(fields["Absent allowed"].allowed_values, None);
    assert!(!fields["Absent allowed"].allowed_values_complete);
}

#[test]
fn issue_subresource_reads_reject_empty_issue_before_network() {
    let comments_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/comments_page.json"),
    ));
    assert_eq!(
        issue_comments(&client_with(&comments_transport), "", 20, None)
            .expect_err("empty issue must fail")
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(comments_transport.call_count(), 0);

    let transitions_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/transitions.json"),
    ));
    assert_eq!(
        issue_transitions(&client_with(&transitions_transport), "")
            .expect_err("empty issue must fail")
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(transitions_transport.call_count(), 0);
}

#[test]
fn mutation_plan_keeps_plain_intent_and_one_wire_ready_payload() {
    let create_transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"startAt":0,"total":3,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}]}"#,
        ),
        json_response(
            200,
            r#"{"startAt":1,"total":3,"fields":[{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},{"fieldId":"assignee","name":"Assignee","required":false,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"accountId":"abc","displayName":"Agent"}]}]}"#,
        ),
    ]);
    let create = plan_create_issue(
        &client_with_scripted(&create_transport),
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([
                ("summary".to_owned(), json!("Create contract test")),
                ("description".to_owned(), json!("line one\nline two")),
                (
                    "assignee".to_owned(),
                    json!({"account_id":"abc","display_name":"Agent"}),
                ),
            ]),
        },
    )
    .unwrap();

    assert_eq!(create_transport.requests.borrow().len(), 2);
    assert_get_request(
        &create_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes/10001",
        &[("maxResults", "100")],
    );
    assert_get_request(
        &create_transport.requests.borrow()[1],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/createmeta/ACCL/issuetypes/10001",
        &[("maxResults", "100"), ("startAt", "1")],
    );
    assert_eq!(
        serde_json::to_value(&create).unwrap(),
        json!({
            "operation":"issue.create",
            "applied":false,
            "target":{"project_key":"ACCL","issue_type_id":"10001"},
            "changes":{"fields":{"assignee":{"account_id":"abc","display_name":"Agent"},"description":"line one\nline two","summary":"Create contract test"}},
            "validation":{"local":"passed","metadata":"passed"}
        })
    );
    assert_eq!(
        create.wire_payload(),
        &json!({"fields":{
            "project":{"key":"ACCL"},
            "issuetype":{"id":"10001"},
            "description":{"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[{"type":"text","text":"line one"}]},
                {"type":"paragraph","content":[{"type":"text","text":"line two"}]}
            ]},
            "assignee":{"accountId":"abc"},
            "summary":"Create contract test"
        }})
    );

    let update_transport = CaptureTransport::new(json_response(
        200,
        r#"{"fields":{"description":{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},"assignee":{"fieldId":"assignee","name":"Assignee","required":false,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"accountId":"abc","displayName":"Agent"}]}}}"#,
    ));
    let update = plan_update_issue(
        &client_with(&update_transport),
        "ACCL-1",
        UpdateIssueInput {
            set: BTreeMap::from([
                ("description".to_owned(), json!("updated")),
                (
                    "assignee".to_owned(),
                    json!({"account_id":"abc","display_name":"Agent"}),
                ),
            ]),
            notify_users: None,
        },
    )
    .unwrap();
    assert_get_request(
        &update_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/editmeta",
        &[],
    );
    assert_eq!(update_transport.call_count(), 1);
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({
            "operation":"issue.update",
            "applied":false,
            "target":{"issue":"ACCL-1"},
            "changes":{"set":{"assignee":{"account_id":"abc","display_name":"Agent"},"description":"updated"}},
            "validation":{"local":"passed","metadata":"passed"}
        })
    );
    assert_eq!(
        update.wire_payload(),
        &json!({"fields":{
            "assignee":{"accountId":"abc"},
            "description":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"updated"}]}]}
        }})
    );

    let comment = plan_comment(
        "ACCL-1",
        CommentInput {
            body: "plain\rtext\n".into(),
            internal: false,
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&comment).unwrap(),
        json!({
            "operation":"issue.comment",
            "applied":false,
            "target":{"issue":"ACCL-1"},
            "changes":{"body":"plain\rtext\n"},
            "validation":{"local":"passed","metadata":"not_applicable"}
        })
    );
    assert_eq!(
        comment.wire_payload(),
        &json!({"body":{"type":"doc","version":1,"content":[
            {"type":"paragraph","content":[{"type":"text","text":"plain"}]},
            {"type":"paragraph","content":[{"type":"text","text":"text"}]},
            {"type":"paragraph","content":[]}
        ]}})
    );

    let transition_transport = CaptureTransport::new(json_response(
        200,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{"description":{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},"assignee":{"fieldId":"assignee","name":"Assignee","required":false,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"accountId":"abc","displayName":"Agent"}]}}}]}"#,
    ));
    let transition = plan_transition_issue(
        &client_with(&transition_transport),
        "ACCL-1",
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::from([
                ("description".to_owned(), json!("transitioned")),
                (
                    "assignee".to_owned(),
                    json!({"account_id":"abc","display_name":"Agent"}),
                ),
            ]),
            comment: None,
            notify_users: None,
        },
    )
    .unwrap();
    assert_eq!(transition_transport.call_count(), 1);
    assert_get_request(
        &transition_transport.requests.borrow()[0],
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/transitions",
        &[("expand", "transitions.fields")],
    );
    assert_eq!(
        serde_json::to_value(&transition).unwrap(),
        json!({
            "operation":"issue.transition",
            "applied":false,
            "target":{"issue":"ACCL-1"},
            "changes":{"transition_id":"31","fields":{"assignee":{"account_id":"abc","display_name":"Agent"},"description":"transitioned"}},
            "validation":{"local":"passed","metadata":"passed"}
        })
    );
    assert_eq!(
        transition.wire_payload(),
        &json!({
            "transition":{"id":"31"},
            "fields":{
                "assignee":{"accountId":"abc"},
                "description":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"transitioned"}]}]}
            }
        })
    );
}

#[test]
fn create_planning_validates_required_fields_from_later_metadata_pages() {
    let transport = ScriptedTransport::new([
        json_response(
            200,
            r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}]}"#,
        ),
        json_response(
            200,
            r#"{"startAt":1,"total":2,"fields":[{"fieldId":"customfield_10042","name":"Required tenant field","required":true,"operations":["set"],"schema":{"type":"number"},"allowedValues":[]}]}"#,
        ),
    ]);
    let error = plan_create_issue(
        &client_with_scripted(&transport),
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([("summary".to_owned(), json!("x"))]),
        },
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
    assert_eq!(transport.requests.borrow().len(), 2);
}

#[test]
fn create_planning_rejects_nonprogressing_or_contradictory_metadata_pages() {
    for (responses, expected_calls) in [
        (
            vec![json_response(200, r#"{"startAt":0,"total":2,"fields":[]}"#)],
            1,
        ),
        (
            vec![
                json_response(
                    200,
                    r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}]}"#,
                ),
                json_response(
                    200,
                    r#"{"startAt":1,"total":3,"fields":[{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]}]}"#,
                ),
            ],
            2,
        ),
    ] {
        let transport = ScriptedTransport::new(responses);
        let error = plan_create_issue(
            &client_with_scripted(&transport),
            CreateIssueInput {
                project_key: "ACCL".to_owned(),
                issue_type_id: "10001".to_owned(),
                fields: BTreeMap::from([("summary".to_owned(), json!("x"))]),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid);
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
        assert_eq!(transport.requests.borrow().len(), expected_calls);
    }
}

#[test]
fn create_planning_enforces_finite_page_field_and_duplicate_budgets() {
    let field = |id: &str| {
        format!(
            r#"{{"fieldId":"{id}","name":"Field","required":false,"operations":["set"],"schema":{{"type":"string"}},"allowedValues":[]}}"#
        )
    };
    let oversized_page = (0..101)
        .map(|index| field(&format!("customfield_{index}")))
        .collect::<Vec<_>>()
        .join(",");
    let cases = vec![
        (
            "wrong start",
            vec![json_response(200, r#"{"startAt":1,"total":1,"fields":[]}"#)],
            1,
        ),
        (
            "oversized page",
            vec![json_response(
                200,
                &format!(r#"{{"startAt":0,"total":101,"fields":[{oversized_page}]}}"#),
            )],
            1,
        ),
        (
            "next past total",
            vec![json_response(
                200,
                &format!(
                    r#"{{"startAt":0,"total":0,"fields":[{}]}}"#,
                    field("summary")
                ),
            )],
            1,
        ),
        (
            "field budget",
            vec![json_response(
                200,
                &format!(
                    r#"{{"startAt":0,"total":10001,"fields":[{}]}}"#,
                    field("summary")
                ),
            )],
            1,
        ),
        (
            "early duplicate",
            vec![
                json_response(
                    200,
                    &format!(
                        r#"{{"startAt":0,"total":2,"fields":[{}]}}"#,
                        field("summary")
                    ),
                ),
                json_response(
                    200,
                    &format!(
                        r#"{{"startAt":1,"total":2,"fields":[{}]}}"#,
                        field("summary")
                    ),
                ),
            ],
            2,
        ),
        (
            "page budget",
            (0..101)
                .map(|page| {
                    json_response(
                        200,
                        &format!(
                            r#"{{"startAt":{page},"total":101,"fields":[{}]}}"#,
                            field(&format!("customfield_{page}"))
                        ),
                    )
                })
                .collect(),
            100,
        ),
    ];

    for (name, responses, expected_calls) in cases {
        let transport = ScriptedTransport::new(responses);
        let error = plan_create_issue(
            &client_with_scripted(&transport),
            CreateIssueInput {
                project_key: "ACCL".to_owned(),
                issue_type_id: "10001".to_owned(),
                fields: BTreeMap::from([("summary".to_owned(), json!("x"))]),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ResponseInvalid, "case: {name}");
        assert_eq!(
            transport.requests.borrow().len(),
            expected_calls,
            "case: {name}"
        );
    }
}

#[test]
fn mutation_local_validation_happens_before_metadata_transport() {
    for input in [
        CreateIssueInput {
            project_key: "".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([("summary".to_owned(), json!("x"))]),
        },
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::new(),
        },
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([
                ("summary".to_owned(), json!("x")),
                ("project".to_owned(), json!({"key":"OTHER"})),
            ]),
        },
    ] {
        let transport = CaptureTransport::new(json_response(500, "{}"));
        let error = plan_create_issue(&client_with(&transport), input).unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaViolation);
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
        assert_eq!(transport.call_count(), 0);
    }

    let transport = CaptureTransport::new(json_response(500, "{}"));
    let error = plan_update_issue(
        &client_with(&transport),
        "ACCL-1",
        UpdateIssueInput {
            set: BTreeMap::new(),
            notify_users: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert_eq!(transport.call_count(), 0);
}

#[test]
fn mutation_metadata_validation_covers_set_type_allowed_clear_and_passthrough() {
    let editmeta = r#"{"fields":{
        "summary":{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},
        "labels":{"fieldId":"labels","name":"Labels","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]},
        "components":{"fieldId":"components","name":"Components","required":true,"operations":["set"],"schema":{"type":"array","items":"component"},"allowedValues":[]},
        "assignee":{"fieldId":"assignee","name":"Assignee","required":false,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"accountId":"abc","displayName":"Agent"}]},
        "status":{"fieldId":"status","name":"Status","required":false,"operations":[],"schema":{"type":"string"},"allowedValues":[]},
        "mystery":{"fieldId":"mystery","name":"Mystery","required":false,"operations":["set"],"schema":{"type":"mystery"}}
    }}"#;

    for (name, field, value) in [
        ("unknown", "unknown", json!("x")),
        ("non-set", "status", json!("x")),
        ("wrong type", "summary", json!(7)),
        ("required clear", "summary", Value::Null),
        ("required array clear", "components", json!([])),
        ("ambiguous clear", "mystery", Value::Null),
        ("disallowed", "assignee", json!({"account_id":"other"})),
    ] {
        let transport = CaptureTransport::new(json_response(200, editmeta));
        let error = plan_update_issue(
            &client_with(&transport),
            "ACCL-1",
            UpdateIssueInput {
                set: BTreeMap::from([(field.to_owned(), value)]),
                notify_users: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaViolation, "case: {name}");
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
        assert_eq!(transport.call_count(), 1, "case: {name}");
    }

    for (field, value) in [("labels", json!([])), ("assignee", Value::Null)] {
        let transport = CaptureTransport::new(json_response(200, editmeta));
        let plan = plan_update_issue(
            &client_with(&transport),
            "ACCL-1",
            UpdateIssueInput {
                set: BTreeMap::from([(field.to_owned(), value.clone())]),
                notify_users: None,
            },
        )
        .unwrap();
        assert_eq!(plan.wire_payload(), &json!({"fields":{field:value}}));
        assert_eq!(transport.call_count(), 1);
    }

    let transport = CaptureTransport::new(json_response(200, editmeta));
    let passthrough = plan_update_issue(
        &client_with(&transport),
        "ACCL-1",
        UpdateIssueInput {
            set: BTreeMap::from([("mystery".to_owned(), json!({"opaque":true}))]),
            notify_users: None,
        },
    )
    .unwrap();
    assert_eq!(
        passthrough.validation.metadata,
        jira_ops::model::ValidationLevel::Partial
    );
    assert_eq!(transport.call_count(), 1);

    let known_without_candidates = CaptureTransport::new(json_response(
        200,
        r#"{"fields":{"summary":{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"}}}}"#,
    ));
    let plan = plan_update_issue(
        &client_with(&known_without_candidates),
        "ACCL-1",
        UpdateIssueInput {
            set: BTreeMap::from([("summary".to_owned(), json!("known type"))]),
            notify_users: None,
        },
    )
    .unwrap();
    assert_eq!(
        plan.validation.metadata,
        jira_ops::model::ValidationLevel::Passed
    );
    assert_eq!(known_without_candidates.call_count(), 1);
}

fn create_hierarchy_plan(
    metadata: &str,
    issue_type_id: &str,
    fields: BTreeMap<String, Value>,
) -> Result<MutationPlan, Box<jira_ops::error::AppError>> {
    let transport = CaptureTransport::new(json_response(200, metadata));
    let result = plan_create_issue(
        &client_with(&transport),
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: issue_type_id.to_owned(),
            fields,
        },
    );
    assert_eq!(transport.call_count(), 1);
    result.map_err(Box::new)
}

fn create_hierarchy_fields(
    entries: impl IntoIterator<Item = (&'static str, Value)>,
) -> BTreeMap<String, Value> {
    std::iter::once(("summary".to_owned(), json!("Hierarchy child")))
        .chain(
            entries
                .into_iter()
                .map(|(id, value)| (id.to_owned(), value)),
        )
        .collect()
}

#[test]
fn create_hierarchy_modern_parent_is_optional_and_validates_generic_issuelink_selectors() {
    let metadata = include_str!("fixtures/create_meta_story_parent.json");
    let omitted = create_hierarchy_plan(metadata, "10002", create_hierarchy_fields([])).unwrap();
    assert_eq!(omitted.validation.metadata, ValidationLevel::Passed);
    assert_eq!(
        omitted.wire_payload(),
        &json!({"fields":{"issuetype":{"id":"10002"},"project":{"key":"ACCL"},"summary":"Hierarchy child"}})
    );

    for selector in [json!({"key":"ACCL-10"}), json!({"id":"10010"})] {
        let plan = create_hierarchy_plan(
            metadata,
            "10002",
            create_hierarchy_fields([("parent", selector.clone())]),
        )
        .unwrap();
        assert_eq!(plan.validation.metadata, ValidationLevel::Passed);
        assert_eq!(plan.wire_payload()["fields"]["parent"], selector);
    }

    for (case, selector) in [
        ("scalar", json!("ACCL-10")),
        ("empty object", json!({})),
        ("empty key", json!({"key":""})),
        ("empty id", json!({"id":""})),
        ("extra member", json!({"key":"ACCL-10","extra":true})),
        (
            "contradictory members",
            json!({"id":"10010","key":"ACCL-10"}),
        ),
    ] {
        let error = create_hierarchy_plan(
            metadata,
            "10002",
            create_hierarchy_fields([("parent", selector)]),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaViolation, "case: {case}");
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
    }
}

#[test]
fn create_hierarchy_subtask_parent_is_required_and_accepts_each_issuelink_selector() {
    let metadata = include_str!("fixtures/create_meta_subtask_parent.json");
    let missing =
        create_hierarchy_plan(metadata, "10005", create_hierarchy_fields([])).unwrap_err();
    assert_eq!(missing.code, ErrorCode::SchemaViolation);
    assert_eq!(
        missing.operation_outcome,
        Some(OperationOutcome::NotApplied)
    );

    for selector in [json!({"key":"ACCL-20"}), json!({"id":"10020"})] {
        let plan = create_hierarchy_plan(
            metadata,
            "10005",
            create_hierarchy_fields([("parent", selector.clone())]),
        )
        .unwrap();
        assert_eq!(plan.validation.metadata, ValidationLevel::Passed);
        assert_eq!(plan.wire_payload()["fields"]["parent"], selector);
    }
}

#[test]
fn create_hierarchy_unregistered_opaque_object_passes_through_as_partial() {
    let metadata = r#"{"startAt":0,"total":2,"fields":[
        {"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},
        {"fieldId":"customfield_70001","name":"Opaque object","required":false,"operations":["set"],"schema":{"type":"object"}}
    ]}"#;
    let opaque = json!({"tenant_shape":{"nested":true},"unmodeled":7});
    let plan = create_hierarchy_plan(
        metadata,
        "10002",
        create_hierarchy_fields([("customfield_70001", opaque.clone())]),
    )
    .unwrap();
    assert_eq!(plan.validation.metadata, ValidationLevel::Partial);
    assert_eq!(plan.wire_payload()["fields"]["customfield_70001"], opaque);
}

#[test]
fn create_hierarchy_metadata_exposes_system_selectors_and_exact_classic_marker() {
    let modern_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/create_meta_story_parent.json"),
    ));
    let modern = issue_create_meta(
        &client_with(&modern_transport),
        "ACCL",
        Some("10002"),
        20,
        None,
    )
    .unwrap();
    let modern = serde_json::to_value(modern).unwrap();
    assert_eq!(
        modern["data"][1],
        json!({
            "id":"parent",
            "name":"Parent",
            "required":false,
            "operations":["set"],
            "schema":{"type":"issuelink","items":null,"custom":null,"system":"parent"},
            "input_kind":"object",
            "supported_selector_members":["id","key"],
            "allowed_values_complete":false
        })
    );

    let classic_transport = CaptureTransport::new(json_response(
        200,
        include_str!("fixtures/create_meta_story_epic_link.json"),
    ));
    let classic = issue_create_meta(
        &client_with(&classic_transport),
        "ACCL",
        Some("10002"),
        20,
        None,
    )
    .unwrap();
    let classic = serde_json::to_value(classic).unwrap();
    assert_eq!(
        classic["data"][1],
        json!({
            "id":"customfield_78431",
            "name":"Localized relationship label",
            "required":false,
            "operations":["set"],
            "schema":{"type":"any","items":null,"custom":"com.pyxis.greenhopper.jira:gh-epic-link","system":null},
            "input_kind":"passthrough",
            "supported_selector_members":[],
            "allowed_values_complete":false
        })
    );

    let catalog_transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":3,"isLast":true,"values":[
            {"id":"customfield_78431","name":"Localized relationship label","schema":{"type":"any","custom":"com.pyxis.greenhopper.jira:gh-epic-link"}},
            {"id":"customfield_10014","name":"Epic Link","schema":{"type":"string","custom":"tenant.lookalike:not-epic-link"}},
            {"id":"customfield_99999","name":"Different field","schema":{"type":"any","custom":"com.pyxis.greenhopper.jira:gh-epic-link"}}
        ]}"#,
    ));
    let catalog = serde_json::to_value(
        field_list(&client_with(&catalog_transport), None, 100, None).unwrap(),
    )
    .unwrap();
    let marker = "com.pyxis.greenhopper.jira:gh-epic-link";
    let create_candidate = classic["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["schema"]["custom"] == marker)
        .expect("create metadata exposes exact classic marker");
    let candidate_id = create_candidate["id"].as_str().unwrap();
    let correlated = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|field| field["id"] == candidate_id && field["schema"]["custom"] == marker)
        .collect::<Vec<_>>();
    assert_eq!(correlated.len(), 1);
    assert_eq!(correlated[0]["id"], "customfield_78431");
    assert_ne!(correlated[0]["name"], "Epic Link");

    let plan = create_hierarchy_plan(
        include_str!("fixtures/create_meta_story_epic_link.json"),
        "10002",
        create_hierarchy_fields([("customfield_78431", json!("ACCL-10"))]),
    )
    .unwrap();
    assert_eq!(plan.validation.metadata, ValidationLevel::Partial);
    assert_eq!(
        plan.wire_payload()["fields"]["customfield_78431"],
        "ACCL-10"
    );
    assert!(plan.wire_payload()["fields"].get("parent").is_none());
}

#[test]
fn mutation_metadata_validates_array_items_and_canonicalizes_selectors() {
    let editmeta = r#"{"fields":{
        "labels":{"fieldId":"labels","name":"Labels","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]},
        "components":{"fieldId":"components","name":"Components","required":false,"operations":["set"],"schema":{"type":"array","items":"component"},"allowedValues":[{"id":"1","name":"API"}]},
        "unsupported":{"fieldId":"unsupported","name":"Unsupported","required":false,"operations":["set"],"schema":{"type":"array","items":"mystery"},"allowedValues":[]},
        "assignee":{"fieldId":"assignee","name":"Assignee","required":false,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"accountId":"abc","displayName":"Agent"}]}
    }}"#;

    for (name, field, value) in [
        ("wrong primitive item", "labels", json!(["ok", 7])),
        (
            "contradictory selector",
            "components",
            json!([{"id":"1","name":"Other"}]),
        ),
        (
            "unexpected selector member",
            "assignee",
            json!({"account_id":"abc","extra":true}),
        ),
    ] {
        let transport = CaptureTransport::new(json_response(200, editmeta));
        let error = plan_update_issue(
            &client_with(&transport),
            "ACCL-1",
            UpdateIssueInput {
                set: BTreeMap::from([(field.to_owned(), value)]),
                notify_users: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaViolation, "case: {name}");
        assert_eq!(transport.call_count(), 1, "case: {name}");
    }

    for (field, value, expected_wire, expected_metadata) in [
        (
            "labels",
            json!(["one", "two"]),
            json!(["one", "two"]),
            "passed",
        ),
        (
            "components",
            json!([{"id":"1","name":"API"}]),
            json!([{"id":"1"}]),
            "passed",
        ),
        (
            "assignee",
            json!({"account_id":"abc","display_name":"Agent"}),
            json!({"accountId":"abc"}),
            "passed",
        ),
        (
            "unsupported",
            json!([{"opaque":true}]),
            json!([{"opaque":true}]),
            "partial",
        ),
    ] {
        let transport = CaptureTransport::new(json_response(200, editmeta));
        let plan = plan_update_issue(
            &client_with(&transport),
            "ACCL-1",
            UpdateIssueInput {
                set: BTreeMap::from([(field.to_owned(), value.clone())]),
                notify_users: None,
            },
        )
        .unwrap();
        assert_eq!(plan.changes, json!({"set":{field:value}}), "field: {field}");
        assert_eq!(
            plan.wire_payload(),
            &json!({"fields":{field:expected_wire}}),
            "field: {field}"
        );
        assert_eq!(
            serde_json::to_value(plan.validation.metadata).unwrap(),
            json!(expected_metadata)
        );
        assert_eq!(transport.call_count(), 1);
    }
}

#[test]
fn empty_array_metadata_is_partial_unless_item_validation_is_modeled() {
    let editmeta = r#"{"fields":{
        "unsupported":{"fieldId":"unsupported","name":"Unsupported","required":false,"operations":["set"],"schema":{"type":"array","items":"mystery"},"allowedValues":[]},
        "absent":{"fieldId":"absent","name":"Absent","required":false,"operations":["set"],"schema":{"type":"array"},"allowedValues":[]},
        "modeled":{"fieldId":"modeled","name":"Modeled","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]},
        "selector":{"fieldId":"selector","name":"Selector","required":false,"operations":["set"],"schema":{"type":"array","items":"component"},"allowedValues":[{"id":"1","name":"API"}]}
    }}"#;

    for (field, expected_metadata) in [
        ("unsupported", "partial"),
        ("absent", "partial"),
        ("modeled", "passed"),
        ("selector", "passed"),
    ] {
        let transport = CaptureTransport::new(json_response(200, editmeta));
        let plan = plan_update_issue(
            &client_with(&transport),
            "ACCL-1",
            UpdateIssueInput {
                set: BTreeMap::from([(field.to_owned(), json!([]))]),
                notify_users: None,
            },
        )
        .unwrap();
        assert_eq!(plan.wire_payload(), &json!({"fields":{field:[]}}));
        assert_eq!(
            serde_json::to_value(plan.validation.metadata).unwrap(),
            json!(expected_metadata),
            "field: {field}"
        );
        assert_eq!(transport.call_count(), 1);
    }
}

#[test]
fn create_and_transition_reject_wrong_modeled_array_items() {
    let create_transport = CaptureTransport::new(json_response(
        200,
        r#"{"startAt":0,"total":2,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},{"fieldId":"labels","name":"Labels","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]}]}"#,
    ));
    let create_error = plan_create_issue(
        &client_with(&create_transport),
        CreateIssueInput {
            project_key: "ACCL".to_owned(),
            issue_type_id: "10001".to_owned(),
            fields: BTreeMap::from([
                ("summary".to_owned(), json!("x")),
                ("labels".to_owned(), json!(["ok", false])),
            ]),
        },
    )
    .unwrap_err();
    assert_eq!(create_error.code, ErrorCode::SchemaViolation);
    assert_eq!(create_transport.call_count(), 1);

    let transition_transport = CaptureTransport::new(json_response(
        200,
        r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{"labels":{"fieldId":"labels","name":"Labels","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]}}}]}"#,
    ));
    let transition_error = plan_transition_issue(
        &client_with(&transition_transport),
        "ACCL-1",
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::from([("labels".to_owned(), json!([7]))]),
            comment: None,
            notify_users: None,
        },
    )
    .unwrap_err();
    assert_eq!(transition_error.code, ErrorCode::SchemaViolation);
    assert_eq!(transition_transport.call_count(), 1);
}

#[test]
fn adf_planning_rejects_paragraph_amplification_before_any_jira_call() {
    let body = "\n".repeat(10_000);
    let error = plan_comment(
        "ACCL-1",
        CommentInput {
            body: body.into(),
            internal: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));

    let body = "\n".repeat(9_999);
    let plan = plan_comment(
        "ACCL-1",
        CommentInput {
            body: body.into(),
            internal: false,
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload()["body"]["content"]
            .as_array()
            .unwrap()
            .len(),
        10_000
    );
}

#[test]
fn content_planning_enforces_exact_one_mib_source_boundary() {
    const MAX_SOURCE_BYTES: usize = 1024 * 1024;
    let accepted = "x".repeat(MAX_SOURCE_BYTES);
    let plan = plan_comment(
        "ACCL-1",
        CommentInput {
            body: accepted.clone().into(),
            internal: false,
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload()["body"]["content"][0]["content"][0]["text"],
        accepted
    );

    let rejected = "x".repeat(MAX_SOURCE_BYTES + 1);
    let error = plan_comment(
        "ACCL-1",
        CommentInput {
            body: rejected.into(),
            internal: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);
    assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
}

#[test]
fn whitespace_only_mutation_issue_targets_fail_before_transport() {
    let transport = CaptureTransport::new(json_response(500, "{}"));
    let update = plan_update_issue(
        &client_with(&transport),
        " \t",
        UpdateIssueInput {
            set: BTreeMap::from([("summary".to_owned(), json!("x"))]),
            notify_users: None,
        },
    )
    .unwrap_err();
    assert_eq!(update.code, ErrorCode::SchemaViolation);
    assert_eq!(transport.call_count(), 0);

    let transition = plan_transition_issue(
        &client_with(&transport),
        " \t",
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::new(),
            comment: None,
            notify_users: None,
        },
    )
    .unwrap_err();
    assert_eq!(transition.code, ErrorCode::SchemaViolation);
    assert_eq!(transport.call_count(), 0);

    let comment = plan_comment(
        " \t",
        CommentInput {
            body: "x".into(),
            internal: false,
        },
    )
    .unwrap_err();
    assert_eq!(comment.code, ErrorCode::SchemaViolation);
    assert_eq!(
        comment.operation_outcome,
        Some(OperationOutcome::NotApplied)
    );
}

#[test]
fn transition_planning_distinguishes_omitted_and_explicit_empty_field_metadata() {
    for (response, expected) in [
        (
            r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{}}]}"#,
            "passed",
        ),
        (
            r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"}}]}"#,
            "partial",
        ),
    ] {
        let transport = CaptureTransport::new(json_response(200, response));
        let plan = plan_transition_issue(
            &client_with(&transport),
            "ACCL-1",
            TransitionInput {
                transition_id: "31".to_owned(),
                fields: BTreeMap::new(),
                comment: None,
                notify_users: None,
            },
        )
        .unwrap();
        assert_get_request(
            &transport.requests.borrow()[0],
            "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/ACCL-1/transitions",
            &[("expand", "transitions.fields")],
        );
        assert_eq!(transport.call_count(), 1);
        assert_eq!(
            serde_json::to_value(plan.validation.metadata).unwrap(),
            json!(expected)
        );
    }
}

#[test]
fn transition_planning_requires_selected_screen_fields_and_exact_id() {
    let response = r#"{"transitions":[{"id":"31","name":"Done","to":{"id":"10002","name":"Done"},"fields":{"resolution":{"fieldId":"resolution","name":"Resolution","required":true,"operations":["set"],"schema":{"type":"object"},"allowedValues":[{"id":"1","name":"Done"}]}}}]}"#;
    for input in [
        TransitionInput {
            transition_id: "99".to_owned(),
            fields: BTreeMap::new(),
            comment: None,
            notify_users: None,
        },
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::new(),
            comment: None,
            notify_users: None,
        },
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::from([("unknown".to_owned(), json!("x"))]),
            comment: None,
            notify_users: None,
        },
    ] {
        let transport = CaptureTransport::new(json_response(200, response));
        let error = plan_transition_issue(&client_with(&transport), "ACCL-1", input).unwrap_err();
        assert!(matches!(
            error.code,
            ErrorCode::NotFound | ErrorCode::SchemaViolation
        ));
        assert_eq!(error.operation_outcome, Some(OperationOutcome::NotApplied));
        assert_eq!(transport.call_count(), 1);
    }

    let transport = CaptureTransport::new(json_response(200, response));
    let plan = plan_transition_issue(
        &client_with(&transport),
        "ACCL-1",
        TransitionInput {
            transition_id: "31".to_owned(),
            fields: BTreeMap::from([("resolution".to_owned(), json!({"id":"1"}))]),
            comment: None,
            notify_users: None,
        },
    )
    .unwrap();
    assert_eq!(
        plan.wire_payload(),
        &json!({"transition":{"id":"31"},"fields":{"resolution":{"id":"1"}}})
    );
    assert_eq!(transport.call_count(), 1);
}

fn assert_get_request(request: &HttpRequest, path: &str, query: &[(&str, &str)]) {
    assert_eq!(request.method, jira_ops::client::HttpMethod::Get);
    assert_eq!(request.url.path(), path);
    let mut actual: Vec<(String, String)> = request.url.query_pairs().into_owned().collect();
    let mut expected: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

fn client_with(transport: &CaptureTransport) -> JiraClient<&CaptureTransport> {
    JiraClient::new(transport, test_credential(), Duration::from_secs(30))
}

fn client_with_scripted(transport: &ScriptedTransport) -> JiraClient<&ScriptedTransport> {
    JiraClient::new(
        transport,
        ResolvedCredential {
            site: Url::parse("https://example.atlassian.net").unwrap(),
            cloud_id: Uuid::nil(),
            email: "agent@example.com".to_owned(),
            account_id: Some("abc123".to_owned()),
            token: SecretString::from("secret-token"),
            source: CredentialSource::Keyring,
        },
        Duration::from_secs(30),
    )
}

fn test_credential() -> ResolvedCredential {
    ResolvedCredential {
        site: Url::parse("https://example.atlassian.net/").unwrap(),
        cloud_id: Uuid::nil(),
        email: "agent@example.com".to_owned(),
        account_id: Some("abc123".to_owned()),
        token: SecretString::from("secret-token"),
        source: CredentialSource::Keyring,
    }
}

fn json_response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: body.as_bytes().to_vec(),
    }
}
