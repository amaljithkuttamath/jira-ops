use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::commands::local_docs::CompletionShell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Json,
    Toon,
}

#[derive(Debug, Parser)]
#[command(
    name = "jira-ops",
    disable_help_subcommand = true,
    disable_version_flag = true,
    subcommand_required = true
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub pretty: bool,

    #[arg(short = 'o', long, global = true, default_value = "json")]
    pub output: OutputFormat,

    #[arg(
        long,
        global = true,
        default_value_t = 30_000,
        value_parser = clap::value_parser!(u64).range(1_000..=120_000)
    )]
    pub timeout_ms: u64,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn parse_args<I>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = OsString>,
    {
        let cli = Self::try_parse_from(std::iter::once(OsString::from("jira-ops")).chain(args))?;
        if cli.pretty && cli.output == OutputFormat::Toon {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--pretty cannot be combined with --output toon",
            ));
        }
        Ok(cli)
    }
}

pub fn parse_error_output(args: &[OsString]) -> (OutputFormat, bool) {
    let pretty = requested_pretty(args);
    match requested_output(args) {
        Ok(OutputFormat::Toon) if pretty => (OutputFormat::Json, false),
        Ok(format) => (format, pretty),
        Err(()) => (OutputFormat::Json, false),
    }
}

fn requested_pretty(args: &[OsString]) -> bool {
    for arg in args {
        match arg.to_str() {
            Some("--") => break,
            Some("--pretty") => return true,
            _ => {}
        }
    }
    false
}

fn requested_output(args: &[OsString]) -> Result<OutputFormat, ()> {
    let mut selected = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args[index].to_str() else {
            index += 1;
            continue;
        };
        if value == "--" {
            break;
        }
        let request = match value {
            "-o" | "--output" => {
                index += 1;
                Some(args.get(index).and_then(|value| value.to_str()).ok_or(())?)
            }
            value if value.starts_with("--output=") => {
                value.split_once('=').map(|(_, value)| value)
            }
            value if value.starts_with("-o=") => value.split_once('=').map(|(_, value)| value),
            value if value.starts_with("-o") && value.len() > 2 => Some(&value[2..]),
            _ => None,
        };
        if let Some(request) = request {
            let format = match request {
                "json" => OutputFormat::Json,
                "toon" => OutputFormat::Toon,
                _ => return Err(()),
            };
            if selected.replace(format).is_some() {
                return Err(());
            }
        }
        index += 1;
    }
    Ok(selected.unwrap_or_default())
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Version,
    Schema(SchemaArgs),
    Config(ConfigArgs),
    Url(UrlArgs),
    Completion(CompletionArgs),
    Man(ManArgs),
    Server(ServerArgs),
    User(UserArgs),
    Board(BoardArgs),
    Release(ReleaseArgs),
    Auth(AuthArgs),
    Me,
    Project(ProjectArgs),
    Field(FieldArgs),
    Issue(IssueArgs),
    Epic(EpicArgs),
    Sprint(SprintArgs),
}

#[derive(Debug, Args)]
pub struct SprintArgs {
    #[command(subcommand)]
    pub command: SprintCommand,
}
#[derive(Debug, Subcommand)]
pub enum SprintCommand {
    List(SprintListArgs),
    Issues(SprintIssuesArgs),
    Add(SprintMutationArgs),
    Close(SprintMutationArgs),
}
#[derive(Debug, Args)]
pub struct SprintListArgs {
    #[arg(long)]
    pub board: u64,
    #[arg(long)]
    pub state: Option<String>,
    #[command(flatten)]
    pub page: PageArgs,
}
#[derive(Debug, Args)]
pub struct SprintIssuesArgs {
    #[arg(value_name = "SPRINT_ID")]
    pub sprint_id: u64,
    #[arg(long)]
    pub fields: Option<FieldsList>,
    #[command(flatten)]
    pub page: PageArgs,
}
#[derive(Debug, Args)]
pub struct SprintMutationArgs {
    #[arg(value_name = "SPRINT_ID")]
    pub sprint_id: u64,
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct EpicArgs {
    #[command(subcommand)]
    pub command: EpicCommand,
}

#[derive(Debug, Subcommand)]
pub enum EpicCommand {
    List(EpicListArgs),
    Create(IssueCreateArgs),
    Add(EpicMutationArgs),
    Remove(EpicMutationArgs),
}

#[derive(Debug, Args)]
pub struct EpicMutationArgs {
    #[arg(value_name = "EPIC")]
    pub epic: String,
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct EpicListArgs {
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub jql: Option<String>,
    #[arg(long)]
    pub fields: Option<FieldsList>,
    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub command: ServerCommand,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    Info,
}

#[derive(Debug, Args)]
pub struct UserArgs {
    #[command(subcommand)]
    pub command: UserCommand,
}

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    Search(UserSearchArgs),
}

#[derive(Debug, Args)]
pub struct UserSearchArgs {
    #[arg(long)]
    pub query: String,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct BoardArgs {
    #[command(subcommand)]
    pub command: BoardCommand,
}

#[derive(Debug, Subcommand)]
pub enum BoardCommand {
    List(BoardListArgs),
}

#[derive(Debug, Args)]
pub struct BoardListArgs {
    #[arg(long)]
    pub project: Option<String>,

