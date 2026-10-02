# work-tracker — Technical Design

Status: authoritative design for the implementer. Requirements live in
`SPEC.md`. They were confirmed by the owner and this design does not change
them. Where this document is more precise than SPEC, follow this document.
Where they seem to conflict, SPEC wins. Report the conflict; do not
improvise.

Revision 2 (2026-10-02) incorporates a design review. Section 12 lists what
changed, so anyone who read revision 1 can catch up quickly.

Verified on 2026-10-02 against cargo 1.98.1 / rustc 1.98.1. All crate versions
below were checked with `cargo search`, and the full dependency set was
compiled in a scratch crate (`cargo build` OK, default reqwest TLS = rustls
with aws-lc-rs, which builds on this machine).

---

## 0. Key decisions (summary)

| Topic | Decision |
|---|---|
| HTTP | `reqwest` **blocking**. No direct tokio dependency. Concurrency uses `std::thread::scope` (GitLab and YouTrack run in parallel threads). |
| Interactive UI | `dialoguer::Select::interact_opt()` gives Enter to select and Esc/`q` to quit natively. `console` handles widths and colors. |
| Config path | Resolved by hand (no `directories` crate, because it is not Linux-style on macOS): `--config` > `$XDG_CONFIG_HOME/work-tracker/config.toml` > `$HOME/.config/work-tracker/config.toml`. |
| Time window | Generic over `Tz: TimeZone`. Production uses `chrono::Local`. Tests use `chrono_tz::Europe::Berlin` (dev-dependency) so DST tests are deterministic. |
| GitLab | 3 global queries: authored MRs, reviewer MRs, and `/users/:id/events`. Extra MRs found only through events are fetched per project with `iids[]`. |
| YouTrack | Assigned issues use the query language. "Changed/commented by me in window" uses **`/api/activities?author=me&start&end`**, because the query language cannot express it. On 400 the request is retried with a core category set, and only after that does it fall back to the query language. |
| Fetch by ID | A batched `issue id: A or issue id: B` search is only an optimisation. Results are filtered to the requested IDs, every missing ID is fetched one by one, and moved/renamed issues produce an alias map that rewrites MR references. |
| Matching | A lenient regex (project = letters/digits, **no `_`**) plus character-boundary checks, then **filtering against the real YouTrack project short names** (`/api/admin/projects`, or the configured `projects`), which removes false positives such as `UTF-8`. |
| Interactive labels | Plain text (no embedded ANSI), width = terminal columns − 3, so dialoguer's 2-column prefix never makes a row wrap. |
| Exit codes | 0 ok, 1 runtime failure, 2 usage/config error, 3 partial results (one source failed). |

---

## 1. Crate layout

Single package, binary `work-tracker`, library crate `work_tracker` (so that
integration tests and `main.rs` share code). Edition **2024**.

```
work-tracker/
├── Cargo.toml
├── README.md
├── .gitignore                 # /target
├── src/
│   ├── main.rs                # thin: parse CLI, call app::run, map CliError -> exit code
│   ├── lib.rs                 # pub mod declarations only
│   ├── cli.rs                 # clap derive structs
│   ├── error.rs               # CliError { code, source }, ApiError (thiserror)
│   ├── config.rs              # schema, path resolution, load, env overrides, 0600 check, init template
│   ├── window.rs              # Meeting, Window, compute(), parse_user_datetime(), resolve_local(), padded_dates(), format_window_line()
│   ├── model.rs               # domain types: IssueId, Issue, MergeRequest, MrState, Role
│   ├── text.rs                # truncate_bytes() (char-boundary safe), sanitize_line()
│   ├── issue_ref.rs           # issue-ID extraction from text
│   ├── report.rs              # known_projects(), apply_aliases(), build() -> Report { entries, counts }
│   ├── http.rs                # shared blocking client builder, status->ApiError, paginate helpers
│   ├── gitlab/
│   │   ├── mod.rs             # trait GitLabApi + API DTOs (serde) + MrQuery
│   │   ├── client.rs          # GitLabClient: impl GitLabApi with reqwest + pure URL builders
│   │   ├── collect.rs         # collect(api, username, window) -> GitLabOutcome (pure logic over the trait)
│   │   └── fakes.rs           # #[cfg(test)] FakeGitLab (declared `#[cfg(test)] pub mod fakes;`)
│   ├── youtrack/
│   │   ├── mod.rs             # trait YouTrackApi + DTOs
│   │   ├── client.rs          # YouTrackClient: impl YouTrackApi with reqwest + pure URL builders
│   │   ├── collect.rs         # collect(api, window, settings) -> YouTrackOutcome; fetch_by_ids(); fetch_referenced()
│   │   └── fakes.rs           # #[cfg(test)] FakeYouTrack
│   ├── app.rs                 # orchestration: config -> window -> concurrent fetch -> report -> ui
│   └── ui/
│       ├── mod.rs             # OutputMode selection, header rendering
│       ├── rows.rs            # Row model + formatting (pure, unit-tested)
│       ├── interactive.rs     # dialoguer loop + open::that_detached
│       ├── plain.rs           # static table
│       └── json.rs            # serde JSON output
└── tests/
    ├── fixtures/              # hand-written API JSON samples, loaded by unit tests via include_str!("../../tests/fixtures/<f>.json")
    └── cli.rs                 # runs the built binary (CARGO_BIN_EXE_work-tracker): init, window, usage errors; never touches the network
```

End-to-end report building with fake API implementations is tested in
`src/app.rs` (`#[cfg(test)] mod tests`), not under `tests/`. The
`#[cfg(test)]` fakes are only visible to unit tests inside the crate.

