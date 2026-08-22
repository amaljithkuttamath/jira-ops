# Command index

Use `jira-ops --help` for human-readable help. Use
`jira-ops schema --all --pretty` for the complete machine-readable contract or
`jira-ops schema COMMAND... --pretty` for one operation.

| Command | Purpose |
| --- | --- |
| `version` | Show CLI and contract versions |
| `schema [COMMAND...]` | Discover all commands or one command contract |
| `config get`, `config set`, `config unset` | Manage saved non-secret defaults |
| `url issue`, `url project` | Return canonical Jira URLs without opening a browser |
| `completion` | Generate shell completion text |
| `man` | Generate man pages into a validated empty directory |
| `server info` | Read stable Jira Cloud server metadata |
| `user search` | Search users with privacy-trimmed output |
| `board list` | List Jira Software boards |
| `release list` | List project releases |
| `auth login`, `auth status`, `auth logout` | Manage API-token authentication |
| `me` | Show the authenticated Jira user |
| `project list`, `project get` | Read projects |
| `project templates`, `project create` | Discover templates and plan or create projects |
| `field list` | List or search fields |
| `issue get`, `issue search` | Read issues |
| `issue create-meta` | Discover issue types and field metadata |
| `issue create`, `issue clone`, `issue update`, `issue delete` | Plan, apply, or explicitly confirm issue changes |
| `issue assign` | Plan or apply assignment and unassignment |
| `issue link types`, `issue link get`, `issue link add`, `issue link remove` | Discover, inspect, add, or remove issue links |
| `issue remote-link list`, `issue remote-link get`, `issue remote-link add`, `issue remote-link remove` | Read or safely change HTTPS remote links |
| `issue worklog list`, `issue worklog add`, `issue worklog update`, `issue worklog delete` | Read or safely change worklogs and estimates |
| `epic list`, `epic create`, `epic add`, `epic remove` | Discover, create, or change epic membership |
| `sprint list`, `sprint issues`, `sprint add`, `sprint close` | Discover sprints, move issues, or close an active sprint |
| `issue watcher list`, `issue watcher add`, `issue watcher remove` | Read or change watchers |
| `issue comments`, `issue comment` | List or add comments |
| `issue transitions`, `issue transition` | Discover or apply workflow transitions |
