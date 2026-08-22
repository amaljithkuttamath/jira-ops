use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::clone::CloneIssueInput;
use crate::commands::settings::{ConfigPatch, ConfigUnsetInput};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AssignmentInput, CommentInput, CreateIssueInput, DeleteIssueInput, EpicMembershipInput,
    EpicRemoveInput, LinkInput, ProjectCreateInput, RemoteLinkInput, RemoveLinkInput,
    RemoveRemoteLinkInput, SprintAddInput, SprintCloseInput, TransitionInput, UpdateIssueInput,
    WatcherInput, WorklogDeleteInput, WorklogWriteInput,
};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEffect {
    Read,
    LocalWrite,
    JiraWrite,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    Idempotent,
    NonIdempotent,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct PositionalSpec {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub value_type: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub repeated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct FlagSpec {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub value_type: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultValue>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(untagged)]
pub enum DefaultValue {
    Boolean(bool),
    Unsigned(u64),
}

#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub effect: CommandEffect,
    pub idempotency: Idempotency,
    pub positionals: &'static [PositionalSpec],
    pub flags: &'static [FlagSpec],
    pub errors: &'static [ErrorCode],
    pub example_argv: &'static [&'static str],
    pub paginated: bool,
}

const NO_POSITIONALS: &[PositionalSpec] = &[];
const COMMAND_PATH: &[PositionalSpec] = &[PositionalSpec {
    name: "COMMAND",
    value_type: "string",
    required: false,
    repeated: true,
    description: None,
    pattern: None,
}];
const ISSUE_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "ISSUE",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: None,
}];
const PROJECT_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "PROJECT",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: None,
}];
const SHELL_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "SHELL",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: Some(r"^(bash|zsh|fish|power-shell|elvish)$"),
}];
const MUTATION_ISSUE_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "ISSUE",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: Some(r"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"),
}];
const LINK_ID_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "LINK_ID",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: Some(r"^[1-9][0-9]*$"),
}];
const ISSUE_REMOTE_LINK_POSITIONALS: &[PositionalSpec] = &[
    MUTATION_ISSUE_POSITIONAL[0],
    PositionalSpec {
        name: "REMOTE_LINK_ID",
        value_type: "string",
        required: true,
        repeated: false,
        description: None,
        pattern: Some(r"^[1-9][0-9]*$"),
    },
];
const ISSUE_WORKLOG_POSITIONALS: &[PositionalSpec] = &[
    MUTATION_ISSUE_POSITIONAL[0],
    PositionalSpec {
        name: "WORKLOG_ID",
        value_type: "string",
        required: true,
        repeated: false,
        description: None,
        pattern: Some(r"^[1-9][0-9]*$"),
    },
];
const EPIC_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "EPIC",
    value_type: "string",
    required: true,
    repeated: false,
    description: None,
    pattern: Some(r"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"),
}];
const SPRINT_POSITIONAL: &[PositionalSpec] = &[PositionalSpec {
    name: "SPRINT_ID",
    value_type: "integer",
    required: true,
    repeated: false,
    description: None,
    pattern: Some(r"^[1-9][0-9]*$"),
}];

const NO_FLAGS: &[FlagSpec] = &[];
const SCHEMA_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--all",
    value_type: "boolean",
    required: false,
    default: Some(DefaultValue::Boolean(false)),
}];
const LOGIN_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--site",
        value_type: "string",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--email",
        value_type: "string",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--token-stdin",
        value_type: "boolean",
        required: true,
        default: None,
    },
];
const PAGE_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--limit",
        value_type: "integer",
        required: false,
        default: Some(DefaultValue::Unsigned(20)),
    },
    FlagSpec {
        name: "--cursor",
        value_type: "string",
        required: false,
        default: None,
    },
];
const FIELD_LIST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--query",
        value_type: "string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const USER_SEARCH_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--query",
        value_type: "string",
        required: true,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const BOARD_LIST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--project",
        value_type: "string",
        required: false,
        default: None,
    },
    FlagSpec {
        name: "--type",
        value_type: "string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const RELEASE_LIST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--status",
        value_type: "string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const ISSUE_GET_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--fields",
    value_type: "field_list",
    required: false,
    default: None,
}];
const ISSUE_SEARCH_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--jql",
        value_type: "string",
        required: true,
        default: None,
    },
    ISSUE_GET_FLAGS[0],
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const EPIC_LIST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--project",
        value_type: "string",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--jql",
        value_type: "string",
        required: false,
        default: None,
    },
    FlagSpec {
        name: "--fields",
        value_type: "comma_separated_string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const SPRINT_LIST_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--board",
        value_type: "integer",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--state",
        value_type: "string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const SPRINT_ISSUES_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--fields",
        value_type: "comma_separated_string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const CREATE_META_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--project",
        value_type: "string",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--issue-type",
        value_type: "string",
        required: false,
        default: None,
    },
    PAGE_FLAGS[0],
    PAGE_FLAGS[1],
];
const MUTATION_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "--input",
        value_type: "stdin",
        required: true,
        default: None,
    },
    FlagSpec {
        name: "--apply",
        value_type: "boolean",
        required: false,
        default: Some(DefaultValue::Boolean(false)),
    },
];
const LOCAL_INPUT_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--input",
    value_type: "stdin",
    required: true,
    default: None,
}];
const MAN_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--output-dir",
    value_type: "directory",
    required: true,
    default: None,
}];
const PROJECT_TEMPLATE_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--type",
    value_type: "string",
    required: false,
    default: None,
}];