    #[arg(long = "type")]
    pub board_type: Option<String>,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    pub command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
    List(ReleaseListArgs),
}

#[derive(Debug, Args)]
pub struct ReleaseListArgs {
    #[arg(value_name = "PROJECT")]
    pub project: String,

    #[arg(long)]
    pub status: Option<String>,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Get,
    Set(LocalInputArgs),
    Unset(LocalInputArgs),
}

#[derive(Debug, Args)]
pub struct LocalInputArgs {
    #[arg(long, value_name = "-", value_parser = parse_stdin_marker)]
    pub input: String,
}

#[derive(Debug, Args)]
pub struct UrlArgs {
    #[command(subcommand)]
    pub command: UrlCommand,
}

#[derive(Debug, Subcommand)]
pub enum UrlCommand {
    Issue(UrlIssueArgs),
    Project(UrlProjectArgs),
}

#[derive(Debug, Args)]
pub struct UrlIssueArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
}

#[derive(Debug, Args)]
pub struct UrlProjectArgs {
    #[arg(value_name = "PROJECT")]
    pub project: String,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: CompletionShell,
}

#[derive(Debug, Args)]
pub struct ManArgs {
    #[arg(long = "output-dir", value_name = "DIRECTORY")]
    pub output_dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    #[arg(long, conflicts_with = "path")]
    pub all: bool,

    #[arg(value_name = "COMMAND")]
    pub path: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    Login(AuthLoginArgs),
    Status,
    Logout,
}

#[derive(Debug, Args)]
pub struct AuthLoginArgs {
    #[arg(long)]
    pub site: String,

    #[arg(long)]
    pub email: String,