### 1.1 Domain types (`model.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(into = "String")]   // serializes as "SP-123"; needs Clone + the From impl below. No Deserialize.
pub struct IssueId { pub project: String /* UPPERCASE ASCII alnum */, pub number: u32 }
impl From<IssueId> for String { fn from(id: IssueId) -> String { id.to_string() } }
impl fmt::Display for IssueId { /* "{project}-{number}" */ }
impl FromStr for IssueId { type Err = String; /* case-insensitive "sp-123" -> IssueId{ "SP", 123 };
    whole string must match ^[A-Za-z][A-Za-z0-9]{0,19}-[0-9]{1,7}$ (after trim); number 0 is rejected */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    // MR roles
    Author, Reviewer, Approver, Commenter, Merger, Participant,
    // Issue roles (Commenter is shared)
    Assignee, Updater,
    Referenced, // issue fetched only because an in-window MR references it
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MrState { Opened, Draft, Merged, Closed, Locked }
// Mapping from GitLab: state=="opened" && draft -> Draft; "opened" -> Opened;
// "merged" -> Merged; "closed" -> Closed; "locked" -> Locked; unknown -> Opened (log in verbose).

#[derive(Debug, Clone, Serialize)]
pub struct MergeRequest {   // also derive PartialEq for tests
    pub project_id: u64,
    pub iid: u64,
    pub project_path: String,          // "group/sub/project"
    pub title: String,
    #[serde(skip)] pub description: String,
    pub source_branch: String,
    pub state: MrState,
    pub web_url: String,
    pub author: String,                // username
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub roles: BTreeSet<Role>,
    pub issue_ids: Vec<IssueId>,       // extracted + validated, first-occurrence order
}
// MrKey = (project_id, iid) is the identity used for dedup.

#[derive(Debug, Clone, Serialize)]
pub struct Issue {          // also derive PartialEq for tests
    pub id: IssueId,
    pub summary: String,
    pub state: String,                 // YouTrack State value name, or fallback (see 6.5)
    pub resolved: bool,
    pub web_url: String,               // "<yt_url>/issue/<idReadable>"
    pub updated: DateTime<Utc>,
    pub roles: BTreeSet<Role>,
    pub in_window: bool,               // false for Referenced-only issues
}
```

### 1.2 Traits (network boundary)

All fetching goes through these traits. `collect.rs`, `report.rs` and
`app.rs` depend only on the traits, so tests can use in-memory fakes. Both
traits require `Send + Sync`, because the implementations are shared across
`thread::scope` threads. The real clients paginate internally (5.3) and
return a `Listing`, so fakes never simulate pages.

```rust
// http.rs: pure types, usable without a network
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> { pub items: Vec<T>, pub has_more: bool }
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> { pub items: Vec<T>, pub truncated: bool } // truncated = page cap reached while has_more

// gitlab/mod.rs
#[derive(Debug, Clone, PartialEq)]
pub struct MrQuery { pub author_id: Option<u64>, pub reviewer_id: Option<u64>,
                     pub updated_after: DateTime<Utc> }
pub trait GitLabApi: Send + Sync {
    fn current_user(&self) -> Result<GlUser, ApiError>;                           // GET /user
    fn users_by_username(&self, username: &str) -> Result<Vec<GlUser>, ApiError>; // GET /users?username=
    fn merge_requests(&self, q: &MrQuery) -> Result<Listing<GlMergeRequest>, ApiError>; // cap 20 pages
    fn user_events(&self, user_id: u64, after: NaiveDate, before: NaiveDate)
        -> Result<Listing<GlEvent>, ApiError>;                                    // cap 30 pages
    fn project_merge_requests(&self, project_id: u64, iids: &[u64])
        -> Result<Vec<GlMergeRequest>, ApiError>;                                 // one request, iids.len() <= 20
}

// youtrack/mod.rs
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityQuery { pub categories: Vec<&'static str>, pub start_ms: i64, pub end_ms: i64,
                           pub issue_query: Option<String> }
pub trait YouTrackApi: Send + Sync {
    fn project_short_names(&self) -> Result<Vec<String>, ApiError>;               // /api/admin/projects
    fn search_issues(&self, query: &str) -> Result<Listing<YtIssue>, ApiError>;   // cap 20 pages
    fn issue_by_id(&self, id: &str) -> Result<Option<YtIssue>, ApiError>;         // 404 -> Ok(None)
    fn my_activities(&self, q: &ActivityQuery) -> Result<Listing<YtActivity>, ApiError>; // cap 25 pages
}
```

When `collect` receives a `Listing` with `truncated == true`, it pushes the
warning `<what>: stopped after <cap> pages; results may be incomplete`.
`what` is one of `GitLab authored MRs`, `GitLab reviewer MRs`,
`GitLab events`, `YouTrack assigned issues`, `YouTrack activities` or
`YouTrack changed issues`.

### 1.3 Errors (`error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{service} rejected the token (401 Unauthorized) at {url} — check {hint}")]
    Unauthorized { service: &'static str, url: String, hint: &'static str },
    #[error("{service}: access denied (403) for {url}{hint}")]
    Forbidden { service: &'static str, url: String, hint: String /* "" or "; <advice>" */ },
    #[error("{service}: not found (404): {url}")]
    NotFound { service: &'static str, url: String },
    #[error("{service}: HTTP {status} for {url}: {body}")]
    Http { service: &'static str, status: u16, url: String, body: String /* first 300 bytes, char-safe */ },
    #[error("{service}: request to {url} failed: {source}")]
    Network { service: &'static str, url: String, #[source] source: reqwest::Error },
    #[error("{service}: unexpected response from {url}: {source}")]
    Decode { service: &'static str, url: String, #[source] source: serde_json::Error },
    #[error("{service}: {message}")]
    Other { service: &'static str, message: String },   // e.g. GitLab user "eric" not found
}
impl ApiError {
    /// Some(401|403|404|status of Http); None for Network/Decode/Other.
    pub fn status(&self) -> Option<u16>;
}
```

- `service` is always exactly `"GitLab"` or `"YouTrack"`.
- Unauthorized `hint`: `"[gitlab].token or GITLAB_TOKEN"` /
  `"[youtrack].token or YOUTRACK_TOKEN"`.
- Status mapping is a pure function in `http.rs`:
  `pub fn map_status(service: &'static str, status: u16, url: &str, body: &str) -> ApiError`
  (called only for non-2xx):
  - 401 → `Unauthorized` with that service's hint.
  - 403, GitLab, body contains `insufficient_scope` → `Forbidden` with hint
    `"; the GitLab token lacks the read_api scope (create a token with read_api)"`.
  - 403, GitLab, any other body → `Forbidden` with hint `""`.
  - 403, YouTrack → `Forbidden` with hint
    `"; the token's user lacks permission for this resource (check project access and that the token has the YouTrack scope)"`.
  - 404 → `NotFound`.
  - Any other status → `Http` with `body = text::truncate_bytes(body, 300)`.
- Never put the token in any message or verbose log. URLs never contain
  secrets, because tokens travel only in headers.

```rust
pub struct CliError { pub code: u8, pub err: anyhow::Error }
// CliError::usage(e) -> code 2, CliError::runtime(e) -> code 1,
// impl From<ConfigError> for CliError { code = e.exit_code() }, impl From<WindowError> (code 2).
```

Decode the body as text first, then run `serde_json::from_str`. That way a
decode failure produces `ApiError::Decode` with a URL, and `--verbose` can
print the status and byte count.

---

## 2. Dependencies

```toml
[package]
name = "work-tracker"
version = "0.1.0"
edition = "2024"
rust-version = "1.98"           # the real toolchain; clippy MSRV lints then match what is used

[[bin]]
name = "work-tracker"
path = "src/main.rs"

[dependencies]
anyhow     = "1.0.104"
chrono     = { version = "0.4.45", features = ["serde"] }
clap       = { version = "4.6.7", features = ["derive"] }
console    = "0.16.6"          # same major as dialoguer's dependency; widths, truncation, colors (honours NO_COLOR)
dialoguer  = "0.12.0"          # Select::interact_opt => Enter / Esc / q
open       = "5.4.4"           # open::that_detached(url)
regex      = "1.13.1"
reqwest    = { version = "0.13.5", features = ["blocking", "json", "query"] }
serde      = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror  = "2.0.21"
toml       = "1.1.6"           # toml::from_str works as in 0.8

[dev-dependencies]
chrono-tz = "0.10.4"           # deterministic Europe/Berlin for DST tests
tempfile  = "3.27.0"           # config/init/permission tests
```

Notes:
- reqwest 0.13 made `.query()` opt-in. The `query` feature is required.
  Default features stay on: rustls with the platform verifier, so the system
  CA store works for self-hosted instances with an internal CA.
- No tokio, futures, directories, comfy-table or mock-server crates. HTTP
  logic is tested through pure URL builders and a pure `paginate` helper,
  never against a server.
- TTY detection uses `std::io::IsTerminal` (std).

---

## 3. Configuration

### 3.1 Schema (TOML)

```rust
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct RawConfig {
    gitlab: Option<RawGitLab>,
    youtrack: Option<RawYouTrack>,
    #[serde(default)] meetings: Vec<RawMeeting>,
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct RawGitLab { url: String, #[serde(default)] token: String, username: Option<String> }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct RawYouTrack { url: String, #[serde(default)] token: String,
                     projects: Option<Vec<String>>, state_field: Option<String> }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct RawMeeting { weekday: String, start: String, end: String }
```

Parsed form. `parse_config` builds it and **never checks tokens**:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub path: PathBuf,
    pub gitlab: Option<GitLabSection>,
    pub youtrack: Option<YouTrackSection>,
    pub meetings: Vec<Meeting>,           // window::Meeting
}
#[derive(Clone, PartialEq)]
pub struct GitLabSection { pub url: String /* normalized */, pub token: String /* after env override; may be "" */,
                           pub username: Option<String> }
#[derive(Clone, PartialEq)]
pub struct YouTrackSection { pub url: String, pub token: String /* may be "" */,
                             pub projects: Vec<String> /* trimmed, UPPERCASE, deduped, order kept */,
                             pub state_field: String /* default "State" */ }

// Validated per-run source configs, produced by validate_sources. Tokens are non-empty here.
#[derive(Clone, PartialEq)] pub struct GitLabConfig { pub url: String, pub token: String, pub username: Option<String> }
#[derive(Clone, PartialEq)] pub struct YouTrackConfig { pub url: String, pub token: String,
                                                       pub projects: Vec<String>, pub state_field: String }
#[derive(Debug, Clone, PartialEq)]
pub struct Sources { pub gitlab: Option<GitLabConfig>, pub youtrack: Option<YouTrackConfig>,
                     pub notes: Vec<String> /* e.g. "YouTrack not configured; skipping" */ }
```
The four token-carrying structs implement `Debug` **by hand** and print
`token: "***"` (or `token: ""` when it is empty). Do not derive Debug on
them. A test checks that the `{:?}` output never contains the token.

Validation in `parse_config`. Each failure is `ConfigError::Invalid`
(exit 2), and its message names the field. The path is added by the error's
Display.
- `url` must start with `http://` or `https://`. Trim trailing `/`. For
  GitLab, also strip a trailing `/api/v4` (then trailing `/` again) in case
  the user pasted it. Error: `[gitlab].url: must start with http:// or https://`.
- `weekday`: case-insensitive `mon|tue|wed|thu|fri|sat|sun` or full
  English names (`monday`…).
  Error: `meetings[1].weekday: invalid weekday "tues" (use mon..sun)`.
- `start`/`end`: `NaiveTime::parse_from_str(s, "%H:%M")`. `end > start` is
  required, because meetings cannot cross midnight.
  Errors: `meetings[0].start: invalid time "9am" (use HH:MM)`,
  `meetings[0]: end 09:00 must be after start 10:00`.
- **Overlap:** two meetings on the same weekday whose intervals overlap
  (`a.start < b.end && b.start < a.end`, which includes exact duplicates)
  are rejected:
  `meetings[0] and meetings[2] overlap (tue 09:00–11:00, tue 09:30–09:45)`.
  Back-to-back meetings (`a.end == b.start`) are allowed. This keeps "a
  meeting in progress does not count" unambiguous: no meeting can end while
  another one is still running.
- `projects`: each entry is trimmed and uppercased. An empty string is an
  error: `[youtrack].projects[1]: empty project name`.
- `state_field`: if set, it must be non-empty after trimming.
- An empty `meetings` list is valid. Window computation then fails unless
  `--since` is given (4.3).
- A file with neither `[gitlab]` nor `[youtrack]` parses fine (so `window`
  works). `validate_sources` rejects it.
- Unknown keys fail through `deny_unknown_fields`, and so do TOML syntax
  errors. The message is `invalid config: <toml error>`; the toml error
  includes line and column.

Env overrides happen inside `parse_config` through the injected `env`
closure:
- `GITLAB_TOKEN`, if set and non-empty, replaces `gitlab.token`.
- `YOUTRACK_TOKEN`, if set and non-empty, replaces `youtrack.token`.
- Env tokens only override tokens. They never create a missing section,
  because a URL is required.

`validate_sources(cfg, no_gitlab, no_youtrack) -> Result<Sources, ConfigError>`:
- Source disabled by its `--no-*` flag → `None`, no note.
- Section missing → `None` plus the note `GitLab not configured; skipping` /
  `YouTrack not configured; skipping`.
- Section present but the token is empty after env override →
  `Err(Invalid)`:
  `GitLab token missing: set [gitlab].token in <path> or GITLAB_TOKEN`
  (and the YouTrack equivalent). This holds even when the other source is
  fine.
- Both end up `None` → `Err(Invalid)`:
  `no data source to query: configure [gitlab] and/or [youtrack] in <path>, or drop --no-gitlab/--no-youtrack`.
- `notes` become report warnings.

### 3.2 Loading API

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot determine config dir: neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigDir,
    #[error("No config file at {}. Run 'work-tracker init' to create one.", .0.display())]
    NotFound(PathBuf),
    #[error("Config already exists at {} (use --force to overwrite)", .0.display())]
    AlreadyExists(PathBuf),
    #[error("{}: {msg}", path.display())]
    Invalid { path: PathBuf, msg: String },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, #[source] source: std::io::Error },
}
impl ConfigError { pub fn exit_code(&self) -> u8 { /* AlreadyExists | Io => 1, everything else => 2 */ } }

pub fn default_config_path(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf>;
// xdg set, non-empty, and absolute -> xdg/work-tracker/config.toml
// else home set and non-empty     -> home/.config/work-tracker/config.toml
// else None   (the XDG spec says to ignore a relative XDG_CONFIG_HOME)
pub fn resolve_config_path(cli: Option<&Path>) -> Result<PathBuf, ConfigError>;
// cli path if given, else default_config_path(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME")), None -> NoConfigDir
pub fn insecure_mode(mode: u32) -> bool;                  // (mode & 0o777) != 0o600
pub fn permission_warning(path: &Path) -> Option<String>;  // #[cfg(unix)] via metadata().permissions().mode(); non-unix: None
pub fn parse_config(text: &str, path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Config, ConfigError>;
pub fn load_config(path: &Path) -> Result<(Config, Vec<String> /* warnings */), ConfigError>;
pub fn validate_sources(cfg: &Config, no_gitlab: bool, no_youtrack: bool) -> Result<Sources, ConfigError>;
pub fn init_config(path: &Path, force: bool) -> Result<(), ConfigError>;  // 3.3
pub const TEMPLATE: &str = "...";                                         // 3.3
```

`load_config(path)`:
1. `!path.exists()` → `NotFound(path)`.
2. `permission_warning(path)`. **Rule: warn whenever the permission bits
   are not exactly 0600** (SPEC wording, applied literally). So 0644, 0640,
   0400 and 0700 all warn, and 0600 does not. The text has no `warning: `
   prefix; the printer adds it:
   `<path> has mode 0644 (expected 0600) and contains API tokens; run: chmod 600 <path>`.
3. `fs::read_to_string` (errors → `Io`), then `parse_config(text, path, &|k| env::var(k).ok())`.

Who calls what (`app.rs`):
- **report:** `resolve_config_path` → `load_config` (print its warnings to
  stderr right away as `warning: …`) → `validate_sources` (its notes join the
  report warnings) → window → fetch.
- **window:** `resolve_config_path`. If the file exists, call `load_config`
  (meeting or URL errors fail; tokens are never checked). If it does not
  exist and `--since` was given, continue with no meetings. If it does not
  exist and there is no `--since`, fail with `NotFound` (exit 2).
- **init:** `resolve_config_path` → `init_config(path, force)`.

### 3.3 `work-tracker init [--force]`

- Signature: `pub fn init_config(path: &Path, force: bool) -> Result<(), ConfigError>`.
  `app.rs` passes `resolve_config_path(cli.config)`. Tests pass a path inside
  a `tempfile::TempDir`.
- If the file exists and `force` is false → `ConfigError::AlreadyExists`
  (exit 1).
- Create the parent dirs with `DirBuilder::new().recursive(true).mode(0o700)`.
- Write with `OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)`,
  then `set_permissions(0o600)` explicitly. The second call handles an
  existing file under `--force`.
- `app.rs` (not `init_config`) prints `Created <path> (mode 0600). Edit it to add your GitLab and YouTrack tokens.` to stdout.

Template (write exactly this, a `const TEMPLATE: &str`):

```toml
# work-tracker configuration
# Location: $XDG_CONFIG_HOME/work-tracker/config.toml (default ~/.config/work-tracker/config.toml)
# This file contains API tokens: keep it private (chmod 600).
# Tokens can also be supplied via the GITLAB_TOKEN and YOUTRACK_TOKEN environment variables,
# which take precedence over the values below.

[gitlab]
# Base URL of your (self-hosted) GitLab instance, without /api/v4.
url = "https://gitlab.example.com"
# Personal access token with the read_api scope.
token = ""
# Optional: your GitLab username. Normally resolved automatically from the token.
# username = "eric"

[youtrack]
# Base URL of your YouTrack instance (the REST API is <url>/api).
url = "https://youtrack.uponu.com"
# Permanent token (Profile > Account Security > Tokens), starts with "perm:".
token = ""
# Optional: only consider these project short names. Default: all projects.
# projects = ["SP", "MS", "P"]
# Optional: name of the custom field shown as the issue state. Default: "State".
# state_field = "State"

# Weekly recurring meetings in local time. The report covers the time since the
# end of the most recently finished meeting.
# weekday: mon, tue, wed, thu, fri, sat, sun   start/end: HH:MM (24h)

[[meetings]]
weekday = "tue"
start = "09:00"
end = "10:00"

[[meetings]]
weekday = "thu"
start = "14:00"
end = "15:00"
```

The template must parse with `toml::from_str::<RawConfig>` and with
`parse_config` (add unit tests). `validate_sources` on it fails with the
GitLab token-missing error, because the tokens are empty, which is intended.

---

## 4. Time window (`window.rs`)

### 4.1 Types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meeting { pub weekday: Weekday, pub start: NaiveTime, pub end: NaiveTime }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRequest {
    pub now: DateTime<Utc>,             // Utc::now() or --now
    pub since: Option<DateTime<Utc>>,   // --since
    pub until: Option<DateTime<Utc>>,   // --until
    pub previous: u32,                  // --previous count (0 = current window)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOrigin {
    Meeting { meeting: Meeting, ended_at: DateTime<Utc>, previous: u32 },
    Explicit,                            // --since given
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window { pub start: DateTime<Utc>, pub end: DateTime<Utc>, pub origin: WindowOrigin }
impl Window { pub fn contains(&self, t: DateTime<Utc>) -> bool { self.start <= t && t <= self.end } }

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WindowError {
    #[error("no meetings configured; add [[meetings]] to the config or pass --since")] NoMeetings,
    #[error("--since ({0}) must be before --until ({1})")] SinceNotBeforeUntil(String, String),
    #[error("--since ({0}) must be before now ({1})")] SinceNotBeforeNow(String, String),
    #[error("invalid date/time \"{0}\"; accepted: 2026-09-29T10:00, \"2026-09-29 10:00\", 2026-09-29, or RFC 3339 (2026-09-29T10:00:00+02:00)")]
    BadDateTime(String),
}
// The two String fields of SinceNot* are local times formatted "%Y-%m-%d %H:%M".

pub fn compute<Tz: TimeZone>(tz: &Tz, meetings: &[Meeting], req: &WindowRequest) -> Result<Window, WindowError>;
pub fn parse_user_datetime<Tz: TimeZone>(tz: &Tz, s: &str) -> Result<DateTime<Utc>, WindowError>;
pub fn resolve_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> DateTime<Tz>;
pub const PAD_DAYS: i64 = 2;
/// (UTC date of w.start − PAD_DAYS, UTC date of w.end + PAD_DAYS). Used by GitLab events (5.2) and YouTrack queries (6.2/6.3).
pub fn padded_dates(w: &Window) -> (NaiveDate, NaiveDate);
pub fn format_window_line<Tz: TimeZone>(tz: &Tz, w: &Window, now: DateTime<Utc>) -> String
    where Tz::Offset: std::fmt::Display;                                 // 4.6
```

Production calls these with `&chrono::Local`. Tests call them with
`&chrono_tz::Europe::Berlin`. Do not set `TZ` env vars in tests, because they
race across threads. Setting `TZ` on a spawned child process, as
`tests/cli.rs` does, is fine.

### 4.2 Local-time resolution (DST)

```
resolve_local(tz, naive):
  match tz.from_local_datetime(&naive):
    Single(t)        -> t
    Ambiguous(a, b)  -> earliest of a, b        # fall-back hour: first occurrence
    None (DST gap)   ->
       if tz.from_local_datetime(naive + 1h).earliest() is Some(t) -> t   # 02:30 -> 03:30 (gap length 1h)
       else for k in 1..=16: try naive + k*15min, return first .earliest()  # exotic gaps
       else unreachable -> treat naive as UTC (cannot happen with real tz data)
```

Each candidate meeting end is computed by combining a **local calendar date**
with the configured **local wall time** and resolving it. That makes the
computation DST-correct by construction. Never add `Duration::days(7)` to an
instant.

### 4.3 Algorithm

```
finished_meeting_ends(tz, meetings, now_utc, needed: usize) -> Vec<(DateTime<Utc>, Meeting)>:
    now_local  = now_utc.with_timezone(tz)
    today      = now_local.date_naive()
    max_back   = 7 * (needed as i64) + 7          # always yields >= needed candidates
    cands = []
    for d in 0..=max_back:
        date = today - d days
        for m in meetings where m.weekday == date.weekday():
            end_utc = resolve_local(tz, date.and_time(m.end)).with_timezone(&Utc)
            if end_utc <= now_utc:                # a meeting in progress (start <= now < end) is NOT finished
                cands.push((end_utc, m))
    sort cands by end_utc descending; dedup by end_utc (two meetings ending at the same instant)
    return cands

compute(tz, meetings, req):
    if let Some(since) = req.since:
        match req.until:
            Some(until) -> if since >= until: Err(SinceNotBeforeUntil(fmt(since), fmt(until))); end = until
            None        -> if since >= req.now: Err(SinceNotBeforeNow(fmt(since), fmt(req.now))); end = req.now
        return Window { start: since, end, origin: Explicit }      # explicit windows are never zero-length
    if meetings.is_empty(): Err(NoMeetings)
    anchor = req.until.unwrap_or(req.now)         # --until alone = "as if now were --until"
    k = req.previous as usize
    cands = finished_meeting_ends(tz, meetings, anchor, k + 1)
    start = cands[k].0
    end   = if k == 0 { anchor } else { cands[k - 1].0 }
    Window { start, end, origin: Meeting { meeting: cands[k].1, ended_at: start, previous: req.previous } }
```

Semantics:
- **Boundary rule:** a meeting counts as finished when `end <= now`. At
  exactly the end instant the window is zero-length (`start == end`). That is
  allowed.
- **`--previous` (count; `--previous --previous` or `-pp` = 2):** shifts by
  whole meeting intervals, so windows are contiguous. The current window is
  `[M1.end, now]`, `--previous` gives `[M2.end, M1.end]`, and
  `--previous ×2` gives `[M3.end, M2.end]`. Here M1 is the most recently
  finished meeting.
- `--previous` conflicts with `--since` (clap `conflicts_with`). It is
  allowed with `--until` and `--now`.
- A single configured meeting yields a full-week window (the same weekday one
  week earlier).
- Overlapping meetings are rejected at config validation (3.1), so
  `finished_meeting_ends` never has to reason about one meeting ending inside
  another one's running interval.
- `--now` replaces `Utc::now()` everywhere, including GitLab and YouTrack
  window filtering.

### 4.4 Parsing user datetimes (`--since/--until/--now`)

Try these in order:
1. `DateTime::parse_from_rfc3339(s)`, converted to Utc.
2. Naive formats, resolved with `resolve_local(tz, …)`:
   `%Y-%m-%dT%H:%M:%S`, `%Y-%m-%dT%H:%M`, `%Y-%m-%d %H:%M:%S`, `%Y-%m-%d %H:%M`.
3. `%Y-%m-%d` (NaiveDate) at 00:00 local.

Otherwise return `BadDateTime`. Every window error is a usage error (exit 2).
Surrounding whitespace is trimmed before parsing.

### 4.5 Required test cases (tz = Europe/Berlin)

Reference calendar: Thu 2026-09-24, Tue 2026-09-29, Wed 2026-09-30,
Thu 2026-10-01, Fri 2026-10-02. DST ends Sun 2026-10-25 (03:00 CEST → 02:00
CET). DST starts Sun 2026-03-29 (02:00 CET → 03:00 CEST).
Meetings M = [Tue 09:00–10:00, Thu 14:00–15:00] unless stated otherwise.
Write the expected values as Berlin local times converted to UTC in the
assertions. Also add one assertion on literal UTC per DST test.

| # | now (local) | config / flags | expected start | expected end |
|---|---|---|---|---|
| 1 | Wed 09-30 11:00 | M | Tue 09-29 10:00 | Wed 09-30 11:00 |
| 2 | Tue 09-29 08:30 | M | Thu 09-24 15:00 | Tue 09-29 08:30 |
| 3 | Tue 09-29 09:30 (running) | M | Thu 09-24 15:00 | Tue 09-29 09:30 |
| 4 | Tue 09-29 10:30 | M | Tue 09-29 10:00 | Tue 09-29 10:30 |
| 5 | Thu 10-01 14:30 (running) | M | Tue 09-29 10:00 | Thu 10-01 14:30 |
| 6 | Thu 10-01 15:00:00 exactly | M | Thu 10-01 15:00 | Thu 10-01 15:00 (zero-length) |
| 7 | Mon 09-28 12:00 (week wrap) | M | Thu 09-24 15:00 | Mon 09-28 12:00 |
| 8 | Sun 10-04 18:00 (week wrap) | M | Thu 10-01 15:00 | Sun 10-04 18:00 |
| 9 | Tue 09-29 09:30 | only Tue 09–10 | Tue 09-22 10:00 | Tue 09-29 09:30 |
| 10 | Wed 09-30 11:00 | only Tue 09–10 | Tue 09-29 10:00 | Wed 09-30 11:00 |
| 11 | Wed 09-30 11:00 | M, previous=1 | Thu 09-24 15:00 | Tue 09-29 10:00 |
| 12 | Wed 09-30 11:00 | M, previous=2 | Tue 09-22 10:00 | Thu 09-24 15:00 |
| 13 | Tue 10-27 08:30 CET | M (DST end in between) | Thu 10-22 15:00 CEST = **13:00Z** | **07:30Z** |
| 14 | Tue 03-31 08:30 CEST | M (DST start in between) | Thu 03-26 15:00 CET = **14:00Z** | **06:30Z** |
| 15 | Sun 03-29 12:00 | only Sun 02:00–02:30 (end in gap) | 03:30 CEST = **01:30Z** | now |
| 16 | Sun 10-25 12:00 | only Sun 02:00–02:30 (ambiguous) | earliest 02:30 CEST = **00:30Z** | now |
| 17 | Fri 10-02 12:00 | `--since 2026-09-28T08:00` | Mon 09-28 08:00 | Fri 10-02 12:00 |
| 18 | Fri 10-02 12:00 | `--since 2026-09-28 --until 2026-09-29T12:00` | Mon 09-28 00:00 | Tue 09-29 12:00 |
| 19 | Fri 10-02 12:00 (ignored) | `--until 2026-09-29T08:30` only | Thu 09-24 15:00 | Tue 09-29 08:30 |
| 20 | Fri 10-02 12:00 | `--since 2026-09-29T12:00 --until 2026-09-29T08:00` | Err(SinceNotBeforeUntil) | |
| 21 | Fri 10-02 12:00 | no meetings, no since | Err(NoMeetings) | |
| 22 | Fri 10-02 12:00 | no meetings, `--since 2026-09-28T08:00` | Mon 09-28 08:00 (Explicit) | Fri 10-02 12:00 |
| 23 | Fri 10-02 12:00 | `--since 2026-10-03T00:00`, no `--until` | Err(SinceNotBeforeNow) | |
| 24 | Fri 10-02 12:00 (ignored) | M, previous=1, `--until 2026-09-30T11:00` | Thu 09-24 15:00 | Tue 09-29 10:00 |

For the error cases, also assert the message text. Case 23 must say
`must be before now`, not `--until`.

Parser tests: each accepted format, an RFC 3339 string with an offset, the
garbage strings `"yesterday"` and `"2026-13-01"`, and a naive time inside the
DST gap (resolved forward).

### 4.6 Window line (`format_window_line`)

Used by the report header (8.2) and by the `window` command (9):

```
Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)
```
- Both timestamps are local, formatted `%a %d %b %H:%M`. A timestamp whose
  local year differs from `now`'s local year gets the prefix `%Y ` (for
  example `2026 Thu 31 Dec 15:00`).
- Two spaces, then the parenthetical: the duration `end − start` is
  `{d}d {h}h` when ≥ 1 day, otherwise `{h}h {m}m` (zero-length: `0h 0m`),
  followed by `, ` and the origin:
  - `since <Wd> HH:MM–HH:MM meeting` (Meeting origin, previous = 0)
  - `previous ×N, since <Wd> HH:MM–HH:MM meeting` (N ≥ 1; the meeting whose
    end is the window start)
  - `explicit range` (Explicit)
  `<Wd>` is `%a` (Tue). The dash is U+2013, the arrow U+2192, the times sign U+00D7.
- Tests: each origin, a zero-length window, the year prefix (now in 2027,
  start in 2026), and a duration under one day (`0h 30m`).

---

## 5. GitLab plan

Base: `{url}/api/v4`. Header `PRIVATE-TOKEN: <token>`. Header
`User-Agent: work-tracker/<version>`. Client timeout 30 s.
Times are sent as RFC 3339 UTC (`2026-09-29T08:00:00Z`, format via
`to_rfc3339_opts(SecondsFormat::Secs, true)`).
`GitLabClient::new(cfg: &GitLabConfig, verbose: bool) -> Result<GitLabClient, ApiError>`.
With `verbose`, every request logs `GET <url> -> <status> (<n> items, <bytes> B)` to stderr.
Every URL comes from a pure builder fn in `client.rs`
(`fn mr_list_url(base, q: &MrQuery, page: u32) -> Url`, `fn events_url(base, uid, after, before, page) -> Url`,
`fn project_mrs_url(base, pid, iids) -> Url`, `fn user_url(base)`, `fn users_by_username_url(base, u)`).
Each builder has a test.

### 5.1 Identity

- When `username` is configured: `users_by_username(u)` (`GET /users?username=<u>`).
  Take `[0]`. An empty array → `ApiError::Other { service: "GitLab", message: "user \"<u>\" not found" }`,
  which fails the GitLab source.
- Otherwise: `GET /user`, giving `{ id, username }`.
- A 401 here becomes `ApiError::Unauthorized` and fails the GitLab source.

### 5.2 Queries (run in parallel inside the GitLab thread with a nested `thread::scope`, or sequentially; either is fine)

A. **Authored:** `GET /merge_requests?scope=all&state=all&author_id={uid}&updated_after={W.start}&order_by=updated_at&sort=desc&per_page=100&page={n}`

B. **Reviewer:** `GET /merge_requests?scope=all&state=all&reviewer_id={uid}&updated_after={W.start}&order_by=updated_at&sort=desc&per_page=100&page={n}`

C. **My events:** `GET /users/{uid}/events?after={A}&before={B}&sort=desc&per_page=100&page={n}`
   with `(A, B) = window::padded_dates(W)`, i.e. the UTC date of W.start
   **minus 2 days** and the UTC date of W.end **plus 2 days**, formatted `%Y-%m-%d`.
   Why 2 days: GitLab's EventsFinder filters with
   `created_at > after.end_of_day` and `created_at < before.beginning_of_day`,
   evaluated in the **GitLab server's configured time zone** (anywhere from
   UTC−12 to UTC+14), not in UTC. The server-local date can differ from the
   UTC date by one day, and both bounds exclude whole days, so one day of
   padding loses events. Example: server Europe/Berlin, run at 01:30 local
   on day D+1. With `before = D+1` the cutoff is D 22:00Z, which drops the
   last 1.5 hours. With 2 days of padding, the `after` cutoff is at the
   latest (S−2) 23:59:59 at UTC−12 = (S−1) 11:59:59Z, which is before
   W.start. The `before` cutoff is at the earliest (E+2) 00:00 at UTC+14 =
   (E+1) 10:00Z, which is after W.end.
   Afterwards keep only events with `W.contains(created_at)`, the exact
   filter, so the padding costs only a few extra events. Do not pass
   `target_type`, because one query then covers MR events, note events and
   push events.

`scope=all` is mandatory, because the `/merge_requests` default is
`created_by_me`. `updated_before` is deliberately **not** sent (see 5.4).

### 5.3 Pagination (`http.rs`)

```rust
pub fn paginate<T>(max_pages: u32,
                   mut fetch_page: impl FnMut(u32 /* 0-based page index */) -> Result<Page<T>, ApiError>)
    -> Result<Listing<T>, ApiError>
// for i in 0..max_pages { let p = fetch_page(i)?; items.extend(p.items); if !p.has_more { return Ok(Listing{items, truncated: false}) } }
// Ok(Listing { items, truncated: true })      // the cap was reached and the last page still had more
// The first error aborts and is returned. A query never yields partial results.
```
Each client maps the page index to its request:
- **GitLab:** `page = i + 1`, `per_page = 100`. Compute `has_more` with the
  pure fn `pub fn gitlab_has_more(next_page_header: Option<&str>, n_items: usize, per_page: usize) -> bool`:
  header present → `!value.trim().is_empty()`. Header absent (some proxies
  strip it) → `n_items == per_page`.
- **YouTrack:** `$top = T`, `$skip = i * T`, with T = 100 for issue searches
  and T = 200 for activities. `has_more = n_items == T`.

Page caps: GitLab MR queries 20, GitLab events 30, YouTrack issue search 20,
YouTrack activities 25. A truncated listing produces the warning from 1.2.

### 5.4 Inclusion rules (pure function in `gitlab/collect.rs`)

Let `ev(mr)` be the set of my in-window events attributed to the MR (5.5).
An MR is included in the report when **any** of these holds:
1. `author.id == uid` and (`W.contains(created_at)` or `W.contains(updated_at)` or `ev(mr)` is non-empty, or an in-window push to its source branch exists). Role: **Author**.
2. `uid ∈ reviewers[].id` and (`W.contains(updated_at)` or `ev(mr)` is non-empty). Role: **Reviewer**.
3. `ev(mr)` is non-empty. Roles come from the event classification.

Roles accumulate in a `BTreeSet`. For example, an author who also commented
gets {author, commenter}.

Known limitation (document it in README): with `--previous` or `--until`, an
MR that was touched in the window and then again after it is found only
through rule 1 (created in window), an event, or a push. GitLab cannot filter
"updated within a past interval" without also dropping such MRs. YouTrack
assigned issues have the same limitation (6.2).

When the MR listing for A or B is truncated, or the events are, push the
warning from 1.2. `collect` fails the whole GitLab source (returns `Err`) on
any error from identity, A, B or C. Errors in 5.6 only produce warnings.

### 5.5 Event classification (pure)

```rust
pub enum EventHit { Mr { key: (u64, u64), role: Role }, Push { project_id: u64, branch: String } }
pub fn classify(ev: &GlEvent) -> Option<EventHit>
```
- `target_type == "MergeRequest"` with `project_id` and `target_iid`:
  `action_name` "approved" → Approver. "opened"/"created" → Author.
  "accepted"/"merged" → Merger. Anything else ("closed", "reopened",
  "updated"…) → Participant.
- `target_type ∈ {"Note","DiffNote","DiscussionNote"}` and
  `note.noteable_type == "MergeRequest"` and `note.noteable_iid` → Commenter,
  key `(project_id, note.noteable_iid)`. Use `noteable_iid`, not `target_iid`.
- `push_data.ref` with `push_data.ref_type == "branch"` (action
  "pushed to"/"pushed new") → Push. It is used only to mark activity on
  already-fetched authored MRs by matching `(project_id, source_branch)`.
- Everything else → None.

### 5.6 MRs known only from events

Collect the `(project_id, iid)` keys from events that are not in A ∪ B.
Group them by project and chunk them into groups of 20:
`GET /projects/{pid}/merge_requests?state=all&per_page=100&iids[]=1&iids[]=2…`
(reqwest: `.query(&[("iids[]", "1"), ("iids[]", "2")])`).
Cap the total at 200 keys and warn when the cap is exceeded. A 403 or 404 for
a project produces a warning (`GitLab: cannot read project <pid> (403); skipping 3 MRs`)
and is not fatal.

### 5.7 DTOs and conversion

```rust
#[derive(Deserialize)] pub struct GlUser { pub id: u64, pub username: String }
#[derive(Deserialize)] pub struct GlUserRef { pub id: u64, pub username: String }
#[derive(Deserialize)] pub struct GlReferences { pub full: String }        // "group/proj!123"
#[derive(Deserialize)] pub struct GlMergeRequest {
    pub id: u64, pub iid: u64, pub project_id: u64, pub title: String,
    pub description: Option<String>, pub state: String,
    #[serde(default)] pub draft: bool, pub source_branch: String, pub web_url: String,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc>,
    pub author: GlUserRef, #[serde(default)] pub reviewers: Vec<GlUserRef>,
    pub references: Option<GlReferences>,
}
#[derive(Deserialize)] pub struct GlEvent {
    pub action_name: String, pub target_type: Option<String>, pub target_iid: Option<u64>,
    pub target_title: Option<String>, pub project_id: Option<u64>, pub created_at: DateTime<Utc>,
    pub note: Option<GlNote>, pub push_data: Option<GlPushData>,
}
#[derive(Deserialize)] pub struct GlNote { pub noteable_type: Option<String>, pub noteable_iid: Option<u64> }
#[derive(Deserialize)] pub struct GlPushData { #[serde(rename = "ref")] pub git_ref: Option<String>, pub ref_type: Option<String> }
```
- Do not use `deny_unknown_fields` on API DTOs.
- `project_path`: `references.full` up to the last `!`. Fallback: the
  `web_url` path between the host and `/-/merge_requests`.
- `MrState` mapping as in 1.1. `web_url` is used verbatim for opening.
- Dedup A, B and the extras by `(project_id, iid)` with a
  `BTreeMap<(u64,u64), MergeRequest>`, merging roles.

`pub fn collect(api: &dyn GitLabApi, username: Option<&str>, window: &Window) -> Result<GitLabOutcome, ApiError>`
`pub struct GitLabOutcome { pub user: GlUser, pub mrs: Vec<MergeRequest>, pub warnings: Vec<String> }`.
All DTOs derive `Debug, Clone, Deserialize`, because fakes clone them.
At this stage `issue_ids` stays empty. It is filled in `app.rs` after the
known project names are available (section 7).

---

## 6. YouTrack plan

Base `{url}/api`. Headers: `Authorization: Bearer <token>`,
`Accept: application/json`, and User-Agent. Timeout 30 s.
`YouTrackClient::new(cfg: &YouTrackConfig, verbose: bool) -> Result<YouTrackClient, ApiError>`.
The verbose log matches GitLab's. URLs come from pure builder fns in
`client.rs` (`issues_search_url(base, query, top, skip)`, `issue_url(base, id)`,
`activities_url(base, q: &ActivityQuery, top, skip)`, `admin_projects_url(base)`),
and each has a test.

Settings passed to `collect` (built by `app.rs` from the `[youtrack]` section):
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct YtSettings { pub base_url: String, pub projects: Vec<String>, pub state_field: String }
impl Default for YtSettings { /* base_url "", projects [], state_field "State" */ }
pub fn project_clause(projects: &[String]) -> Option<String>;
// []          -> None
// ["SP"]      -> Some("project: SP")
// ["SP","MS"] -> Some("(project: SP or project: MS)")   (explicit `or`: documented syntax; no reliance on comma lists)
```

Constant field selector:
```
ISSUE_FIELDS = "idReadable,summary,created,updated,resolved,project(shortName),customFields(name,value(name,login,fullName))"
```
YouTrack returns `updated`/`resolved` as Unix epoch milliseconds (UTC).
Unknown fields such as `$type` are ignored by serde.

### 6.1 Project short names (for matching)

`GET /api/admin/projects?fields=shortName,archived&$top=1000` returns every
project the token can see. Archived projects are included, because old MRs
may reference them. Names are uppercased. On a 401 the YouTrack source fails,
as it does for any other call. On any other error, push the warning
`YouTrack: cannot list projects (<err>); issue-ID matching uses the configured projects`
and set `YouTrackOutcome.admin_projects = None`. The known set used for
matching is computed in exactly one place, `report::known_projects` (7.3).

`collect` call order: (1) 6.1, (2) 6.2, (3) 6.3 including its fallbacks,
(4) `fetch_by_ids` for activity IDs that 6.2 did not return. Errors from
(2), from the last fallback step of (3), and any 401 fail the source.
Errors from (1), and per-ID errors in (4), only produce warnings.

### 6.2 Issues assigned to me (SPEC item 3)

`GET /api/issues?query={Q}&fields={ISSUE_FIELDS}&$top=100&$skip={k}`

```
Q = [project_clause " "] "assignee: me updated: {d0} .. {d1}"
(d0, d1) = window::padded_dates(W)        formatted %Y-%m-%d   (±2 days, the same helper as GitLab)
```
Examples (W = 2026-09-29T08:00Z .. 2026-09-30T09:00Z):
`assignee: me updated: 2026-09-27 .. 2026-10-02` and
`(project: SP or project: MS) assignee: me updated: 2026-09-27 .. 2026-10-02`.

Rationale: YouTrack interprets date literals in the *user profile's*
timezone, which the tool cannot know. YouTrack ranges include whole days at
both ends, so ±1 day would be enough. One shared ±2-day helper keeps both
sources on one rule. The exact filter runs client-side: keep only issues with
`W.contains(updated)`. Role: **Assignee**. Pagination per 5.3 (cap 20 pages).

Known limitation (README): `updated` is the *last* update time. For a past
window (`--previous`/`--until`), an assigned issue updated inside the window
and again later fails the client-side filter. It is still found if I
authored an activity on it inside the window (6.3). This matches the GitLab
caveat in 5.4. No extra query is made for it.

### 6.3 Issues I changed or commented on (SPEC item 4)

The query language was checked and is **insufficient**. `updater:` means
"*last* updated by", and `commenter:` / `commented:` cannot be tied
together ("my comment *within* the range"). So use the activities API, which
does exactly this:

```
GET /api/activities
    ?categories={categories joined with ","}
    &author=me
    &start={W.start epoch ms}
    &end={W.end epoch ms}
    &reverse=true
    [&issueQuery={project_clause}]          # only when settings.projects is non-empty
    &fields=timestamp,category(id),target(idReadable,issue(idReadable))
    &$top=200&$skip={k}                     # cap 25 pages
```

Category sets (decided; `pub const` slices in `youtrack/mod.rs`):
- `FULL_CATEGORIES` = CommentsCategory, CommentTextCategory,
  CommentAttachmentsCategory, CustomFieldCategory, SummaryCategory,
  DescriptionCategory, IssueCreatedCategory, IssueResolvedCategory,
  ProjectCategory, LinksCategory, AttachmentsCategory,
  AttachmentRenameCategory, TagsCategory, SprintCategory, WorkItemCategory.
- `CORE_CATEGORIES` = CommentsCategory, CustomFieldCategory,
  SummaryCategory, DescriptionCategory, IssueCreatedCategory.
- Deliberately excluded: VcsChangeCategory and VcsChangeStateCategory
  (written by the VCS integration, and the MR already represents that work),
  plus vote, star, reaction, visibility and markdown-flag categories, which
  are not work.

Roles: CommentsCategory, CommentTextCategory and CommentAttachmentsCategory
→ **Commenter**. Any other or missing category → **Updater**.

Target resolution: the target type depends on the category. For comments it
is an `IssueComment` with `issue.idReadable`. For field, summary or creation
changes it is the `Issue` with `idReadable`. Deserialize both as optional.
The issue ID is `target.idReadable.or(target.issue.idReadable)`, parsed as
`IssueId`. Skip activities with no parseable ID. Keep only
`W.contains(timestamp)` as a defensive check.

Fallback chain. Each step is its own function, so fakes can drive every path:
1. `my_activities` with `FULL_CATEGORIES`.
2. If step 1 fails with `status() == Some(400)`: push the warning
   `YouTrack: server rejected some activity categories; retrying with the core set`
   and retry with `CORE_CATEGORIES`.
3. If step 1 fails with 404, or step 2 fails with 400 or 404: push the
   warning `YouTrack: activities API unavailable (HTTP <status>); using approximate updater/commenter query`
   and run `search_issues` with
   `Q = [project_clause " "] "(updater: me or commenter: me) updated: {d0} .. {d1}"`,
   keeping only `W.contains(updated)`. Those issues get roles {Updater}
   (approximate), are complete already, and skip step 4. The truncation
   warning label is `YouTrack changed issues`.
4. Any other error at any step (401, 403, 5xx, network, decode) is returned
   and fails the YouTrack source.

After activities succeed: activity IDs that 6.2 already returned merge their
roles into those issues. The remaining IDs are fetched with `fetch_by_ids`
(6.4) and become issues with `in_window = true` and the activity roles.
IDs the fetch reports as missing are dropped silently. Aliases from that
fetch are ignored here, because activity IDs are already canonical.

### 6.4 Fetching issues by ID (`fetch_by_ids`, in `youtrack/collect.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FetchedById {
    pub issues: Vec<YtIssue>,                          // deduped by canonical id
    pub aliases: BTreeMap<IssueId, IssueId>,           // requested -> canonical (moved/renamed issues)
    pub missing: Vec<IssueId>,                         // not found (404) or failed
}
pub fn fetch_by_ids(api: &dyn YouTrackApi, ids: &[IssueId], warnings: &mut Vec<String>)
    -> Result<FetchedById, ApiError>
```
Process the IDs in chunks of **20**:
1. **Batch:** `search_issues("issue id: SP-1 or issue id: SP-2 or …")`. The
   clauses are single-value and joined with an explicit `or`, because the
   docs only show the single-value form of `issue id:`. The batch is just an
   optimisation, and its result is never trusted blindly:
   - Keep only results whose `idReadable` parses to an `IssueId` that is in
     the requested chunk. Discard everything else, because YouTrack may treat
     an unresolvable value as full-text search and return unrelated issues.
   - Batch error with 401 → return the error. Any other batch error → treat
     the whole chunk as not returned (verbose log only, no warning).
2. **Per ID:** fetch every requested ID that step 1 did not return with
   `issue_by_id`, **whatever the batch's HTTP status was**.
   - `Ok(None)` (404) → `missing`, no warning, because the ID may be a false
     positive like `FIX-12`.
   - `Ok(Some(i))` whose idReadable equals the requested ID
     (case-insensitive) → keep it.
   - `Ok(Some(i))` with a **different** idReadable (a moved or renamed
     issue; YouTrack resolves the old ID) → keep it and record
     `aliases[requested] = canonical`.
   - 401 → return the error. Any other error → warning
     `YouTrack: cannot fetch <id> (<err>)`, and the ID goes to `missing`.
   - At most 50 per-ID requests per `fetch_by_ids` call. Beyond that, push
     `YouTrack: too many issues to fetch individually; skipped <n>` and move
     the rest to `missing`.
3. Dedupe `issues` by canonical ID.

Uses: activity IDs (6.3) and MR-referenced IDs (7.4).

```rust
pub fn fetch_referenced(api: &dyn YouTrackApi, ids: &[IssueId], settings: &YtSettings, warnings: &mut Vec<String>)
    -> Result<(Vec<Issue>, BTreeMap<IssueId, IssueId>), ApiError>
```
It takes the first 50 IDs (above 50, warn
`YouTrack: <n> issues referenced by MRs; fetching the first 50`), calls
`fetch_by_ids`, and converts the results to
`Issue { roles: {Referenced}, in_window: false }`. It returns the issues and
the alias map.

### 6.5 Conversion

```rust
#[derive(Deserialize)] pub struct YtIssue {
    #[serde(rename = "idReadable")] pub id_readable: String,
    pub summary: Option<String>, pub updated: Option<i64>, pub resolved: Option<i64>,
    #[serde(rename = "customFields", default)] pub custom_fields: Vec<YtCustomField>,
}
#[derive(Deserialize)] pub struct YtCustomField { pub name: String, pub value: Option<serde_json::Value> }
#[derive(Deserialize)] pub struct YtActivity { pub timestamp: i64, pub category: Option<YtCategory>, pub target: Option<YtTarget> }
#[derive(Deserialize)] pub struct YtCategory { pub id: String }
#[derive(Deserialize)] pub struct YtTarget { #[serde(rename = "idReadable")] pub id_readable: Option<String>, pub issue: Option<YtIssueRef> }
#[derive(Deserialize)] pub struct YtIssueRef { #[serde(rename = "idReadable")] pub id_readable: String }
```
- State: find the custom field whose `name` matches `state_field` (default
  "State", case-insensitive). Read the value as follows: object → `.name`;
  array → names joined with ", "; string → itself; null or missing →
  `"Resolved"` if `resolved.is_some()` else `"Open"`.
- `web_url = format!("{yt_url}/issue/{idReadable}")`.
- `updated`: ms converted to `DateTime<Utc>` (`DateTime::from_timestamp_millis`). Missing → `UNIX_EPOCH`.
- Dedup by `IssueId`, merging roles.

`pub fn collect(api: &dyn YouTrackApi, window: &Window, settings: &YtSettings) -> Result<YouTrackOutcome, ApiError>`
`pub struct YouTrackOutcome { pub issues: Vec<Issue>, pub admin_projects: Option<BTreeSet<String>>, pub warnings: Vec<String> }`.
`admin_projects` is the result of 6.1, or `None` if that call failed.
Conversion: `pub fn convert_issue(yt: &YtIssue, settings: &YtSettings, roles: BTreeSet<Role>, in_window: bool) -> Option<Issue>`
returns `None` when `idReadable` does not parse as an `IssueId`, and the caller skips that issue.
All DTOs derive `Debug, Clone, Deserialize`.

---

## 7. Matching (`issue_ref.rs`, `report.rs`)

### 7.1 Regex and boundary rules

```rust
static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[a-z][a-z0-9]{0,19}-[0-9]{1,7}").unwrap());

pub fn extract_issue_ids(text: &str) -> Vec<IssueId>      // raw: boundary rules only, no project filter
```
The project part allows **letters and digits only, no `_`**. This is a
decision: the owner's project short names (SP, MS, P) are alphanumeric, and
allowing `_` would let `feature_SP-123` match as `FEATURE_SP-123`, which hides
`SP-123`. Short names that contain `_` are not supported (README limitation).

For each match `m` in `RE.find_iter(text)`:
1. **Left boundary:** the character before `m.start()` (if any) must not be
   ASCII alphanumeric. `_`, `-`, `/`, `(`, `"`, whitespace and so on are
   allowed. This rejects UUID fragments: in `3f2a-4b1c` the `f2a-4` hit is
   preceded by `3`.
2. **Right boundary:** the character after `m.end()` (if any) must not be
   ASCII alphanumeric. Digits are greedy, so a following digit is impossible
   except past the 7-digit limit (`SP-12345678` is rejected). A following
   letter is rejected: `ab-12cd`, `a1b2-3cd4`. `-`, `_`, `/`, `.`, `:`, `)`,
   whitespace and so on are allowed, so branch names like `SP-123-fix-login`
   and `sp-123_fix` match.
3. **Separators before the ID:** a preceding `-` or `_` is allowed. In
   `bugfix-SP-12` the leftmost-first engine first tries `bugfix-S…`, which
   fails because `-` must be followed by digits, so it then finds `SP-12`.
   `feature_SP-123`, `eric_sp-12-fix` and `1234_SP-5` yield `SP-123`, `SP-12`
   and `SP-5`. `fix-12-SP-3` yields both `FIX-12` and `SP-3`, and the project
   filter (7.3) removes `FIX`.
4. A glued alphanumeric prefix is part of the match, not a boundary:
   `x1SP-2` matches as `X1SP-2` (never `SP-2`), and the project filter drops
   it. A rejected match never hides a valid one. A left-rejected match
   starts mid-word, so any sub-match would too. A right-rejected match ends
   before a letter or digit, so any sub-match ending earlier would be
   followed by a digit.
5. Parse the number as `u32`. Zero is rejected.
6. Normalize: uppercase the project to get an `IssueId`. Dedup while keeping
   first-occurrence order.

Version and encoding strings (`UTF-8`, `ISO-8601`, `x86-64`, `sha-256`,
`rc-1`) match the regex syntactically and are removed by the project filter.

### 7.2 Sources

`mr_issue_ids(mr: &MergeRequest, known: Option<&BTreeSet<String>>) -> Vec<IssueId>`:
order is title, then `source_branch`, then description, deduped across
sources. In the description:
- Skip lines inside fenced code blocks (a line whose trimmed start is
  ```` ``` ```` or `~~~` toggles the fence).
- Only the first 64 KiB is scanned: `text::truncate_bytes(desc, 65_536)`.
  `pub fn truncate_bytes(s: &str, max: usize) -> &str` returns `s` when
  `s.len() <= max`. Otherwise it walks `max` back until `s.is_char_boundary(i)`
  and returns `&s[..i]`. Never slice blindly: `&desc[..65536]` panics inside
  a multi-byte character.
- `text::sanitize_line(s: &str) -> String` replaces every `char::is_control`
  character (`\n`, `\r`, `\t`, ESC, …) with a space. All UI labels use it
  (8.3).
- URLs like `https://youtrack.uponu.com/issue/SP-123` match naturally,
  because the preceding char is `/`.

### 7.3 Project filter and known set

```rust
pub fn known_projects(admin: Option<&BTreeSet<String>>, configured: &[String]) -> Option<BTreeSet<String>>  // report.rs
pub fn mr_issue_ids(mr: &MergeRequest, known: Option<&BTreeSet<String>>) -> Vec<IssueId>   // issue_ref.rs: 7.2 + this filter
pub fn filter_known(ids: Vec<IssueId>, known: Option<&BTreeSet<String>>) -> Vec<IssueId>  // issue_ref.rs
```

| admin (6.1 result) | configured `[youtrack].projects` | known |
|---|---|---|
| `Some(A)` | empty | `Some(A)` |
| `Some(A)` | `C` non-empty | `Some(A ∩ C)` |
| `None` (call failed, or YouTrack disabled / skipped / failed) | `C` non-empty | `Some(C)` |
| `None` | empty | `None` |

`configured` comes from the `[youtrack]` section whenever that section
exists, even with `--no-youtrack`. So `--no-youtrack` and a failed admin call
behave the same.

- `known = Some(set)`: keep IDs whose `project ∈ set`.
- `known = None`: keep everything except the denylist
  `{UTF, ISO, SHA, RFC, CVE, X86, X64, ARM, AES, RSA, HTTP, TLS, SSL, IPV, GPL, MD, RC, V, WIN, MR, PR}`.

### 7.4 Pipeline (in `app.rs` `build_report`, after both threads join)

1. `known = report::known_projects(yt_outcome.map(|o| o.admin_projects.as_ref()).flatten(), &yt_settings.projects)`.
2. For each MR: `mr.issue_ids = mr_issue_ids(mr, known.as_ref())`.
3. `referenced` = the union of all `mr.issue_ids` minus the IDs of fetched
   issues, in first-seen order.
4. Only if the YouTrack source ran and succeeded:
   `fetch_referenced(api, &referenced, …)` returns `(ref_issues, aliases)`.
   Append each referenced issue **unless an issue with the same ID is
   already present**; the existing in-window issue wins. An `Err` produces
   the warning `YouTrack: cannot fetch referenced issues (<err>)`, and the
   pipeline continues with no aliases.
5. `report::apply_aliases(&mut mrs, &aliases)` replaces every MR issue ID
   that is an alias key with its canonical ID, then dedupes while keeping the
   first occurrence. Example: an MR titled `OLD-12 fix` whose issue moved to
   `NEW-5` ends up with `issue_ids == [NEW-5]` and joins the NEW-5 group.
6. `report::build(mrs, issues) -> Report`.

### 7.5 Grouping model (`report.rs`)

```rust
pub enum Entry {
    Issue { issue: Issue, mrs: Vec<MergeRequest> },   // mrs may be empty
    OrphanMr(MergeRequest),                           // no referenced issue present in `issues`
}
pub struct Report { pub entries: Vec<Entry>, pub counts: Counts }
pub struct Counts { pub issues: usize, pub mrs: usize /* distinct */, pub linked_groups: usize,
                    pub orphan_mrs: usize, pub issues_without_mr: usize }
```
- Each MR is attached to **every** issue in `issues` that it references.
  This is rare, and the MR then appears under each such group. Counts use
  distinct MRs.
- MRs referencing only unfetched or unknown issues become `OrphanMr`. Their
  `issue_ids` are still displayed.
- Sort key: `Issue` entries use `max(issue.updated, mrs.updated_at…)`.
  `OrphanMr` entries use `updated_at`. Sort entries **descending** by key,
  tie-break ascending by display ref (`SP-12` / `group/proj!3`) for
  determinism. Inside a group, sort MRs by `updated_at` descending.
- Every in-window issue appears. `build` **drops any issue with
  `in_window == false` that ends up with no MRs**. This is a defensive guard,
  for example against an alias that was never applied. So referenced issues
  only ever appear together with their MRs.

```rust
pub fn apply_aliases(mrs: &mut [MergeRequest], aliases: &BTreeMap<IssueId, IssueId>);
pub fn build(mrs: Vec<MergeRequest>, issues: Vec<Issue>) -> Report;
// Report/Entry/Counts derive Debug, Clone, PartialEq. Counts are computed after the drop above.
```

---

## 8. UI

### 8.1 Mode selection (`ui/mod.rs`)

```
if --json                                   -> Json   (stdout; no header line, window inside JSON)
else if --plain || !stdout.is_terminal() || !stderr.is_terminal() || !stdin.is_terminal() -> Plain
else                                        -> Interactive
```
Implement this as the pure fn
`pub fn select_mode(json: bool, plain: bool, stdout_tty: bool, stderr_tty: bool, stdin_tty: bool) -> OutputMode`
with `enum OutputMode { Json, Plain, Interactive }`.
Empty report (no entries): plain and interactive print the header plus
`No activity found in this window.` and skip the prompt. JSON prints the
normal document with `entries: []`. The exit code is 0, or 3 if a source
failed.
While fetching, if stderr is a TTY, print `Fetching GitLab and YouTrack…` to
stderr and clear it with `console::Term::stderr().clear_last_lines(1)`.

### 8.2 Header (stdout, plain and interactive)

```
Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)
3 issues · 5 MRs · 2 linked · 1 MR without issue · 1 issue without MR
```
- Line 1 is `window::format_window_line(&Local, &window, now)` (4.6).
- Line 2 is `pub fn format_counts(c: &Counts) -> String` (in `ui/mod.rs`),
  pluralized: `1 issue`/`N issues`, `1 MR`/`N MRs`, `N linked`,
  `1 MR without issue`/`N MRs without issue`,
  `1 issue without MR`/`N issues without MR`. The separator is ` · `.
- Warnings are printed to **stderr** after the header, one per line,
  prefixed with `warning: `.

### 8.3 Rows (`ui/rows.rs`, pure)

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Url(String),
    ChooseMr { issue_url: String, mrs: Vec<(String /* label: "!456 group/proj  <title>" */, String /* url */)> },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateStyle { Opened, Draft, Merged, Closed, IssueOpen, IssueResolved }
#[derive(Debug, Clone, PartialEq)]
pub struct State { pub text: String, pub style: StateStyle }
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub issue: String,               // "SP-123"; "" for an MR without issue
    pub mr: String,                  // "proj!456", "proj!456 +2"; "" for an issue without MR
    pub title: String,
    pub issue_state: Option<State>, pub mr_state: Option<State>,
    pub updated: String,             // already formatted (local time)
    pub roles: String, pub urls: Vec<String>, pub target: Target,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout { Interactive { width: usize }, Plain { title_width: usize, roles_width: usize } }
pub fn build_rows<Tz: TimeZone>(report: &Report, tz: &Tz, now_year: i32) -> Vec<Row>;
pub fn plain_layout(rows: &[Row]) -> Layout;
pub fn format_row(row: &Row, layout: Layout, color: bool) -> String;
pub fn format_header(layout: Layout, color: bool) -> String;
```
`build_rows` emits **one row per entry**, in entry order, so every row has
the same columns. An `Entry::Issue` row carries the issue and its MRs; an
`OrphanMr` row leaves the issue cells empty. Every text cell goes through
`text::sanitize_line`, so no label contains a newline, tab or ESC.

Columns, separated by 2 spaces, with a header line (`ISSUE  MR  TITLE  ISSUE
STATE  MR STATE  UPDATED  ROLES`, plus `URL` in plain) above the rows:

| col | width | content |
|---|---|---|
| issue | 9 | `SP-123`; `-` for an MR without issue |
| mr | 16 | first MR (most recently updated) as `<last path segment>!<iid>`, plus ` +N` for N more MRs; `-` for an issue without MR. When too long, the project name is cut from the **left** with a leading `…`, keeping `!iid +N`. |
| title | flex | issue summary; for an orphan MR its title, with ` → SP-9, SP-10` for refs |
| issue state | 12 | YouTrack state; `-` for an orphan MR |
| mr state | 8 | state of the MR in the mr column: `opened`/`draft`/`merged`/`closed`/`locked`; `-` without MR |
| updated | 11 | latest of the issue and its MRs; local `%m-%d %H:%M`, for another year `%Y-%m-%d` |
| roles | 18 (interactive) / widest, at least 18 (plain) | union of issue and MR roles joined with `,`. `Referenced` shows as `ref`, and only when it is the sole role. |

Every column except the last is padded with `console::pad_str` and truncated
with `console::truncate_str(s, w, "…")`. Both are Unicode-width aware.

Widths:
- **Interactive:** `Layout::Interactive { width }` with
  `width = term_cols − 3`. dialoguer 0.12's `ColorfulTheme` renders each item
  as `"{prefix} {text}"`, which adds 2 columns, plus 1 safety column. The
  fixed columns take 9+16+12+8+11+18 = 74, plus 6 separators × 2 = 86.
  `title_width = max(20, width − 86)`. As a last step, truncate the whole
  label to `width` with `console::truncate_str(label, width, "…")`, so
  `console::measure_text_width(label) <= width` **always** holds (tested at
  widths 60, 101 and 160, with and without colour). Do not pad trailing
  spaces after the roles column.
- **Plain:** `plain_layout(&rows)`: `title_width` = min(80, widest title),
  at least 5; `roles_width` = widest roles, at least 18, so the links line
  up. Roles are not truncated. The last column lists the URLs (issue first,
  then every MR), so links are clickable in a terminal. There are no
  trailing spaces.

Colour (`color = true` on a TTY that allows it, `console::colors_enabled()` /
`colors_enabled_stderr()` respect `NO_COLOR`): issue id cyan bold, MR ref
bright blue, updated and roles dim, `-` placeholders dim, header bold. States:
merged = magenta, opened = green, draft = yellow, closed/locked = red, issue
resolved = dim. The visible text is identical with and without colour.

### 8.4 Interactive loop (`ui/interactive.rs`)

```
(term_rows, term_cols) = console::Term::stdout().size()
width  = term_cols.saturating_sub(3).max(40)
rows   = build_rows(report, &Local, now_year)
plain, colored = rows.map(|r| format_row(r, Layout::Interactive { width }, false / colors_enabled_stderr()))
// The window line, counts, warnings and the help line are printed BEFORE the Select,
// outside dialoguer's redraw region. The column header is the Select prompt.
used   = 3 (window, counts, help) + number of warning lines
max_length = term_rows.saturating_sub(used + 1).max(3)    // 1 safety line. dialoguer 0.12 itself reserves 2 rows
// for the prompt and page indicator: visible items = clamp(max_length, 3, term_rows) - 2 (dialoguer src/paging.rs)
cursor = 0
loop:
    sel = Select::with_theme(&ListTheme { plain, colored, header: true, .. })
            .with_prompt(format_header(..))      // cut by 16 cols when paged, for " [Page n/m] "
            .items(&["0", "1", ...]).default(cursor).max_length(max_length)
            .report(false)
            .interact_opt()?                     // Some(i) on Enter, None on Esc/q
    match sel:
      None -> break (exit 0)
      Some(i) -> cursor = i; match rows[i].target:
         Url(u)                         -> open_url(u)
         ChooseMr { issue_url, mrs }    -> // issue group with >= 2 MRs
             items = mrs labels (truncated to width) + ["Open issue in YouTrack"]
             sub = Select…interact_opt()?; Some(j) -> open selected; None -> back to main list
```
`ListTheme` wraps `ColorfulTheme`. dialoguer only sees index keys and the
theme renders `colored[i]` (inactive) or `plain[i]` (active, so the cyan
highlight covers the whole row). Reason: dialoguer 0.12 sizes items by their
**byte** length when redrawing (`prompts/select.rs`), so ANSI codes or
non-ASCII text in a full-width label made it clear lines above the list. The
prompt is written as `"  {header}"`, aligned with the item prefix. The MR
sub-menu uses the same theme with the normal `? <help> ›` prompt.

Target rules (`build_rows` sets them; they are unit-tested):
- An issue row with exactly 1 MR opens that MR's URL (SPEC).
- An issue row with ≥2 MRs offers the choice (`ChooseMr`), including
  "Open issue in YouTrack".
- An issue row without MR opens the issue.
- An orphan MR row opens that MR.

**Decision (known limitation, in README):** for an issue with exactly one MR,
the YouTrack issue cannot be opened from the interactive list. SPEC fixes
that the row opens the MR.
The issue URL is available through `--plain` (URL column) and `--json`
(`web_url`).

`open_url(u)`: `open::that_detached(u)`. Afterwards print
`Opened <u>` to stderr. On error print
`warning: could not open browser (<err>); URL: <u>` and continue the loop.
A `dialoguer::Error` (IO, for example when the terminal goes away) becomes a
runtime error (exit 1). Ctrl-C ends the process (default behaviour); no
handler is needed.

### 8.5 JSON (`ui/json.rs`)

```json
{
  "window": { "start": "2026-09-29T10:00:00+02:00", "end": "...", "origin": "meeting|previous|explicit", "previous": 0 },
  "counts": { "issues": 3, "merge_requests": 5, "linked_groups": 2, "orphan_merge_requests": 1, "issues_without_mr": 1 },
  "entries": [
    { "kind": "issue", "issue": { "id": "SP-123", "summary": "...", "state": "In Progress", "resolved": false,
        "web_url": "...", "updated": "...", "roles": ["assignee"], "in_window": true },
      "merge_requests": [ { "project_id": 1, "iid": 456, "project_path": "g/p", "title": "...", "source_branch": "...",
        "state": "merged", "web_url": "...", "author": "eric", "created_at": "...", "updated_at": "...",
        "roles": ["author"], "issue_ids": ["SP-123"] } ] },
    { "kind": "merge_request", "merge_request": { ... } }
  ],
  "warnings": ["..."]
}
```
Times are RFC 3339 in the local offset. Pretty-print with
`serde_json::to_writer_pretty(stdout)`. Define dedicated `Serialize` view
structs in `json.rs` so domain types don't need to match this shape exactly.

---

## 9. CLI surface (`cli.rs`)

```
work-tracker [OPTIONS]                 # default: report
work-tracker init [--force]            # write config template (0600)
work-tracker window [OPTIONS]          # print resolved window only; no network, tokens not required;
                                       # config file optional when --since is given (3.2)

Global options (global = true):
  -c, --config <PATH>        config file (default: $XDG_CONFIG_HOME/work-tracker/config.toml)
      --since <DATETIME>     window start (overrides meetings)       [conflicts: --previous]
      --until <DATETIME>     window end (default: now)
      --now <DATETIME>       pretend the current time is DATETIME (testing)
  -p, --previous             shift back one meeting interval; repeat for more (-pp)   [ArgAction::Count]
Report options:
      --plain                static table instead of interactive list   [conflicts: --json]
      --json                 dump merged data as JSON
      --no-gitlab            skip GitLab
      --no-youtrack          skip YouTrack
  -v, --verbose              log HTTP requests (method, URL, status, item count) to stderr; never tokens
```

clap derive: `#[command(name = "work-tracker", version, about)]`,
`Option<Command>` subcommand with `Init { #[arg(long)] force: bool }` and
`Window`. When no subcommand is given, run the report.

`window` output (stdout), with times shown in both local and UTC:
```
Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)
start: 2026-09-29T10:00:00+02:00 (2026-09-29T08:00:00Z)
end:   2026-09-30T11:00:00+02:00 (2026-09-30T09:00:00Z)
```

### 9.1 Exit codes

| code | meaning |
|---|---|
| 0 | success (including empty results, user quit with Esc/q) |
| 1 | runtime failure: every enabled source failed, browser/terminal IO error, `init` target exists |
| 2 | usage/config error: clap errors (clap's own exit 2), config missing/invalid, bad datetime, since ≥ until, no meetings, both sources disabled |
| 3 | partial results: at least one source failed, output produced from the others |

`main.rs`:
```rust
fn main() -> ExitCode {
    match work_tracker::app::run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),          // 0 or 3
        Err(e)   => { eprintln!("error: {:#}", e.err); ExitCode::from(e.code) }
    }
}
```
`{:#}` prints the anyhow context chain. Add context at the call sites, for
example `.with_context(|| format!("reading config {}", path.display()))`.

### 9.2 Orchestration (`app.rs`)

```rust
pub fn run(cli: Cli) -> Result<u8, CliError>;
// testable core, no I/O besides the trait objects:
pub fn build_report(gitlab: Option<(&dyn GitLabApi, Option<&str> /* configured username */)>,
                    youtrack: Option<&dyn YouTrackApi>,
                    yt: &YtSettings, window: &Window) -> ReportOutcome;
#[derive(Debug)]
pub struct ReportOutcome { pub report: Report, pub warnings: Vec<String>,
                           pub failed_sources: Vec<&'static str>, pub enabled_sources: usize }
```
`yt` is built from `Config.youtrack` **whenever that section exists**, even
if YouTrack is skipped, so that `projects` still feeds 7.3:
`YtSettings { base_url: sec.url, projects: sec.projects, state_field: sec.state_field }`.
Without the section it is `YtSettings::default()`.

`run` flow for the report:
1. `resolve_config_path` → `load_config` → print config warnings →
   `validate_sources(&cfg, cli.no_gitlab, cli.no_youtrack)`.
2. `now = --now (parse_user_datetime(&Local, …)) or Utc::now()`. Parse
   `--since`/`--until` the same way, then
   `window::compute(&Local, &cfg.meetings, &req)`.
3. Build `GitLabClient` / `YouTrackClient` from `Sources`, passing `cli.verbose`.
4. Choose the output mode (8.1). Show the spinner line if stderr is a TTY.
   Call `build_report`, then clear the spinner.
5. `warnings = sources.notes + outcome.warnings`. If
   `failed_sources.len() == enabled_sources`, return `CliError` code 1 with
   the joined warnings. Otherwise render (json / plain / interactive) and
   return `Ok(3)` if any source failed, else `Ok(0)`.

Inside `build_report`:
```rust
let (gl, yt) = std::thread::scope(|s| {
    let g = gitlab.map(|(api, user)| s.spawn(move || gitlab::collect::collect(api, user, window)));
    let y = youtrack.map(|api| s.spawn(move || youtrack::collect::collect(api, window, yt)));
    (g.map(|h| h.join().expect("gitlab thread panicked")),
     y.map(|h| h.join().expect("youtrack thread panicked")))
});
// then the 7.4 pipeline
```
A source error (`Err(ApiError)`) adds `"<error>"` to the warnings (the
`ApiError` Display already starts with the service name) and adds the
service name to `failed_sources`. `enabled_sources` counts the `Some`
arguments.

---

## 10. Test plan

All tests are offline. `cargo test` must pass with no network. The quality
gates are `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`
and `cargo fmt --check`.

| Module | Required unit tests |
|---|---|
| `config` | `default_config_path`: XDG set → XDG path; XDG empty or relative → HOME path; neither → None. The full SPEC example parses (`parse_config` with an env closure returning None). Env override replaces the token; an empty env var does not override; env without a section does not create one. `validate_sources`: a missing token gives an error naming the path and the env var; a missing section gives the note `GitLab not configured; skipping`; `no_gitlab = true` gives None with no note; both None → error. Invalid weekday, bad time, end ≤ start → errors naming `meetings[i]`. Overlapping meetings on the same weekday → error naming both indices; back-to-back meetings OK; the same times on different weekdays OK. Unknown key → error. URL trailing `/` and `/api/v4` trimmed; `ftp://` rejected. Projects uppercased; empty project rejected. `insecure_mode`: 0o644 → true, 0o640 → true, 0o400 → true, 0o700 → true, 0o600 → false, 0o100600 (file-type bits set) → false. `TEMPLATE` parses as `RawConfig` and through `parse_config`; `validate_sources` on it fails with the GitLab token-missing error. `init_config` (tempfile dir): creates missing parent dirs and the file with mode 0600 and content == `TEMPLATE`; an existing file → `AlreadyExists`; `force = true` overwrites and resets a 0644 file to 0600. `{:?}` of the section and config structs never contains the token. `ConfigError::exit_code` mapping. `permission_warning` on a 0644 temp file mentions `0644` and `chmod 600` |
| `window` | all 24 cases from 4.5 (Europe/Berlin), with error messages for 20, 21 and 23; `resolve_local` gap → +1h, ambiguous → earliest; `parse_user_datetime` for each format plus failures; `Window::contains` inclusive at both ends; `finished_meeting_ends` returns ≥ k+1 candidates for k = 0..3; `padded_dates` → (start date − 2, end date + 2), including a window starting at 00:30Z; `format_window_line` (4.6 tests) |
| `text` | `truncate_bytes`: ASCII under/at/over the limit; a multi-byte char straddling the limit (`"a€"` with max 2 → `"a"`); a 4-byte emoji straddling byte 65 536 does not panic and yields a char-boundary prefix ≤ 65 536 bytes. `sanitize_line` replaces `\n`, `\r`, `\t` and ESC with spaces |
| `model` | `IssueId::from_str` (`sp-123` → `SP-123`; rejects `SP-0`, `SP`, `-1`, `SP-`, `S_P-1`); Display; serde serializes as `"SP-123"`; `MrState` mapping incl. draft |
| `issue_ref` | positive: `"SP-123 fix"`, `"fix sp-45"` → `SP-45`, `"feature/SP-123-login"`, `"sp-123_fix"`, `"bugfix-SP-12"`, `"feature_SP-123"` → `SP-123`, `"eric_sp-12-fix"` → `SP-12`, `"1234_SP-5"` → `SP-5`, `"Resolve \"MS-7: x\""`, `"(P-7)"`, URL `.../issue/SP-9`, multiple IDs with dedup and order. Negative: `"3f2a-4b1c-9d"` (UUID) → empty, `"a1b2-3cd4"` → empty, `"SP-0"` → empty, `"SP-12abc"` → empty, `"SP-12345678"` → empty, `"x1SP-2"` → raw `[X1SP-2]` and nothing with known {SP}. Fenced code block in a description is ignored. An ID placed after byte 65 536 of a description is not found, and a description with a multi-byte char at the cap does not panic. Project filter: known {SP} keeps `SP-1` and drops `UTF-8`/`ISO-8601`. Known `None` plus the denylist drops `UTF-8` and `sha-256` and keeps `SP-1`. Source order is title > branch > description |
| `report` | `known_projects`: all four rows of the 7.3 table. `apply_aliases`: `OLD-12` → `NEW-5`; `[OLD-12, NEW-5]` → `[NEW-5]`. MR referencing a fetched issue → `LINK` group. MR referencing two issues → appears in both groups, counted once. MR with no refs → orphan. Ref to an unfetched issue → orphan with `issue_ids` kept. In-window issue without MR → `Entry::Issue` with empty mrs. Referenced (in_window false) issue without MRs → dropped. Sort by max(updated) desc, tie-break by ref. MRs inside a group sorted desc. Counts correct |
| `http` | `paginate`: stops when has_more is false (`truncated == false`); `truncated == true` when the cap is reached while has_more; an error on page 2 is returned. `gitlab_has_more`: header `"2"` → true, `""` → false, absent with 100/100 → true, absent with 37/100 → false. `map_status`: 401/403/404/400/500 → variants; GitLab 403 with body `{"error":"insufficient_scope"}` → message contains `read_api`; GitLab 403 without it → no hint; YouTrack 403 → hint; messages contain the URL; the `Http` body is cut to 300 bytes without panicking on multi-byte text. `ApiError::status()` |
| `gitlab::collect` (FakeGitLab) | inclusion rules 5.4 (authored created/updated in window; authored updated after window with no events → excluded; authored with an in-window push to its source branch → included; reviewer updated in window; event-only MR fetched via `project_merge_requests`). `classify` for approved / commented (Note, DiffNote, DiscussionNote with noteable_iid) / opened / accepted / push / issue note → None. Events outside the window ignored. Dedup across A/B/extras merges roles. `project_path` from references and from the web_url fallback. A configured username uses `users_by_username`; an empty result → `ApiError::Other` "not found". A 403 on the extra project fetch → warning, not error. Event date padding: for W = 2026-09-29T08:00Z..2026-09-30T09:00Z the fake records `after = 2026-09-27`, `before = 2026-10-02`. A truncated listing → warning. Unauthorized on `current_user` → `Err` |
| `youtrack::collect` (FakeYouTrack) | `project_clause` for 0/1/2 projects. Exact assigned query string: `assignee: me updated: 2026-09-27 .. 2026-10-02` and `(project: SP or project: MS) assignee: me updated: 2026-09-27 .. 2026-10-02`. Client-side `updated` filter. Activities → IDs from `target.idReadable` and `target.issue.idReadable`. Comment categories → Commenter, others → Updater. Fallback chain: FULL 400 then CORE OK → activities used, one warning, the fake recorded two activity calls; FULL 400 then CORE 400 → query fallback with exact query `(updater: me or commenter: me) updated: …`, two warnings; FULL 404 → query fallback directly; FULL 401 → `Err`. State extraction: object, array, string, null+resolved, null+unresolved, custom `state_field`. `fetch_by_ids`: exact batch query `issue id: SP-1 or issue id: SP-2`; chunking at 20 (45 IDs → 3 batch calls); an unrelated extra result in a batch is discarded; an ID missing from a successful batch is fetched with `issue_by_id`; batch 400 → every ID fetched individually; per-ID 404 → `missing`, no warning; alias (`issue_by_id("OLD-12")` returns `NEW-5`) → `aliases[OLD-12] = NEW-5` and NEW-5 kept; more than 50 per-ID fetches → warning; batch 401 → `Err`. `fetch_referenced` caps at 50 with a warning. A `project_short_names` failure → `admin_projects == None` plus a warning |
| client URL builders | pure fns in `gitlab/client.rs` and `youtrack/client.rs` that return `reqwest::Url` for each request. Test query parameters (`scope=all`, `iids[]` repeated, events `after`/`before`, `$top`/`$skip = i*T`, `fields`, `categories`, `author=me`, ms timestamps, `issueQuery`) by parsing `url.query_pairs()` |
| DTO deserialization | `tests/fixtures/*.json` (hand-written from the API docs), loaded with `include_str!`: GitLab MR list, events (MR approved, note on MR, push, issue note), user. YouTrack issues (with `$type` noise and the State field as an object), activities (comment and custom-field targets). Each deserializes and converts correctly |
| `ui::rows` | one row per entry with issue/MR refs. Target rules (1 MR → `Url(mr)`, ≥2 → `ChooseMr`, 0 → `Url(issue)`, orphan → `Url(mr)`). MR-column left truncation keeps `!iid +N`. `format_row(Interactive{width: 120}, color=false)` matches an expected literal string; the header aligns with the rows. `measure_text_width(label) <= width` at widths 60, 101 and 160, with and without colour. Stripping colour gives the uncoloured row. A title containing `\n` is sanitized. A plain row ends with all its URLs and no trailing spaces. Updated-date format for the current and another year |
| `ui` (mod) | `format_counts` singular/plural; mode selection as a pure fn `select_mode(json, plain, stdout_tty, stderr_tty, stdin_tty) -> OutputMode` |
| `ui::json` | the serialized output has the documented keys; roles lowercase; IssueId serialized as `"SP-123"`; origin `meeting` / `previous` / `explicit` |
| `app` (unit tests in `src/app.rs`, using the fakes) | `build_report` with a fake GitLab and a fake YouTrack yields a linked group, an orphan, and a referenced issue fetched by ID. Alias: MR title `OLD-12 fix`, with the YouTrack fake resolving OLD-12 to NEW-5 → the MR sits under the NEW-5 group and there is no OLD-12 row. GitLab fake returns Unauthorized → YouTrack results present, `failed_sources == ["GitLab"]`. Both fail → `failed_sources.len() == enabled_sources == 2`. YouTrack `None` with `YtSettings.projects = ["SP"]` → an MR titled `SP-1 UTF-8 FIX-2` has `issue_ids == [SP-1]` |
| `tests/cli.rs` (built binary, no network; child env sets `TZ=Europe/Berlin`, `HOME`/`XDG_CONFIG_HOME` to a temp dir, and removes `GITLAB_TOKEN`/`YOUTRACK_TOKEN`) | `init --config <tmp>/wt/config.toml` → exit 0, file mode 0600; running it again → exit 1. `window --config <cfg> --now 2026-09-30T11:00` (cfg = the template with meetings) → exit 0, stdout contains `start: 2026-09-29T10:00:00+02:00 (2026-09-29T08:00:00Z)`. `window --config <missing> --since 2026-09-28T08:00 --now 2026-10-02T12:00` → exit 0. `window --config <missing>` → exit 2, stderr contains `No config file`. Report with the template (empty tokens) → exit 2, stderr contains `GitLab token missing`. Report with `--no-gitlab --no-youtrack` → exit 2. `--since 2026-10-03 --now 2026-10-02T12:00 window` → exit 2 |

Fakes (`src/gitlab/fakes.rs`, `src/youtrack/fakes.rs`, each declared
`#[cfg(test)] pub mod fakes;`):
- Error injection uses a cloneable enum, because `ApiError` is not `Clone`
  (`reqwest::Error`):
  `#[derive(Clone, Debug)] pub enum FakeErr { Unauthorized, Forbidden, NotFound, Status(u16) }`
  with `fn to_api(&self, service: &'static str, url: &str) -> ApiError`.
  Put it in `http.rs` under `#[cfg(test)]` so both fakes share it.
- `FakeGitLab { user: Result<GlUser, FakeErr>, users_by_name: Vec<GlUser>, authored: Result<Vec<GlMergeRequest>, FakeErr>, reviewer: Result<…>, events: Result<Vec<GlEvent>, FakeErr>, project_mrs: HashMap<u64, Result<Vec<GlMergeRequest>, FakeErr>>, truncate: bool, calls: Mutex<Vec<String>> }`.
  `merge_requests` dispatches on `author_id` vs `reviewer_id`.
  `project_merge_requests` filters by the requested iids. `calls` records
  e.g. `"user_events 7 2026-09-27 2026-10-02"`.
- `FakeYouTrack { projects: Result<Vec<String>, FakeErr>, searches: HashMap<String /* exact query */, Result<Vec<YtIssue>, FakeErr>>, by_id: HashMap<String, Option<YtIssue>>, activities: Vec<(usize /* categories.len() */, Result<Vec<YtActivity>, FakeErr>)>, calls: Mutex<Vec<String>> }`.
  An unknown search query returns `Ok(empty)`. An unknown `by_id` returns
  `Ok(None)`. `activities` is matched by the length of the category list
  (FULL vs CORE). `calls` records every query string.
- Builders such as `fn mr(pid, iid, title, branch, desc, updated) -> GlMergeRequest`
  and `fn yt_issue(id, summary, updated_ms, state) -> YtIssue` live in the same
  fakes modules.
## 11. README.md (implementer writes it)

Sections:
- Install (`cargo install --path .`)
- Configuration (`work-tracker init`, path rules, chmod 600 (the tool warns
  whenever the mode is not exactly 0600), env tokens, required token scopes:
  GitLab `read_api`, YouTrack permanent token)
- Usage examples (`work-tracker`, `--plain`, `--json | jq`, `--previous`,
  `--since/--until`, `window --now ...`)
- How the window is computed (with the SPEC examples; overlapping meetings
  are rejected)
- How MRs and issues are matched (title, branch, description; separators
  `-`, `_`, `/`; project filter; moved issues resolved through aliases)
- Exit codes
- Known limitations:
  - Past windows (`--previous`/`--until`): a GitLab MR (5.4) or a YouTrack
    assigned issue (6.2) touched inside the window **and again later** is
    found only through my own events/activities (or MR creation in the
    window).
  - If the activities API is unavailable, "changed/commented by me" falls
    back to the approximate `updater:/commenter:` query.
  - An MR referencing two issues appears under both.
  - In a group with exactly one MR, the interactive list opens the MR; the
    issue URL is shown with `--plain`/`--json`.
  - YouTrack project short names containing `_` are not recognised in MR
    text.

---

## 12. Revision log

Revision 3 (list view):
- 8.3 / 8.4: one row per task instead of an issue line with MR child rows.
  Issue and MR references get their own columns (`-` when absent), MR state
  has its own column, roles are the union, plain lists every URL and has a
  column header. Interactive labels are now coloured per column; the
  `ListTheme` index-key workaround keeps dialoguer's redraw correct.

Revision 2 (review findings resolved):
- 5.2 / 6.2: event and query date padding is now ±2 days through
  `window::padded_dates`, because GitLab's EventsFinder uses the server's
  time zone and excludes whole days.
- 6.4 / 7.4 / 7.5: fetch by ID uses `issue id: A or issue id: B` batches.
  Results are filtered to the requested IDs, every missing ID is fetched one
  by one regardless of status, and an alias map for moved issues rewrites MR
  references. Referenced issues without MRs are dropped.
- 8.3 / 8.4: interactive label width = cols − 3, and it is always truncated
  to that width. Labels are uncolored. `max_length` subtracts the header and
  warning lines.
- 7.1: `_` removed from the project class and allowed as a left separator.
- 8.4: an explicit decision and README limitation for opening the issue in
  single-MR groups.
- 7.2 / `text.rs`: char-boundary-safe 64 KiB cap.
- 3.2: the permission warning applies the SPEC rule literally
  (`mode & 0o777 != 0o600`).
- 3.1 / 3.2: concrete config API (`parse_config` never checks tokens,
  `validate_sources -> Sources`, `init_config(path, force)`, `ConfigError`
  exit codes). `window` works without a config file when `--since` is given.
- 6.3: activity categories decided (full/core sets), with a
  400 → core → query fallback chain.
- 5.4 / 6.2 / 11: the past-window limitation now covers YouTrack assigned
  issues too.
- 7.3: a single `known_projects` table, used whether or not YouTrack ran.
- 3.1: overlapping meetings are rejected.
- 4.5: concrete `now` for every case; new `SinceNotBeforeNow` error; cases
  23 and 24 added.
- 1.2 / 5.3: `paginate(max_pages, fetch_page) -> Listing { items, truncated }`
  with an explicit `Page { items, has_more }`, plus GitLab and YouTrack page
  mappings. Trait methods return `Listing`.
- 1.1 / 2 / 4.1: `IssueId` uses derive + `#[serde(into = "String")]`;
  `rust-version = "1.98"`; derives listed for the window types.
- 1.3: `Forbidden` carries a hint (`read_api` for GitLab
  `insufficient_scope`). `ApiError::Other` and `ApiError::status()` added.
