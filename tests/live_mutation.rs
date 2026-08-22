use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use jira_ops::client::{JiraClient, UreqTransport};
use jira_ops::commands::clone::{CloneIssueInput, apply_clone_issue, plan_clone_issue};
use jira_ops::commands::destructive::{apply_delete_issue, plan_delete_issue};
use jira_ops::commands::epic::{
    apply_epic_add, apply_epic_remove, plan_epic_add, plan_epic_remove,
};
use jira_ops::commands::issue::{apply_create_issue, plan_create_issue};
use jira_ops::commands::remote_link::{apply_remote_link_add, plan_remote_link_add};
use jira_ops::commands::worklog::{apply_worklog_add, plan_worklog_add};
use jira_ops::config::{CredentialSource, ResolvedCredential};
use jira_ops::model::{
    CreateIssueInput, DeleteIssueInput, EpicMembershipInput, EpicRemoveInput, EstimateAdjustment,
    RemoteLinkInput, WorklogWriteInput,
};
use secrecy::SecretString;
use serde_json::json;
use url::Url;
use uuid::Uuid;

#[test]
#[ignore = "requires dedicated Jira Cloud mutation sandbox credentials"]
fn parity_mutations_touch_only_resources_created_by_this_test() {
    assert_eq!(
        env("JIRA_OPS_LIVE_MUTATION_ACK"),
        "TEST_OWNED_RESOURCES_ONLY"
    );
    let client = client();
    let prefix = format!("jira-ops-live-{}", Uuid::new_v4());
    let created = RefCell::new(Vec::<String>::new());
    let result = catch_unwind(AssertUnwindSafe(|| {
        let issue = create_issue(
            &client,
            &prefix,
            &env("JIRA_OPS_LIVE_MUTATION_ISSUE_TYPE_ID"),
        );
        created.borrow_mut().push(issue.clone());

        let epic = create_issue(
            &client,
            &format!("{prefix}-epic"),
            &env("JIRA_OPS_LIVE_MUTATION_EPIC_ISSUE_TYPE_ID"),
        );
        created.borrow_mut().push(epic.clone());

        let epic_add_input = EpicMembershipInput {
            issue_keys: vec![issue.clone()],
            notify_users: false,
        };
        let epic_add = plan_epic_add(&epic, epic_add_input.clone()).unwrap();
        apply_epic_add(&client, &epic, epic_add_input.notify_users, epic_add).unwrap();
        let epic_remove_input = EpicRemoveInput {
            issue_keys: vec![issue.clone()],
            confirm_epic: epic.clone(),
            confirm_issue_keys: vec![issue.clone()],
            notify_users: false,
        };
        let epic_remove = plan_epic_remove(&epic, epic_remove_input.clone()).unwrap();
        apply_epic_remove(&client, &epic, epic_remove_input.notify_users, epic_remove).unwrap();

        let remote = plan_remote_link_add(
            &issue,
            RemoteLinkInput {
                url: Url::parse("https://example.invalid/jira-ops-live").unwrap(),
                title: prefix.clone(),
                relationship: Some("validated by".into()),
            },
        )
        .unwrap();
        apply_remote_link_add(&client, &issue, remote).unwrap();

        let worklog_input = WorklogWriteInput {
            time_spent: "1m".into(),
            started: None,
            comment: None,
            adjustment: EstimateAdjustment::Leave,
            notify_users: false,
        };
        let worklog = plan_worklog_add(&issue, worklog_input).unwrap();
        apply_worklog_add(&client, &issue, &EstimateAdjustment::Leave, false, worklog).unwrap();

        let cloned = apply_clone_issue(
            &client,
            plan_clone_issue(
                &client,
                &issue,
                CloneIssueInput {
                    summary: Some(format!("{prefix}-clone")),
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap()
        .issue
        .key;
        created.borrow_mut().push(cloned);
    }));

    for issue in created.borrow().iter().rev() {
        let plan = plan_delete_issue(
            issue,
            DeleteIssueInput {
                confirm_issue: issue.clone(),
                cascade: true,
            },
        )
        .unwrap();
        apply_delete_issue(&client, issue, plan).expect("cleanup test-owned issue");
    }
    if let Err(panic) = result {
        resume_unwind(panic);
    }
}

fn create_issue(client: &JiraClient<UreqTransport>, prefix: &str, issue_type_id: &str) -> String {
    let plan = plan_create_issue(
        client,
        CreateIssueInput {
            project_key: env("JIRA_OPS_LIVE_MUTATION_PROJECT"),
            issue_type_id: issue_type_id.into(),
            fields: BTreeMap::from([("summary".into(), json!(prefix))]),
        },
    )
    .unwrap();
    apply_create_issue(client, plan).unwrap().issue.key
}

fn client() -> JiraClient<UreqTransport> {
    JiraClient::new(
        UreqTransport,
        ResolvedCredential {
            site: Url::parse(&env("JIRA_OPS_LIVE_MUTATION_SITE")).unwrap(),
            cloud_id: Uuid::parse_str(&env("JIRA_OPS_LIVE_MUTATION_CLOUD_ID")).unwrap(),
            email: env("JIRA_OPS_LIVE_MUTATION_EMAIL"),
            account_id: None,
            token: SecretString::from(env("JIRA_OPS_LIVE_MUTATION_API_TOKEN")),
            source: CredentialSource::Environment,
        },
        Duration::from_secs(30),
    )
}
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("required live-test variable {name} is missing"))
}