const INPUT_ERRORS: &[ErrorCode] = &[ErrorCode::InvalidInput, ErrorCode::Internal];
const STATUS_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::ConfigConflict,
    ErrorCode::LocalStatePartial,
    ErrorCode::Internal,
];
const LOCAL_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::ConfigConflict,
    ErrorCode::KeyringUnavailable,
    ErrorCode::LocalStatePartial,
    ErrorCode::AuthInvalid,
    ErrorCode::ConnectionFailed,
    ErrorCode::Timeout,
    ErrorCode::ResponseInvalid,
    ErrorCode::Internal,
];
const REMOTE_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::InvalidCursor,
    ErrorCode::ConfigMissing,
    ErrorCode::ConfigConflict,
    ErrorCode::KeyringUnavailable,
    ErrorCode::AuthMissing,
    ErrorCode::AuthInvalid,
    ErrorCode::ScopeMissing,
    ErrorCode::Forbidden,
    ErrorCode::NotFound,
    ErrorCode::RateLimited,
    ErrorCode::RemoteRejected,
    ErrorCode::RemoteUnavailable,
    ErrorCode::Timeout,
    ErrorCode::ConnectionFailed,
    ErrorCode::ResponseInvalid,
    ErrorCode::ResponseTooLarge,
    ErrorCode::Internal,
];
const MUTATION_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::InvalidJson,
    ErrorCode::SchemaViolation,
    ErrorCode::ConfigMissing,
    ErrorCode::ConfigConflict,
    ErrorCode::KeyringUnavailable,
    ErrorCode::LocalStatePartial,
    ErrorCode::AuthMissing,
    ErrorCode::AuthInvalid,
    ErrorCode::ScopeMissing,
    ErrorCode::Forbidden,
    ErrorCode::NotFound,
    ErrorCode::Conflict,
    ErrorCode::RateLimited,
    ErrorCode::RemoteRejected,
    ErrorCode::RemoteUnavailable,
    ErrorCode::Timeout,
    ErrorCode::ConnectionFailed,
    ErrorCode::ResponseInvalid,
    ErrorCode::ResponseTooLarge,
    ErrorCode::MutationOutcomeUnknown,
    ErrorCode::MutationResponseInvalid,
    ErrorCode::Internal,
];
const DESTRUCTIVE_ERRORS: &[ErrorCode] = &[
    ErrorCode::DestructiveConfirmationRequired,
    ErrorCode::InvalidInput,
    ErrorCode::InvalidJson,
    ErrorCode::SchemaViolation,
    ErrorCode::ConfigMissing,
    ErrorCode::ConfigConflict,
    ErrorCode::KeyringUnavailable,
    ErrorCode::LocalStatePartial,
    ErrorCode::AuthMissing,
    ErrorCode::AuthInvalid,
    ErrorCode::ScopeMissing,
    ErrorCode::Forbidden,
    ErrorCode::NotFound,
    ErrorCode::Conflict,
    ErrorCode::RateLimited,
    ErrorCode::RemoteRejected,
    ErrorCode::RemoteUnavailable,
    ErrorCode::Timeout,
    ErrorCode::ConnectionFailed,
    ErrorCode::ResponseInvalid,
    ErrorCode::ResponseTooLarge,
    ErrorCode::MutationOutcomeUnknown,
    ErrorCode::MutationResponseInvalid,
    ErrorCode::Internal,
];
const COMMENT_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::InvalidJson,
    ErrorCode::SchemaViolation,
    ErrorCode::ConfigMissing,
    ErrorCode::ConfigConflict,
    ErrorCode::KeyringUnavailable,
    ErrorCode::LocalStatePartial,
    ErrorCode::AuthMissing,
    ErrorCode::AuthInvalid,
    ErrorCode::ScopeMissing,
    ErrorCode::Forbidden,
    ErrorCode::NotFound,
    ErrorCode::Conflict,
    ErrorCode::RateLimited,
    ErrorCode::RemoteRejected,
    ErrorCode::RemoteUnavailable,
    ErrorCode::Timeout,
    ErrorCode::ConnectionFailed,
    ErrorCode::ResponseInvalid,
    ErrorCode::ResponseTooLarge,
    ErrorCode::UnsupportedJiraCapability,
    ErrorCode::MutationOutcomeUnknown,
    ErrorCode::MutationResponseInvalid,
    ErrorCode::Internal,
];
const CONFIG_ERRORS: &[ErrorCode] = &[
    ErrorCode::InvalidInput,
    ErrorCode::InvalidJson,
    ErrorCode::SchemaViolation,
    ErrorCode::ConfigMissing,
    ErrorCode::LocalStatePartial,
    ErrorCode::Internal,
];

static COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        name: "version",
        summary: "Show CLI and contract versions",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: INPUT_ERRORS,
        example_argv: &["version"],
        paginated: false,
    },
    CommandSpec {
        name: "schema",
        summary: "Discover the command contract",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: COMMAND_PATH,
        flags: SCHEMA_FLAGS,
        errors: INPUT_ERRORS,
        example_argv: &["schema", "issue", "get"],
        paginated: false,
    },
    CommandSpec {
        name: "config.get",
        summary: "Get saved non-secret defaults",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: CONFIG_ERRORS,
        example_argv: &["config", "get"],
        paginated: false,
    },
    CommandSpec {
        name: "config.set",
        summary: "Set saved non-secret defaults",
        effect: CommandEffect::LocalWrite,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: LOCAL_INPUT_FLAGS,
        errors: CONFIG_ERRORS,
        example_argv: &["config", "set", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "config.unset",
        summary: "Unset saved non-secret defaults",
        effect: CommandEffect::LocalWrite,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: LOCAL_INPUT_FLAGS,
        errors: CONFIG_ERRORS,
        example_argv: &["config", "unset", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "url.issue",
        summary: "Return a canonical issue browse URL",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_POSITIONAL,
        flags: NO_FLAGS,
        errors: CONFIG_ERRORS,
        example_argv: &["url", "issue", "ACCL-1"],
        paginated: false,
    },
    CommandSpec {
        name: "url.project",
        summary: "Return a canonical project browse URL",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: PROJECT_POSITIONAL,
        flags: NO_FLAGS,
        errors: CONFIG_ERRORS,
        example_argv: &["url", "project", "ACCL"],
        paginated: false,
    },
    CommandSpec {
        name: "completion",
        summary: "Generate shell completion text",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: SHELL_POSITIONAL,
        flags: NO_FLAGS,
        errors: INPUT_ERRORS,
        example_argv: &["completion", "bash"],
        paginated: false,
    },
    CommandSpec {
        name: "man",
        summary: "Generate man pages into an empty directory",
        effect: CommandEffect::LocalWrite,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: MAN_FLAGS,
        errors: INPUT_ERRORS,
        example_argv: &["man", "--output-dir", "jira-ops-man"],
        paginated: false,
    },
    CommandSpec {
        name: "server.info",
        summary: "Get Jira Cloud server information",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["server", "info"],
        paginated: false,
    },
    CommandSpec {
        name: "user.search",
        summary: "Search Jira users with privacy-trimmed output",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: USER_SEARCH_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["user", "search", "--query", "Agent"],
        paginated: true,
    },
    CommandSpec {
        name: "board.list",
        summary: "List Jira Software boards",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: BOARD_LIST_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["board", "list", "--project", "ACCL"],
        paginated: true,
    },
    CommandSpec {
        name: "release.list",
        summary: "List Jira project releases",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: PROJECT_POSITIONAL,
        flags: RELEASE_LIST_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["release", "list", "ACCL"],
        paginated: true,
    },
    CommandSpec {
        name: "auth.login",
        summary: "Validate and save a scoped token",
        effect: CommandEffect::LocalWrite,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: LOGIN_FLAGS,
        errors: LOCAL_ERRORS,
        example_argv: &[
            "auth",
            "login",
            "--site",
            "https://example.atlassian.net",
            "--email",
            "agent@example.com",
            "--token-stdin",
        ],
        paginated: false,
    },
    CommandSpec {
        name: "auth.status",
        summary: "Inspect local credential configuration",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: STATUS_ERRORS,
        example_argv: &["auth", "status"],
        paginated: false,
    },
    CommandSpec {
        name: "auth.logout",
        summary: "Remove saved local credentials",
        effect: CommandEffect::LocalWrite,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: LOCAL_ERRORS,
        example_argv: &["auth", "logout"],
        paginated: false,
    },
    CommandSpec {
        name: "me",
        summary: "Get the authenticated Jira user",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["me"],
        paginated: false,
    },
    CommandSpec {
        name: "project.list",
        summary: "List visible Jira projects",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: PAGE_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["project", "list", "--limit", "20"],
        paginated: true,
    },
    CommandSpec {
        name: "project.get",
        summary: "Get one Jira project",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: PROJECT_POSITIONAL,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["project", "get", "ACCL"],
        paginated: false,
    },
    CommandSpec {
        name: "project.templates",
        summary: "List local project templates",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: PROJECT_TEMPLATE_FLAGS,
        errors: INPUT_ERRORS,
        example_argv: &["project", "templates", "--type", "software"],
        paginated: false,
    },
    CommandSpec {
        name: "project.create",
        summary: "Plan project creation; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["project", "create", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "field.list",
        summary: "List and search Jira fields",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: FIELD_LIST_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["field", "list", "--query", "story"],
        paginated: true,
    },
    CommandSpec {
        name: "issue.get",
        summary: "Get one issue",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_POSITIONAL,
        flags: ISSUE_GET_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "get", "ACCL-1"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.search",
        summary: "Search issues with enhanced JQL",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: ISSUE_SEARCH_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "search", "--jql", "project = ACCL"],
        paginated: true,
    },
    CommandSpec {
        name: "issue.create-meta",
        summary: "Discover issue types or create fields",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: CREATE_META_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "create-meta", "--project", "ACCL"],
        paginated: true,
    },
    CommandSpec {
        name: "issue.create",
        summary: "Plan issue; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "create", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.clone",
        summary: "Plan a guarded issue clone; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "clone", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.delete",
        summary: "Plan confirmed issue deletion; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &["issue", "delete", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.update",
        summary: "Plan update; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "update", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.assign",
        summary: "Plan assignment; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "assign", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.link.types",
        summary: "List issue link types",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "link", "types"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.link.get",
        summary: "Get one projected issue link",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: LINK_ID_POSITIONAL,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "link", "get", "10000"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.link.add",
        summary: "Plan issue link; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "link", "add", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.link.remove",
        summary: "Plan confirmed issue link removal; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: LINK_ID_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &["issue", "link", "remove", "10000", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.remote-link.list",
        summary: "List projected remote issue links",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "remote-link", "list", "ACCL-1"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.remote-link.get",
        summary: "Get one projected remote issue link",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_REMOTE_LINK_POSITIONALS,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "remote-link", "get", "ACCL-1", "10000"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.remote-link.add",
        summary: "Plan a remote issue link; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "remote-link", "add", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.remote-link.remove",
        summary: "Plan confirmed remote-link removal; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: ISSUE_REMOTE_LINK_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &[
            "issue",
            "remote-link",
            "remove",
            "ACCL-1",
            "10000",
            "--input",
            "-",
        ],
        paginated: false,
    },
    CommandSpec {
        name: "issue.watcher.list",
        summary: "List projected issue watchers",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_POSITIONAL,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "watcher", "list", "ACCL-1"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.worklog.list",
        summary: "List projected worklogs",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: PAGE_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "worklog", "list", "ACCL-1"],
        paginated: true,
    },
    CommandSpec {
        name: "issue.worklog.add",
        summary: "Plan a worklog; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "worklog", "add", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.worklog.update",
        summary: "Plan a worklog update; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: ISSUE_WORKLOG_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &[
            "issue", "worklog", "update", "ACCL-1", "10000", "--input", "-",
        ],
        paginated: false,
    },
    CommandSpec {
        name: "issue.worklog.delete",
        summary: "Plan confirmed worklog deletion; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: ISSUE_WORKLOG_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &[
            "issue", "worklog", "delete", "ACCL-1", "10000", "--input", "-",
        ],
        paginated: false,
    },
    CommandSpec {
        name: "issue.watcher.add",
        summary: "Plan watcher add; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "watcher", "add", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.watcher.remove",
        summary: "Plan watcher removal; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "watcher", "remove", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.comments",
        summary: "List issue comments",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_POSITIONAL,
        flags: PAGE_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "comments", "ACCL-1"],
        paginated: true,
    },
    CommandSpec {
        name: "issue.comment",
        summary: "Plan comment; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: COMMENT_ERRORS,
        example_argv: &["issue", "comment", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.transitions",
        summary: "List available issue transitions",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: ISSUE_POSITIONAL,
        flags: NO_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["issue", "transitions", "ACCL-1"],
        paginated: false,
    },
    CommandSpec {
        name: "issue.transition",
        summary: "Plan transition; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: MUTATION_ISSUE_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["issue", "transition", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "epic.list",
        summary: "List project epics",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: EPIC_LIST_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["epic", "list", "--project", "ACCL"],
        paginated: true,
    },
    CommandSpec {
        name: "epic.create",
        summary: "Plan epic creation; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: NO_POSITIONALS,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["epic", "create", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "epic.add",
        summary: "Plan adding issues to an epic; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: EPIC_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["epic", "add", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "epic.remove",
        summary: "Plan confirmed removal from an epic; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: EPIC_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &["epic", "remove", "ACCL-1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "sprint.list",
        summary: "List board sprints",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: NO_POSITIONALS,
        flags: SPRINT_LIST_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["sprint", "list", "--board", "1"],
        paginated: true,
    },
    CommandSpec {
        name: "sprint.issues",
        summary: "List sprint issues",
        effect: CommandEffect::Read,
        idempotency: Idempotency::Idempotent,
        positionals: SPRINT_POSITIONAL,
        flags: SPRINT_ISSUES_FLAGS,
        errors: REMOTE_ERRORS,
        example_argv: &["sprint", "issues", "1"],
        paginated: true,
    },
    CommandSpec {
        name: "sprint.add",
        summary: "Plan moving issues into a sprint; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: SPRINT_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: MUTATION_ERRORS,
        example_argv: &["sprint", "add", "1", "--input", "-"],
        paginated: false,
    },
    CommandSpec {
        name: "sprint.close",
        summary: "Plan confirmed sprint closure; --apply writes",
        effect: CommandEffect::JiraWrite,
        idempotency: Idempotency::NonIdempotent,
        positionals: SPRINT_POSITIONAL,
        flags: MUTATION_FLAGS,
        errors: DESTRUCTIVE_ERRORS,
        example_argv: &["sprint", "close", "1", "--input", "-"],
        paginated: false,
    },
];

pub fn command_specs() -> &'static [CommandSpec] {
    COMMAND_SPECS
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SchemaData {
    Index(SchemaIndex),
    Operation(Box<OperationSchema>),
    All(FullSchema),
}

#[derive(Debug, Serialize)]
pub struct SchemaIndex {
    contract_version: &'static str,
    cli_version: &'static str,
    global_flags: Value,
    output: OutputCapability,
    commands: Vec<CommandIndex>,
}

#[derive(Debug, Serialize)]
struct OutputCapability {
    default: &'static str,
    formats: &'static [OutputFormatSpec],
    flags: &'static [&'static str],
}

#[derive(Debug, Serialize)]
struct OutputFormatSpec {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec_version: Option<&'static str>,
}

const OUTPUT_FORMATS: &[OutputFormatSpec] = &[
    OutputFormatSpec {
        name: "json",
        spec_version: None,
    },
    OutputFormatSpec {
        name: "toon",
        spec_version: Some("3.0"),
    },
];

#[derive(Debug, Serialize)]
struct CommandIndex {
    name: &'static str,
    effect: CommandEffect,
    summary: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FullSchema {
    contract_version: &'static str,
    cli_version: &'static str,
    global_flags: Value,
    commands: Vec<OperationSchema>,
}

#[derive(Debug, Serialize)]
pub struct OperationSchema {
    contract_version: &'static str,
    command: &'static str,
    summary: &'static str,
    effect: CommandEffect,
    idempotency: Idempotency,
    positionals: &'static [PositionalSpec],
    flags: &'static [FlagSpec],
    global_flags: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdin_schema: Option<Value>,
    success_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagination: Option<PaginationSpec>,
    errors: BTreeMap<String, u8>,
    example: ExampleSpec,
}

#[derive(Debug, Serialize)]
struct PaginationSpec {
    input: &'static str,
    output: &'static str,
    default_limit: u16,
    maximum_limit: u16,
}

#[derive(Debug, Serialize)]
struct ExampleSpec {
    argv: &'static [&'static str],
    stdin: Value,
}

