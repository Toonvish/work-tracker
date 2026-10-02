use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{NaiveTime, Weekday};
use serde::Deserialize;

use crate::window::Meeting;

// ---------------------------------------------------------------------------
// Raw (deserialized) schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    gitlab: Option<RawGitLab>,
    youtrack: Option<RawYouTrack>,
    #[serde(default)]
    meetings: Vec<RawMeeting>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGitLab {
    url: String,
    #[serde(default)]
    token: String,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawYouTrack {
    url: String,
    #[serde(default)]
    token: String,
    projects: Option<Vec<String>>,
    state_field: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMeeting {
    weekday: String,
    start: String,
    end: String,
}

// ---------------------------------------------------------------------------
// Parsed configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub path: PathBuf,
    pub gitlab: Option<GitLabSection>,
    pub youtrack: Option<YouTrackSection>,
    pub meetings: Vec<Meeting>,
}

#[derive(Clone, PartialEq)]
pub struct GitLabSection {
    /// Normalized (no trailing `/`, no `/api/v4`).
    pub url: String,
    /// After env override; may be empty.
    pub token: String,
    pub username: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct YouTrackSection {
    pub url: String,
    /// After env override; may be empty.
    pub token: String,
    /// Trimmed, UPPERCASE, deduped, order kept.
    pub projects: Vec<String>,
    /// Default "State".
    pub state_field: String,
}

/// Validated per-run GitLab config. The token is non-empty.
#[derive(Clone, PartialEq)]
pub struct GitLabConfig {
    pub url: String,
    pub token: String,
    pub username: Option<String>,
}

/// Validated per-run YouTrack config. The token is non-empty.
#[derive(Clone, PartialEq)]
pub struct YouTrackConfig {
    pub url: String,
    pub token: String,
    pub projects: Vec<String>,
    pub state_field: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sources {
    pub gitlab: Option<GitLabConfig>,
    pub youtrack: Option<YouTrackConfig>,
    /// E.g. "YouTrack not configured; skipping".
    pub notes: Vec<String>,
}

fn mask(token: &str) -> &'static str {
    if token.is_empty() { "" } else { "***" }
}

impl fmt::Debug for GitLabSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitLabSection")
            .field("url", &self.url)
            .field("token", &mask(&self.token))
            .field("username", &self.username)
            .finish()
    }
}

impl fmt::Debug for YouTrackSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YouTrackSection")
            .field("url", &self.url)
            .field("token", &mask(&self.token))
            .field("projects", &self.projects)
            .field("state_field", &self.state_field)
            .finish()
    }
}

impl fmt::Debug for GitLabConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitLabConfig")
            .field("url", &self.url)
            .field("token", &mask(&self.token))
            .field("username", &self.username)
            .finish()
    }
}

