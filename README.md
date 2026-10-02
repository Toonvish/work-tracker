# work-tracker

A small Rust CLI that answers one question: **what did I work on since the
last team meeting?**

It asks GitLab (merge requests) and YouTrack (issues) what you touched, links
merge requests to issues by the issue IDs they mention, and shows the result
as an interactive list. Press Enter on a row to open the merge request in your
browser. Every row shows the current state (MR: opened / draft / merged /
closed; issue: the YouTrack `State` field).

The time window is derived from your weekly meetings and the current time, so
you never type dates. By default the window is "from the end of the most
recently finished meeting until now".

## Install

```sh
cargo install --path .
```

This installs the `work-tracker` binary into `~/.cargo/bin`. A stable Rust
toolchain (1.98 or newer) is required. Linux is the primary target; the
config file permission check is Unix-only.

## Configuration

Create a config template, then edit it:

```sh
work-tracker init            # writes the template with mode 0600
work-tracker init --force    # overwrite an existing file (mode is reset to 0600)
```

`init` refuses to overwrite an existing file (exit code 1) unless `--force`
is given.

### Config file location

The first rule that applies wins:

1. `--config <path>` (or `-c <path>`)
2. `$XDG_CONFIG_HOME/work-tracker/config.toml` (only if the variable is set,
   non-empty and an absolute path)
3. `~/.config/work-tracker/config.toml`

### File permissions

Tokens are stored in plain text in the config file, so keep it private:

```sh
chmod 600 ~/.config/work-tracker/config.toml
```

`work-tracker init` creates the file with mode 0600. Whenever the permission
bits of the config file are **not exactly 0600** (0644, 0640, 0400 and 0700
all count), the tool prints a warning to stderr and continues.

### Tokens

| Service | Token | Notes |
|---|---|---|
| GitLab | Personal access token with the **`read_api`** scope | Profile > Access tokens. A token without `read_api` gives a 403 `insufficient_scope` error that says so. |
| YouTrack | **Permanent token** (starts with `perm:`) | Profile > Account Security > Tokens. The token's user must be able to read the projects you care about. |

The environment variables `GITLAB_TOKEN` and `YOUTRACK_TOKEN` override the
tokens in the file when they are set and non-empty. This makes it possible to
keep the file token-free. An environment token never creates a missing
`[gitlab]` / `[youtrack]` section, because a URL is still required.

Tokens are only ever sent in HTTP headers. They never appear in error
messages, `Debug` output or `--verbose` logs.

### Options

| Key | Section | Meaning |
|---|---|---|
| `url` | `[gitlab]`, `[youtrack]` | Base URL (required). A trailing `/` is removed. For GitLab a pasted trailing `/api/v4` is removed too. Must start with `http://` or `https://`. |
| `token` | `[gitlab]`, `[youtrack]` | API token (see above). |
| `username` | `[gitlab]` | Optional. Your GitLab username. By default it is resolved from the token with `GET /user`. |
| `projects` | `[youtrack]` | Optional list of project short names. Limits YouTrack queries and the issue IDs recognised in MR text. Default: all projects. |
| `state_field` | `[youtrack]` | Optional name of the custom field that holds the issue state. Default: `State`. |
| `weekday`, `start`, `end` | `[[meetings]]` | One weekly recurring meeting in local time. `weekday` is `mon`..`sun` (or `monday`..`sunday`, case-insensitive), `start`/`end` are `HH:MM` (24h) and `end` must be after `start`. |

A source whose section is missing is skipped with a note (for example
`GitLab not configured; skipping`). A section that is present but has no
token (after the environment override) is an error.

### Example config

```toml
[gitlab]
url = "https://gitlab.example.com"
token = "glpat-xxxxxxxxxxxxxxxxxxxx"
# username = "eric"

[youtrack]
url = "https://youtrack.uponu.com"
token = "perm:xxxxxxxxxxxxxxxxxxxx"
projects = ["SP", "MS", "P"]
# state_field = "State"

[[meetings]]
weekday = "tue"
start = "09:00"
end = "10:00"

[[meetings]]
weekday = "thu"
start = "14:00"
end = "15:00"
```

## Usage

```sh
work-tracker                 # interactive list for the current window
work-tracker --plain         # static table (also used when stdout is not a terminal)
work-tracker --json | jq .   # machine-readable output
work-tracker -p              # previous window (between the two last finished meetings)
work-tracker -pp             # the window before that
work-tracker --since 2026-09-28T08:00                       # explicit start, end = now
work-tracker --since 2026-09-28 --until 2026-09-29T12:00    # explicit range
work-tracker --until 2026-09-29T08:30                       # as if it were that time
work-tracker window --now 2026-09-30T11:00                  # only print the window, no network
work-tracker --no-gitlab     # skip GitLab
work-tracker --no-youtrack   # skip YouTrack
work-tracker -v              # log every HTTP request (never tokens) to stderr
work-tracker --config ./my.toml
```