pub fn schema_for(path: &[String], all: bool) -> Result<SchemaData, AppError> {
    if all {
        return Ok(SchemaData::All(FullSchema {
            contract_version: "1",
            cli_version: env!("CARGO_PKG_VERSION"),
            global_flags: global_flags_schema(),
            commands: command_specs().iter().map(operation_schema).collect(),
        }));
    }

    if path.is_empty() {
        return Ok(SchemaData::Index(SchemaIndex {
            contract_version: "1",
            cli_version: env!("CARGO_PKG_VERSION"),
            global_flags: global_flags_schema(),
            output: OutputCapability {
                default: "json",
                formats: OUTPUT_FORMATS,
                flags: &["-o", "--output"],
            },
            commands: command_specs()
                .iter()
                .map(|spec| CommandIndex {
                    name: spec.name,
                    effect: spec.effect,
                    summary: spec.summary,
                })
                .collect(),
        }));
    }

    let name = path.join(".");
    command_specs()
        .iter()
        .find(|spec| spec.name == name)
        .map(operation_schema)
        .map(Box::new)
        .map(SchemaData::Operation)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidInput,
                format!("unknown command schema {name}"),
                RetrySafety::Safe,
            )
        })
}

fn operation_schema(spec: &CommandSpec) -> OperationSchema {
    let mutation = mutation_kind(spec.name);
    OperationSchema {
        contract_version: "1",
        command: spec.name,
        summary: spec.summary,
        effect: spec.effect,
        idempotency: spec.idempotency,
        positionals: spec.positionals,
        flags: spec.flags,
        global_flags: global_flags_schema(),
        stdin_schema: mutation.map(mutation_stdin_schema),
        success_schema: match mutation {
            Some(MutationKind::ConfigSet | MutationKind::ConfigUnset) | None => {
                read_success_schema(spec.name)
            }
            Some(kind) => mutation_success_schema(kind),
        },
        pagination: spec.paginated.then_some(PaginationSpec {
            input: "--cursor",
            output: "meta.next_cursor",
            default_limit: 20,
            maximum_limit: 100,
        }),
        errors: spec
            .errors
            .iter()
            .map(|code| (error_code_name(*code), code.exit_class().code()))
            .collect(),
        example: ExampleSpec {
            argv: spec.example_argv,
            stdin: mutation.map_or(Value::Null, mutation_example),
        },
    }
}

fn global_flags_schema() -> Value {
    json!({
        "pretty":{
            "flags":["--pretty"],"type":"boolean","default":false,
            "conflicts_with":"output=toon"
        },
        "output":{
            "flags":["-o","--output"],"type":"string","enum":["json","toon"],
            "default":"json","conflicts_with":"pretty=true"
        },
        "timeout_ms":{
            "flags":["--timeout-ms"],"type":"integer","minimum":1000,
            "maximum":120000,"default":30000
        }
    })
}

fn error_code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .expect("error codes are serializable")
        .as_str()
        .expect("error codes serialize as strings")
        .to_owned()
}

#[derive(Clone, Copy)]
enum MutationKind {
    Create,
    Clone,
    Delete,
    ProjectCreate,
    Update,
    Comment,
    Transition,
    Assignment,
    LinkAdd,
    LinkRemove,
    RemoteLinkAdd,
    RemoteLinkRemove,
    WorklogAdd,
    WorklogUpdate,
    WorklogDelete,
    EpicCreate,
    EpicAdd,
    EpicRemove,
    SprintAdd,
    SprintClose,
    WatcherAdd,
    WatcherRemove,
    ConfigSet,
    ConfigUnset,
}

fn mutation_kind(name: &str) -> Option<MutationKind> {
    match name {
        "issue.create" => Some(MutationKind::Create),
        "issue.clone" => Some(MutationKind::Clone),
        "issue.delete" => Some(MutationKind::Delete),
        "project.create" => Some(MutationKind::ProjectCreate),
        "issue.update" => Some(MutationKind::Update),
        "issue.comment" => Some(MutationKind::Comment),
        "issue.transition" => Some(MutationKind::Transition),
        "issue.assign" => Some(MutationKind::Assignment),
        "issue.link.add" => Some(MutationKind::LinkAdd),
        "issue.link.remove" => Some(MutationKind::LinkRemove),
        "issue.remote-link.add" => Some(MutationKind::RemoteLinkAdd),
        "issue.remote-link.remove" => Some(MutationKind::RemoteLinkRemove),
        "issue.worklog.add" => Some(MutationKind::WorklogAdd),
        "issue.worklog.update" => Some(MutationKind::WorklogUpdate),
        "issue.worklog.delete" => Some(MutationKind::WorklogDelete),
        "epic.create" => Some(MutationKind::EpicCreate),
        "epic.add" => Some(MutationKind::EpicAdd),
        "epic.remove" => Some(MutationKind::EpicRemove),
        "sprint.add" => Some(MutationKind::SprintAdd),
        "sprint.close" => Some(MutationKind::SprintClose),
        "issue.watcher.add" => Some(MutationKind::WatcherAdd),
        "issue.watcher.remove" => Some(MutationKind::WatcherRemove),
        "config.set" => Some(MutationKind::ConfigSet),
        "config.unset" => Some(MutationKind::ConfigUnset),
        _ => None,
    }
}

