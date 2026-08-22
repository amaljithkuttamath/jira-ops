use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use jira_ops::client::{
    HttpMethod, HttpRequest, HttpResponse, JiraClient, JiraTransport, TransportFailure,
};
use jira_ops::commands::sprint::{
    apply_sprint_add, apply_sprint_close, parse_sprint_state, plan_sprint_add, plan_sprint_close,
    sprint_list, validate_sprint_add, validate_sprint_close,
};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::error::ErrorCode;
use jira_ops::model::{SprintAddInput, SprintCloseInput, SprintState};
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

struct Scripted {
    requests: RefCell<Vec<HttpRequest>>,
    responses: RefCell<VecDeque<HttpResponse>>,
}

impl JiraTransport for &Scripted {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        self.requests.borrow_mut().push(request);
        Ok(self.responses.borrow_mut().pop_front().expect("response"))
    }
}

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn client(transport: &Scripted) -> JiraClient<&Scripted> {
    JiraClient::new(
        transport,
        ResolvedCredential {
            site: Url::parse("https://example.atlassian.net/").unwrap(),
            cloud_id: Uuid::nil(),
            email: "agent@example.com".into(),
            account_id: Some("abc".into()),
            token: SecretString::from("token"),
            source: CredentialSource::Keyring,
        },
        Duration::from_secs(30),
    )
}

#[test]
fn sprint_add_enforces_fifty_unique_issue_limit() {
    let input = SprintAddInput {
        issue_keys: (1..=51).map(|n| format!("OPS-{n}")).collect(),
    };
    assert_eq!(
        validate_sprint_add(&input).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
}
#[test]
fn sprint_close_binds_confirmation_and_timestamp() {
    let mismatch = SprintCloseInput {
        confirm_sprint_id: 8,
        complete_date: None,
    };
    assert_eq!(
        validate_sprint_close(7, &mismatch).unwrap_err().code,
        ErrorCode::DestructiveConfirmationRequired
    );
    let invalid = SprintCloseInput {
        confirm_sprint_id: 7,
        complete_date: Some("2026-08-20T09:00:00".into()),
    };
    assert_eq!(
        validate_sprint_close(7, &invalid).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
}

#[test]
fn sprint_list_add_and_close_use_exact_agile_contracts() {
    let transport = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([
            response(
                200,
                r#"{"startAt":0,"total":1,"values":[{"id":7,"name":"Iteration 7","state":"active","startDate":"2026-08-01T09:00:00.000Z","endDate":"2026-08-14T17:00:00.000Z","goal":"Ship"}]}"#,
            ),
            response(204, ""),
            response(
                200,
                r#"{"id":7,"name":"Iteration 7","state":"active","startDate":"2026-08-01T09:00:00.000Z","endDate":"2026-08-14T17:00:00.000Z","goal":"Ship"}"#,
            ),
            response(
                200,
                r#"{"id":7,"name":"Iteration 7","state":"closed","startDate":"2026-08-01T09:00:00.000Z","endDate":"2026-08-14T17:00:00.000Z","completeDate":"2026-08-14T16:00:00.000Z","goal":"Ship"}"#,
            ),
        ])),
    };

    assert_eq!(
        parse_sprint_state(Some("active")).unwrap(),
        Some(SprintState::Active)
    );
    let listed = sprint_list(&client(&transport), 12, Some(SprintState::Active), 20, None).unwrap();
    assert_eq!(listed.data[0].id, 7);
    assert_eq!(listed.meta.unwrap().count, 1);

    let add_plan = plan_sprint_add(
        7,
        SprintAddInput {
            issue_keys: vec!["OPS-1".into(), "OPS-2".into()],
        },
    )
    .unwrap();
    let added = apply_sprint_add(&client(&transport), 7, add_plan).unwrap();
    assert_eq!(added.issue_keys.unwrap(), vec!["OPS-1", "OPS-2"]);

    let close_plan = plan_sprint_close(
        &client(&transport),
        7,
        SprintCloseInput {
            confirm_sprint_id: 7,
            complete_date: Some("2026-08-14T16:00:00Z".into()),
        },
    )
    .unwrap();
    assert_eq!(close_plan.wire_payload()["state"], "closed");
    let closed = apply_sprint_close(&client(&transport), 7, close_plan).unwrap();
    assert_eq!(closed.sprint_id, 7);

    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert_eq!(requests[2].method, HttpMethod::Get);
    assert_eq!(requests[3].method, HttpMethod::Put);
    assert_eq!(
        requests[0].url.query(),
        Some("state=active&startAt=0&maxResults=20")
    );
}

#[test]
fn sprint_rejects_bad_state_board_and_nonactive_close() {
    assert_eq!(
        parse_sprint_state(Some("paused")).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
    let transport = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([response(
            200,
            r#"{"id":7,"name":"Iteration 7","state":"closed"}"#,
        )])),
    };
    assert_eq!(
        sprint_list(&client(&transport), 0, None, 20, None)
            .unwrap_err()
            .code,
        ErrorCode::SchemaViolation
    );
    assert_eq!(
        plan_sprint_close(
            &client(&transport),
            7,
            SprintCloseInput {
                confirm_sprint_id: 7,
                complete_date: None,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );
}