    #[arg(long, required = true, action = ArgAction::SetTrue)]
    pub token_stdin: bool,
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    List(PageArgs),
    Get(ProjectGetArgs),
    Templates(ProjectTemplatesArgs),
    Create(ProjectCreateArgs),
}

#[derive(Debug, Args)]
pub struct ProjectGetArgs {
    #[arg(value_name = "PROJECT")]
    pub project: String,
}

#[derive(Debug, Args)]
pub struct ProjectTemplatesArgs {
    #[arg(long = "type", value_name = "TYPE")]
    pub project_type: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProjectCreateArgs {
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct FieldArgs {
    #[command(subcommand)]
    pub command: FieldCommand,
}

#[derive(Debug, Subcommand)]
pub enum FieldCommand {
    List(FieldListArgs),
}

#[derive(Debug, Args)]
pub struct FieldListArgs {
    #[arg(long)]
    pub query: Option<String>,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct IssueArgs {
    #[command(subcommand)]
    pub command: IssueCommand,
}

#[derive(Debug, Subcommand)]
pub enum IssueCommand {
    Get(IssueGetArgs),
    Search(IssueSearchArgs),
    CreateMeta(IssueCreateMetaArgs),
    Create(IssueCreateArgs),
    Clone(IssueMutationArgs),
    Delete(IssueMutationArgs),
    Update(IssueMutationArgs),
    Comments(IssueCommentsArgs),
    Comment(IssueMutationArgs),
    Transitions(IssueTransitionsArgs),
    Transition(IssueMutationArgs),
    Assign(IssueCreateArgs),
    Link(IssueLinkArgs),
    RemoteLink(IssueRemoteLinkArgs),
    Worklog(IssueWorklogArgs),
    Watcher(IssueWatcherArgs),
}

#[derive(Debug, Args)]
pub struct IssueWorklogArgs {
    #[command(subcommand)]
    pub command: IssueWorklogCommand,
}

#[derive(Debug, Subcommand)]
pub enum IssueWorklogCommand {
    List(IssueWorklogListArgs),
    Add(IssueMutationArgs),
    Update(IssueWorklogMutationArgs),
    Delete(IssueWorklogMutationArgs),
}

#[derive(Debug, Args)]
pub struct IssueWorklogListArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct IssueWorklogMutationArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
    #[arg(value_name = "WORKLOG_ID")]
    pub worklog_id: String,
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct IssueRemoteLinkArgs {
    #[command(subcommand)]
    pub command: IssueRemoteLinkCommand,
}

#[derive(Debug, Subcommand)]
pub enum IssueRemoteLinkCommand {
    List(IssueWatcherListArgs),
    Get(IssueRemoteLinkGetArgs),
    Add(IssueMutationArgs),
    Remove(IssueRemoteLinkMutationArgs),
}

#[derive(Debug, Args)]
pub struct IssueRemoteLinkGetArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
    #[arg(value_name = "REMOTE_LINK_ID")]
    pub remote_link_id: String,
}

#[derive(Debug, Args)]
pub struct IssueRemoteLinkMutationArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
    #[arg(value_name = "REMOTE_LINK_ID")]
    pub remote_link_id: String,
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct IssueLinkArgs {
    #[command(subcommand)]
    pub command: IssueLinkCommand,
}

#[derive(Debug, Subcommand)]
pub enum IssueLinkCommand {
    Types,
    Get(IssueLinkGetArgs),
    Add(IssueCreateArgs),
    Remove(IssueLinkMutationArgs),
}

#[derive(Debug, Args)]
pub struct IssueLinkGetArgs {
    #[arg(value_name = "LINK_ID")]
    pub link_id: String,
}

#[derive(Debug, Args)]
pub struct IssueLinkMutationArgs {
    #[arg(value_name = "LINK_ID")]
    pub link_id: String,

    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct IssueWatcherArgs {
    #[command(subcommand)]
    pub command: IssueWatcherCommand,
}

#[derive(Debug, Subcommand)]
pub enum IssueWatcherCommand {
    List(IssueWatcherListArgs),
    Add(IssueCreateArgs),
    Remove(IssueCreateArgs),
}

#[derive(Debug, Args)]
pub struct IssueWatcherListArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
}

#[derive(Debug, Args)]
pub struct IssueGetArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,

    #[arg(long)]
    pub fields: Option<FieldsList>,
}

#[derive(Debug, Args)]
pub struct IssueSearchArgs {
    #[arg(long)]
    pub jql: String,

    #[arg(long)]
    pub fields: Option<FieldsList>,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct IssueCreateMetaArgs {
    #[arg(long)]
    pub project: String,

    #[arg(long)]
    pub issue_type: Option<String>,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct IssueCommentsArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,

    #[command(flatten)]
    pub page: PageArgs,
}

#[derive(Debug, Args)]
pub struct IssueTransitionsArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,
}

#[derive(Debug, Args)]
pub struct IssueCreateArgs {
    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct IssueMutationArgs {
    #[arg(value_name = "ISSUE")]
    pub issue: String,

    #[command(flatten)]
    pub mutation: MutationArgs,
}

#[derive(Debug, Args)]
pub struct MutationArgs {
    #[arg(long, value_name = "-", value_parser = parse_stdin_marker)]
    pub input: String,

