# work-tracker — Requirements Spec

Rust CLI that answers: "what did I work on since the last team meeting?" by
querying GitLab merge requests and YouTrack issues, correlating them, and
presenting an interactive list with the ability to open items in the browser.

Owner: Eric (eric.stampa@uponu.com). Date: 2026-10-02.
All decisions below were confirmed with the owner; do not re-litigate them.

## 1. Meetings and time window

- Meetings are configured as a list of weekly recurring slots
  `{ weekday, start, end }` in local time. Owner's current meetings:
  - Tuesday 09:00–10:00
  - Thursday 14:00–15:00
- Window rule (CONFIRMED): **window = [end of the most recently *finished*
  meeting, now]**.
  - A meeting currently in progress does not count as finished; use the one
    before it.
  - Examples (meetings above):
    - Wed 11:00 → Tue 10:00 .. Wed 11:00
    - Tue 08:30 → previous Thu 15:00 .. Tue 08:30
    - Tue 09:30 (meeting running) → previous Thu 15:00 .. Tue 09:30
    - Tue 10:30 → Tue 10:00 .. Tue 10:30 (short window, that is fine)
  - With a single configured meeting, the window spans a full week.
  - Must handle week wrap-around and DST correctly (use local timezone via
    chrono `Local`; compute in local time, convert to UTC/RFC3339 for APIs).
- CLI overrides: `--since <datetime>` / `--until <datetime>` and
  `--previous` (shift back one meeting interval: window between the two
  most recently finished meetings) are desirable. `--now <datetime>` for
  testing the window logic is useful.
- Always print the resolved window at the top of the output.

## 2. Configuration

- Location: `$XDG_CONFIG_HOME/work-tracker/config.toml`, default
  `~/.config/work-tracker/config.toml`. `--config <path>` overrides.
- Format: TOML. Tokens stored plain in the file; the tool must warn if the
  file mode is not 0600 (and `work-tracker init` should create it with 0600).
- Env overrides: `GITLAB_TOKEN`, `YOUTRACK_TOKEN`.
- YouTrack config is self-contained (do NOT read youtrack-cli's config).
- Proposed shape (designer may refine, keep it this simple):

```toml
[gitlab]
url = "https://gitlab.paar-it.de"    # self-hosted
token = "glpat-..."
# optional: username = "eric"        # otherwise resolved via GET /user

[youtrack]
url = "https://youtrack.uponu.com"
token = "perm:..."
# optional: projects = ["SP", "MS", "P"]   # limit; default all

[[meetings]]
weekday = "tue"
start = "09:00"
end = "10:00"

[[meetings]]
weekday = "thu"
start = "14:00"
end = "15:00"
```

- `work-tracker init` writes a commented template (0600) if none exists.
- Clear error messages when config is missing or a token is invalid.

## 3. Data sources (what counts as "work I did")

GitLab (self-hosted, all projects the user can see — use the global
`/api/v4/merge_requests?scope=all` style endpoints, not per-project):
1. MRs authored by me, created or updated in the window.
2. MRs by others where I am reviewer, or where I approved or commented in
   the window (user events API `/users/:id/events` with `after`/`before`,
   and/or `reviewer_id` filter; designer decides the precise query set and
   must handle pagination).

YouTrack (REST API at `<url>/api`, bearer token):
3. Issues assigned to me whose `updated` is in the window.
4. Issues I commented on or changed in the window even if not assigned to
   me. Prefer YouTrack query language (`updater: me`, `commenter: me`,
   `updated: <start> .. <end>`) over the activities API if that is
   sufficient; designer verifies.

Fetch GitLab and YouTrack concurrently. Deduplicate items.

## 4. Matching MRs ↔ issues

Issue IDs look like `SP-123`, `ms-45`, `P-7` (project short name, dash,
number; case-insensitive). Extract them from (CONFIRMED):
- MR title
- MR source branch name
- MR description

Then join with fetched YouTrack issues by ID. If an MR references an issue
that was not fetched (e.g. updated outside the window), fetch that issue by
ID so its title and state can be shown (bounded, batched if possible).
Issues with no MR and MRs with no issue are still shown.

## 5. Output / UI (CONFIRMED: interactive list)

- Default: interactive terminal list (e.g. `inquire`/`dialoguer`-style
  select, or a small ratatui list if needed). Each row shows:
  - issue ID, title,
    MR `project!iid`, **current state** (MR: opened/draft/merged/
    closed; Issue: YouTrack `State` field), last-updated time, my role
    (author / reviewer / commenter).
  - One row per task (issue with its MR(s), issue without MR, or MR
    without issue), all rows with the same columns; colour per column.
    (Changed 2026-10-02; was: issue line with its MR(s) beneath.)
- Enter opens the selected item's web URL in the browser (`xdg-open` /
  `open` crate). Opening an issue group opens the MR if there is exactly one,
  otherwise offers the choice. `q`/Esc quits.
- `--plain` (or non-TTY stdout) prints a static table instead; `--json`
  dumps the merged data.
- Show the resolved time window and counts in a header.

## 6. Non-functional

- Rust 2021/2024 edition, stable toolchain (cargo 1.98 available). Build
  must pass `cargo build`, `cargo test`, `cargo clippy -- -D warnings`,
  `cargo fmt --check`.
- Unit tests required for: time-window computation (all examples above,
  week wrap, meeting in progress, DST change), issue-ID extraction, matching
  and grouping, config parsing (incl. env overrides). HTTP clients should be
  behind traits so matching/grouping can be tested without network.
- No network access is available during automated testing; tests must not
  hit real services. Manual verification against real GitLab/YouTrack is
  done by the owner afterwards.
- Keep dependencies reasonable: clap, serde/serde_json, toml, reqwest
  (blocking or tokio — designer picks one and sticks with it), chrono,
  directories/xdg, anyhow/thiserror, regex, inquire or dialoguer, open.
- README.md documenting install, config, usage.
