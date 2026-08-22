use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use jira_ops::client::{
    HttpMethod, HttpRequest, HttpResponse, JiraClient, JiraTransport, RequestEffect,
    TransportFailure,
};
use jira_ops::commands::remote_link::{
    apply_remote_link_add, apply_remote_link_remove, plan_remote_link_add, plan_remote_link_remove,
    remote_link_get, remote_link_list, validate_remote_link_input,
};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::error::ErrorCode;
use jira_ops::model::{RemoteLinkInput, RemoveRemoteLinkInput};
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
fn client(t: &Scripted) -> JiraClient<&Scripted> {
    JiraClient::new(
        t,
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
fn add_rejects_non_https_before_transport() {
    let error = validate_remote_link_input(&RemoteLinkInput {
        url: Url::parse("http://example.com/1").unwrap(),
        title: "Ticket".into(),
        relationship: None,
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaViolation);
}

#[test]
fn list_and_get_project_only_safe_fields() {
    let t = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([
            response(
                200,
                r#"[{"id":7,"globalId":"g","relationship":"causes","object":{"url":"https://tracker.example/7","title":"Ticket 7","summary":"private"}}]"#,
            ),
            response(
                200,
                r#"{"id":7,"globalId":"g","relationship":"causes","object":{"url":"https://tracker.example/7","title":"Ticket 7"}}"#,
            ),
        ])),
    };
    let listed = remote_link_list(&client(&t), "OPS-1").unwrap();
    assert_eq!(listed.meta.unwrap().count, 1);
    assert_eq!(listed.data[0].title, "Ticket 7");
    let got = remote_link_get(&client(&t), "OPS-1", "7").unwrap();
    assert_eq!(got.data.id, 7);
    assert_eq!(
        t.requests.borrow()[1].url.path().rsplit('/').next(),
        Some("7")
    );
}

#[test]
fn add_and_confirmed_remove_use_exact_write_contracts() {
    let t = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([
            response(201, r#"{"id":7,"self":"ignored"}"#),
            response(
                200,
                r#"{"id":7,"object":{"url":"https://tracker.example/7","title":"Ticket 7"}}"#,
            ),
            response(204, ""),
        ])),
    };
    let add = plan_remote_link_add(
        "OPS-1",
        RemoteLinkInput {
            url: Url::parse("https://tracker.example/7").unwrap(),
            title: "Ticket 7".into(),
            relationship: Some("causes".into()),
        },
    )
    .unwrap();
    let applied = apply_remote_link_add(&client(&t), "OPS-1", add).unwrap();
    assert_eq!(applied.remote_link_id, 7);
    let remove = plan_remote_link_remove(
        &client(&t),
        "OPS-1",
        "7",
        RemoveRemoteLinkInput {
            confirm_remote_link_id: "7".into(),
        },
    )
    .unwrap();
    apply_remote_link_remove(&client(&t), "OPS-1", "7", remove).unwrap();
    let requests = t.requests.borrow();
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[2].method, HttpMethod::Delete);
    assert!(
        requests
            .iter()
            .skip(2)
            .all(|r| r.effect == RequestEffect::JiraWrite)
    );
}

#[test]
fn remove_confirmation_precedes_transport() {
    let t = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::new()),
    };
    let error = plan_remote_link_remove(
        &client(&t),
        "OPS-1",
        "7",
        RemoveRemoteLinkInput {
            confirm_remote_link_id: "8".into(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DestructiveConfirmationRequired);
    assert!(t.requests.borrow().is_empty());
}
