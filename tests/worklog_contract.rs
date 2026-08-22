use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use jira_ops::client::{
    HttpMethod, HttpRequest, HttpResponse, JiraClient, JiraTransport, TransportFailure,
};
use jira_ops::commands::worklog::{
    apply_worklog_add, apply_worklog_delete, apply_worklog_update, compile_adjustment_query,
    normalize_started, plan_worklog_add, plan_worklog_delete, plan_worklog_update,
    validate_worklog_write, worklog_list,
};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::error::ErrorCode;
use jira_ops::model::{EstimateAdjustment, WorklogDeleteInput, WorklogWriteInput};
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
fn started_requires_an_explicit_offset_and_normalizes_for_jira() {
    let invalid = WorklogWriteInput {
        time_spent: "1h 30m".into(),
        started: Some("2026-08-20T09:30:00".into()),
        comment: None,
        adjustment: EstimateAdjustment::Auto,
        notify_users: true,
    };
    assert_eq!(
        validate_worklog_write(&invalid).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
    assert_eq!(
        normalize_started("2026-08-20T09:30:00-04:00").unwrap(),
        "2026-08-20T09:30:00.000-0400"
    );
}

#[test]
fn duration_and_adjustment_modes_are_strict_and_deterministic() {
    let invalid = WorklogWriteInput {
        time_spent: "90 minutes".into(),
        started: None,
        comment: None,
        adjustment: EstimateAdjustment::Auto,
        notify_users: true,
    };
    assert_eq!(
        validate_worklog_write(&invalid).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
    assert_eq!(
        compile_adjustment_query(&EstimateAdjustment::Auto, true).unwrap(),
        "adjustEstimate=auto&notifyUsers=true"
    );
    assert_eq!(
        compile_adjustment_query(
            &EstimateAdjustment::New {
                new_estimate: "2h".into()
            },
            false
        )
        .unwrap(),
        "adjustEstimate=new&newEstimate=2h&notifyUsers=false"
    );
    assert_eq!(
        compile_adjustment_query(
            &EstimateAdjustment::Manual {
                reduce_by: "30m".into()
            },
            true
        )
        .unwrap(),
        "adjustEstimate=manual&reduceBy=30m&notifyUsers=true"
    );
    assert_eq!(
        compile_adjustment_query(&EstimateAdjustment::Leave, false).unwrap(),
        "adjustEstimate=leave&notifyUsers=false"
    );
}

#[test]
fn worklog_list_and_lifecycle_use_exact_contracts() {
    let transport = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([
            response(
                200,
                r#"{"startAt":0,"total":1,"worklogs":[{"id":"17","author":{"accountId":"abc","displayName":"Agent","active":true},"started":"2026-08-20T09:30:00.000-0400","timeSpent":"1h","timeSpentSeconds":3600,"comment":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"done"}]}]},"updated":"2026-08-20T14:00:00.000+0000"}]}"#,
            ),
            response(
                201,
                r#"{"id":"18","author":{"accountId":"abc","displayName":"Agent","active":true},"started":"2026-08-20T09:30:00.000-0400","timeSpent":"1h","timeSpentSeconds":3600}"#,
            ),
            response(
                200,
                r#"{"id":"18","author":{"accountId":"abc","displayName":"Agent","active":true},"started":"2026-08-20T09:30:00.000-0400","timeSpent":"2h","timeSpentSeconds":7200}"#,
            ),
            response(204, ""),
        ])),
    };

    let listed = worklog_list(&client(&transport), "OPS-1", 20, None).unwrap();
    assert_eq!(listed.data[0].comment.as_deref(), Some("done"));
    assert_eq!(listed.meta.unwrap().count, 1);

    let add_input = WorklogWriteInput {
        time_spent: "1h".into(),
        started: Some("2026-08-20T09:30:00-04:00".into()),
        comment: None,
        adjustment: EstimateAdjustment::Auto,
        notify_users: false,
    };
    let add_plan = plan_worklog_add("OPS-1", add_input.clone()).unwrap();
    assert_eq!(
        add_plan.wire_payload()["started"],
        "2026-08-20T09:30:00.000-0400"
    );
    let added = apply_worklog_add(
        &client(&transport),
        "OPS-1",
        &add_input.adjustment,
        add_input.notify_users,
        add_plan,
    )
    .unwrap();
    assert_eq!(added.worklog_id, "18");

    let update_input = WorklogWriteInput {
        time_spent: "2h".into(),
        started: None,
        comment: None,
        adjustment: EstimateAdjustment::Leave,
        notify_users: true,
    };
    let update_plan = plan_worklog_update("OPS-1", "18", update_input.clone()).unwrap();
    let updated = apply_worklog_update(
        &client(&transport),
        "OPS-1",
        "18",
        &update_input.adjustment,
        update_input.notify_users,
        update_plan,
    )
    .unwrap();
    assert_eq!(updated.worklog_id, "18");

    let delete_input = WorklogDeleteInput {
        confirm_worklog_id: "18".into(),
        adjustment: EstimateAdjustment::Manual {
            reduce_by: "2h".into(),
        },
        notify_users: false,
    };
    let delete_plan = plan_worklog_delete("OPS-1", "18", delete_input.clone()).unwrap();
    let deleted = apply_worklog_delete(
        &client(&transport),
        "OPS-1",
        "18",
        &delete_input.adjustment,
        delete_input.notify_users,
        delete_plan,
    )
    .unwrap();
    assert_eq!(deleted.worklog_id, "18");

    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert_eq!(requests[2].method, HttpMethod::Put);
    assert_eq!(requests[3].method, HttpMethod::Delete);
    assert_eq!(
        requests[1].url.query(),
        Some("adjustEstimate=auto&notifyUsers=false")
    );
}

#[test]
fn worklog_rejects_invalid_pages_ids_and_confirmation_before_writes() {
    let transport = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([response(
            200,
            r#"{"startAt":1,"total":0,"worklogs":[]}"#,
        )])),
    };
    assert_eq!(
        worklog_list(&client(&transport), "OPS-1", 20, None)
            .unwrap_err()
            .code,
        ErrorCode::ResponseInvalid
    );
    let input = WorklogWriteInput {
        time_spent: "1h".into(),
        started: None,
        comment: None,
        adjustment: EstimateAdjustment::Auto,
        notify_users: true,
    };
    assert_eq!(
        plan_worklog_update("OPS-1", "0", input).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
    assert_eq!(
        plan_worklog_delete(
            "OPS-1",
            "17",
            WorklogDeleteInput {
                confirm_worklog_id: "18".into(),
                adjustment: EstimateAdjustment::Auto,
                notify_users: true,
            },
        )
        .unwrap_err()
        .code,
        ErrorCode::DestructiveConfirmationRequired
    );
}