fn mutation_stdin_schema(kind: MutationKind) -> Value {
    let schema = match kind {
        MutationKind::Create => schemars::schema_for!(CreateIssueInput),
        MutationKind::Clone => schemars::schema_for!(CloneIssueInput),
        MutationKind::Delete => schemars::schema_for!(DeleteIssueInput),
        MutationKind::ProjectCreate => schemars::schema_for!(ProjectCreateInput),
        MutationKind::Update => schemars::schema_for!(UpdateIssueInput),
        MutationKind::Comment => schemars::schema_for!(CommentInput),
        MutationKind::Transition => schemars::schema_for!(TransitionInput),
        MutationKind::Assignment => schemars::schema_for!(AssignmentInput),
        MutationKind::LinkAdd => schemars::schema_for!(LinkInput),
        MutationKind::LinkRemove => schemars::schema_for!(RemoveLinkInput),
        MutationKind::RemoteLinkAdd => schemars::schema_for!(RemoteLinkInput),
        MutationKind::RemoteLinkRemove => schemars::schema_for!(RemoveRemoteLinkInput),
        MutationKind::WorklogAdd | MutationKind::WorklogUpdate => {
            schemars::schema_for!(WorklogWriteInput)
        }
        MutationKind::WorklogDelete => schemars::schema_for!(WorklogDeleteInput),
        MutationKind::EpicCreate => schemars::schema_for!(CreateIssueInput),
        MutationKind::EpicAdd => schemars::schema_for!(EpicMembershipInput),
        MutationKind::EpicRemove => schemars::schema_for!(EpicRemoveInput),
        MutationKind::SprintAdd => schemars::schema_for!(SprintAddInput),
        MutationKind::SprintClose => schemars::schema_for!(SprintCloseInput),
        MutationKind::WatcherAdd | MutationKind::WatcherRemove => {
            schemars::schema_for!(WatcherInput)
        }
        MutationKind::ConfigSet => schemars::schema_for!(ConfigPatch),
        MutationKind::ConfigUnset => schemars::schema_for!(ConfigUnsetInput),
    };
    let mut value = serde_json::to_value(schema).expect("derived mutation schema is serializable");
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
    }
    match kind {
        MutationKind::Create => {
            if let Some(fields) = value
                .pointer_mut("/properties/fields")
                .and_then(Value::as_object_mut)
            {
                fields.remove("additionalProperties");
                fields.insert("required".to_owned(), json!(["summary"]));
                fields.insert(
                    "not".to_owned(),
                    json!({"anyOf":[
                        {"required":["project"]},
                        {"required":["issuetype"]}
                    ]}),
                );
            }
            set_nonblank_string(&mut value, "project_key");
            set_nonblank_string(&mut value, "issue_type_id");
        }
        MutationKind::Clone => {}
        MutationKind::Delete => set_strict_issue_key(&mut value, "confirm_issue"),
        MutationKind::ProjectCreate => {
            value["required"] = json!([
                "key",
                "name",
                "project_type_key",
                "project_template_key",
                "lead_account_id"
            ]);
            if let Some(key) = value
                .pointer_mut("/properties/key")
                .and_then(Value::as_object_mut)
            {
                key.insert("pattern".to_owned(), json!(r"^[A-Z][A-Z0-9_]{1,9}$"));
            }
            if let Some(name) = value
                .pointer_mut("/properties/name")
                .and_then(Value::as_object_mut)
            {
                name.insert("minLength".to_owned(), json!(1));
                name.insert("maxLength".to_owned(), json!(80));
            }
            if let Some(assignee) = value
                .pointer_mut("/properties/assignee_type")
                .and_then(Value::as_object_mut)
            {
                assignee.insert(
                    "enum".to_owned(),
                    json!(["UNASSIGNED", "PROJECT_LEAD", null]),
                );
            }
            set_nonblank_string(&mut value, "project_type_key");
            set_nonblank_string(&mut value, "project_template_key");
            set_nonblank_string(&mut value, "lead_account_id");
        }
        MutationKind::Update => {
            if let Some(set) = value
                .pointer_mut("/properties/set")
                .and_then(Value::as_object_mut)
            {
                set.insert("minProperties".to_owned(), json!(1));
            }
        }
        MutationKind::Comment => set_string_min_length(&mut value, "body"),
        MutationKind::Transition => set_nonblank_string(&mut value, "transition_id"),
        MutationKind::Assignment => {
            set_strict_issue_key(&mut value, "issue_key");
            set_bounded_identifier(&mut value, "account_id", 1024);
            value["properties"]["account_id"]["type"] = json!(["string", "null"]);
        }
        MutationKind::LinkAdd => {
            set_strict_issue_key(&mut value, "inward_issue");
            set_strict_issue_key(&mut value, "outward_issue");
            set_bounded_identifier(&mut value, "type_name", 255);
        }
        MutationKind::LinkRemove => set_bounded_identifier(&mut value, "confirm_link_id", 64),
        MutationKind::RemoteLinkAdd => {
            set_bounded_identifier(&mut value, "title", 255);
            set_bounded_identifier(&mut value, "relationship", 255);
            value["properties"]["url"]["format"] = json!("uri");
            value["properties"]["url"]["pattern"] = json!(r"^https://");
        }
        MutationKind::RemoteLinkRemove => {
            set_bounded_identifier(&mut value, "confirm_remote_link_id", 64)
        }
        MutationKind::WorklogAdd | MutationKind::WorklogUpdate => {
            value["properties"]["time_spent"]["pattern"] =
                json!(r"^[1-9][0-9]*[wdhm]( [1-9][0-9]*[wdhm])*$");
            value["properties"]["started"]["format"] = json!("date-time");
        }
        MutationKind::WorklogDelete => set_bounded_identifier(&mut value, "confirm_worklog_id", 64),
        MutationKind::EpicCreate => {
            set_nonblank_string(&mut value, "project_key");
            set_nonblank_string(&mut value, "issue_type_id");
        }
        MutationKind::EpicAdd => {}
        MutationKind::EpicRemove => set_strict_issue_key(&mut value, "confirm_epic"),
        MutationKind::SprintAdd => {}
        MutationKind::SprintClose => {
            value["properties"]["complete_date"]["format"] = json!("date-time");
        }
        MutationKind::WatcherAdd | MutationKind::WatcherRemove => {
            set_strict_issue_key(&mut value, "issue_key");
            set_bounded_identifier(&mut value, "account_id", 1024);
        }
        MutationKind::ConfigSet => {
            value["anyOf"] = json!([
                {"required":["default_project"]},
                {"required":["default_board"]}
            ]);
        }
        MutationKind::ConfigUnset => {
            value["anyOf"] = json!([
                {"properties":{"default_project":{"const":true}},"required":["default_project"]},
                {"properties":{"default_board":{"const":true}},"required":["default_board"]}
            ]);
        }
    }
    value
}

fn read_success_schema(command: &str) -> Value {
    match command {
        "version" => plain_success_schema(strict_object(
            &["cli_version", "contract_version"],
            json!({"cli_version":string(),"contract_version":string()}),
        )),
        "schema" => plain_success_schema(schema_command_data_schema()),
        "config.get" | "config.set" | "config.unset" => {
            plain_success_schema(config_defaults_schema())
        }
        "url.issue" | "url.project" => plain_success_schema(strict_object(
            &["url"],
            json!({"url":{"type":"string","format":"uri","pattern":"^https://"}}),
        )),
        "completion" => plain_success_schema(string()),
        "man" => plain_success_schema(strict_object(&["files"], json!({"files":array(string())}))),
        "server.info" => plain_success_schema(strict_object(
            &[
                "version",
                "deployment_type",
                "build_number",
                "build_date",
                "server_time",
            ],
            json!({
                "version":string(),"deployment_type":string(),"build_number":integer(),
                "build_date":string(),"server_time":string()
            }),
        )),
        "user.search" => page_success_schema(array(user_schema()), false),
        "board.list" => page_success_schema(array(board_schema()), false),
        "release.list" => page_success_schema(array(release_schema()), false),
        "auth.login" => plain_success_with_warnings(strict_object(
            &[
                "site",
                "cloud_id",
                "email",
                "account_id",
                "display_name",
                "credential_source",
            ],
            json!({
                "site":string(),"cloud_id":string(),"email":string(),"account_id":string(),
                "display_name":string(),"credential_source":{"const":"keyring"}
            }),
        )),
        "auth.status" => plain_success_schema(strict_object(
            &["configured", "identity_source", "credential_source"],
            json!({
                "configured":{"type":"boolean"},
                "identity_source":{"enum":["saved","environment",null]},
                "credential_source":{"enum":["keyring_configured","environment","none"]},
                "site":string(),"cloud_id":string(),"email":string()
            }),
        )),
        "auth.logout" => plain_success_schema(strict_object(
            &[
                "removed_config",
                "removed_keyring",
                "environment_credentials_active",
            ],
            json!({
                "removed_config":{"type":"boolean"},"removed_keyring":{"type":"boolean"},
                "environment_credentials_active":{"type":"boolean"}
            }),
        )),
        "me" => plain_success_schema(account_schema()),
        "project.list" => page_success_schema(array(project_item_schema()), true),
        "project.get" => plain_success_schema(strict_object(
            &["id", "key", "name", "type", "style"],
            json!({"id":string(),"key":string(),"name":string(),"type":string(),"style":string()}),
        )),
        "project.templates" => plain_success_schema(array(strict_object(
            &["name", "project_type_key", "project_template_key"],
            json!({"name":string(),"project_type_key":string(),"project_template_key":string()}),
        ))),
        "field.list" => page_success_schema(array(field_item_schema()), true),
        "issue.get" => plain_success_schema(issue_projection_schema()),
        "issue.search" | "epic.list" | "sprint.issues" => {
            page_success_schema(array(issue_projection_schema()), true)
        }
        "issue.create-meta" => create_meta_success_schema(),
        "issue.comments" => page_success_schema(array(comment_schema()), true),
        "issue.transitions" => count_success_schema(array(transition_schema())),
        "issue.link.types" => count_success_schema(array(link_type_schema())),
        "issue.link.get" => plain_success_schema(link_schema()),
        "issue.remote-link.list" => count_success_schema(array(remote_link_schema())),
        "issue.remote-link.get" => plain_success_schema(remote_link_schema()),
        "issue.worklog.list" => page_success_schema(array(worklog_schema()), false),
        "issue.watcher.list" => count_success_schema(array(watcher_schema())),
        "sprint.list" => page_success_schema(array(sprint_schema()), false),
        _ => unreachable!("public success schema missing for {command}"),
    }
}

