use std::io::IsTerminal;

use chrono::{Local, SecondsFormat, TimeZone, Utc};

use crate::cli::{Cli, Command};
use crate::config::{ConfigError, init_config, load_config, resolve_config_path, validate_sources};
use crate::error::{ApiError, CliError};
use crate::gitlab::GitLabApi;
use crate::gitlab::client::GitLabClient;
use crate::issue_ref::mr_issue_ids;
use crate::model::{Issue, IssueId, MergeRequest};
use crate::report::{Report, apply_aliases, build, known_projects};
use crate::ui::{render, select_mode};
use crate::window::{Window, WindowRequest, compute, format_window_line, parse_user_datetime};
use crate::youtrack::client::YouTrackClient;
use crate::youtrack::collect::fetch_referenced;
use crate::youtrack::{YouTrackApi, YtSettings};

/// Build the window request from the CLI flags. Shared by `window` and the report.
pub(crate) fn window_request<Tz: TimeZone>(cli: &Cli, tz: &Tz) -> Result<WindowRequest, CliError> {
    let now = match cli.now.as_deref() {
        Some(s) => parse_user_datetime(tz, s)?,
        None => Utc::now(),
    };
    let since = cli
        .since
        .as_deref()
        .map(|s| parse_user_datetime(tz, s))
        .transpose()?;
    let until = cli
        .until
        .as_deref()
        .map(|s| parse_user_datetime(tz, s))
        .transpose()?;
    Ok(WindowRequest {
        now,
        since,
        until,
        previous: u32::from(cli.previous),
    })
}

