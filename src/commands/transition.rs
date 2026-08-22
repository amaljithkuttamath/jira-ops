use crate::client::{JiraClient, JiraTransport, WriteEndpoint};
use crate::commands::issue::{
    compile_fields, encoded_path, mutation_path_with_notify, normalize_field_metadata,
    normalize_metadata_map, validate_issue, validate_mutation_issue_key,
};
use crate::commands::{mutation_not_applied, schema_violation};
use crate::content::compile_content;
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedIssueKey, AppliedIssueMutation, CountMeta, JiraTransitions, MutationPlan,
    TransitionInput, TransitionItem, TransitionStatus, ValidationLevel,
};
use crate::output::SuccessEnvelope;
use serde_json::json;

pub fn validate_transition_input(issue: &str, input: &TransitionInput) -> Result<(), AppError> {
    validate_mutation_issue_key(issue)?;
    if input.transition_id.trim().is_empty() {
        return Err(schema_violation("transition_id must not be blank"));
    }
    Ok(())
}

pub fn plan_transition_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    input: TransitionInput,
) -> Result<MutationPlan, AppError> {
    validate_transition_input(issue, &input)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "transitions"])
        .map_err(mutation_not_applied)?;
    let response: JiraTransitions = client
        .get_json(&format!("{path}?expand=transitions.fields"))
        .map_err(mutation_not_applied)?;
    let mut matches = response
        .transitions
        .into_iter()
        .filter(|transition| transition.id == input.transition_id);
    let transition = matches.next().ok_or_else(|| {
        mutation_not_applied(AppError::new(
            ErrorCode::NotFound,
            "the requested transition is not currently available",
            RetrySafety::Safe,
        ))
    })?;
    if matches.next().is_some() {
        return Err(mutation_not_applied(AppError::new(
            ErrorCode::ResponseInvalid,
            "Jira returned duplicate transition identifiers",
            RetrySafety::Safe,
        )));
    }

    let (wire_fields, metadata) = match transition.fields {
        None => {
            if !input.fields.is_empty() {
                return Err(schema_violation(
                    "transition field metadata was omitted; supplied fields cannot be validated",
                ));
            }
            (input.fields.clone(), ValidationLevel::Partial)
        }
        Some(fields) => {
            let metadata = normalize_metadata_map(fields)?;
            compile_fields(&metadata, &input.fields, true, true, true)?
        }
    };
    let mut changes = json!({"transition_id": input.transition_id, "fields": input.fields});
    let mut wire = json!({"transition":{"id": input.transition_id}, "fields": wire_fields});
    if let Some(comment) = input.comment {
        changes
            .as_object_mut()
            .expect("transition changes are an object")
            .insert(
                "comment".to_owned(),
                serde_json::to_value(&comment)
                    .map_err(|_| schema_violation("transition comment is invalid"))?,
            );
        wire.as_object_mut()
            .expect("transition wire payload is an object")
            .insert(
                "update".to_owned(),
                json!({"comment":[{"add":{"body":compile_content(&comment)?}}]}),
            );
    }
    if let Some(notify_users) = input.notify_users {
        changes
            .as_object_mut()
            .expect("transition changes are an object")
            .insert("notify_users".to_owned(), json!(notify_users));
    }
    Ok(MutationPlan::dry_run(
        "issue.transition",
        json!({"issue": issue}),
        changes,
        metadata,
        wire,
    ))
}

pub fn apply_transition_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    issue_key: &str,
    plan: MutationPlan,
) -> Result<AppliedIssueMutation, AppError> {
    validate_mutation_issue_key(issue_key)?;
    let path = mutation_path_with_notify(
        encoded_path(&["rest", "api", "3", "issue", issue_key, "transitions"])?,
        plan.changes
            .get("notify_users")
            .and_then(serde_json::Value::as_bool),
    );
    client.jira_write(
        WriteEndpoint::TransitionIssue,
        &path,
        &plan.into_wire_payload(),
    )?;
    Ok(AppliedIssueMutation {
        operation: "issue.transition",
        applied: true,
        issue: AppliedIssueKey {
            key: issue_key.to_owned(),
        },
    })
}

pub fn issue_transitions<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
) -> Result<SuccessEnvelope<Vec<TransitionItem>, CountMeta>, AppError> {
    validate_issue(issue)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "transitions"])?;
    let response: JiraTransitions =
        client.get_json(&format!("{path}?expand=transitions.fields"))?;
    let data = response
        .transitions
        .into_iter()
        .map(|transition| {
            let fields = transition
                .fields
                .unwrap_or_default()
                .into_iter()
                .map(|(id, field)| normalize_field_metadata(field, Some(&id)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TransitionItem {
                id: transition.id,
                name: transition.name,
                to: TransitionStatus {
                    id: transition.to.id,
                    name: transition.to.name,
                },
                fields,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let count = data.len();
    Ok(SuccessEnvelope::with_meta(data, CountMeta { count }))
}