fn string() -> Value {
    json!({"type":"string"})
}

fn nullable_string() -> Value {
    json!({"type":["string","null"]})
}

fn integer() -> Value {
    json!({"type":"integer"})
}

fn array(items: Value) -> Value {
    json!({"type":"array","items":items})
}

fn strict_object(required: &[&str], properties: Value) -> Value {
    json!({
        "type":"object","required":required,"properties":properties,
        "additionalProperties":false
    })
}

fn success_envelope(data: Value, meta: Option<Value>, warnings: bool) -> Value {
    let mut properties = serde_json::Map::from_iter([("data".to_owned(), data)]);
    if let Some(meta) = meta {
        properties.insert("meta".to_owned(), meta);
    }
    if warnings {
        properties.insert("warnings".to_owned(), warnings_schema());
    }
    strict_object(&["data"], Value::Object(properties))
}

fn plain_success_schema(data: Value) -> Value {
    success_envelope(data, None, false)
}

fn plain_success_with_warnings(data: Value) -> Value {
    success_envelope(data, None, true)
}

fn warnings_schema() -> Value {
    array(strict_object(
        &["code", "message"],
        json!({"code":string(),"message":string()}),
    ))
}

fn page_meta_schema() -> Value {
    strict_object(
        &["count", "next_cursor"],
        json!({
            "count":{"type":"integer","minimum":0},
            "next_cursor":nullable_string()
        }),
    )
}

fn page_success_schema(data: Value, warnings: bool) -> Value {
    let mut schema = success_envelope(data, Some(page_meta_schema()), warnings);
    schema["required"] = json!(["data", "meta"]);
    schema
}

fn count_success_schema(data: Value) -> Value {
    let mut schema = success_envelope(
        data,
        Some(strict_object(
            &["count"],
            json!({"count":{"type":"integer","minimum":0}}),
        )),
        false,
    );
    schema["required"] = json!(["data", "meta"]);
    schema
}

fn schema_command_data_schema() -> Value {
    let index = strict_object(
        &[
            "contract_version",
            "cli_version",
            "global_flags",
            "output",
            "commands",
        ],
        json!({
            "contract_version":string(),"cli_version":string(),"global_flags":{"type":"object"},
            "output":{"type":"object"},"commands":{"type":"array"}
        }),
    );
    let operation = strict_object(
        &[
            "contract_version",
            "command",
            "summary",
            "effect",
            "idempotency",
            "positionals",
            "flags",
            "global_flags",
            "success_schema",
            "errors",
            "example",
        ],
        json!({
            "contract_version":string(),"command":string(),"summary":string(),"effect":string(),
            "idempotency":string(),"positionals":{"type":"array"},"flags":{"type":"array"},
            "global_flags":{"type":"object"},"stdin_schema":{"type":"object"},
            "success_schema":{"type":"object"},"pagination":{"type":"object"},
            "errors":{"type":"object"},"example":{"type":"object"}
        }),
    );
    let all = strict_object(
        &[
            "contract_version",
            "cli_version",
            "global_flags",
            "commands",
        ],
        json!({
            "contract_version":string(),"cli_version":string(),"global_flags":{"type":"object"},
            "commands":{"type":"array"}
        }),
    );
    json!({"type":"object","oneOf":[index,operation,all]})
}

fn config_defaults_schema() -> Value {
    strict_object(
        &["default_project", "default_board"],
        json!({
            "default_project":nullable_string(),
            "default_board":{"type":["integer","null"],"minimum":1}
        }),
    )
}

fn account_schema() -> Value {
    strict_object(
        &["account_id", "display_name", "active"],
        json!({
            "account_id":string(),"display_name":string(),"active":{"type":"boolean"},
            "email":string()
        }),
    )
}

fn user_schema() -> Value {
    strict_object(
        &["account_id", "display_name", "active", "account_type"],
        json!({
            "account_id":string(),"display_name":string(),"active":{"type":"boolean"},
            "account_type":string()
        }),
    )
}

fn board_schema() -> Value {
    strict_object(
        &["id", "name", "type", "project_key"],
        json!({"id":integer(),"name":string(),"type":string(),"project_key":nullable_string()}),
    )
}

fn release_schema() -> Value {
    strict_object(
        &[
            "id",
            "name",
            "archived",
            "released",
            "start_date",
            "release_date",
        ],
        json!({
            "id":string(),"name":string(),"archived":{"type":"boolean"},
            "released":{"type":"boolean"},"start_date":nullable_string(),
            "release_date":nullable_string()
        }),
    )
}

fn project_item_schema() -> Value {
    strict_object(
        &["id", "key", "name", "project_type", "simplified"],
        json!({
            "id":string(),"key":string(),"name":string(),"project_type":string(),
            "simplified":{"type":"boolean"}
        }),
    )
}

fn field_schema() -> Value {
    strict_object(
        &["type", "items", "custom", "system"],
        json!({
            "type":nullable_string(),"items":nullable_string(),"custom":nullable_string(),
            "system":nullable_string()
        }),
    )
}

fn field_item_schema() -> Value {
    strict_object(
        &["id", "name", "custom"],
        json!({
            "id":string(),"name":string(),"custom":{"type":"boolean"},"schema":field_schema()
        }),
    )
}

fn issue_assignee_schema() -> Value {
    strict_object(
        &["account_id", "display_name"],
        json!({"account_id":string(),"display_name":string()}),
    )
}

fn issue_projection_schema() -> Value {
    strict_object(
        &["key"],
        json!({
            "key":string(),"summary":nullable_string(),"status":nullable_string(),
            "assignee":{"oneOf":[issue_assignee_schema(),{"type":"null"}]},
            "updated":nullable_string(),"description":nullable_string(),
            "fields":{"type":"object"}
        }),
    )
}

