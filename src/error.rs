use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitClass {
    Success,
    Input,
    LocalState,
    Auth,
    RemoteRejected,
    RemoteTransient,
    Network,
    MutationOutcome,
    Internal,
}

impl ExitClass {
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Input => 2,
            Self::LocalState => 3,
            Self::Auth => 4,
            Self::RemoteRejected => 5,
            Self::RemoteTransient => 6,
            Self::Network => 7,
            Self::MutationOutcome => 8,
            Self::Internal => 70,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    InvalidJson,
    SchemaViolation,
    InvalidCursor,
    DestructiveConfirmationRequired,
    ConfigMissing,
    ConfigConflict,
    KeyringUnavailable,
    LocalStatePartial,
    AuthMissing,
    AuthInvalid,
    ScopeMissing,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    RemoteRejected,
    RemoteUnavailable,
    Timeout,
    ConnectionFailed,
    ResponseInvalid,
    ResponseTooLarge,
    UnsupportedJiraCapability,
    MutationOutcomeUnknown,
    MutationResponseInvalid,
    MutationPartial,
    InvalidState,
    Internal,
}

impl ErrorCode {
    pub const fn exit_class(self) -> ExitClass {
        match self {
            Self::InvalidInput
            | Self::InvalidJson
            | Self::SchemaViolation
            | Self::InvalidCursor
            | Self::DestructiveConfirmationRequired => ExitClass::Input,
            Self::ConfigMissing
            | Self::ConfigConflict
            | Self::KeyringUnavailable
            | Self::LocalStatePartial => ExitClass::LocalState,
            Self::AuthMissing | Self::AuthInvalid | Self::ScopeMissing | Self::Forbidden => {
                ExitClass::Auth
            }
            Self::NotFound
            | Self::Conflict
            | Self::RemoteRejected
            | Self::ResponseTooLarge
            | Self::UnsupportedJiraCapability
            | Self::InvalidState => ExitClass::RemoteRejected,
            Self::RateLimited | Self::RemoteUnavailable | Self::ResponseInvalid => {
                ExitClass::RemoteTransient
            }
            Self::Timeout | Self::ConnectionFailed => ExitClass::Network,
            Self::MutationOutcomeUnknown
            | Self::MutationResponseInvalid
            | Self::MutationPartial => ExitClass::MutationOutcome,
            Self::Internal => ExitClass::Internal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySafety {
    Safe,
    Unsafe,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    NotApplied,
    Applied,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_outcome: Option<OperationOutcome>,
    pub retry_safety: RetrySafety,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>, retry_safety: RetrySafety) -> Self {
        Self {
            code,
            message: message.into(),
            operation_outcome: None,
            retry_safety,
            status: None,
            retry_after_ms: None,
            rate_limit_reason: None,
            details: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ExitClass};

    #[test]
    fn every_error_code_has_the_normative_exit_class() {
        let cases = [
            (ErrorCode::InvalidInput, ExitClass::Input),
            (ErrorCode::InvalidJson, ExitClass::Input),
            (ErrorCode::SchemaViolation, ExitClass::Input),
            (ErrorCode::InvalidCursor, ExitClass::Input),
            (ErrorCode::DestructiveConfirmationRequired, ExitClass::Input),
            (ErrorCode::ConfigMissing, ExitClass::LocalState),
            (ErrorCode::ConfigConflict, ExitClass::LocalState),
            (ErrorCode::KeyringUnavailable, ExitClass::LocalState),
            (ErrorCode::LocalStatePartial, ExitClass::LocalState),
            (ErrorCode::AuthMissing, ExitClass::Auth),
            (ErrorCode::AuthInvalid, ExitClass::Auth),
            (ErrorCode::ScopeMissing, ExitClass::Auth),
            (ErrorCode::Forbidden, ExitClass::Auth),
            (ErrorCode::NotFound, ExitClass::RemoteRejected),
            (ErrorCode::Conflict, ExitClass::RemoteRejected),
            (ErrorCode::RemoteRejected, ExitClass::RemoteRejected),
            (ErrorCode::ResponseTooLarge, ExitClass::RemoteRejected),
            (
                ErrorCode::UnsupportedJiraCapability,
                ExitClass::RemoteRejected,
            ),
            (ErrorCode::InvalidState, ExitClass::RemoteRejected),
            (ErrorCode::RateLimited, ExitClass::RemoteTransient),
            (ErrorCode::RemoteUnavailable, ExitClass::RemoteTransient),
            (ErrorCode::ResponseInvalid, ExitClass::RemoteTransient),
            (ErrorCode::Timeout, ExitClass::Network),
            (ErrorCode::ConnectionFailed, ExitClass::Network),
            (
                ErrorCode::MutationOutcomeUnknown,
                ExitClass::MutationOutcome,
            ),
            (
                ErrorCode::MutationResponseInvalid,
                ExitClass::MutationOutcome,
            ),
            (ErrorCode::MutationPartial, ExitClass::MutationOutcome),
            (ErrorCode::Internal, ExitClass::Internal),
        ];

        for (code, expected) in cases {
            assert_eq!(code.exit_class(), expected, "{code:?}");
        }
    }

    #[test]
    fn destructive_confirmation_is_input_class() {
        assert_eq!(
            ErrorCode::DestructiveConfirmationRequired.exit_class(),
            ExitClass::Input
        );
    }
}