fn local_and_utc(t: chrono::DateTime<Utc>) -> String {
    format!(
        "{} ({})",
        t.with_timezone(&Local)
            .to_rfc3339_opts(SecondsFormat::Secs, false),
        t.to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

fn print_window(w: &Window, line: &str) {
    println!("{line}");
    println!("start: {}", local_and_utc(w.start));
    println!("end:   {}", local_and_utc(w.end));
}

/// Result of fetching and merging both sources.
#[derive(Debug)]
pub struct ReportOutcome {
    pub report: Report,
    pub warnings: Vec<String>,
    /// Service names (`"GitLab"` / `"YouTrack"`) whose fetch failed.
    pub failed_sources: Vec<&'static str>,
    pub enabled_sources: usize,
}

/// Fetches both sources concurrently, then runs the 7.4 pipeline. No I/O besides the trait objects.
pub fn build_report(
    gitlab: Option<(&dyn GitLabApi, Option<&str>)>,
    youtrack: Option<&dyn YouTrackApi>,
    yt: &YtSettings,
    window: &Window,
) -> ReportOutcome {
    let enabled_sources = usize::from(gitlab.is_some()) + usize::from(youtrack.is_some());
    let (gl_res, yt_res) = std::thread::scope(|s| {
        let g = gitlab
            .map(|(api, user)| s.spawn(move || crate::gitlab::collect::collect(api, user, window)));
        let y =
            youtrack.map(|api| s.spawn(move || crate::youtrack::collect::collect(api, window, yt)));
        (
            g.map(|h| h.join().expect("gitlab thread panicked")),
            y.map(|h| h.join().expect("youtrack thread panicked")),
        )
    });

    let mut warnings: Vec<String> = Vec::new();
    let mut failed_sources: Vec<&'static str> = Vec::new();
    let fail = |name: &'static str, e: ApiError, w: &mut Vec<String>, f: &mut Vec<&'static str>| {
        w.push(e.to_string());
        f.push(name);
    };

    let mut mrs: Vec<MergeRequest> = Vec::new();
    match gl_res {
        Some(Ok(out)) => {
            warnings.extend(out.warnings);
            mrs = out.mrs;
        }
        Some(Err(e)) => fail("GitLab", e, &mut warnings, &mut failed_sources),
        None => {}
    }

    let mut issues: Vec<Issue> = Vec::new();
    let mut admin = None;
    let mut youtrack_ok = false;
    match yt_res {
        Some(Ok(out)) => {
            warnings.extend(out.warnings);
            issues = out.issues;
            admin = out.admin_projects;
            youtrack_ok = true;
        }
        Some(Err(e)) => fail("YouTrack", e, &mut warnings, &mut failed_sources),
        None => {}
    }

    // 7.4 step 1-2: known projects, then issue IDs per MR.
    let known = known_projects(admin.as_ref(), &yt.projects, &issues, youtrack_ok);
    for mr in &mut mrs {
        mr.issue_ids = mr_issue_ids(mr, known.as_ref());
    }

    // Step 3: referenced IDs that were not fetched already, first-seen order.
    let mut referenced: Vec<IssueId> = Vec::new();
    for id in mrs.iter().flat_map(|m| m.issue_ids.iter()) {
        if !issues.iter().any(|i| &i.id == id) && !referenced.contains(id) {
            referenced.push(id.clone());
        }
    }

    // Step 4: fetch them, only when YouTrack ran and succeeded.
    let mut aliases = Default::default();
    if let (true, Some(api)) = (youtrack_ok, youtrack)
        && !referenced.is_empty()
    {
        match fetch_referenced(api, &referenced, yt, &mut warnings) {
            Ok((found, found_aliases)) => {
                for issue in found {
                    if !issues.iter().any(|i| i.id == issue.id) {
                        issues.push(issue);
                    }
                }
                aliases = found_aliases;
            }
            Err(e) => warnings.push(format!("YouTrack: cannot fetch referenced issues ({e})")),
        }
    }

    // Steps 5-6.
    apply_aliases(&mut mrs, &aliases);
    ReportOutcome {
        report: build(mrs, issues),
        warnings,
        failed_sources,
        enabled_sources,
    }
}

fn client_error(e: ApiError) -> CliError {
    match e {
        ApiError::Other { .. } => CliError::usage(e),
        _ => CliError::runtime(e),
    }
}

fn run_report(cli: &Cli) -> Result<u8, CliError> {
    let path = resolve_config_path(cli.config.as_deref())?;
    let (cfg, config_warnings) = load_config(&path)?;
    for w in &config_warnings {
        eprintln!("warning: {w}");
    }
    let sources = validate_sources(&cfg, cli.no_gitlab, cli.no_youtrack)?;

    let req = window_request(cli, &Local)?;
    let window = compute(&Local, &cfg.meetings, &req)?;

    let yt_settings = cfg
        .youtrack
        .as_ref()
        .map(|sec| YtSettings {
            base_url: sec.url.clone(),
            projects: sec.projects.clone(),
            state_field: sec.state_field.clone(),
        })
        .unwrap_or_default();

    let gl_client = sources
        .gitlab
        .as_ref()
        .map(|c| GitLabClient::new(c, cli.verbose))
        .transpose()
        .map_err(client_error)?;
    let yt_client = sources
        .youtrack
        .as_ref()
        .map(|c| YouTrackClient::new(c, cli.verbose))
        .transpose()
        .map_err(client_error)?;
    let gitlab = gl_client.as_ref().map(|c| {
        (
            c as &dyn GitLabApi,
            sources.gitlab.as_ref().and_then(|g| g.username.as_deref()),
        )
    });
    let youtrack = yt_client.as_ref().map(|c| c as &dyn YouTrackApi);

    let stderr_tty = std::io::stderr().is_terminal();
    let mode = select_mode(
        cli.json,
        cli.plain,
        std::io::stdout().is_terminal(),
        stderr_tty,
        std::io::stdin().is_terminal(),
    );

    if stderr_tty {
        eprintln!("Fetching GitLab and YouTrack\u{2026}");
    }
    let outcome = build_report(gitlab, youtrack, &yt_settings, &window);
    if stderr_tty {
        let _ = console::Term::stderr().clear_last_lines(1);
    }

    let mut warnings = sources.notes;
    warnings.extend(outcome.warnings);
    if outcome.failed_sources.len() == outcome.enabled_sources {
        return Err(CliError::runtime(anyhow::anyhow!(warnings.join("\n"))));
    }
    render(mode, &outcome.report, &window, &warnings, req.now)?;
    Ok(if outcome.failed_sources.is_empty() {
        0
    } else {
        3
    })
}

pub fn run(cli: Cli) -> Result<u8, CliError> {
    match &cli.command {
        Some(Command::Init { force }) => {
            let path = resolve_config_path(cli.config.as_deref())?;
            init_config(&path, *force)?;
            println!(
                "Created {} (mode 0600). Edit it to add your GitLab and YouTrack tokens.",
                path.display()
            );
            Ok(0)
        }
        Some(Command::Window) => {
            let path = resolve_config_path(cli.config.as_deref())?;
            let meetings = if path.exists() {
                let (config, warnings) = load_config(&path)?;
                for w in warnings {
                    eprintln!("warning: {w}");
                }
                config.meetings
            } else if cli.since.is_some() {
                Vec::new()
            } else {
                return Err(ConfigError::NotFound(path).into());
            };
            let req = window_request(&cli, &Local)?;
            let w = compute(&Local, &meetings, &req)?;
            print_window(&w, &format_window_line(&Local, &w, req.now));
            Ok(0)
        }
        None => run_report(&cli),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitlab::fakes::{FakeGitLab, mr, ts};
    use crate::http::FakeErr;
    use crate::report::Entry;
    use crate::window::WindowOrigin;
    use crate::youtrack::fakes::{FakeYouTrack, yt_issue};

    const ASSIGNED_Q: &str = "assignee: me updated: 2026-09-27 .. 2026-10-02";

    fn window() -> Window {
        Window {
            start: ts("2026-09-29T08:00:00Z"),
            end: ts("2026-09-30T09:00:00Z"),
            origin: WindowOrigin::Explicit,
        }
    }

    fn in_window_ms() -> i64 {
        ts("2026-09-29T12:00:00Z").timestamp_millis()
    }

    fn settings(projects: &[&str]) -> YtSettings {
        YtSettings {
            base_url: "https://yt.example.com".into(),
            projects: projects.iter().map(|s| s.to_string()).collect(),
            state_field: "State".into(),
        }
    }

    fn id(s: &str) -> IssueId {
        s.parse().unwrap()
    }

    fn run_both(gl: &FakeGitLab, yt: &FakeYouTrack, projects: &[&str]) -> ReportOutcome {
        build_report(Some((gl, None)), Some(yt), &settings(projects), &window())
    }

    #[test]
    fn link_orphan_and_referenced_issue() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![
            mr(1, 10, "SP-1 fix", "fix", "", t),
            mr(1, 11, "unrelated work", "misc", "", t),
            mr(2, 12, "other", "feature/SP-2-x", "", t),
        ]);
        let mut yt = FakeYouTrack::new();
        yt.projects = Ok(vec!["SP".into()]);
        yt.add_search(
            ASSIGNED_Q,
            vec![yt_issue("SP-1", "Fix it", in_window_ms(), "In Progress")],
        );
        // SP-2 is only fetchable by ID.
        yt.add_by_id("SP-2", yt_issue("SP-2", "Second", 0, "Open"));

        let out = run_both(&gl, &yt, &[]);
        assert!(out.failed_sources.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.enabled_sources, 2);
        let entries = &out.report.entries;
        assert_eq!(entries.len(), 3, "{entries:#?}");

        let group = |ident: &str| {
            entries.iter().find_map(|e| match e {
                Entry::Issue { issue, mrs } if issue.id == id(ident) => Some((issue, mrs)),
                _ => None,
            })
        };
        let (i1, mrs1) = group("SP-1").expect("SP-1 group");
        assert!(i1.in_window);
        assert_eq!(mrs1.len(), 1);
        assert_eq!(mrs1[0].title, "SP-1 fix");
        assert_eq!(mrs1[0].issue_ids, vec![id("SP-1")]);

        let (i2, mrs2) = group("SP-2").expect("referenced SP-2 group");
        assert!(!i2.in_window);
        assert_eq!(mrs2.len(), 1);
        assert_eq!(mrs2[0].iid, 12);

        let orphans: Vec<_> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::OrphanMr(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].iid, 11);
        assert_eq!(out.report.counts.linked_groups, 2);
        assert_eq!(out.report.counts.orphan_mrs, 1);
        assert_eq!(out.report.counts.mrs, 3);
    }

    #[test]
    fn alias_rewrites_mr_reference() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "OLD-12 fix", "fix", "", t)]);
        let mut yt = FakeYouTrack::new();
        yt.projects = Ok(vec!["OLD".into(), "NEW".into()]);
        yt.add_by_id("OLD-12", yt_issue("NEW-5", "Moved", 0, "Open"));

        let out = run_both(&gl, &yt, &[]);
        assert!(out.failed_sources.is_empty(), "{:?}", out.warnings);
        let entries = &out.report.entries;
        assert_eq!(entries.len(), 1, "{entries:#?}");
        match &entries[0] {
            Entry::Issue { issue, mrs } => {
                assert_eq!(issue.id, id("NEW-5"));
                assert_eq!(mrs.len(), 1);
                assert_eq!(mrs[0].issue_ids, vec![id("NEW-5")]);
            }
            other => panic!("expected the NEW-5 group, got {other:?}"),
        }
        for e in entries {
            if let Entry::Issue { issue, .. } = e {
                assert_ne!(issue.id, id("OLD-12"));
            }
        }
    }

    #[test]
    fn in_window_issue_wins_over_referenced_fetch() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "SP-1 fix", "fix", "", t)]);
        let mut yt = FakeYouTrack::new();
        yt.projects = Ok(vec!["SP".into()]);
        yt.add_search(
            ASSIGNED_Q,
            vec![yt_issue("SP-1", "Fix it", in_window_ms(), "In Progress")],
        );
        let out = run_both(&gl, &yt, &[]);
        // SP-1 was fetched in the window, so it is not fetched again by ID.
        assert!(yt.calls_with("issue_by_id").is_empty());
        assert_eq!(out.report.entries.len(), 1);
    }

    #[test]
    fn gitlab_failure_keeps_youtrack_results() {
        let mut gl = FakeGitLab::new();
        gl.user = Err(FakeErr::Unauthorized);
        let mut yt = FakeYouTrack::new();
        yt.projects = Ok(vec!["SP".into()]);
        yt.add_search(
            ASSIGNED_Q,
            vec![yt_issue("SP-1", "Fix it", in_window_ms(), "Open")],
        );
        let out = run_both(&gl, &yt, &[]);
        assert_eq!(out.failed_sources, vec!["GitLab"]);
        assert_eq!(out.enabled_sources, 2);
        assert_eq!(out.report.entries.len(), 1);
        assert!(
            out.warnings.iter().any(|w| w.starts_with("GitLab")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn youtrack_failure_keeps_gitlab_results_and_skips_referenced_fetch() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "SP-1 fix", "fix", "", t)]);
        let mut yt = FakeYouTrack::new();
        yt.projects = Err(FakeErr::Unauthorized);
        let out = run_both(&gl, &yt, &["SP"]);
        assert_eq!(out.failed_sources, vec!["YouTrack"]);
        assert_eq!(out.report.entries.len(), 1);
        match &out.report.entries[0] {
            Entry::OrphanMr(m) => assert_eq!(m.issue_ids, vec![id("SP-1")]),
            other => panic!("expected orphan, got {other:?}"),
        }
        assert!(yt.calls_with("issue_by_id").is_empty());
        assert!(yt.search_queries().is_empty());
    }

    #[test]
    fn both_sources_failing() {
        let mut gl = FakeGitLab::new();
        gl.user = Err(FakeErr::Unauthorized);
        let mut yt = FakeYouTrack::new();
        yt.projects = Err(FakeErr::Unauthorized);
        let out = run_both(&gl, &yt, &[]);
        assert_eq!(out.failed_sources, vec!["GitLab", "YouTrack"]);
        assert_eq!(out.failed_sources.len(), out.enabled_sources);
        assert_eq!(out.enabled_sources, 2);
        assert!(out.report.entries.is_empty());
        assert_eq!(out.warnings.len(), 2);
    }

    #[test]
    fn single_disabled_source_counts_only_enabled() {
        let gl = FakeGitLab::new();
        let out = build_report(Some((&gl, None)), None, &YtSettings::default(), &window());
        assert_eq!(out.enabled_sources, 1);
        assert!(out.failed_sources.is_empty());
    }

    #[test]
    fn known_set_from_configured_projects_without_youtrack() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "SP-1 UTF-8 FIX-2", "fix", "", t)]);
        let out = build_report(Some((&gl, None)), None, &settings(&["SP"]), &window());
        assert!(out.failed_sources.is_empty());
        match &out.report.entries[0] {
            Entry::OrphanMr(m) => assert_eq!(m.issue_ids, vec![id("SP-1")]),
            other => panic!("expected orphan, got {other:?}"),
        }
    }

    #[test]
    fn no_known_set_uses_denylist() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "SP-1 UTF-8", "fix", "", t)]);
        let out = build_report(Some((&gl, None)), None, &YtSettings::default(), &window());
        match &out.report.entries[0] {
            Entry::OrphanMr(m) => assert_eq!(m.issue_ids, vec![id("SP-1")]),
            other => panic!("expected orphan, got {other:?}"),
        }
    }

    #[test]
    fn configured_username_is_passed_to_gitlab() {
        let mut gl = FakeGitLab::new();
        gl.users_by_name = vec![crate::gitlab::GlUser {
            id: 7,
            username: "eric".into(),
        }];
        let out = build_report(
            Some((&gl, Some("eric"))),
            None,
            &YtSettings::default(),
            &window(),
        );
        assert!(out.failed_sources.is_empty());
        assert!(gl.calls().iter().any(|c| c == "users_by_username eric"));
    }

    #[test]
    fn referenced_fetch_error_becomes_warning() {
        let t = ts("2026-09-29T12:00:00Z");
        let mut gl = FakeGitLab::new();
        gl.authored = Ok(vec![mr(1, 10, "SP-2 fix", "fix", "", t)]);
        let mut yt = FakeYouTrack::new();
        yt.projects = Ok(vec!["SP".into()]);
        // A 401 on the by-id lookup is fatal inside fetch_by_ids, so the pipeline warns and goes on.
        yt.by_id.insert("SP-2".into(), Err(FakeErr::Unauthorized));
        let out = run_both(&gl, &yt, &[]);
        assert!(out.failed_sources.is_empty());
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("YouTrack: cannot fetch referenced issues (")),
            "{:?}",
            out.warnings
        );
        assert_eq!(out.report.entries.len(), 1);
    }
}