Interactive list: Enter opens the selected item in the browser, arrow keys
move, Esc or `q` quits. Each task is one row: an issue together with its
MR(s), an issue without MR, or an MR without issue. All rows share the same
columns, under a header line: issue ID, MR (`project!iid`, `+N` when the
issue has more MRs), title, issue state, MR state, last-updated time and your
roles (author, reviewer, approver, commenter, assignee, updater, ...). A `-`
marks a missing issue or MR. Columns are coloured on a terminal that allows it
(`NO_COLOR` turns it off). A row with exactly one MR opens that MR; a row with
several MRs asks which one to open (or the issue itself). `--plain` prints the
same columns followed by every URL of the row (issue first, then the MRs).

Accepted date/time formats for `--since`, `--until` and `--now`:
`2026-09-29T10:00`, `2026-09-29T10:00:30`, `"2026-09-29 10:00"`, `2026-09-29`
(local midnight) and RFC 3339 with an offset (`2026-09-29T10:00:00+02:00`).
Naive values are interpreted in the local time zone.

The `window` subcommand needs no tokens and no network. It also works without
a config file when `--since` is given. Its output looks like this:

```
Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)
start: 2026-09-29T10:00:00+02:00 (2026-09-29T08:00:00Z)
end:   2026-09-30T11:00:00+02:00 (2026-09-30T09:00:00Z)
```

The resolved window is always printed at the top of the report.

### Subcommands

| Command | What it does |
|---|---|
| *(none)* | Fetch GitLab and YouTrack for the resolved window and show the list. |
| `init [--force]` | Write a commented config template with mode 0600 to the config path. |
| `window` | Print the resolved window only (no tokens, no network). Honours `--config`, `--since`, `--until`, `--now`, `--previous`. |

### Keybindings in the interactive list

| Key | Action |
|---|---|
| Up / Down (or `k` / `j`) | Move the selection |
| Enter | Open the selected item in the browser (a group with several MRs asks which one) |
| Esc or `q` | Quit |

## How the window is computed

**window = [end of the most recently finished meeting, now].** A meeting that
is still running does not count as finished; the one before it is used.
Meetings are weekly recurring slots in local time. The calculation works in
local calendar dates and wall-clock times, so week wrap-around and daylight
saving changes are handled correctly (the result is converted to UTC for the
APIs). A meeting end inside a DST gap moves forward; an ambiguous time during
the fall-back hour uses its first occurrence.

With meetings Tuesday 09:00-10:00 and Thursday 14:00-15:00:

| Now | Window start | Window end |
|---|---|---|
| Wed 11:00 | Tue 10:00 | Wed 11:00 |
| Tue 08:30 | previous Thu 15:00 | Tue 08:30 |
| Tue 09:30 (meeting running) | previous Thu 15:00 | Tue 09:30 |
| Tue 10:30 | Tue 10:00 | Tue 10:30 (short window, that is fine) |

More rules:

- **Single meeting:** the window spans a full week (the same weekday one week
  earlier).
- **`--previous` / `-p`:** shifts back by whole meeting intervals, so the
  windows are contiguous. If the current window is `[M1.end, now]`, then `-p`
  gives `[M2.end, M1.end]` and `-pp` gives `[M3.end, M2.end]`, where M1 is the
  most recently finished meeting. `--previous` cannot be combined with
  `--since`.
- **`--since`** replaces the start; the end is `--until` or now. **`--until`**
  alone means "as if now were that time". `--since` must be before the end.
- **`--now`** replaces the current time everywhere (useful for testing).
- A meeting counts as finished when its end is at or before now. At exactly
  the end instant the window is empty (zero-length).
- **Overlapping meetings are rejected** when the config is loaded (two
  meetings on the same weekday whose time ranges overlap, including exact
  duplicates). Back-to-back meetings (one ends when the next starts) and the
  same times on different weekdays are fine.
- With no `[[meetings]]` configured you must pass `--since`.

## How MRs and issues are matched

Issue IDs look like `SP-123`, `ms-45` or `P-7` (project short name, dash,
number; case-insensitive). The tool extracts them from, in this order:

1. the MR title,
2. the MR source branch name,
3. the MR description (only the first 64 KiB).

Matching rules:

- Separators in front of an ID may be `-`, `_`, `/`, `(`, quotes, whitespace
  and so on, so `feature/SP-123-login`, `sp-123_fix`, `bugfix-SP-12`,
  `feature_SP-123` and `Resolve "MS-7: x"` all work. Pasted issue URLs such
  as `https://youtrack.uponu.com/issue/SP-9` work too.