    #[arg(long, action = ArgAction::SetTrue)]
    pub apply: bool,
}

fn parse_stdin_marker(value: &str) -> Result<String, String> {
    if value == "-" {
        Ok(value.to_owned())
    } else {
        Err("--input accepts only the exact stdin marker -".to_owned())
    }
}

pub fn is_mutation_invocation(args: &[OsString]) -> bool {
    let mut index = 0;
    skip_global_arguments(args, &mut index);
    let family = args.get(index).and_then(|value| value.to_str());
    index += 1;
    skip_global_arguments(args, &mut index);
    match family {
        Some("project") => args.get(index).and_then(|value| value.to_str()) == Some("create"),
        Some("issue") => match args.get(index).and_then(|value| value.to_str()) {
            Some(
                "create" | "clone" | "delete" | "update" | "comment" | "transition" | "assign",
            ) => true,
            Some("link") => {
                index += 1;
                skip_global_arguments(args, &mut index);
                matches!(
                    args.get(index).and_then(|value| value.to_str()),
                    Some("add" | "remove")
                )
            }
            Some("remote-link") => {
                index += 1;
                skip_global_arguments(args, &mut index);
                matches!(
                    args.get(index).and_then(|value| value.to_str()),
                    Some("add" | "remove")
                )
            }
            Some("worklog") => {
                index += 1;
                skip_global_arguments(args, &mut index);
                matches!(
                    args.get(index).and_then(|value| value.to_str()),
                    Some("add" | "update" | "delete")
                )
            }
            Some("watcher") => {
                index += 1;
                skip_global_arguments(args, &mut index);
                matches!(
                    args.get(index).and_then(|value| value.to_str()),
                    Some("add" | "remove")
                )
            }
            _ => false,
        },
        Some("epic") => matches!(
            args.get(index).and_then(|value| value.to_str()),
            Some("create" | "add" | "remove")
        ),
        Some("sprint") => matches!(
            args.get(index).and_then(|v| v.to_str()),
            Some("add" | "close")
        ),
        _ => false,
    }
}

fn skip_global_arguments(args: &[OsString], index: &mut usize) {
    while let Some(value) = args.get(*index).and_then(|value| value.to_str()) {
        match value {
            "--pretty" => *index += 1,
            "-o" | "--output" => *index += 2,
            value
                if value.starts_with("--output=")
                    || value.starts_with("-o=")
                    || (value.starts_with("-o") && value.len() > 2) =>
            {
                *index += 1
            }
            "--timeout-ms" => *index += 2,
            value if value.starts_with("--timeout-ms=") => *index += 1,
            _ => break,
        }
    }
}

#[derive(Debug, Args)]
pub struct PageArgs {
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u16).range(1..=100)
    )]
    pub limit: u16,

    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldsList(Vec<String>);

impl FieldsList {
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl FromStr for FieldsList {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut seen = BTreeSet::new();
        let mut fields = Vec::new();

        for raw in value.split(',') {
            let field = raw.trim_matches(|character: char| character.is_ascii_whitespace());
            if field.is_empty() {
                return Err("field list contains an empty field ID".to_owned());
            }
            if !seen.insert(field.to_owned()) {
                return Err(format!("field list contains duplicate field ID {field}"));
            }
            fields.push(field.to_owned());
        }

        Ok(Self(fields))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use clap::CommandFactory;

    use super::Cli;
    use crate::schema::command_specs;

    #[derive(Debug)]
    struct LeafShape {
        name: String,
        flags: BTreeMap<String, bool>,
        positionals: Vec<(String, bool)>,
    }

    #[test]
    fn parser_leaves_and_arguments_match_runtime_descriptors() {
        let mut leaves = Vec::new();
        collect_leaves(&Cli::command(), &mut Vec::new(), &mut leaves);

        let parser_names: BTreeSet<&str> = leaves.iter().map(|leaf| leaf.name.as_str()).collect();
        let schema_names: BTreeSet<&str> = command_specs().iter().map(|spec| spec.name).collect();
        assert_eq!(parser_names, schema_names);

        for leaf in leaves {
            let spec = command_specs()
                .iter()
                .find(|spec| spec.name == leaf.name)
                .expect("descriptor for parser leaf");
            let flags: BTreeMap<String, bool> = spec
                .flags
                .iter()
                .map(|flag| (flag.name.trim_start_matches("--").to_owned(), flag.required))
                .collect();
            let positionals: Vec<(String, bool)> = spec
                .positionals
                .iter()
                .map(|positional| (positional.name.to_owned(), positional.required))
                .collect();

            assert_eq!(leaf.flags, flags, "flag drift for {}", leaf.name);
            assert_eq!(
                leaf.positionals, positionals,
                "positional drift for {}",
                leaf.name
            );
        }
    }

    fn collect_leaves(
        command: &clap::Command,
        prefix: &mut Vec<String>,
        leaves: &mut Vec<LeafShape>,
    ) {
        let subcommands: Vec<&clap::Command> = command.get_subcommands().collect();
        if !subcommands.is_empty() {
            for subcommand in subcommands {
                prefix.push(subcommand.get_name().to_owned());
                collect_leaves(subcommand, prefix, leaves);
                prefix.pop();
            }
            return;
        }

        let mut flags = BTreeMap::new();
        let mut positionals = Vec::new();
        for argument in command.get_arguments() {
            if let Some(long) = argument.get_long() {
                if !matches!(long, "help" | "output" | "pretty" | "timeout-ms") {
                    flags.insert(long.to_owned(), argument.is_required_set());
                }
            } else {
                let name = argument
                    .get_value_names()
                    .and_then(|names| names.first())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| argument.get_id().to_string().to_uppercase());
                positionals.push((name, argument.is_required_set()));
            }
        }

        leaves.push(LeafShape {
            name: prefix.join("."),
            flags,
            positionals,
        });
    }
}
