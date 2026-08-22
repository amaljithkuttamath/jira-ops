# jira-ops

`jira-ops` is a predictable, agent-friendly CLI for Jira Cloud. It returns
structured data, exposes machine-readable command schemas, and plans every
mutation before it writes.

- Compact JSON by default, with optional token-efficient TOON output.
- Non-interactive commands with stable IDs, cursors, and exit classes.
- Metadata-driven issue creation and transitions for different Jira sites.
- Dry-run writes by default; applying a mutation is always explicit.

> Public beta: Jira Cloud and API-token authentication are supported. OAuth
> and self-managed Jira are not yet supported.

## Install

On macOS or Linux, install with Homebrew:

```bash
brew install amaljithkuttamath/tap/jira-ops
```

Upgrade after a new release:

```bash
brew upgrade jira-ops
```

Alternatively, download the archive for macOS, Linux, or Windows from
[GitHub Releases](https://github.com/amaljithkuttamath/jira-ops/releases),
extract it, and place `jira-ops` on your `PATH`.

Or install from source with Rust 1.90 or newer:

```bash
cargo install --locked --git https://github.com/amaljithkuttamath/jira-ops --tag v0.2.0-beta.2
```

Confirm the installation:

```bash
jira-ops version
```

Release archives include SHA-256 checksums and GitHub build-provenance
attestations. See the [security policy](SECURITY.md#release-integrity) for
verification commands.

## Authenticate

Create an [Atlassian API token](https://id.atlassian.com/manage-profile/security/api-tokens),
then save it in your operating system's credential store:

```bash
read -r -s jira_ops_token
printf '%s\n' "$jira_ops_token" | jira-ops auth login \
  --site https://your-site.atlassian.net \
  --email you@example.com \
  --token-stdin
unset jira_ops_token
```

Verify the active account:

```bash
jira-ops me --pretty
```

For CI and other headless environments, see
[authentication](docs/auth.md#headless-environment).

## Use

List visible projects:

```bash
jira-ops project list --limit 20 --pretty
```

Search issues with JQL:

```bash
jira-ops issue search \
  --jql 'project = DEMO ORDER BY updated DESC' \
  --fields summary,status,assignee,updated \
  --limit 20 \
  --pretty
```

Inspect one issue:

```bash
jira-ops issue get DEMO-1 --fields summary,status,description --pretty
```

### Plan and apply a write

First discover the site's issue types and field requirements:

```bash
jira-ops issue create-meta --project DEMO --limit 100 --pretty
jira-ops issue create-meta --project DEMO --issue-type 10001 --limit 100 --pretty
```

Use the returned issue-type ID to plan an issue creation:

```bash
printf '%s\n' '{"project_key":"DEMO","issue_type_id":"10001","fields":{"summary":"Prepare release","description":"Verify the release artifacts"}}' | jira-ops issue create --input - --pretty
```

The command validates the input and returns a plan without writing to Jira.
Review that plan, then rerun the same invocation with `--apply` when the write
is intended. Set `JIRA_READ_ONLY=1` in environments where writes must be
impossible.

Other common workflows:

```bash
# Discover available workflow transitions.
jira-ops issue transitions DEMO-1 --pretty

# Plan a comment.
printf '%s\n' '{"body":"Release verification completed."}' | jira-ops issue comment DEMO-1 --input - --pretty

# Plan an issue update.
printf '%s\n' '{"set":{"summary":"Updated release title"}}' | jira-ops issue update DEMO-1 --input - --pretty
```

See [recipes](docs/recipes.md) for assignment, links, watchers, worklogs, epics,
sprints, comments, transitions, pagination, and project creation.

## Use from an agent

Agents should discover the contract instead of scraping help text or guessing
Jira field names:

```bash
jira-ops schema --all
jira-ops schema issue create
```

Each invocation produces at most one document:

- Exit `0`: a success document on standard output.
- Nonzero exit: an error document on standard error.
- JSON is compact by default; add `--pretty` for humans.
- Add `-o toon` when the consumer supports TOON and token cost matters.
- Paginated results return an opaque `meta.next_cursor`.

The scoped schema describes arguments, input, output, effects, idempotency,
pagination, and error-to-exit mappings. See the
[machine-use guide](docs/agent-guide.md) for retries, mutation outcomes, cursor
handling, and the complete agent loop.

## Documentation

- [Recipes](docs/recipes.md): practical Jira workflows
- [Command index](docs/commands.md): complete command surface
- [Authentication](docs/auth.md): credential store and headless setup
- [Machine-use guide](docs/agent-guide.md): schemas and automation contracts
- [Security](SECURITY.md): reporting, token hygiene, and release verification
- [Changelog](CHANGELOG.md): release notes and beta limits

Run `jira-ops --help` for human-readable command help or
`jira-ops schema --all --pretty` for the complete machine-readable command
surface.

## Project scope

`jira-ops` is an independent, third-party project. It is not affiliated with,
endorsed by, or sponsored by Atlassian. Jira and Atlassian are trademarks of
Atlassian Pty Ltd.

Licensed under either [MIT](LICENSE-MIT) or
[Apache License 2.0](LICENSE-APACHE), at your option.