- An ID must not be glued to other letters or digits, so UUID fragments
  (`3f2a-4b1c`) and `SP-12abc` are not IDs.
- Text inside fenced code blocks (lines between ``` or ~~~ fences) in the MR
  description is ignored.
- **Project filter:** candidates are checked against the real YouTrack
  project short names (from `/api/admin/projects`, or the configured
  `projects`, or the intersection of both). This removes false positives such
  as `UTF-8` or `ISO-8601`. If no project list is available, a built-in
  denylist of common look-alikes (`UTF`, `ISO`, `SHA`, `RFC`, `CVE`, ...)
  is used instead. The configured `projects` are used even when YouTrack is
  skipped with `--no-youtrack`.
- An MR is joined with the YouTrack issues fetched for the window. If it
  references an issue that was not fetched (for example one last updated
  outside the window), that issue is fetched by ID, in bounded batches, so its
  title and state can be shown.
- **Moved or renamed issues:** if YouTrack resolves a referenced ID to a
  different issue (the issue moved to another project), the MR is attached to
  the new ID via an alias, so `OLD-12` in a title ends up under `NEW-5`.
- MRs without a known issue and issues without an MR are still listed.

## Exit codes

| code | meaning |
|---|---|
| 0 | success (including empty results, and quitting the list with Esc/q) |
| 1 | runtime failure: every enabled source failed, browser/terminal I/O error, `init` target already exists |
| 2 | usage or config error: clap errors, config missing/invalid, bad date/time, since not before until/now, no meetings, both sources disabled or unconfigured, missing token |
| 3 | partial results: at least one source failed, output was produced from the others (the failure is printed as a warning on stderr) |

## Known limitations

- **Past windows.** With `--previous` or `--until`, a GitLab MR or a YouTrack
  assigned issue that was touched inside the window **and again later** is
  found only through your own events/activities in the window (or, for MRs,
  through MR creation in the window). Neither API can filter "updated within a
  past interval" without also dropping such items. This applies to both GitLab
  MRs and YouTrack assigned issues.
- **Approximate YouTrack fallback.** "Issues I changed or commented on" use
  the activities API. If it is unavailable (HTTP 400/404), the tool falls back
  to the approximate `updater: me` / `commenter: me` query language and prints
  a warning.
- **An MR referencing two issues appears twice**, once under each issue
  group. It is counted once in the totals.
- **Single-MR groups.** In a group with exactly one MR the interactive list
  opens the MR, so the YouTrack issue URL is available only through
  `--plain` (URL column) and `--json` (`web_url`).
- **Project short names containing `_` are not recognised** in MR text. IDs
  such as `MY_PROJ-12` are not matched (the underscore is treated as a
  separator, so `feature_SP-123` still finds `SP-123`).
- Result lists are capped (GitLab 20 pages of 100 MRs, 30 pages of events,
  YouTrack 20 pages of issues and 25 pages of activities, 50 issues fetched
  by ID). When a cap is reached a warning is printed.
- Interactive mode needs a terminal on stdin, stdout and stderr; otherwise the
  plain table is printed.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `No config file at ...` (exit 2) | Run `work-tracker init`, then edit the file, or pass `--config <path>`. |
| `warning: ... has mode 0644 (expected 0600)` | `chmod 600` the config file; it contains tokens. |
| `... rejected the token (401 Unauthorized)` | The token is wrong, expired or revoked. Check `GITLAB_TOKEN` / `YOUTRACK_TOKEN`, which override the file. |
| GitLab 403 `insufficient_scope` | Create the token with the `read_api` scope. |
| `request to ... failed: Connection refused` / DNS error | Check the `url` values, VPN and proxy settings (`HTTPS_PROXY` is honoured). Use `-v` to see each request. |
| Exit code 3 and a warning | One source failed; the output shows the other. Use `--no-gitlab` / `--no-youtrack` to silence a source you do not need. |
| An MR is not linked to its issue | The issue ID must appear in the MR title, source branch or description, and its project must exist in YouTrack (and in `projects`, if set). Use `--json` to see the extracted IDs. |
| Wrong window | Run `work-tracker window` (add `--now ...` to simulate) and check the `[[meetings]]` weekdays and times; they are interpreted in the local time zone. |
| Plain table instead of the interactive list | The list needs a terminal on stdin, stdout and stderr; pipes and redirects fall back to the table. |
| Browser does not open | `xdg-open` (Linux) must be installed and working; the URL is available with `--plain` or `--json`. |

## Development

```sh
cargo build
cargo test           # offline; never contacts GitLab or YouTrack
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
