pub mod assignment;
pub mod auth;
pub mod board;
pub mod clone;
pub mod comment;
pub mod destructive;
pub mod epic;
pub mod field;
pub mod issue;
pub mod link;
pub mod local_docs;
pub mod project;
pub mod release;
pub mod remote_link;
pub mod server;
pub mod settings;
pub mod sprint;
pub mod transition;
pub mod user;
pub mod watcher;
pub mod worklog;

use std::time::Duration;
use std::{collections::BTreeSet, ffi::OsString, io::Read};

use crate::client::{JiraClient, JiraTransport};
use crate::config::{
    ConfigStore, CredentialSource, CredentialStore, EnvironmentSource, resolve_credential,
};
use crate::error::{AppError, ErrorCode, OperationOutcome, RetrySafety};
use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

pub const MAX_MUTATION_INPUT_BYTES: usize = 1024 * 1024;
const DUPLICATE_KEY_MARKER: &str = "duplicate JSON object key";

pub fn authenticated_client<'a, T: JiraTransport>(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
    transport: &'a T,
    timeout: Duration,
) -> Result<JiraClient<&'a T>, AppError> {
    let credential = resolve_credential(environment, config, credentials)?;
    if credential.source == CredentialSource::Environment {
        let bound_cloud_id = auth::tenant_info(transport, &credential.site, timeout)?;
        if bound_cloud_id != credential.cloud_id {
            return Err(AppError::new(
                ErrorCode::ConfigConflict,
                "JIRA_SITE does not resolve to JIRA_CLOUD_ID",
                RetrySafety::Safe,
            ));
        }
    }
    Ok(JiraClient::new(transport, credential, timeout))
}

pub fn read_json_input<T: DeserializeOwned>(reader: &mut dyn Read) -> Result<T, AppError> {
    let mut bytes = Vec::with_capacity(MAX_MUTATION_INPUT_BYTES.min(8 * 1024));
    reader
        .take((MAX_MUTATION_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            mutation_input_error(ErrorCode::InvalidInput, "failed to read mutation input")
        })?;
    if bytes.len() > MAX_MUTATION_INPUT_BYTES {
        return Err(mutation_input_error(
            ErrorCode::InvalidInput,
            "mutation input exceeded the 1 MiB limit",
        ));
    }
    std::str::from_utf8(&bytes).map_err(|_| {
        mutation_input_error(ErrorCode::InvalidJson, "mutation input must be UTF-8 JSON")
    })?;

    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let unique = UniqueValue::deserialize(&mut deserializer).map_err(|error| {
        if error.to_string().contains(DUPLICATE_KEY_MARKER) {
            mutation_input_error(
                ErrorCode::SchemaViolation,
                "mutation input contains a duplicate object key",
            )
        } else {
            mutation_input_error(
                ErrorCode::InvalidJson,
                "mutation input must contain one JSON document",
            )
        }
    })?;
    deserializer.end().map_err(|_| {
        mutation_input_error(
            ErrorCode::InvalidJson,
            "mutation input must contain exactly one JSON document",
        )
    })?;
    if !unique.0.is_object() {
        return Err(mutation_input_error(
            ErrorCode::SchemaViolation,
            "mutation input must be a JSON object",
        ));
    }
    serde_json::from_value(unique.0).map_err(|_| {
        mutation_input_error(
            ErrorCode::SchemaViolation,
            "mutation input does not match the command schema",
        )
    })
}

pub fn reject_read_only_apply(
    environment: &impl EnvironmentSource,
    apply: bool,
) -> Result<(), AppError> {
    if apply && environment.value("JIRA_READ_ONLY") == Some(OsString::from("1")) {
        return Err(mutation_input_error(
            ErrorCode::ConfigConflict,
            "JIRA_READ_ONLY=1 forbids applying Jira mutations",
        ));
    }
    Ok(())
}

pub fn mutation_not_applied(mut error: AppError) -> AppError {
    error.operation_outcome = Some(OperationOutcome::NotApplied);
    error.retry_safety = RetrySafety::Safe;
    error
}

pub fn validate_confirmation(actual: &str, expected: &str, field: &str) -> Result<(), AppError> {
    if actual == expected {
        return Ok(());
    }
    Err(mutation_not_applied(AppError::new(
        ErrorCode::DestructiveConfirmationRequired,
        format!("{field} must exactly match the mutation target"),
        RetrySafety::Safe,
    )))
}

pub fn validate_confirmed_set(
    actual: &[String],
    expected: &[String],
    field: &str,
) -> Result<(), AppError> {
    let mut actual = actual.to_vec();
    let mut expected = expected.to_vec();
    actual.sort();
    expected.sort();
    if actual == expected {
        return Ok(());
    }
    Err(mutation_not_applied(AppError::new(
        ErrorCode::DestructiveConfirmationRequired,
        format!("{field} must exactly match the mutation target set"),
        RetrySafety::Safe,
    )))
}

pub fn schema_violation(message: impl Into<String>) -> AppError {
    mutation_input_error(ErrorCode::SchemaViolation, message)
}

fn mutation_input_error(code: ErrorCode, message: impl Into<String>) -> AppError {
    mutation_not_applied(AppError::new(code, message, RetrySafety::Safe))
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        UniqueValue::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen = BTreeSet::new();
        let mut values = Map::new();
        while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "{DUPLICATE_KEY_MARKER}: {key}"
                )));
            }
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}
