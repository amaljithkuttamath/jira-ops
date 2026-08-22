use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use jira_ops::client::{
    HttpMethod, HttpRequest, HttpResponse, JiraClient, JiraTransport, TransportFailure,
};
use jira_ops::commands::epic::{
    apply_epic_add, apply_epic_remove, epic_jql, plan_epic_add, plan_epic_remove,
    validate_epic_membership, validate_epic_remove,
};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::error::ErrorCode;
use jira_ops::model::{EpicMembershipInput, EpicRemoveInput};
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
fn epic_membership_is_unique_and_bounded_to_fifty() {
    let input = EpicMembershipInput {
        issue_keys: (1..=51).map(|n| format!("OPS-{n}")).collect(),
        notify_users: true,
    };
    assert_eq!(
        validate_epic_membership(&input).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
    let duplicate = EpicMembershipInput {
        issue_keys: vec!["OPS-2".into(), "OPS-2".into()],
        notify_users: true,
    };
    assert_eq!(
        validate_epic_membership(&duplicate).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
}

#[test]
fn epic_remove_requires_exact_epic_and_issue_set() {
    let input = EpicRemoveInput {
        issue_keys: vec!["OPS-2".into(), "OPS-3".into()],
        confirm_epic: "OPS-1".into(),
        confirm_issue_keys: vec!["OPS-2".into()],
        notify_users: true,
    };
    assert_eq!(
        validate_epic_remove("OPS-1", &input).unwrap_err().code,
        ErrorCode::DestructiveConfirmationRequired
    );
}

#[test]
fn epic_membership_lifecycle_uses_exact_agile_contracts() {
    let transport = Scripted {
        requests: RefCell::new(vec![]),
        responses: RefCell::new(VecDeque::from([
            HttpResponse {
                status: 204,
                headers: BTreeMap::new(),
                body: vec![],
            },
            HttpResponse {
                status: 204,
                headers: BTreeMap::new(),
                body: vec![],
            },
        ])),
    };
    let add_input = EpicMembershipInput {
        issue_keys: vec!["OPS-2".into(), "OPS-3".into()],
        notify_users: false,
    };
    let add_plan = plan_epic_add("OPS-1", add_input.clone()).unwrap();
    let added = apply_epic_add(
        &client(&transport),
        "OPS-1",
        add_input.notify_users,
        add_plan,
    )
    .unwrap();
    assert_eq!(added.issue_keys, vec!["OPS-2", "OPS-3"]);

    let remove_input = EpicRemoveInput {
        issue_keys: vec!["OPS-2".into(), "OPS-3".into()],
        confirm_epic: "OPS-1".into(),
        confirm_issue_keys: vec!["OPS-3".into(), "OPS-2".into()],
        notify_users: true,
    };
    let remove_plan = plan_epic_remove("OPS-1", remove_input.clone()).unwrap();
    let removed = apply_epic_remove(
        &client(&transport),
        "OPS-1",
        remove_input.notify_users,
        remove_plan,
    )
    .unwrap();
    assert_eq!(removed.issue_keys, vec!["OPS-2", "OPS-3"]);

    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert!(requests[0].url.path().ends_with("/epic/OPS-1/issue"));
    assert!(requests[1].url.path().ends_with("/epic/none/issue"));
    assert_eq!(requests[0].url.query(), Some("notifyUsers=false"));
    assert_eq!(requests[1].url.query(), Some("notifyUsers=true"));
}

#[test]
fn epic_jql_is_project_bound_and_rejects_bad_keys() {
    assert_eq!(
        epic_jql("OPS", None).unwrap(),
        "project = OPS AND issuetype = Epic"
    );
    assert_eq!(
        epic_jql("OPS", Some("status = Open")).unwrap(),
        "project = OPS AND issuetype = Epic AND (status = Open)"
    );
    assert_eq!(
        epic_jql("ops", None).unwrap_err().code,
        ErrorCode::SchemaViolation
    );
}