fn field_metadata_schema() -> Value {
    strict_object(
        &[
            "id",
            "name",
            "required",
            "operations",
            "schema",
            "input_kind",
            "supported_selector_members",
            "allowed_values_complete",
        ],
        json!({
            "id":string(),"name":string(),"required":{"type":"boolean"},
            "operations":array(string()),"schema":field_schema(),
            "input_kind":{"enum":["string","number","boolean","array","object","adf_text","passthrough"]},
            "supported_selector_members":array(string()),"allowed_values":{"type":"array"},
            "allowed_values_complete":{"type":"boolean"}
        }),
    )
}

fn create_meta_success_schema() -> Value {
    let issue_type = strict_object(
        &["id", "name", "subtask"],
        json!({"id":string(),"name":string(),"subtask":{"type":"boolean"}}),
    );
    let data = array(json!({"oneOf":[issue_type,field_metadata_schema()]}));
    let meta = strict_object(
        &["kind", "project", "count", "next_cursor"],
        json!({
            "kind":{"enum":["issue_types","fields"]},"project":string(),
            "issue_type_id":string(),"count":{"type":"integer","minimum":0},
            "next_cursor":nullable_string()
        }),
    );
    let mut schema = success_envelope(data, Some(meta), true);
    schema["required"] = json!(["data", "meta"]);
    schema
}

fn comment_schema() -> Value {
    strict_object(
        &["id", "author", "body", "created", "updated"],
        json!({
            "id":string(),"author":issue_assignee_schema(),"body":string(),
            "created":string(),"updated":string()
        }),
    )
}

fn transition_schema() -> Value {
    strict_object(
        &["id", "name", "to", "fields"],
        json!({
            "id":string(),"name":string(),
            "to":strict_object(&["id","name"],json!({"id":string(),"name":string()})),
            "fields":array(field_metadata_schema())
        }),
    )
}

fn link_type_schema() -> Value {
    strict_object(
        &["id", "name", "inward", "outward"],
        json!({"id":string(),"name":string(),"inward":string(),"outward":string()}),
    )
}

fn link_schema() -> Value {
    let linked_issue = strict_object(&["key"], json!({"key":string()}));
    strict_object(
        &["id", "type", "inward_issue", "outward_issue"],
        json!({
            "id":string(),"type":link_type_schema(),"inward_issue":linked_issue,
            "outward_issue":strict_object(&["key"],json!({"key":string()}))
        }),
    )
}

fn watcher_schema() -> Value {
    strict_object(
        &["account_id", "display_name", "active"],
        json!({"account_id":string(),"display_name":string(),"active":{"type":"boolean"}}),
    )
}

fn worklog_schema() -> Value {
    strict_object(
        &[
            "id",
            "author",
            "started",
            "time_spent",
            "time_spent_seconds",
            "comment",
            "updated",
        ],
        json!({
            "id":string(),"author":account_schema(),"started":string(),"time_spent":string(),
            "time_spent_seconds":integer(),"comment":nullable_string(),"updated":nullable_string()
        }),
    )
}

fn sprint_schema() -> Value {
    strict_object(
        &[
            "id",
            "name",
            "state",
            "start_date",
            "end_date",
            "complete_date",
            "goal",
        ],
        json!({
            "id":integer(),"name":string(),"state":{"enum":["future","active","closed"]},
            "start_date":nullable_string(),"end_date":nullable_string(),
            "complete_date":nullable_string(),"goal":nullable_string()
        }),
    )
}

fn remote_link_schema() -> Value {
    json!({
        "type":"object",
        "required":["id","global_id","title","url","relationship"],
        "properties":{
            "id":{"type":"integer","minimum":1},
            "global_id":{"type":["string","null"]},
            "title":{"type":"string"},
            "url":{"type":"string","format":"uri","pattern":"^https://"},
            "relationship":{"type":["string","null"]}
        },
        "additionalProperties":false
    })
}

fn mutation_success_schema(kind: MutationKind) -> Value {
    let operation = match kind {
        MutationKind::Create => "issue.create",
        MutationKind::Clone => "issue.clone",
        MutationKind::Delete => "issue.delete",
        MutationKind::ProjectCreate => "project.create",
        MutationKind::Update => "issue.update",
        MutationKind::Comment => "issue.comment",
        MutationKind::Transition => "issue.transition",
        MutationKind::Assignment => "issue.assign",
        MutationKind::LinkAdd => "issue.link.add",
        MutationKind::LinkRemove => "issue.link.remove",
        MutationKind::RemoteLinkAdd => "issue.remote-link.add",
        MutationKind::RemoteLinkRemove => "issue.remote-link.remove",
        MutationKind::WorklogAdd => "issue.worklog.add",
        MutationKind::WorklogUpdate => "issue.worklog.update",
        MutationKind::WorklogDelete => "issue.worklog.delete",
        MutationKind::EpicCreate => "epic.create",
        MutationKind::EpicAdd => "epic.add",
        MutationKind::EpicRemove => "epic.remove",
        MutationKind::SprintAdd => "sprint.add",
        MutationKind::SprintClose => "sprint.close",
        MutationKind::WatcherAdd => "issue.watcher.add",
        MutationKind::WatcherRemove => "issue.watcher.remove",
        MutationKind::ConfigSet | MutationKind::ConfigUnset => unreachable!(),
    };
    if matches!(kind, MutationKind::ProjectCreate) {
        let planned = strict_object(
            &["operation", "method", "path", "body"],
            json!({
                "operation":{"const":"project.create"},"method":{"const":"POST"},
                "path":{"const":"/rest/api/3/project"},"body":{"type":"object"}
            }),
        );
        let applied = strict_object(
            &["operation", "outcome", "project"],
            json!({
                "operation":{"const":"project.create"},"outcome":{"const":"applied"},
                "project":strict_object(&["id","key"],json!({"id":string(),"key":string()}))
            }),
        );
        return plain_success_schema(json!({
            "type":"object","oneOf":[planned,applied]
        }));
    }
    let dry_run = strict_object(
        &["operation", "applied", "target", "changes", "validation"],
        json!({
            "operation":{"const":operation},"applied":{"const":false},
            "target":{"type":"object"},"changes":{"type":"object"},
            "validation":strict_object(
                &["local","metadata"],
                json!({
                    "local":{"const":"passed"},
                    "metadata":{"enum":["passed","partial","not_applicable"]}
                })
            )
        }),
    );
    let issue = match kind {
        MutationKind::Create | MutationKind::Clone | MutationKind::EpicCreate => strict_object(
            &["id", "key", "url"],
            json!({"id":string(),"key":string(),"url":string()}),
        ),
        MutationKind::Delete
        | MutationKind::Update
        | MutationKind::Comment
        | MutationKind::Transition
        | MutationKind::Assignment => strict_object(&["key"], json!({"key":string()})),
        MutationKind::LinkAdd | MutationKind::LinkRemove => Value::Null,
        MutationKind::RemoteLinkAdd | MutationKind::RemoteLinkRemove => {
            strict_object(&["key"], json!({"key":string()}))
        }
        MutationKind::WorklogAdd | MutationKind::WorklogUpdate | MutationKind::WorklogDelete => {
            strict_object(&["key"], json!({"key":string()}))
        }
        MutationKind::EpicAdd | MutationKind::EpicRemove => Value::Null,
        MutationKind::SprintAdd | MutationKind::SprintClose => Value::Null,
        MutationKind::WatcherAdd | MutationKind::WatcherRemove => {
            strict_object(&["key"], json!({"key":string()}))
        }
        MutationKind::ProjectCreate => unreachable!(),
        MutationKind::ConfigSet | MutationKind::ConfigUnset => unreachable!(),
    };
    let mut properties = serde_json::Map::from_iter([
        ("operation".to_owned(), json!({"const":operation})),
        ("applied".to_owned(), json!({"const":true})),
    ]);
    let mut required = vec!["operation", "applied"];
    if !matches!(
        kind,
        MutationKind::LinkAdd
            | MutationKind::LinkRemove
            | MutationKind::EpicAdd
            | MutationKind::EpicRemove
            | MutationKind::SprintAdd
            | MutationKind::SprintClose
    ) {
        properties.insert("issue".to_owned(), issue);
        required.push("issue");
    }
    if matches!(kind, MutationKind::Comment) {
        properties.insert(
            "comment".to_owned(),
            strict_object(&["id"], json!({"id":string()})),
        );
        required.push("comment");
    }
    if matches!(kind, MutationKind::Assignment) {
        properties.insert(
            "assignment".to_owned(),
            strict_object(&["account_id"], json!({"account_id":nullable_string()})),
        );
        required.push("assignment");
    }
    if matches!(kind, MutationKind::LinkAdd) {
        properties.insert(
            "link".to_owned(),
            strict_object(
                &["inward_issue", "outward_issue", "type_name"],
                json!({
                    "inward_issue":string(),"outward_issue":string(),"type_name":string()
                }),
            ),
        );
        required.push("link");
    }
    if matches!(kind, MutationKind::LinkRemove) {
        properties.insert("link_id".to_owned(), json!({"type":"string"}));
        required.push("link_id");
    }
    if matches!(
        kind,
        MutationKind::RemoteLinkAdd | MutationKind::RemoteLinkRemove
    ) {
        properties.insert(
            "remote_link_id".to_owned(),
            json!({"type":"integer","minimum":1}),
        );
        required.push("remote_link_id");
    }
    if matches!(
        kind,
        MutationKind::WorklogAdd | MutationKind::WorklogUpdate | MutationKind::WorklogDelete
    ) {
        properties.insert("worklog_id".to_owned(), json!({"type":"string"}));
        required.push("worklog_id");
    }
    if matches!(kind, MutationKind::EpicAdd | MutationKind::EpicRemove) {
        properties.insert("epic".to_owned(), json!({"type":"string"}));
        properties.insert(
            "issue_keys".to_owned(),
            json!({"type":"array","items":{"type":"string"},"minItems":1,"maxItems":50}),
        );
        required.extend(["epic", "issue_keys"]);
    }
    if matches!(kind, MutationKind::SprintAdd | MutationKind::SprintClose) {
        properties.insert(
            "sprint_id".to_owned(),
            json!({"type":"integer","minimum":1}),
        );
        required.push("sprint_id");
        if matches!(kind, MutationKind::SprintAdd) {
            properties.insert(
                "issue_keys".to_owned(),
                json!({"type":"array","items":{"type":"string"},"minItems":1,"maxItems":50}),
            );
            required.push("issue_keys");
        }
    }
    if matches!(kind, MutationKind::WatcherAdd | MutationKind::WatcherRemove) {
        properties.insert(
            "watcher".to_owned(),
            strict_object(&["account_id"], json!({"account_id":string()})),
        );
        required.push("watcher");
    }
    let applied = strict_object(&required, Value::Object(properties));
    plain_success_schema(json!({"type":"object","oneOf":[dry_run,applied]}))
}

