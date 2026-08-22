# Machine-use guide

`jira-ops` is designed for deterministic automation. Treat its command schema,
process streams, exit class, and mutation outcome as one contract.

`jira-ops` has no interactive TUI or prompts. Agents discover identifiers with
read commands, inspect the dry-run document, and explicitly add `--apply`.
Canonical URL commands return data and never launch a browser.

## Bootstrap from the schema

Start every integration by reading the root schema:

```bash
jira-ops schema --all
```

For a specific action, request the scoped schema:

```bash
jira-ops schema issue create
```

The scoped document includes the command effect, idempotency, arguments,
success schema, error-to-exit mapping, and an argv example. Mutation input
schemas and pagination contracts appear when applicable. `contract_version`
changes only when this machine contract changes.

Use the schema instead of scraping human help text or guessing field names.

## Process contract

Each invocation produces at most one document:

- exit `0`: success document on standard output; standard error is empty
- nonzero exit: error document on standard error; standard output is empty
- JSON: default, compact, one trailing line feed
- pretty JSON: `--pretty`
- TOON: `-o toon` or `--output toon`

Select TOON per invocation when the consumer can decode TOON:

```bash
jira-ops project list --limit 20 -o toon
```

Keep JSON for tools that require strict JSON. Do not combine `--pretty` with
TOON.

## Exit classes

| Exit | Class | Typical action |
| ---: | --- | --- |
| `0` | success | Consume `data` and optional `meta` or `warnings` |
| `2` | input | Correct syntax, JSON, schema, or cursor |
| `3` | local state | Correct configuration or credential-store state |
| `4` | authentication | Refresh credentials, scopes, or permissions |
| `5` | rejected | Correct the target, conflict, or request |
| `6` | remote transient | Respect rate-limit metadata and retry only when safe |
| `7` | network | Retry reads with bounded backoff; inspect mutations first |
| `8` | mutation outcome | Stop automatic execution and reconcile with Jira |
| `70` | internal | Preserve the document and report the failure |

For errors, `error.retry_safety` is authoritative. Mutations also include
`error.operation_outcome` as `not_applied`, `applied`, or `unknown`. Never infer
the result from an error message.

## Mutation loop

1. Read the scoped schema.
2. Discover current Jira metadata when the operation depends on fields or
   transitions. Discover link type names before adding an issue link.
3. Send one JSON object through standard input with `--input -`.
4. Omit the apply flag and inspect the returned plan.
5. Confirm the target, changes, and validation state.
6. Run the same invocation with explicit apply intent.
7. Reconcile using a read command and the returned stable identifier.

Example planning call:

```bash
printf '%s\n' '{"body":"Automated check completed."}' | jira-ops issue comment ACCL-1 --input -
```

Mutation input is limited to 1 MiB, must be UTF-8, must contain exactly one JSON
object, rejects duplicate keys, and rejects unknown top-level properties.

Set `JIRA_READ_ONLY=1` for discovery and planning sessions that must not write.
The CLI itself never retries Jira write requests.

Assignment, issue-link addition, watcher addition, and watcher removal are
validated and planned locally. Their dry runs need no credentials. Applying
them resolves credentials only after the plan is complete.

## Metadata-driven fields

Do not assume that issue type names, transition IDs, or custom field IDs are
portable between sites.

- `field list` discovers site fields.
- `issue create-meta --project KEY` discovers issue types.
- `issue create-meta --project KEY --issue-type ID` discovers create fields.
- `issue transitions ISSUE` discovers available transitions and their fields.
- `issue link types` discovers the exact relationship names and directions.
- `issue link get LINK_ID` projects one existing relationship.
- `issue watcher list ISSUE` returns stable watcher account IDs.

When `validation.metadata` is `partial`, a value passed local checks but Jira did
not expose enough metadata for complete validation. Review it more carefully
before applying.

## Pagination

Paginated commands return `meta.count` and `meta.next_cursor`. The cursor is
opaque and is cryptographically fingerprinted to the command and exact query.

```bash
jira-ops issue search --jql 'project = ACCL' --limit 20 --cursor "$next_cursor"
```

Continue only when `next_cursor` is non-null. Preserve the original JQL, fields,
limit, project, issue type, or issue key. A changed input invalidates the cursor.

## Stable projections

Read commands return compact, stable projections instead of raw Jira responses.
Requested custom fields remain under `data.fields`; a requested but absent field
is explicit `null`. Descriptions and comment bodies are plain text.

Warnings are structured as `warnings[]` with `code` and `message`. Preserve them
when making a decision, even when the command exits `0`.

## Timeouts and retries

The default global timeout is 30 seconds. Set a per-invocation value from 1 to
120 seconds:

```bash
jira-ops me --timeout-ms 10000
```

For reads, use bounded exponential backoff for exit `6` or `7`, honor
`retry_after_ms`, and cap attempts. For writes, follow `retry_safety` and
`operation_outcome`; do not retry when either is unsafe or unknown.

## Authentication hygiene

Prefer the system credential store on interactive workstations and the complete
four-variable tuple in ephemeral headless environments. Never include an API
token in argv, logs, plans, prompts, issue fields, or saved command output. See
[authentication](auth.md) and the [security policy](../SECURITY.md).