impl fmt::Debug for YouTrackConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YouTrackConfig")
            .field("url", &self.url)
            .field("token", &mask(&self.token))
            .field("projects", &self.projects)
            .field("state_field", &self.state_field)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

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
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ConfigError {
    /// AlreadyExists | Io => 1, everything else => 2.
    pub fn exit_code(&self) -> u8 {
        match self {
            ConfigError::AlreadyExists(_) | ConfigError::Io { .. } => 1,
            ConfigError::NoConfigDir | ConfigError::NotFound(_) | ConfigError::Invalid { .. } => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Paths and permissions
// ---------------------------------------------------------------------------

/// XDG set, non-empty and absolute -> `xdg/work-tracker/config.toml`;
/// else HOME set and non-empty -> `home/.config/work-tracker/config.toml`; else None.
pub fn default_config_path(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(xdg) = xdg {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() && xdg.is_absolute() {
            return Some(xdg.join("work-tracker").join("config.toml"));
        }
    }
    match home {
        Some(home) if !home.is_empty() => Some(
            PathBuf::from(home)
                .join(".config")
                .join("work-tracker")
                .join("config.toml"),
        ),
        _ => None,
    }
}

pub fn resolve_config_path(cli: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(p) = cli {
        return Ok(p.to_path_buf());
    }
    default_config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .ok_or(ConfigError::NoConfigDir)
}

/// True when the permission bits are not exactly 0600.
pub fn insecure_mode(mode: u32) -> bool {
    (mode & 0o777) != 0o600
}

#[cfg(unix)]
pub fn permission_warning(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    if insecure_mode(mode) {
        Some(format!(
            "{p} has mode {m:04o} (expected 0600) and contains API tokens; run: chmod 600 {p}",
            p = path.display(),
            m = mode & 0o777
        ))
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn permission_warning(_path: &Path) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------

fn invalid(path: &Path, msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_path_buf(),
        msg: msg.into(),
    }
}

fn normalize_url(
    raw: &str,
    field: &str,
    strip_suffix: Option<&str>,
    path: &Path,
) -> Result<String, ConfigError> {
    let mut url = raw.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(invalid(
            path,
            format!("{field}: must start with http:// or https://"),
        ));
    }
    if let Some(suffix) = strip_suffix
        && let Some(stripped) = url.strip_suffix(suffix)
    {
        url = stripped.trim_end_matches('/').to_string();
    }
    Ok(url)
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tuesday" => Some(Weekday::Tue),
        "wed" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

fn weekday_abbr(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

fn parse_meetings(raw: &[RawMeeting], path: &Path) -> Result<Vec<Meeting>, ConfigError> {
    let mut meetings = Vec::with_capacity(raw.len());
    for (i, m) in raw.iter().enumerate() {
        let weekday = parse_weekday(&m.weekday).ok_or_else(|| {
            invalid(
                path,
                format!(
                    "meetings[{i}].weekday: invalid weekday \"{}\" (use mon..sun)",
                    m.weekday
                ),
            )
        })?;
        let start = parse_time(&m.start).ok_or_else(|| {
            invalid(
                path,
                format!(
                    "meetings[{i}].start: invalid time \"{}\" (use HH:MM)",
                    m.start
                ),
            )
        })?;
        let end = parse_time(&m.end).ok_or_else(|| {
            invalid(
                path,
                format!("meetings[{i}].end: invalid time \"{}\" (use HH:MM)", m.end),
            )
        })?;
        if end <= start {
            return Err(invalid(
                path,
                format!(
                    "meetings[{i}]: end {} must be after start {}",
                    fmt_time(end),
                    fmt_time(start)
                ),
            ));
        }
        meetings.push(Meeting {
            weekday,
            start,
            end,
        });
    }

    for (j, b) in meetings.iter().enumerate() {
        for (i, a) in meetings.iter().enumerate().take(j) {
            if a.weekday == b.weekday && a.start < b.end && b.start < a.end {
                return Err(invalid(
                    path,
                    format!(
                        "meetings[{i}] and meetings[{j}] overlap ({} {}–{}, {} {}–{})",
                        weekday_abbr(a.weekday),
                        fmt_time(a.start),
                        fmt_time(a.end),
                        weekday_abbr(b.weekday),
                        fmt_time(b.start),
                        fmt_time(b.end)
                    ),
                ));
            }
        }
    }
    Ok(meetings)
}

fn parse_projects(raw: Option<Vec<String>>, path: &Path) -> Result<Vec<String>, ConfigError> {
    let mut out: Vec<String> = Vec::new();
    for (i, p) in raw.unwrap_or_default().into_iter().enumerate() {
        let p = p.trim().to_uppercase();
        if p.is_empty() {
            return Err(invalid(
                path,
                format!("[youtrack].projects[{i}]: empty project name"),
            ));
        }
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

/// Parse and validate a config file's text. Never checks that tokens are present.
pub fn parse_config(
    text: &str,
    path: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, ConfigError> {
    let raw: RawConfig =
        toml::from_str(text).map_err(|e| invalid(path, format!("invalid config: {e}")))?;

    let env_token = |key: &str| env(key).filter(|v| !v.is_empty());

    let gitlab = match raw.gitlab {
        Some(g) => Some(GitLabSection {
            url: normalize_url(&g.url, "[gitlab].url", Some("/api/v4"), path)?,
            token: env_token("GITLAB_TOKEN").unwrap_or(g.token),
            username: g.username,
        }),
        None => None,
    };

    let youtrack = match raw.youtrack {
        Some(y) => {
            let state_field = match y.state_field {
                Some(s) => {
                    let s = s.trim().to_string();
                    if s.is_empty() {
                        return Err(invalid(path, "[youtrack].state_field: must not be empty"));
                    }
                    s
                }
                None => "State".to_string(),
            };
            Some(YouTrackSection {
                url: normalize_url(&y.url, "[youtrack].url", Some("/api"), path)?,
                token: env_token("YOUTRACK_TOKEN").unwrap_or(y.token),
                projects: parse_projects(y.projects, path)?,
                state_field,
            })
        }
        None => None,
    };

    let meetings = parse_meetings(&raw.meetings, path)?;

    Ok(Config {
        path: path.to_path_buf(),
        gitlab,
        youtrack,
        meetings,
    })
}

pub fn load_config(path: &Path) -> Result<(Config, Vec<String>), ConfigError> {
    if !path.exists() {
        return Err(ConfigError::NotFound(path.to_path_buf()));
    }
    let warnings: Vec<String> = permission_warning(path).into_iter().collect();
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg = parse_config(&text, path, &|k| std::env::var(k).ok())?;
    Ok((cfg, warnings))
}

pub fn validate_sources(
    cfg: &Config,
    no_gitlab: bool,
    no_youtrack: bool,
) -> Result<Sources, ConfigError> {
    let mut notes = Vec::new();

    let gitlab = if no_gitlab {
        None
    } else {
        match &cfg.gitlab {
            None => {
                notes.push("GitLab not configured; skipping".to_string());
                None
            }
            Some(g) if g.token.is_empty() => {
                return Err(invalid(
                    &cfg.path,
                    format!(
                        "GitLab token missing: set [gitlab].token in {} or GITLAB_TOKEN",
                        cfg.path.display()
                    ),
                ));
            }
            Some(g) => Some(GitLabConfig {
                url: g.url.clone(),
                token: g.token.clone(),
                username: g.username.clone(),
            }),
        }
    };

    let youtrack = if no_youtrack {
        None
    } else {
        match &cfg.youtrack {
            None => {
                notes.push("YouTrack not configured; skipping".to_string());
                None
            }
            Some(y) if y.token.is_empty() => {
                return Err(invalid(
                    &cfg.path,
                    format!(
                        "YouTrack token missing: set [youtrack].token in {} or YOUTRACK_TOKEN",
                        cfg.path.display()
                    ),
                ));
            }
            Some(y) => Some(YouTrackConfig {
                url: y.url.clone(),
                token: y.token.clone(),
                projects: y.projects.clone(),
                state_field: y.state_field.clone(),
            }),
        }
    };

    if gitlab.is_none() && youtrack.is_none() {
        return Err(invalid(
            &cfg.path,
            format!(
                "no data source to query: configure [gitlab] and/or [youtrack] in {}, or drop --no-gitlab/--no-youtrack",
                cfg.path.display()
            ),
        ));
    }

    Ok(Sources {
        gitlab,
        youtrack,
        notes,
    })
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

pub fn init_config(path: &Path, force: bool) -> Result<(), ConfigError> {
    let io_err = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };

    if path.exists() && !force {
        return Err(ConfigError::AlreadyExists(path.to_path_buf()));
    }

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent).map_err(io_err)?;
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(io_err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err)?;
    }
    std::io::Write::write_all(&mut file, TEMPLATE.as_bytes()).map_err(io_err)?;
    Ok(())
}

pub const TEMPLATE: &str = r##"# work-tracker configuration
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
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn p() -> PathBuf {
        PathBuf::from("/cfg/config.toml")
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        parse_config(text, &p(), &no_env)
    }

    fn err_msg(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    const SPEC_EXAMPLE: &str = r#"
[gitlab]
url = "https://gitlab.example.com"   # self-hosted
token = "glpat-abc"
username = "eric"

[youtrack]
url = "https://youtrack.uponu.com"
token = "perm:xyz"
projects = ["SP", "MS", "P"]

[[meetings]]
weekday = "tue"
start = "09:00"
end = "10:00"

[[meetings]]
weekday = "thu"
start = "14:00"
end = "15:00"
"#;

    // --- default_config_path -------------------------------------------------

    #[test]
    fn default_path_xdg_absolute() {
        assert_eq!(
            default_config_path(Some("/xdg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/xdg/work-tracker/config.toml"))
        );
    }

    #[test]
    fn default_path_xdg_empty_or_relative_uses_home() {
        let want = Some(PathBuf::from("/home/u/.config/work-tracker/config.toml"));
        assert_eq!(
            default_config_path(Some("".into()), Some("/home/u".into())),
            want
        );
        assert_eq!(
            default_config_path(Some("rel/dir".into()), Some("/home/u".into())),
            want
        );
        assert_eq!(default_config_path(None, Some("/home/u".into())), want);
    }

    #[test]
    fn default_path_neither() {
        assert_eq!(default_config_path(None, None), None);
        assert_eq!(default_config_path(Some("".into()), Some("".into())), None);
        assert_eq!(default_config_path(Some("rel".into()), None), None);
    }

    #[test]
    fn resolve_prefers_cli_path() {
        let got = resolve_config_path(Some(Path::new("/some/where.toml"))).unwrap();
        assert_eq!(got, PathBuf::from("/some/where.toml"));
    }

    // --- parsing -------------------------------------------------------------

    #[test]
    fn spec_example_parses() {
        let cfg = parse(SPEC_EXAMPLE).unwrap();
        let gl = cfg.gitlab.unwrap();
        assert_eq!(gl.url, "https://gitlab.example.com");
        assert_eq!(gl.token, "glpat-abc");
        assert_eq!(gl.username.as_deref(), Some("eric"));
        let yt = cfg.youtrack.unwrap();
        assert_eq!(yt.url, "https://youtrack.uponu.com");
        assert_eq!(yt.token, "perm:xyz");
        assert_eq!(yt.projects, vec!["SP", "MS", "P"]);
        assert_eq!(yt.state_field, "State");
        assert_eq!(
            cfg.meetings,
            vec![
                Meeting {
                    weekday: Weekday::Tue,
                    start: t(9, 0),
                    end: t(10, 0)
                },
                Meeting {
                    weekday: Weekday::Thu,
                    start: t(14, 0),
                    end: t(15, 0)
                },
            ]
        );
        assert_eq!(cfg.path, p());
    }

    #[test]
    fn env_override_replaces_token() {
        let env = |k: &str| match k {
            "GITLAB_TOKEN" => Some("env-gl".to_string()),
            "YOUTRACK_TOKEN" => Some("env-yt".to_string()),
            _ => None,
        };
        let cfg = parse_config(SPEC_EXAMPLE, &p(), &env).unwrap();
        assert_eq!(cfg.gitlab.unwrap().token, "env-gl");
        assert_eq!(cfg.youtrack.unwrap().token, "env-yt");
    }

    #[test]
    fn empty_env_does_not_override() {
        let env = |_: &str| Some(String::new());
        let cfg = parse_config(SPEC_EXAMPLE, &p(), &env).unwrap();
        assert_eq!(cfg.gitlab.unwrap().token, "glpat-abc");
        assert_eq!(cfg.youtrack.unwrap().token, "perm:xyz");
    }

    #[test]
    fn env_without_section_creates_nothing() {
        let env = |_: &str| Some("tok".to_string());
        let cfg = parse_config("", &p(), &env).unwrap();
        assert!(cfg.gitlab.is_none());
        assert!(cfg.youtrack.is_none());
        assert!(cfg.meetings.is_empty());
    }

    #[test]
    fn missing_token_parses_as_empty() {
        let cfg = parse("[gitlab]\nurl = \"https://g.example.com\"\n").unwrap();
        assert_eq!(cfg.gitlab.unwrap().token, "");
    }

    #[test]
    fn invalid_weekday() {
        let msg = err_msg(
            "[[meetings]]\nweekday = \"tue\"\nstart = \"09:00\"\nend = \"10:00\"\n\
             [[meetings]]\nweekday = \"tues\"\nstart = \"09:00\"\nend = \"10:00\"\n",
        );
        assert!(
            msg.contains("meetings[1].weekday: invalid weekday \"tues\" (use mon..sun)"),
            "{msg}"
        );
        assert!(msg.contains("/cfg/config.toml"), "{msg}");
    }

    #[test]
    fn full_and_mixed_case_weekday_names() {
        let cfg = parse(
            "[[meetings]]\nweekday = \"Monday\"\nstart = \"09:00\"\nend = \"10:00\"\n\
             [[meetings]]\nweekday = \"SUN\"\nstart = \"09:00\"\nend = \"10:00\"\n",
        )
        .unwrap();
        assert_eq!(cfg.meetings[0].weekday, Weekday::Mon);
        assert_eq!(cfg.meetings[1].weekday, Weekday::Sun);
    }

    #[test]
    fn invalid_time() {
        let msg = err_msg("[[meetings]]\nweekday = \"tue\"\nstart = \"9am\"\nend = \"10:00\"\n");
        assert!(
            msg.contains("meetings[0].start: invalid time \"9am\" (use HH:MM)"),
            "{msg}"
        );
        let msg = err_msg("[[meetings]]\nweekday = \"tue\"\nstart = \"09:00\"\nend = \"25:00\"\n");
        assert!(msg.contains("meetings[0].end"), "{msg}");
    }

    #[test]
    fn end_must_be_after_start() {
        let msg = err_msg("[[meetings]]\nweekday = \"tue\"\nstart = \"10:00\"\nend = \"09:00\"\n");
        assert!(
            msg.contains("meetings[0]: end 09:00 must be after start 10:00"),
            "{msg}"
        );
        let msg = err_msg("[[meetings]]\nweekday = \"tue\"\nstart = \"10:00\"\nend = \"10:00\"\n");
        assert!(msg.contains("meetings[0]"), "{msg}");
    }

    fn meeting_toml(day: &str, start: &str, end: &str) -> String {
        format!("[[meetings]]\nweekday = \"{day}\"\nstart = \"{start}\"\nend = \"{end}\"\n")
    }

    #[test]
    fn overlapping_same_weekday_rejected() {
        let text = format!(
            "{}{}{}",
            meeting_toml("tue", "09:00", "11:00"),
            meeting_toml("thu", "09:00", "11:00"),
            meeting_toml("tue", "09:30", "09:45")
        );
        let msg = err_msg(&text);
        assert!(
            msg.contains("meetings[0] and meetings[2] overlap (tue 09:00–11:00, tue 09:30–09:45)"),
            "{msg}"
        );
    }

    #[test]
    fn exact_duplicate_meetings_overlap() {
        let text = format!(
            "{}{}",
            meeting_toml("tue", "09:00", "10:00"),
            meeting_toml("tue", "09:00", "10:00")
        );
        let msg = err_msg(&text);
        assert!(msg.contains("meetings[0] and meetings[1] overlap"), "{msg}");
    }

    #[test]
    fn back_to_back_and_other_weekday_ok() {
        let text = format!(
            "{}{}{}",
            meeting_toml("tue", "09:00", "10:00"),
            meeting_toml("tue", "10:00", "11:00"),
            meeting_toml("wed", "09:00", "10:00")
        );
        assert_eq!(parse(&text).unwrap().meetings.len(), 3);
    }

    #[test]
    fn unknown_keys_rejected() {
        let msg = err_msg("bogus = 1\n");
        assert!(msg.contains("invalid config"), "{msg}");
        let msg = err_msg("[gitlab]\nurl = \"https://g\"\nfoo = 1\n");
        assert!(msg.contains("invalid config"), "{msg}");
        let msg =
            err_msg("[[meetings]]\nweekday = \"tue\"\nstart = \"09:00\"\nend = \"10:00\"\nx = 1\n");
        assert!(msg.contains("invalid config"), "{msg}");
    }

    #[test]
    fn toml_syntax_error_rejected() {
        let msg = err_msg("[gitlab\n");
        assert!(msg.contains("invalid config"), "{msg}");
    }

    #[test]
    fn urls_are_normalised() {
        let cfg = parse("[gitlab]\nurl = \"https://g.example.com/\"\n").unwrap();
        assert_eq!(cfg.gitlab.unwrap().url, "https://g.example.com");
        let cfg = parse("[gitlab]\nurl = \"https://g.example.com/api/v4\"\n").unwrap();
        assert_eq!(cfg.gitlab.unwrap().url, "https://g.example.com");
        let cfg = parse("[gitlab]\nurl = \"https://g.example.com/api/v4/\"\n").unwrap();
        assert_eq!(cfg.gitlab.unwrap().url, "https://g.example.com");
        let cfg = parse("[youtrack]\nurl = \"http://yt.example.com/\"\n").unwrap();
        assert_eq!(cfg.youtrack.unwrap().url, "http://yt.example.com");
        for raw in [
            "https://yt.example.com/api",
            "https://yt.example.com/api/",
            "https://yt.example.com/",
        ] {
            let cfg = parse(&format!("[youtrack]\nurl = \"{raw}\"\n")).unwrap();
            assert_eq!(cfg.youtrack.unwrap().url, "https://yt.example.com", "{raw}");
        }
        // A sub-path install keeps its prefix; only the trailing /api goes.
        let cfg = parse("[youtrack]\nurl = \"https://example.com/youtrack/api\"\n").unwrap();
        assert_eq!(cfg.youtrack.unwrap().url, "https://example.com/youtrack");
    }

    #[test]
    fn bad_url_scheme_rejected() {
        let msg = err_msg("[gitlab]\nurl = \"ftp://g.example.com\"\n");
        assert!(
            msg.contains("[gitlab].url: must start with http:// or https://"),
            "{msg}"
        );
        let msg = err_msg("[youtrack]\nurl = \"yt.example.com\"\n");
        assert!(
            msg.contains("[youtrack].url: must start with http:// or https://"),
            "{msg}"
        );
    }

    #[test]
    fn projects_uppercased_trimmed_deduped() {
        let cfg = parse(
            "[youtrack]\nurl = \"https://y\"\nprojects = [\" sp \", \"Ms\", \"SP\", \"p\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.youtrack.unwrap().projects, vec!["SP", "MS", "P"]);
    }

    #[test]
    fn empty_project_rejected() {
        let msg = err_msg("[youtrack]\nurl = \"https://y\"\nprojects = [\"SP\", \"  \"]\n");
        assert!(
            msg.contains("[youtrack].projects[1]: empty project name"),
            "{msg}"
        );
    }

    #[test]
    fn state_field_validation() {
        let cfg = parse("[youtrack]\nurl = \"https://y\"\nstate_field = \" Stage \"\n").unwrap();
        assert_eq!(cfg.youtrack.unwrap().state_field, "Stage");
        let msg = err_msg("[youtrack]\nurl = \"https://y\"\nstate_field = \"  \"\n");
        assert!(msg.contains("state_field"), "{msg}");
    }

    // --- validate_sources ----------------------------------------------------

    fn cfg_with(gl_token: &str, yt_token: &str) -> Config {
        Config {
            path: p(),
            gitlab: Some(GitLabSection {
                url: "https://g".into(),
                token: gl_token.into(),
                username: None,
            }),
            youtrack: Some(YouTrackSection {
                url: "https://y".into(),
                token: yt_token.into(),
                projects: vec![],
                state_field: "State".into(),
            }),
            meetings: vec![],
        }
    }

    #[test]
    fn validate_ok() {
        let s = validate_sources(&cfg_with("a", "b"), false, false).unwrap();
        assert_eq!(s.gitlab.unwrap().token, "a");
        assert_eq!(s.youtrack.unwrap().token, "b");
        assert!(s.notes.is_empty());
    }

    #[test]
    fn validate_missing_token_names_path_and_env() {
        let msg = validate_sources(&cfg_with("", "b"), false, false)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("/cfg/config.toml"), "{msg}");
        assert!(msg.contains("GITLAB_TOKEN"), "{msg}");
        assert!(msg.contains("GitLab token missing"), "{msg}");
        let msg = validate_sources(&cfg_with("a", ""), false, false)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("YOUTRACK_TOKEN"), "{msg}");
        assert!(msg.contains("/cfg/config.toml"), "{msg}");
    }

    #[test]
    fn validate_missing_section_gives_note() {
        let mut cfg = cfg_with("a", "b");
        cfg.gitlab = None;
        let s = validate_sources(&cfg, false, false).unwrap();
        assert!(s.gitlab.is_none());
        assert_eq!(s.notes, vec!["GitLab not configured; skipping"]);
        let mut cfg = cfg_with("a", "b");
        cfg.youtrack = None;
        let s = validate_sources(&cfg, false, false).unwrap();
        assert_eq!(s.notes, vec!["YouTrack not configured; skipping"]);
    }

    #[test]
    fn validate_no_flag_gives_none_without_note() {
        let s = validate_sources(&cfg_with("a", "b"), true, false).unwrap();
        assert!(s.gitlab.is_none());
        assert!(s.notes.is_empty());
        assert!(s.youtrack.is_some());
        // A disabled source is not checked for its token.
        let s = validate_sources(&cfg_with("", "b"), true, false).unwrap();
        assert!(s.gitlab.is_none());
    }

    #[test]
    fn validate_both_none_is_error() {
        let msg = validate_sources(&cfg_with("a", "b"), true, true)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("no data source to query"), "{msg}");
        assert!(msg.contains("/cfg/config.toml"), "{msg}");
        let empty = Config {
            path: p(),
            gitlab: None,
            youtrack: None,
            meetings: vec![],
        };
        assert!(validate_sources(&empty, false, false).is_err());
    }

    // --- permissions ---------------------------------------------------------

    #[test]
    fn insecure_mode_cases() {
        for m in [0o644, 0o640, 0o400, 0o700] {
            assert!(insecure_mode(m), "{m:o}");
        }
        assert!(!insecure_mode(0o600));
        assert!(!insecure_mode(0o100600));
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn permission_warning_on_0644() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("config.toml");
        std::fs::write(&f, "").unwrap();
        set_mode(&f, 0o644);
        let w = permission_warning(&f).unwrap();
        assert!(w.contains("0644"), "{w}");
        assert!(w.contains("chmod 600"), "{w}");
        assert!(w.contains(&f.display().to_string()), "{w}");
        set_mode(&f, 0o600);
        assert!(permission_warning(&f).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn load_config_warns_on_loose_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("config.toml");
        std::fs::write(&f, "").unwrap();
        set_mode(&f, 0o644);
        let (_, warnings) = load_config(&f).unwrap();
        assert_eq!(warnings.len(), 1);
        set_mode(&f, 0o600);
        let (_, warnings) = load_config(&f).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn load_config_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = load_config(&dir.path().join("missing.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::NotFound(_)));
        assert!(err.to_string().contains("No config file at"));
        assert!(err.to_string().contains("work-tracker init"));
    }

    // --- template and init ---------------------------------------------------

    #[test]
    fn template_parses() {
        toml::from_str::<RawConfig>(TEMPLATE).unwrap();
        let cfg = parse(TEMPLATE).unwrap();
        assert_eq!(cfg.meetings.len(), 2);
        assert_eq!(
            cfg.gitlab.as_ref().unwrap().url,
            "https://gitlab.example.com"
        );
        assert_eq!(
            cfg.youtrack.as_ref().unwrap().url,
            "https://youtrack.uponu.com"
        );
        assert_eq!(cfg.youtrack.as_ref().unwrap().state_field, "State");
    }

    #[test]
    fn template_fails_validate_sources_with_gitlab_token_error() {
        let cfg = parse(TEMPLATE).unwrap();
        let msg = validate_sources(&cfg, false, false)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("GitLab token missing"), "{msg}");
        assert!(msg.contains("GITLAB_TOKEN"), "{msg}");
    }

    #[test]
    fn init_creates_dirs_and_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("a/b/work-tracker/config.toml");
        init_config(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), TEMPLATE);
        #[cfg(unix)]
        {
            assert_eq!(mode_of(&f), 0o600);
            assert_eq!(mode_of(f.parent().unwrap()), 0o700);
        }
    }

    #[test]
    fn init_twice_is_already_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("wt/config.toml");
        init_config(&f, false).unwrap();
        let err = init_config(&f, false).unwrap_err();
        assert!(matches!(err, ConfigError::AlreadyExists(_)), "{err:?}");
        assert!(err.to_string().contains("already exists"));
    }

    #[cfg(unix)]
    #[test]
    fn init_force_overwrites_and_resets_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("config.toml");
        std::fs::write(&f, "old content that is longer than nothing at all").unwrap();
        set_mode(&f, 0o644);
        init_config(&f, true).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), TEMPLATE);
        assert_eq!(mode_of(&f), 0o600);
    }

    // --- Debug masking and exit codes ----------------------------------------

    #[test]
    fn debug_never_prints_token() {
        let gl = GitLabSection {
            url: "https://g".into(),
            token: "secret-xyz".into(),
            username: None,
        };
        let yt = YouTrackSection {
            url: "https://y".into(),
            token: "secret-xyz".into(),
            projects: vec![],
            state_field: "State".into(),
        };
        let glc = GitLabConfig {
            url: "https://g".into(),
            token: "secret-xyz".into(),
            username: None,
        };
        let ytc = YouTrackConfig {
            url: "https://y".into(),
            token: "secret-xyz".into(),
            projects: vec![],
            state_field: "State".into(),
        };
        let cfg = Config {
            path: p(),
            gitlab: Some(gl.clone()),
            youtrack: Some(yt.clone()),
            meetings: vec![],
        };
        let sources = Sources {
            gitlab: Some(glc.clone()),
            youtrack: Some(ytc.clone()),
            notes: vec![],
        };
        for s in [
            format!("{gl:?}"),
            format!("{yt:?}"),
            format!("{glc:?}"),
            format!("{ytc:?}"),
            format!("{cfg:?}"),
            format!("{sources:?}"),
            format!("{cfg:#?}"),
        ] {
            assert!(!s.contains("secret-xyz"), "{s}");
            assert!(s.contains("***"), "{s}");
        }
    }

    #[test]
    fn debug_empty_token_prints_empty() {
        let gl = GitLabSection {
            url: "https://g".into(),
            token: String::new(),
            username: None,
        };
        let s = format!("{gl:?}");
        assert!(s.contains("token: \"\""), "{s}");
    }

    #[test]
    fn exit_codes() {
        let io = || std::io::Error::other("x");
        assert_eq!(ConfigError::AlreadyExists(p()).exit_code(), 1);
        assert_eq!(
            ConfigError::Io {
                path: p(),
                source: io()
            }
            .exit_code(),
            1
        );
        assert_eq!(ConfigError::NoConfigDir.exit_code(), 2);
        assert_eq!(ConfigError::NotFound(p()).exit_code(), 2);
        assert_eq!(
            ConfigError::Invalid {
                path: p(),
                msg: String::new()
            }
            .exit_code(),
            2
        );
    }
}
