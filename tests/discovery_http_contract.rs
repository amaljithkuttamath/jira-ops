use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use jira_ops::client::{HttpRequest, HttpResponse, JiraClient, JiraTransport, TransportFailure};
use jira_ops::commands::board::board_list;
use jira_ops::commands::release::release_list;
use jira_ops::commands::server::server_info;
use jira_ops::commands::user::user_search;
use jira_ops::config::{CredentialSource, ResolvedCredential};
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

struct CaptureTransport {
    requests: RefCell<Vec<HttpRequest>>,
    response: HttpResponse,
}

impl JiraTransport for &CaptureTransport {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportFailure> {
        self.requests.borrow_mut().push(request);
        Ok(self.response.clone())
    }
}

fn client(transport: &CaptureTransport) -> JiraClient<&CaptureTransport> {
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

fn transport(body: &str) -> CaptureTransport {
    CaptureTransport {
        requests: RefCell::new(Vec::new()),
        response: HttpResponse {
            status: 200,
            headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            body: body.as_bytes().to_vec(),
        },
    }
}

#[test]
fn server_info_projects_stable_fields() {
    let transport = transport(
        r#"{"baseUrl":"https://example.atlassian.net","version":"1001.0.0","deploymentType":"Cloud","buildNumber":10001,"buildDate":"2026-01-01","serverTime":"2026-08-20T12:00:00.000+0000","scmInfo":"private"}"#,
    );
    let result = server_info(&client(&transport)).unwrap();
    assert_eq!(result.data.version, "1001.0.0");
    assert_eq!(result.data.deployment_type, "Cloud");
    let requests = transport.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url.path(),
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/serverInfo"
    );
}

#[test]
fn user_search_is_bounded_and_privacy_trimmed() {
    let transport = transport(
        r#"[{"accountId":"abc","displayName":"Agent","active":true,"accountType":"atlassian","emailAddress":"private@example.com","avatarUrls":{"48x48":"private"}}]"#,
    );
    let result = user_search(&client(&transport), "Agent", 20, None).unwrap();
    assert_eq!(result.data[0].account_id, "abc");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private@example.com")
    );
    assert_eq!(
        transport.requests.borrow()[0].url.query(),
        Some("query=Agent&startAt=0&maxResults=20")
    );
}

#[test]
fn board_list_encodes_filters_and_projects_stable_fields() {
    let transport = transport(
        r#"{"maxResults":20,"startAt":0,"total":1,"isLast":true,"values":[{"id":7,"name":"Operations","type":"scrum","self":"private","location":{"projectKey":"OPS"}}]}"#,
    );
    let result = board_list(&client(&transport), Some("OPS"), Some("scrum"), 20, None).unwrap();
    assert_eq!(result.data[0].name, "Operations");
    assert_eq!(result.data[0].board_type, "scrum");
    assert_eq!(
        transport.requests.borrow()[0].url.query(),
        Some("projectKeyOrId=OPS&type=scrum&startAt=0&maxResults=20")
    );
}

#[test]
fn release_list_filters_status_and_projects_dates() {
    let transport = transport(
        r#"{"startAt":0,"maxResults":20,"total":1,"isLast":true,"values":[{"id":"10","name":"v1","archived":false,"released":true,"startDate":"2026-01-01","releaseDate":"2026-02-01","description":"ignored"}]}"#,
    );
    let result = release_list(&client(&transport), "OPS", Some("released"), 20, None).unwrap();
    assert_eq!(result.data[0].release_date.as_deref(), Some("2026-02-01"));
    let request = &transport.requests.borrow()[0];
    assert_eq!(
        request.url.path(),
        "/ex/jira/00000000-0000-0000-0000-000000000000/rest/api/3/project/OPS/version"
    );
    assert_eq!(
        request.url.query(),
        Some("status=released&startAt=0&maxResults=20")
    );
}
