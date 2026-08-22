use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use jira_ops::client::{
    HttpMethod, HttpRequest, HttpResponse, JiraClient, JiraTransport, RequestEffect,
    TransportFailure,
};
use jira_ops::commands::destructive::{apply_delete_issue, plan_delete_issue};
use jira_ops::commands::link::{apply_remove_link, plan_remove_link};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::error::ErrorCode;
use jira_ops::model::{DeleteIssueInput, RemoveLinkInput};
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
            email: "agent@example.com".to_owned(),
            account_id: Some("abc".to_owned()),
            token: SecretString::from("test-token"),
            source: CredentialSource::Keyring,
        },
        Duration::from_secs(30),
    )
}

#[test]
fn issue_delete_requires_exact_confirmation_without_transport() {
    let error = plan_delete_issue(
        "OPS-1",
        DeleteIssueInput {
            confirm_issue: "OPS-2".to_owned(),
            cascade: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DestructiveConfirmationRequired);
}

#[test]
fn issue_delete_uses_exact_path_query_and_empty_204() {
    let scripted = Scripted {
        requests: RefCell::new(Vec::new()),
        responses: RefCell::new(VecDeque::from([response(204, "")])),
    };
    let plan = plan_delete_issue(
        "OPS-1",
        DeleteIssueInput {
            confirm_issue: "OPS-1".to_owned(),
            cascade: true,
        },
    )
    .unwrap();
    let applied = apply_delete_issue(&client(&scripted), "OPS-1", plan).unwrap();
    assert_eq!(applied.operation, "issue.delete");
    let request = &scripted.requests.borrow()[0];
    assert_eq!(request.method, HttpMethod::Delete);
    assert_eq!(request.effect, RequestEffect::JiraWrite);
    assert_eq!(
        request.url.path(),
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/issue/OPS-1"
    );
    assert_eq!(request.url.query(), Some("deleteSubtasks=true"));
    assert!(request.body.is_empty());
}

#[test]
fn link_remove_checks_confirmation_before_preflight() {
    let scripted = Scripted {
        requests: RefCell::new(Vec::new()),
        responses: RefCell::new(VecDeque::new()),
    };
    let error = plan_remove_link(
        &client(&scripted),
        "10000",
        RemoveLinkInput {
            confirm_link_id: "10001".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DestructiveConfirmationRequired);
    assert!(scripted.requests.borrow().is_empty());
}

#[test]
fn link_remove_preflights_then_accepts_empty_200_or_204() {
    for status in [200, 204] {
        let scripted = Scripted {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(VecDeque::from([
                response(
                    200,
                    r#"{"id":"10000","type":{"id":"1","name":"Blocks","inward":"is blocked by","outward":"blocks"},"inwardIssue":{"id":"10","key":"OPS-1"},"outwardIssue":{"id":"11","key":"OPS-2"}}"#,
                ),
                response(status, ""),
            ])),
        };
        let plan = plan_remove_link(
            &client(&scripted),
            "10000",
            RemoveLinkInput {
                confirm_link_id: "10000".to_owned(),
            },
        )
        .unwrap();
        let applied = apply_remove_link(&client(&scripted), "10000", plan).unwrap();
        assert_eq!(applied.link_id, "10000");
        let requests = scripted.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, HttpMethod::Get);
        assert_eq!(requests[0].effect, RequestEffect::Read);
        assert_eq!(requests[1].method, HttpMethod::Delete);
        assert_eq!(requests[1].effect, RequestEffect::JiraWrite);
        assert!(requests[1].body.is_empty());
    }
}
