# Jira Cloud recipes

These recipes use the beta's current command surface. Mutation examples are dry
runs: they build and validate a plan but do not send a Jira write request. After
reviewing a plan, add `--apply` to that same command when a write is intended.

## Discover before acting

List every command contract:

```bash
jira-ops schema --all --pretty
```

Inspect one mutation's input and output schema:

```bash
jira-ops schema issue update --pretty
```

List local project templates:

```bash
jira-ops project templates --type software --pretty
```

## Read projects and fields

List visible projects:

```bash
jira-ops project list --limit 20 --pretty
```

Get a project by key or ID:

```bash
jira-ops project get ACCL --pretty
```

Search fields by name:

```bash
jira-ops field list --query story --limit 20 --pretty
```

## Search and inspect issues

Search with JQL and request a compact projection:

```bash
jira-ops issue search --jql 'project = ACCL ORDER BY updated DESC' --fields summary,status,assignee,updated --limit 20 --pretty
```

Get one issue. Without `--fields`, the default fields are `summary`, `status`,
`assignee`, and `updated`.

```bash
jira-ops issue get ACCL-1 --fields summary,status,description --pretty
```

Descriptions and comment bodies are projected to plain text. Other requested
fields appear under `data.fields` using their Jira field IDs.

## Discover create metadata

List issue types available for a project:

```bash
jira-ops issue create-meta --project ACCL --limit 100 --pretty
```

List create fields for one issue-type ID:

```bash
jira-ops issue create-meta --project ACCL --issue-type 10001 --limit 100 --pretty
```

Use each field's `input_kind`, `required`, `operations`,
`supported_selector_members`, and `allowed_values` to construct input. Field
metadata, not hard-coded issue names, controls hierarchy fields such as `parent`.

## Plan project creation

Project planning uses the local template registry and needs no credential or
network access:

```bash
printf '%s\n' '{"key":"DEMO","name":"Demo project","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"replace-with-account-id","assignee_type":"UNASSIGNED"}' | jira-ops project create --input - --pretty
```

Project keys contain 2 to 10 uppercase letters, digits, or underscores and must
start with a letter. Names contain 1 to 80 characters with no surrounding
whitespace.

Applying this plan requires the account to have Jira's global **Administer
Jira** permission. A scoped token needs either the classic
`manage:jira-configuration` scope or both granular scopes
`write:project:jira` and `read:project:jira`. Planning remains local and does
not require those permissions.

## Plan issue creation

Use IDs returned by `issue create-meta`. The `fields` object must contain
`summary` and must not contain `project` or `issuetype`.

```bash
printf '%s\n' '{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"Prepare beta release","description":"Verify packages and checksums"}}' | jira-ops issue create --input - --pretty
```

If the selected issue type exposes a parent field with `key` as a supported
selector, plan a child issue like this:

```bash
printf '%s\n' '{"project_key":"ACCL","issue_type_id":"10001","fields":{"summary":"Child work item","parent":{"key":"ACCL-1"}}}' | jira-ops issue create --input - --pretty
```

Create planning reads all create-field metadata pages and validates required
fields, types, allowed values, and screen membership before returning a plan.

## Plan an issue update

Issue mutation targets use uppercase Jira keys such as `ACCL-1`.

```bash
printf '%s\n' '{"set":{"summary":"Updated release title","description":"Review the generated plan first"}}' | jira-ops issue update ACCL-1 --input - --pretty
```

Update planning reads edit metadata and validates that each field supports the
`set` operation. An optional field can be cleared with `null` when its modeled
input kind permits it.

## Plan assignment or unassignment

Use a Jira account ID to plan assignment:

```bash
printf '%s\n' '{"issue_key":"ACCL-1","account_id":"replace-with-account-id"}' | jira-ops issue assign --input - --pretty
```

Use explicit `null` to plan unassignment:

```bash
printf '%s\n' '{"issue_key":"ACCL-1","account_id":null}' | jira-ops issue assign --input - --pretty
```

The account ID can come from `me`, an existing issue projection, or a watcher
list. Both plans are local and need no credentials.

## Inspect and plan issue links

List the site's exact link type names and direction labels:

```bash
jira-ops issue link types --pretty
```

Get one link by its positive decimal ID:

```bash
jira-ops issue link get 10000 --pretty
```

Use an exact type name to plan a relationship. `inward_issue` and
`outward_issue` determine its direction:

```bash
printf '%s\n' '{"inward_issue":"ACCL-1","outward_issue":"ACCL-2","type_name":"Blocks"}' | jira-ops issue link add --input - --pretty
```

Plan destructive operations with a confirmation bound to the exact target:

```bash
printf '%s\n' '{"confirm_issue":"ACCL-1","cascade":false}' | jira-ops issue delete ACCL-1 --input - --pretty
printf '%s\n' '{"confirm_link_id":"10000"}' | jira-ops issue link remove 10000 --input - --pretty
```

These examples remain dry runs. Add `--apply` only after inspecting the plan.

List remote links and plan adding or removing one:

```bash
jira-ops issue remote-link list ACCL-1 --pretty
jira-ops issue remote-link get ACCL-1 10000 --pretty
printf '%s\n' '{"url":"https://tracker.example/tickets/1","title":"Ticket 1"}' | jira-ops issue remote-link add ACCL-1 --input - --pretty
printf '%s\n' '{"confirm_remote_link_id":"10000"}' | jira-ops issue remote-link remove ACCL-1 10000 --input - --pretty
```

List worklogs and plan the lifecycle operations:

```bash
jira-ops issue worklog list ACCL-1 --limit 20 --pretty
printf '%s\n' '{"time_spent":"1h 30m","adjustment":{"mode":"auto"}}' | jira-ops issue worklog add ACCL-1 --input - --pretty
printf '%s\n' '{"time_spent":"2h","adjustment":{"mode":"leave"}}' | jira-ops issue worklog update ACCL-1 10000 --input - --pretty
printf '%s\n' '{"confirm_worklog_id":"10000","adjustment":{"mode":"leave"}}' | jira-ops issue worklog delete ACCL-1 10000 --input - --pretty
```

Discover epics and plan epic changes:

```bash
jira-ops epic list --project ACCL --pretty
printf '%s\n' '{"project_key":"ACCL","issue_type_id":"10000","fields":{"summary":"Delivery epic"}}' | jira-ops epic create --input - --pretty
printf '%s\n' '{"issue_keys":["ACCL-2"],"notify_users":true}' | jira-ops epic add ACCL-1 --input - --pretty
printf '%s\n' '{"issue_keys":["ACCL-2"],"confirm_epic":"ACCL-1","confirm_issue_keys":["ACCL-2"],"notify_users":true}' | jira-ops epic remove ACCL-1 --input - --pretty
```

Discover sprints and plan sprint changes:

```bash
jira-ops sprint list --board 1 --state active --pretty
jira-ops sprint issues 1 --limit 20 --pretty
printf '%s\n' '{"issue_keys":["ACCL-1"]}' | jira-ops sprint add 1 --input - --pretty
printf '%s\n' '{"confirm_sprint_id":1}' | jira-ops sprint close 1 --input - --pretty
```

Link addition plans locally. Link deletion requires the exact link ID in its
confirmation input.

## Read and plan watcher changes

List watchers and their stable account IDs:

```bash
jira-ops issue watcher list ACCL-1 --pretty
```

Plan adding one watcher:

```bash
printf '%s\n' '{"issue_key":"ACCL-1","account_id":"replace-with-account-id"}' | jira-ops issue watcher add --input - --pretty
```

Plan removing one watcher:

```bash
printf '%s\n' '{"issue_key":"ACCL-1","account_id":"replace-with-account-id"}' | jira-ops issue watcher remove --input - --pretty
```

Watcher add and remove plans are local and need no credentials.

## Plan a comment

Comment planning is local and needs no credentials:

```bash
printf '%s\n' '{"body":"Release checklist reviewed."}' | jira-ops issue comment ACCL-1 --input - --pretty
```

The CLI converts plain text to Atlassian Document Format only for the wire
request. The dry-run plan preserves the plain text intent.

## Plan a transition

First inspect currently available transitions and their screen fields:

```bash
jira-ops issue transitions ACCL-1 --pretty
```

Then plan one by exact transition ID:

```bash
printf '%s\n' '{"transition_id":"31","fields":{}}' | jira-ops issue transition ACCL-1 --input - --pretty
```

## Read comments

```bash
jira-ops issue comments ACCL-1 --limit 20 --pretty
```

## Continue a page

Read `meta.next_cursor` from a successful page. Pass it back to the same command
with exactly the same query and limit:

```bash
jira-ops project list --limit 20 --cursor "$next_cursor" --pretty
```

Do not decode, edit, share across commands, or reuse a cursor after changing the
query. The CLI rejects a cursor that is malformed or bound to different inputs.

## Use compact TOON output

Use JSON when another program requires JSON. Use TOON when the consumer supports
TOON and token volume matters:

```bash
jira-ops issue get ACCL-1 --fields summary,status -o toon
```

The envelope and values are the same in both representations. TOON cannot be
combined with `--pretty`.

## Verify a mutation result

After an applied command succeeds, read the returned project key, issue key,
comment ID, or operation name from `data`, then fetch the resource with a read
command. If a write fails, inspect `error.operation_outcome` and
`error.retry_safety` before doing anything else. Never automatically retry an
`unsafe` or `unknown` result.
