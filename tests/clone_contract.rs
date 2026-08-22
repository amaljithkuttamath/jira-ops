use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

use jira_ops::client::{
    HttpRequest, HttpResponse, JiraClient, JiraTransport, RequestEffect, TransportFailure,
};
use jira_ops::commands::clone::{CloneIssueInput, ReplaceRule, plan_clone_issue};
use jira_ops::config::{CredentialSource, ResolvedCredential};
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
        Ok(self.responses.borrow_mut().pop_front().unwrap())
    }
}

fn response(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: std::collections::BTreeMap::from([(
            "content-type".to_owned(),
            "application/json".to_owned(),
        )]),
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
fn clone_reads_source_and_metadata_but_performs_zero_writes_while_planning() {
    let transport = Scripted {
        requests: RefCell::new(Vec::new()),
        responses: RefCell::new(VecDeque::from([
            response(
                r#"{"id":"1","key":"OPS-1","fields":{"project":{"key":"OPS"},"issuetype":{"id":"10001"},"summary":"source summary","description":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":"source body"}]}]},"labels":["one"]}}"#,
            ),
            response(
                r#"{"startAt":0,"total":4,"fields":[{"fieldId":"summary","name":"Summary","required":true,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},{"fieldId":"description","name":"Description","required":false,"operations":["set"],"schema":{"type":"string"},"allowedValues":[]},{"fieldId":"labels","name":"Labels","required":false,"operations":["set"],"schema":{"type":"array","items":"string"},"allowedValues":[]},{"fieldId":"parent","name":"Parent","required":false,"operations":["set"],"schema":{"type":"object","system":"parent"},"allowedValues":[]}]}"#,
            ),
        ])),
    };
    let input = CloneIssueInput {
        summary: Some("Copy: source".to_owned()),
        replacements: vec![ReplaceRule {
            search: "source".to_owned(),
            replacement: "target".to_owned(),
        }],
        ..CloneIssueInput::default()
    };
    let plan = plan_clone_issue(&client(&transport), "OPS-1", input).unwrap();
    assert_eq!(plan.operation, "issue.clone");
    assert_eq!(plan.changes["fields"]["summary"], "Copy: target");
    assert_eq!(transport.requests.borrow().len(), 2);
    assert!(
        transport
            .requests
            .borrow()
            .iter()
            .all(|request| request.effect == RequestEffect::Read)
    );
}

#[test]
fn clone_rejects_empty_replacement_search_before_transport() {
    let transport = Scripted {
        requests: RefCell::new(Vec::new()),
        responses: RefCell::new(VecDeque::new()),
    };
    let error = plan_clone_issue(
        &client(&transport),
        "OPS-1",
        CloneIssueInput {
            replacements: vec![ReplaceRule {
                search: String::new(),
                replacement: "x".to_owned(),
            }],
            ..CloneIssueInput::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, jira_ops::error::ErrorCode::SchemaViolation);
    assert!(transport.requests.borrow().is_empty());
}