fn set_string_min_length(schema: &mut Value, property: &str) {
    if let Some(property) = schema
        .pointer_mut(&format!("/properties/{property}"))
        .and_then(Value::as_object_mut)
    {
        property.insert("minLength".to_owned(), json!(1));
    }
}

fn set_nonblank_string(schema: &mut Value, property: &str) {
    if let Some(property) = schema
        .pointer_mut(&format!("/properties/{property}"))
        .and_then(Value::as_object_mut)
    {
        property.insert("pattern".to_owned(), json!(r"\S"));
    }
}

fn set_strict_issue_key(schema: &mut Value, property: &str) {
    if let Some(property) = schema
        .pointer_mut(&format!("/properties/{property}"))
        .and_then(Value::as_object_mut)
    {
        property.insert(
            "pattern".to_owned(),
            json!(r"^[A-Z][A-Z0-9_]*-[1-9][0-9]*$"),
        );
    }
}

fn set_bounded_identifier(schema: &mut Value, property: &str, max_bytes: usize) {
    if let Some(property) = schema
        .pointer_mut(&format!("/properties/{property}"))
        .and_then(Value::as_object_mut)
    {
        property.insert(
            "pattern".to_owned(),
            json!(r"^(?=.*\S)[^\x00-\x1F\x7F-\x9F]+$"),
        );
        property.insert("maxLength".to_owned(), json!(max_bytes));
        property.insert("x-maxBytes".to_owned(), json!(max_bytes));
    }
}

fn mutation_example(kind: MutationKind) -> Value {
    match kind {
        MutationKind::Create => json!({
            "project_key": "A",
            "issue_type_id": "1",
            "fields": {"summary": "x"}
        }),
        MutationKind::Clone => json!({}),
        MutationKind::Delete => json!({"confirm_issue":"ACCL-1","cascade":false}),
        MutationKind::ProjectCreate => Value::Null,
        MutationKind::Update => json!({"set": {"summary": "x"}}),
        MutationKind::Comment => json!({"body": "x"}),
        MutationKind::Transition => json!({"transition_id": "1", "fields": {}}),
        MutationKind::Assignment => json!({"issue_key":"ACCL-1", "account_id": null}),
        MutationKind::LinkAdd => json!({
            "inward_issue":"ACCL-1",
            "outward_issue":"ACCL-2",
            "type_name":"Blocks"
        }),
        MutationKind::LinkRemove => json!({"confirm_link_id":"10000"}),
        MutationKind::RemoteLinkAdd => {
            json!({"url":"https://tracker.example/1","title":"Ticket 1"})
        }
        MutationKind::RemoteLinkRemove => json!({"confirm_remote_link_id":"10000"}),
        MutationKind::WorklogAdd | MutationKind::WorklogUpdate => {
            json!({"time_spent":"1h","adjustment":{"mode":"auto"},"notify_users":true})
        }
        MutationKind::WorklogDelete => {
            json!({"confirm_worklog_id":"10000","adjustment":{"mode":"leave"},"notify_users":true})
        }
        MutationKind::EpicCreate => {
            json!({"project_key":"ACCL","issue_type_id":"10000","fields":{"summary":"Epic"}})
        }
        MutationKind::EpicAdd => json!({"issue_keys":["ACCL-2"],"notify_users":true}),
        MutationKind::EpicRemove => {
            json!({"issue_keys":["ACCL-2"],"confirm_epic":"ACCL-1","confirm_issue_keys":["ACCL-2"],"notify_users":true})
        }
        MutationKind::SprintAdd => json!({"issue_keys":["ACCL-1"]}),
        MutationKind::SprintClose => json!({"confirm_sprint_id":1}),
        MutationKind::WatcherAdd | MutationKind::WatcherRemove => {
            json!({"issue_key":"ACCL-1", "account_id":"abc"})
        }
        MutationKind::ConfigSet => json!({"default_project":"ACCL"}),
        MutationKind::ConfigUnset => json!({"default_project":true}),
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}
