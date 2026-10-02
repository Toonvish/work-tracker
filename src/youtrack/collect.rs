//! Pure collection logic over `YouTrackApi`: assigned issues, activities with a fallback
//! chain, fetch-by-ID with alias handling, and conversion to `Issue`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};

use super::{
    ACTIVITY_PAGE_CAP, ActivityQuery, CORE_CATEGORIES, FULL_CATEGORIES, ISSUE_PAGE_CAP,
    YouTrackApi, YtActivity, YtIssue, YtSettings, project_clause,
};
use crate::error::ApiError;
use crate::http::Listing;
use crate::model::{Issue, IssueId, Role};
use crate::window::{Window, padded_dates};

/// IDs per batched `issue id: A or issue id: B` search.
const BATCH_CHUNK: usize = 20;
/// Most `issue_by_id` requests per `fetch_by_ids` call.
const PER_ID_CAP: usize = 50;
/// Most IDs `fetch_referenced` looks at.
const REFERENCED_CAP: usize = 50;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FetchedById {
    /// Deduped by canonical id.
    pub issues: Vec<YtIssue>,
    /// requested -> canonical (moved or renamed issues).
    pub aliases: BTreeMap<IssueId, IssueId>,
    /// Not found (404) or failed.
    pub missing: Vec<IssueId>,
}

#[derive(Debug, Clone)]
pub struct YouTrackOutcome {
    pub issues: Vec<Issue>,
    /// Upper-cased project short names, or `None` when the project list could not be read.
    pub admin_projects: Option<BTreeSet<String>>,
    pub warnings: Vec<String>,
}

fn millis(ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms)
}

fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn warn_truncated(warnings: &mut Vec<String>, what: &str, cap: u32, truncated: bool) {
    if truncated {
        warnings.push(format!(
            "{what}: stopped after {cap} pages; results may be incomplete"
        ));
    }
}

/// Reads the state custom field per 6.5. Falls back to `Resolved` / `Open`.
fn extract_state(yt: &YtIssue, state_field: &str) -> String {
    let named = |v: &serde_json::Value| -> Option<String> {
        match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o.get("name")?.as_str().map(str::to_string),
            _ => None,
        }
    };
    let value = yt
        .custom_fields
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(state_field))
        .and_then(|f| f.value.as_ref());
    let state = match value {
        Some(serde_json::Value::Array(items)) => {
            let names: Vec<String> = items.iter().filter_map(named).collect();
            (!names.is_empty()).then(|| names.join(", "))
        }
        Some(v) => named(v),
        None => None,
    };
    match state {
        Some(s) if !s.trim().is_empty() => s,
        _ if yt.resolved.is_some() => "Resolved".to_string(),
        _ => "Open".to_string(),
    }
}

/// Converts the API DTO. `None` when `idReadable` is not a valid `IssueId`.
pub fn convert_issue(
    yt: &YtIssue,
    settings: &YtSettings,
    roles: BTreeSet<Role>,
    in_window: bool,
) -> Option<Issue> {
    let id: IssueId = yt.id_readable.parse().ok()?;
    let web_url = format!(
        "{}/issue/{}",
        settings.base_url.trim_end_matches('/'),
        yt.id_readable
    );
    Some(Issue {
        id,
        summary: yt.summary.clone().unwrap_or_default(),
        state: extract_state(yt, &settings.state_field),
        resolved: yt.resolved.is_some(),
        web_url,
        updated: yt
            .updated
            .and_then(millis)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        roles,
        in_window,
    })
}

/// Inserts `issue`, merging roles (and `in_window`) into an existing entry with the same id.
fn merge_issue(map: &mut BTreeMap<IssueId, Issue>, issue: Issue) {
    match map.get_mut(&issue.id) {
        Some(existing) => {
            existing.roles.extend(issue.roles);
            existing.in_window |= issue.in_window;
        }
        None => {
            map.insert(issue.id.clone(), issue);
        }
    }
}

/// Fetches issues by ID: batched search as an optimisation, then one request per missing ID.
pub fn fetch_by_ids(
    api: &dyn YouTrackApi,
    ids: &[IssueId],
    warnings: &mut Vec<String>,
) -> Result<FetchedById, ApiError> {
    let mut unique: Vec<IssueId> = Vec::new();
    for id in ids {
        if !unique.contains(id) {
            unique.push(id.clone());
        }
    }

    let mut out = FetchedById::default();
    let mut seen: BTreeSet<IssueId> = BTreeSet::new();
    let mut per_id_requests = 0usize;
    let mut skipped = 0usize;

    for chunk in unique.chunks(BATCH_CHUNK) {
        let query = chunk
            .iter()
            .map(|id| format!("issue id: {id}"))
            .collect::<Vec<_>>()
            .join(" or ");
        let mut returned: BTreeSet<IssueId> = BTreeSet::new();
        match api.search_issues(&query) {
            Ok(listing) => {
                for yt in listing.items {
                    let Ok(id) = yt.id_readable.parse::<IssueId>() else {
                        continue;
                    };
                    if chunk.contains(&id) && returned.insert(id.clone()) && seen.insert(id) {
                        out.issues.push(yt);
                    }
                }
            }
            Err(e) if e.status() == Some(401) => return Err(e),
            Err(_) => {} // the batch is only an optimisation
        }

        for id in chunk.iter().filter(|id| !returned.contains(*id)) {
            if per_id_requests >= PER_ID_CAP {
                skipped += 1;
                out.missing.push(id.clone());
                continue;
            }
            per_id_requests += 1;
            match api.issue_by_id(&id.to_string()) {
                Ok(None) => out.missing.push(id.clone()),
                Ok(Some(yt)) => {
                    if !yt.id_readable.eq_ignore_ascii_case(&id.to_string())
                        && let Ok(canonical) = yt.id_readable.parse::<IssueId>()
                    {
                        out.aliases.insert(id.clone(), canonical);
                    }
                    let canonical = yt.id_readable.parse::<IssueId>().unwrap_or(id.clone());
                    if seen.insert(canonical) {
                        out.issues.push(yt);
                    }
                }
                Err(e) if e.status() == Some(401) => return Err(e),
                Err(e) => {
                    warnings.push(format!("YouTrack: cannot fetch {id} ({e})"));
                    out.missing.push(id.clone());
                }
            }
        }
    }

    if skipped > 0 {
        warnings.push(format!(
            "YouTrack: too many issues to fetch individually; skipped {skipped}"
        ));
    }
    Ok(out)
}

/// Fetches the issues MRs refer to (at most 50). Results are `Referenced`, not in the window.
pub fn fetch_referenced(
    api: &dyn YouTrackApi,
    ids: &[IssueId],
    settings: &YtSettings,
    warnings: &mut Vec<String>,
) -> Result<(Vec<Issue>, BTreeMap<IssueId, IssueId>), ApiError> {
    if ids.len() > REFERENCED_CAP {
        warnings.push(format!(
            "YouTrack: {} issues referenced by MRs; fetching the first {REFERENCED_CAP}",
            ids.len()
        ));
    }
    let take = &ids[..ids.len().min(REFERENCED_CAP)];
    let fetched = fetch_by_ids(api, take, warnings)?;
    let issues = fetched
        .issues
        .iter()
        .filter_map(|yt| convert_issue(yt, settings, BTreeSet::from([Role::Referenced]), false))
        .collect();
    Ok((issues, fetched.aliases))
}

/// 6.1: project short names, upper-cased. Only a 401 is fatal.
fn fetch_admin_projects(
    api: &dyn YouTrackApi,
    warnings: &mut Vec<String>,
) -> Result<Option<BTreeSet<String>>, ApiError> {
    match api.project_short_names() {
        Ok(names) => Ok(Some(names.iter().map(|n| n.to_uppercase()).collect())),
        Err(e) if e.status() == Some(401) => Err(e),
        Err(e) => {
            warnings.push(format!(
                "YouTrack: cannot list projects ({e}); issue-ID matching uses the configured projects"
            ));
            Ok(None)
        }
    }
}

/// 6.2: issues assigned to me, filtered client-side by `updated`.
fn fetch_assigned(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    dates: (NaiveDate, NaiveDate),
    warnings: &mut Vec<String>,
) -> Result<Vec<Issue>, ApiError> {
    let mut query = String::new();
    if let Some(clause) = project_clause(&settings.projects) {
        query.push_str(&clause);
        query.push(' ');
    }
    query.push_str(&format!(
        "assignee: me updated: {} .. {}",
        fmt_date(dates.0),
        fmt_date(dates.1)
    ));
    let listing = api.search_issues(&query)?;
    warn_truncated(
        warnings,
        "YouTrack assigned issues",
        ISSUE_PAGE_CAP,
        listing.truncated,
    );
    Ok(in_window_issues(
        listing,
        window,
        settings,
        BTreeSet::from([Role::Assignee]),
    ))
}

fn in_window_issues(
    listing: Listing<YtIssue>,
    window: &Window,
    settings: &YtSettings,
    roles: BTreeSet<Role>,
) -> Vec<Issue> {
    listing
        .items
        .iter()
        .filter(|yt| {
            yt.updated
                .and_then(millis)
                .is_some_and(|t| window.contains(t))
        })
        .filter_map(|yt| convert_issue(yt, settings, roles.clone(), true))
        .collect()
}

/// Result of the activities chain.
enum Activities {
    /// Raw activities from `/api/activities`.
    Events(Vec<YtActivity>),
    /// Complete issues from the approximate query fallback.
    Issues(Vec<Issue>),
}

fn activity_query(
    window: &Window,
    settings: &YtSettings,
    categories: &[&'static str],
) -> ActivityQuery {
    ActivityQuery {
        categories: categories.to_vec(),
        start_ms: window.start.timestamp_millis(),
        end_ms: window.end.timestamp_millis(),
        issue_query: project_clause(&settings.projects),
    }
}

/// One `my_activities` call with a category set.
fn fetch_activities_with(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    categories: &[&'static str],
    warnings: &mut Vec<String>,
) -> Result<Vec<YtActivity>, ApiError> {
    let listing = api.my_activities(&activity_query(window, settings, categories))?;
    warn_truncated(
        warnings,
        "YouTrack activities",
        ACTIVITY_PAGE_CAP,
        listing.truncated,
    );
    Ok(listing.items)
}

/// Step 1 of 6.3.
fn activities_full(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    warnings: &mut Vec<String>,
) -> Result<Vec<YtActivity>, ApiError> {
    fetch_activities_with(api, window, settings, FULL_CATEGORIES, warnings)
}

/// Step 2 of 6.3.
fn activities_core(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    warnings: &mut Vec<String>,
) -> Result<Vec<YtActivity>, ApiError> {
    warnings.push(
        "YouTrack: server rejected some activity categories; retrying with the core set"
            .to_string(),
    );
    fetch_activities_with(api, window, settings, CORE_CATEGORIES, warnings)
}

/// Step 3 of 6.3: approximate updater/commenter query.
fn changed_issues_query(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    dates: (NaiveDate, NaiveDate),
    status: u16,
    warnings: &mut Vec<String>,
) -> Result<Vec<Issue>, ApiError> {
    warnings.push(format!(
        "YouTrack: activities API unavailable (HTTP {status}); using approximate updater/commenter query"
    ));
    let mut query = String::new();
    if let Some(clause) = project_clause(&settings.projects) {
        query.push_str(&clause);
        query.push(' ');
    }
    query.push_str(&format!(
        "(updater: me or commenter: me) updated: {} .. {}",
        fmt_date(dates.0),
        fmt_date(dates.1)
    ));
    let listing = api.search_issues(&query)?;
    warn_truncated(
        warnings,
        "YouTrack changed issues",
        ISSUE_PAGE_CAP,
        listing.truncated,
    );
    Ok(in_window_issues(
        listing,
        window,
        settings,
        BTreeSet::from([Role::Updater]),
    ))
}

/// The whole fallback chain of 6.3.
fn fetch_changed(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
    dates: (NaiveDate, NaiveDate),
    warnings: &mut Vec<String>,
) -> Result<Activities, ApiError> {
    let fallback_status = match activities_full(api, window, settings, warnings) {
        Ok(items) => return Ok(Activities::Events(items)),
        Err(e) => match e.status() {
            Some(400) => match activities_core(api, window, settings, warnings) {
                Ok(items) => return Ok(Activities::Events(items)),
                Err(e) => match e.status() {
                    Some(s @ (400 | 404)) => s,
                    _ => return Err(e),
                },
            },
            Some(404) => 404,
            _ => return Err(e),
        },
    };
    changed_issues_query(api, window, settings, dates, fallback_status, warnings)
        .map(Activities::Issues)
}

/// Issue id and role of each in-window activity. Unparseable or out-of-window ones are skipped.
fn activity_roles(activities: &[YtActivity], window: &Window) -> BTreeMap<IssueId, BTreeSet<Role>> {
    let mut out: BTreeMap<IssueId, BTreeSet<Role>> = BTreeMap::new();
    for a in activities {
        if !millis(a.timestamp).is_some_and(|t| window.contains(t)) {
            continue;
        }
        let Some(target) = &a.target else { continue };
        let raw = target
            .id_readable
            .as_deref()
            .or_else(|| target.issue.as_ref().map(|i| i.id_readable.as_str()));
        let Some(id) = raw.and_then(|r| r.parse::<IssueId>().ok()) else {
            continue;
        };
        let role = match a.category.as_ref().map(|c| c.id.as_str()) {
            Some("CommentsCategory" | "CommentTextCategory" | "CommentAttachmentsCategory") => {
                Role::Commenter
            }
            _ => Role::Updater,
        };
        out.entry(id).or_default().insert(role);
    }
    out
}

/// Collects everything YouTrack contributes to the report (6.1 to 6.3, then fetch-by-ID).
pub fn collect(
    api: &dyn YouTrackApi,
    window: &Window,
    settings: &YtSettings,
) -> Result<YouTrackOutcome, ApiError> {
    let mut warnings = Vec::new();
    let dates = padded_dates(window);

    let admin_projects = fetch_admin_projects(api, &mut warnings)?;

    let mut issues: BTreeMap<IssueId, Issue> = BTreeMap::new();
    for issue in fetch_assigned(api, window, settings, dates, &mut warnings)? {
        merge_issue(&mut issues, issue);
    }

    match fetch_changed(api, window, settings, dates, &mut warnings)? {
        Activities::Issues(found) => {
            for issue in found {
                merge_issue(&mut issues, issue);
            }
        }
        Activities::Events(activities) => {
            let roles = activity_roles(&activities, window);
            let mut to_fetch = Vec::new();
            for (id, r) in &roles {
                match issues.get_mut(id) {
                    Some(existing) => existing.roles.extend(r.iter().copied()),
                    None => to_fetch.push(id.clone()),
                }
            }
            if !to_fetch.is_empty() {
                let fetched = fetch_by_ids(api, &to_fetch, &mut warnings)?;
                for yt in &fetched.issues {
                    let Ok(id) = yt.id_readable.parse::<IssueId>() else {
                        continue;
                    };
                    // Activity ids are canonical; if an alias slipped through, use the requester's roles.
                    let issue_roles = roles.get(&id).cloned().or_else(|| {
                        fetched
                            .aliases
                            .iter()
                            .find(|(_, canonical)| **canonical == id)
                            .and_then(|(requested, _)| roles.get(requested).cloned())
                    });
                    let issue_roles =
                        issue_roles.unwrap_or_else(|| BTreeSet::from([Role::Updater]));
                    if let Some(issue) = convert_issue(yt, settings, issue_roles, true) {
                        merge_issue(&mut issues, issue);
                    }
                }
            }
        }
    }

    Ok(YouTrackOutcome {
        issues: issues.into_values().collect(),
        admin_projects,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FakeErr;
    use crate::window::WindowOrigin;
    use crate::youtrack::fakes::{FakeYouTrack, activity_on_comment, activity_on_issue, yt_issue};

    const IN: i64 = 1_790_683_200_000; // 2026-09-29T12:00Z
    const OUT_AFTER: i64 = 1_790_856_000_000; // 2026-10-01T12:00Z
    const OUT_BEFORE: i64 = 1_790_596_800_000; // 2026-09-28T12:00Z

    const Q_ASSIGNED: &str = "assignee: me updated: 2026-09-27 .. 2026-10-02";
    const Q_CHANGED: &str = "(updater: me or commenter: me) updated: 2026-09-27 .. 2026-10-02";

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn win() -> Window {
        Window {
            start: ts("2026-09-29T08:00:00Z"),
            end: ts("2026-09-30T09:00:00Z"),
            origin: WindowOrigin::Explicit,
        }
    }

    fn settings() -> YtSettings {
        YtSettings {
            base_url: "https://yt.example.com".into(),
            ..YtSettings::default()
        }
    }

    fn id(s: &str) -> IssueId {
        s.parse().unwrap()
    }

    fn ids(v: &[&str]) -> Vec<IssueId> {
        v.iter().map(|s| id(s)).collect()
    }

    fn run(f: &FakeYouTrack) -> YouTrackOutcome {
        collect(f, &win(), &settings()).unwrap()
    }

    fn roles(r: &[Role]) -> BTreeSet<Role> {
        r.iter().copied().collect()
    }

    fn issue_of<'a>(o: &'a YouTrackOutcome, s: &str) -> &'a Issue {
        let want = id(s);
        o.issues
            .iter()
            .find(|i| i.id == want)
            .unwrap_or_else(|| panic!("{s} not in {:?}", o.issues))
    }

    // ---- assigned query ----

    #[test]
    fn assigned_query_without_projects() {
        let mut f = FakeYouTrack::new();
        f.add_search(Q_ASSIGNED, vec![yt_issue("SP-1", "a", IN, "Open")]);
        let o = run(&f);
        assert_eq!(f.search_queries()[0], Q_ASSIGNED);
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].roles, roles(&[Role::Assignee]));
        assert!(o.issues[0].in_window);
    }

    #[test]
    fn assigned_query_with_projects() {
        let f = FakeYouTrack::new();
        let s = YtSettings {
            projects: vec!["SP".into(), "MS".into()],
            ..settings()
        };
        collect(&f, &win(), &s).unwrap();
        assert_eq!(
            f.search_queries()[0],
            "(project: SP or project: MS) assignee: me updated: 2026-09-27 .. 2026-10-02"
        );
        let f = FakeYouTrack::new();
        let s = YtSettings {
            projects: vec!["SP".into()],
            ..settings()
        };
        collect(&f, &win(), &s).unwrap();
        assert_eq!(
            f.search_queries()[0],
            "project: SP assignee: me updated: 2026-09-27 .. 2026-10-02"
        );
    }

    #[test]
    fn activities_get_issue_query_when_projects_set() {
        let f = FakeYouTrack::new();
        let s = YtSettings {
            projects: vec!["SP".into(), "MS".into()],
            ..settings()
        };
        collect(&f, &win(), &s).unwrap();
        let call = &f.calls_with("my_activities")[0];
        assert!(call.ends_with("(project: SP or project: MS)"), "{call}");
        let f = FakeYouTrack::new();
        run(&f);
        assert!(f.calls_with("my_activities")[0].ends_with(" -"));
    }

    #[test]
    fn activity_query_uses_window_millis() {
        let f = FakeYouTrack::new();
        run(&f);
        let call = &f.calls_with("my_activities")[0];
        assert_eq!(
            call,
            &format!(
                "my_activities 15 {} {} -",
                win().start.timestamp_millis(),
                win().end.timestamp_millis()
            )
        );
    }

    #[test]
    fn assigned_issues_outside_window_are_filtered() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            Q_ASSIGNED,
            vec![
                yt_issue("SP-1", "in", IN, "Open"),
                yt_issue("SP-2", "late", OUT_AFTER, "Open"),
                yt_issue("SP-3", "early", OUT_BEFORE, "Open"),
            ],
        );
        let o = run(&f);
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].id, id("SP-1"));
    }

    #[test]
    fn assigned_bounds_are_inclusive() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            Q_ASSIGNED,
            vec![
                yt_issue("SP-1", "s", win().start.timestamp_millis(), "Open"),
                yt_issue("SP-2", "e", win().end.timestamp_millis(), "Open"),
            ],
        );
        assert_eq!(run(&f).issues.len(), 2);
    }

    #[test]
    fn assigned_error_fails_the_source() {
        let mut f = FakeYouTrack::new();
        f.searches
            .insert(Q_ASSIGNED.into(), Err(FakeErr::Status(500)));
        assert_eq!(
            collect(&f, &win(), &settings()).unwrap_err().status(),
            Some(500)
        );
    }

    #[test]
    fn truncation_warnings_use_the_labels() {
        let mut f = FakeYouTrack::new();
        f.truncate = true;
        let o = run(&f);
        assert!(o.warnings.contains(
            &"YouTrack assigned issues: stopped after 20 pages; results may be incomplete".into()
        ));
        assert!(o.warnings.contains(
            &"YouTrack activities: stopped after 25 pages; results may be incomplete".into()
        ));
        // approximate fallback label
        let mut f = FakeYouTrack::new();
        f.truncate = true;
        f.activities = vec![(15, Err(FakeErr::NotFound))];
        let o = run(&f);
        assert!(o.warnings.iter().any(|w| {
            w == "YouTrack changed issues: stopped after 20 pages; results may be incomplete"
        }));
    }

    // ---- project names ----

    #[test]
    fn admin_projects_are_uppercased() {
        let mut f = FakeYouTrack::new();
        f.projects = Ok(vec!["sp".into(), "MS".into()]);
        let o = run(&f);
        assert_eq!(
            o.admin_projects,
            Some(BTreeSet::from(["SP".to_string(), "MS".to_string()]))
        );
        assert!(o.warnings.is_empty());
        assert_eq!(f.calls()[0], "project_short_names");
    }

    #[test]
    fn project_list_failure_is_a_warning() {
        let mut f = FakeYouTrack::new();
        f.projects = Err(FakeErr::Forbidden);
        let o = run(&f);
        assert_eq!(o.admin_projects, None);
        assert_eq!(o.warnings.len(), 1);
        assert!(o.warnings[0].starts_with("YouTrack: cannot list projects ("));
        assert!(o.warnings[0].ends_with("; issue-ID matching uses the configured projects"));
    }

    #[test]
    fn project_list_unauthorized_fails() {
        let mut f = FakeYouTrack::new();
        f.projects = Err(FakeErr::Unauthorized);
        let e = collect(&f, &win(), &settings()).unwrap_err();
        assert_eq!(e.status(), Some(401));
    }

    // ---- activities ----

    #[test]
    fn activity_ids_and_roles() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(
            15,
            Ok(vec![
                activity_on_comment("CommentsCategory", "SP-1", IN),
                activity_on_comment("CommentTextCategory", "SP-2", IN),
                activity_on_comment("CommentAttachmentsCategory", "SP-3", IN),
                activity_on_issue("CustomFieldCategory", "SP-4", IN),
                activity_on_issue("SummaryCategory", "SP-5", IN),
                activity_on_comment("LinksCategory", "SP-6", IN),
            ]),
        )];
        for n in 1..=6 {
            f.add_by_id(
                &format!("SP-{n}"),
                yt_issue(&format!("SP-{n}"), "t", OUT_AFTER, "Open"),
            );
        }
        let o = run(&f);
        for n in 1..=3 {
            assert_eq!(
                issue_of(&o, &format!("SP-{n}")).roles,
                roles(&[Role::Commenter])
            );
        }
        for n in 4..=6 {
            assert_eq!(
                issue_of(&o, &format!("SP-{n}")).roles,
                roles(&[Role::Updater])
            );
        }
    }

    #[test]
    fn missing_category_is_updater() {
        let mut f = FakeYouTrack::new();
        let mut a = activity_on_issue("X", "SP-1", IN);
        a.category = None;
        f.activities = vec![(15, Ok(vec![a]))];
        f.add_by_id("SP-1", yt_issue("SP-1", "t", IN, "Open"));
        assert_eq!(issue_of(&run(&f), "SP-1").roles, roles(&[Role::Updater]));
    }

    #[test]
    fn same_issue_comment_and_change_gets_both_roles() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(
            15,
            Ok(vec![
                activity_on_comment("CommentsCategory", "SP-1", IN),
                activity_on_issue("CustomFieldCategory", "SP-1", IN),
            ]),
        )];
        f.add_by_id("SP-1", yt_issue("SP-1", "t", IN, "Open"));
        let o = run(&f);
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].roles, roles(&[Role::Commenter, Role::Updater]));
    }

    #[test]
    fn activities_outside_window_or_unparseable_are_ignored() {
        let mut f = FakeYouTrack::new();
        let mut no_target = activity_on_issue("CustomFieldCategory", "SP-9", IN);
        no_target.target = None;
        f.activities = vec![(
            15,
            Ok(vec![
                activity_on_issue("CustomFieldCategory", "SP-1", OUT_AFTER),
                activity_on_issue("CustomFieldCategory", "SP-2", OUT_BEFORE),
                activity_on_issue("CustomFieldCategory", "not an id", IN),
                no_target,
            ]),
        )];
        let o = run(&f);
        assert!(o.issues.is_empty());
        assert!(f.calls_with("issue_by_id").is_empty());
        assert!(
            f.search_queries()
                .iter()
                .all(|q| !q.starts_with("issue id"))
        );
    }

    #[test]
    fn activity_ids_not_assigned_are_fetched_in_window() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(
            15,
            Ok(vec![activity_on_comment("CommentsCategory", "SP-7", IN)]),
        )];
        f.add_search(
            "issue id: SP-7",
            vec![yt_issue("SP-7", "Fetched", OUT_AFTER, "Done")],
        );
        let o = run(&f);
        let i = issue_of(&o, "SP-7");
        assert!(i.in_window);
        assert_eq!(i.summary, "Fetched");
        assert_eq!(i.roles, roles(&[Role::Commenter]));
        assert!(f.search_queries().contains(&"issue id: SP-7".to_string()));
    }

    #[test]
    fn activity_ids_already_assigned_merge_roles_without_fetch() {
        let mut f = FakeYouTrack::new();
        f.add_search(Q_ASSIGNED, vec![yt_issue("SP-1", "a", IN, "Open")]);
        f.activities = vec![(
            15,
            Ok(vec![activity_on_comment("CommentsCategory", "SP-1", IN)]),
        )];
        let o = run(&f);
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].roles, roles(&[Role::Assignee, Role::Commenter]));
        assert_eq!(f.search_queries(), vec![Q_ASSIGNED.to_string()]);
        assert!(f.calls_with("issue_by_id").is_empty());
    }

    #[test]
    fn activity_ids_missing_remotely_are_dropped_silently() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(
            15,
            Ok(vec![activity_on_issue("CustomFieldCategory", "SP-7", IN)]),
        )];
        let o = run(&f);
        assert!(o.issues.is_empty());
        assert!(o.warnings.is_empty());
    }

    // ---- fallback chain ----

    #[test]
    fn full_400_then_core_ok() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![
            (15, Err(FakeErr::Status(400))),
            (
                5,
                Ok(vec![activity_on_issue("CustomFieldCategory", "SP-1", IN)]),
            ),
        ];
        f.add_by_id("SP-1", yt_issue("SP-1", "t", IN, "Open"));
        let o = run(&f);
        assert_eq!(o.warnings.len(), 1);
        assert_eq!(
            o.warnings[0],
            "YouTrack: server rejected some activity categories; retrying with the core set"
        );
        assert_eq!(f.calls_with("my_activities").len(), 2);
        assert!(f.calls_with("my_activities")[1].starts_with("my_activities 5 "));
        assert_eq!(issue_of(&o, "SP-1").roles, roles(&[Role::Updater]));
        assert!(!f.search_queries().contains(&Q_CHANGED.to_string()));
    }

    #[test]
    fn full_400_and_core_400_use_query_fallback() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![
            (15, Err(FakeErr::Status(400))),
            (5, Err(FakeErr::Status(400))),
        ];
        f.add_search(Q_CHANGED, vec![yt_issue("SP-5", "approx", IN, "Open")]);
        let o = run(&f);
        assert_eq!(o.warnings.len(), 2);
        assert_eq!(
            o.warnings[1],
            "YouTrack: activities API unavailable (HTTP 400); using approximate updater/commenter query"
        );
        assert!(f.search_queries().contains(&Q_CHANGED.to_string()));
        let i = issue_of(&o, "SP-5");
        assert_eq!(i.roles, roles(&[Role::Updater]));
        assert!(i.in_window);
        // complete issues skip fetch-by-id
        assert!(f.calls_with("issue_by_id").is_empty());
        assert!(
            f.search_queries()
                .iter()
                .all(|q| !q.starts_with("issue id"))
        );
    }

    #[test]
    fn query_fallback_filters_by_window_and_uses_projects() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::NotFound))];
        let q = format!("project: SP {Q_CHANGED}");
        f.add_search(
            &q,
            vec![
                yt_issue("SP-5", "in", IN, "Open"),
                yt_issue("SP-6", "out", OUT_AFTER, "Open"),
            ],
        );
        let s = YtSettings {
            projects: vec!["SP".into()],
            ..settings()
        };
        let o = collect(&f, &win(), &s).unwrap();
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].id, id("SP-5"));
    }

    #[test]
    fn query_fallback_merges_with_assigned() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::NotFound))];
        f.add_search(Q_ASSIGNED, vec![yt_issue("SP-1", "a", IN, "Open")]);
        f.add_search(Q_CHANGED, vec![yt_issue("SP-1", "a", IN, "Open")]);
        let o = run(&f);
        assert_eq!(o.issues.len(), 1);
        assert_eq!(o.issues[0].roles, roles(&[Role::Assignee, Role::Updater]));
    }

    #[test]
    fn full_404_goes_straight_to_query_fallback() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::NotFound))];
        let o = run(&f);
        assert_eq!(f.calls_with("my_activities").len(), 1);
        assert_eq!(o.warnings.len(), 1);
        assert!(o.warnings[0].contains("HTTP 404"));
        assert!(f.search_queries().contains(&Q_CHANGED.to_string()));
    }

    #[test]
    fn core_404_goes_to_query_fallback() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::Status(400))), (5, Err(FakeErr::NotFound))];
        let o = run(&f);
        assert_eq!(o.warnings.len(), 2);
        assert!(o.warnings[1].contains("HTTP 404"));
    }

    #[test]
    fn full_unauthorized_is_an_error() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::Unauthorized))];
        let e = collect(&f, &win(), &settings()).unwrap_err();
        assert_eq!(e.status(), Some(401));
        assert!(!f.search_queries().contains(&Q_CHANGED.to_string()));
    }

    #[test]
    fn other_activity_errors_are_returned() {
        for e in [FakeErr::Forbidden, FakeErr::Status(500)] {
            let mut f = FakeYouTrack::new();
            f.activities = vec![(15, Err(e))];
            assert!(collect(&f, &win(), &settings()).is_err());
        }
        let mut f = FakeYouTrack::new();
        f.activities = vec![
            (15, Err(FakeErr::Status(400))),
            (5, Err(FakeErr::Status(500))),
        ];
        assert_eq!(
            collect(&f, &win(), &settings()).unwrap_err().status(),
            Some(500)
        );
    }

    #[test]
    fn query_fallback_error_is_returned() {
        let mut f = FakeYouTrack::new();
        f.activities = vec![(15, Err(FakeErr::NotFound))];
        f.searches
            .insert(Q_CHANGED.into(), Err(FakeErr::Status(500)));
        assert_eq!(
            collect(&f, &win(), &settings()).unwrap_err().status(),
            Some(500)
        );
    }

    // ---- state extraction / conversion ----

    fn with_field(name: &str, value: Option<serde_json::Value>, resolved: Option<i64>) -> YtIssue {
        YtIssue {
            id_readable: "SP-1".into(),
            summary: Some("s".into()),
            updated: Some(IN),
            resolved,
            custom_fields: vec![crate::youtrack::YtCustomField {
                name: name.into(),
                value,
            }],
        }
    }

    fn state_of(yt: &YtIssue, field: &str) -> String {
        let s = YtSettings {
            state_field: field.into(),
            ..settings()
        };
        convert_issue(yt, &s, BTreeSet::new(), true).unwrap().state
    }

    #[test]
    fn state_object() {
        let yt = with_field(
            "State",
            Some(serde_json::json!({"name": "In Progress"})),
            None,
        );
        assert_eq!(state_of(&yt, "State"), "In Progress");
    }

    #[test]
    fn state_array() {
        let yt = with_field(
            "State",
            Some(serde_json::json!([{"name": "A"}, {"name": "B"}])),
            None,
        );
        assert_eq!(state_of(&yt, "State"), "A, B");
    }

    #[test]
    fn state_string() {
        let yt = with_field("State", Some(serde_json::json!("Blocked")), None);
        assert_eq!(state_of(&yt, "State"), "Blocked");
    }

    #[test]
    fn state_null_falls_back_to_resolved_or_open() {
        let yt = with_field("State", Some(serde_json::Value::Null), Some(IN));
        assert_eq!(state_of(&yt, "State"), "Resolved");
        let yt = with_field("State", Some(serde_json::Value::Null), None);
        assert_eq!(state_of(&yt, "State"), "Open");
        let yt = with_field("State", None, None);
        assert_eq!(state_of(&yt, "State"), "Open");
        let mut yt = with_field("Other", None, Some(IN));
        yt.custom_fields.clear();
        assert_eq!(state_of(&yt, "State"), "Resolved");
    }

    #[test]
    fn state_field_is_case_insensitive_and_configurable() {
        let yt = with_field("state", Some(serde_json::json!({"name": "Open-ish"})), None);
        assert_eq!(state_of(&yt, "State"), "Open-ish");
        let yt = with_field("Stage", Some(serde_json::json!({"name": "Review"})), None);
        assert_eq!(state_of(&yt, "Stage"), "Review");
        // the default field name does not match "Stage"
        assert_eq!(state_of(&yt, "State"), "Open");
    }

    #[test]
    fn convert_sets_url_time_and_flags() {
        let s = YtSettings {
            base_url: "https://yt.example.com/".into(),
            ..settings()
        };
        let mut yt = yt_issue("sp-9", "Title", IN, "Open");
        yt.id_readable = "SP-9".into();
        let i = convert_issue(&yt, &s, roles(&[Role::Assignee]), true).unwrap();
        assert_eq!(i.web_url, "https://yt.example.com/issue/SP-9");
        assert_eq!(i.updated, ts("2026-09-29T12:00:00Z"));
        assert!(!i.resolved);
        assert!(i.in_window);
        assert_eq!(i.summary, "Title");
    }

    #[test]
    fn convert_missing_updated_is_epoch_and_bad_id_is_none() {
        let mut yt = yt_issue("SP-9", "x", IN, "Open");
        yt.updated = None;
        yt.summary = None;
        let i = convert_issue(&yt, &settings(), BTreeSet::new(), false).unwrap();
        assert_eq!(i.updated, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(i.summary, "");
        let mut bad = yt_issue("SP-9", "x", IN, "Open");
        bad.id_readable = "garbage".into();
        assert!(convert_issue(&bad, &settings(), BTreeSet::new(), true).is_none());
    }

    #[test]
    fn issues_fixture_deserializes_and_converts() {
        let items: Vec<YtIssue> =
            serde_json::from_str(include_str!("../../tests/fixtures/youtrack_issues.json"))
                .unwrap();
        assert_eq!(items.len(), 2);
        let a = convert_issue(&items[0], &settings(), roles(&[Role::Assignee]), true).unwrap();
        assert_eq!(a.id, id("SP-101"));
        assert_eq!(a.summary, "Login page rejects valid passwords");
        assert_eq!(a.state, "In Progress");
        assert!(!a.resolved);
        assert_eq!(a.updated, ts("2026-09-29T12:00:00Z"));
        assert_eq!(a.web_url, "https://yt.example.com/issue/SP-101");
        let b = convert_issue(&items[1], &settings(), roles(&[Role::Assignee]), true).unwrap();
        assert_eq!(b.id, id("MS-45"));
        assert_eq!(b.state, "Done");
        assert!(b.resolved);
        assert_eq!(b.updated, ts("2026-09-29T14:30:00Z"));
        assert_eq!(b.web_url, "https://yt.example.com/issue/MS-45");
    }

    // ---- fetch_by_ids ----

    fn fetch(f: &FakeYouTrack, list: &[&str]) -> (FetchedById, Vec<String>) {
        let mut w = Vec::new();
        let r = fetch_by_ids(f, &ids(list), &mut w).unwrap();
        (r, w)
    }

    #[test]
    fn batch_query_is_exact() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            "issue id: SP-1 or issue id: SP-2",
            vec![
                yt_issue("SP-1", "a", IN, "Open"),
                yt_issue("SP-2", "b", IN, "Open"),
            ],
        );
        let (r, w) = fetch(&f, &["SP-1", "SP-2"]);
        assert_eq!(
            f.search_queries(),
            vec!["issue id: SP-1 or issue id: SP-2".to_string()]
        );
        assert_eq!(r.issues.len(), 2);
        assert!(r.missing.is_empty());
        assert!(w.is_empty());
        assert!(f.calls_with("issue_by_id").is_empty());
    }

    #[test]
    fn forty_five_ids_make_three_batches() {
        let f = FakeYouTrack::new();
        let list: Vec<String> = (1..=45).map(|n| format!("SP-{n}")).collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        // every id is "missing" => per-ID fetches; stays within the 50 cap
        let (r, w) = fetch(&f, &refs);
        assert_eq!(f.search_queries().len(), 3);
        assert_eq!(f.calls_with("issue_by_id").len(), 45);
        assert_eq!(r.missing.len(), 45);
        assert!(w.is_empty());
        let first = &f.search_queries()[0];
        assert_eq!(first.matches("issue id:").count(), 20);
        assert_eq!(f.search_queries()[2].matches("issue id:").count(), 5);
    }

    #[test]
    fn unrelated_batch_results_are_discarded() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            "issue id: SP-1",
            vec![
                yt_issue("SP-1", "a", IN, "Open"),
                yt_issue("ZZ-99", "noise", IN, "Open"),
                yt_issue("junk", "noise", IN, "Open"),
            ],
        );
        let (r, _) = fetch(&f, &["SP-1"]);
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].id_readable, "SP-1");
    }

    #[test]
    fn id_missing_from_successful_batch_is_fetched_singly() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            "issue id: SP-1 or issue id: SP-2",
            vec![yt_issue("SP-1", "a", IN, "Open")],
        );
        f.add_by_id("SP-2", yt_issue("SP-2", "b", IN, "Open"));
        let (r, _) = fetch(&f, &["SP-1", "SP-2"]);
        assert_eq!(f.calls_with("issue_by_id"), vec!["issue_by_id SP-2"]);
        assert_eq!(r.issues.len(), 2);
    }

    #[test]
    fn batch_400_fetches_every_id_of_the_chunk() {
        let mut f = FakeYouTrack::new();
        f.searches.insert(
            "issue id: SP-1 or issue id: SP-2".into(),
            Err(FakeErr::Status(400)),
        );
        f.add_by_id("SP-1", yt_issue("SP-1", "a", IN, "Open"));
        f.add_by_id("SP-2", yt_issue("SP-2", "b", IN, "Open"));
        let (r, w) = fetch(&f, &["SP-1", "SP-2"]);
        assert_eq!(f.calls_with("issue_by_id").len(), 2);
        assert_eq!(r.issues.len(), 2);
        assert!(w.is_empty());
    }

    #[test]
    fn per_id_404_is_missing_without_warning() {
        let f = FakeYouTrack::new();
        let (r, w) = fetch(&f, &["FIX-12"]);
        assert_eq!(r.missing, ids(&["FIX-12"]));
        assert!(r.issues.is_empty());
        assert!(w.is_empty());
    }

    #[test]
    fn per_id_other_error_warns_and_goes_missing() {
        let mut f = FakeYouTrack::new();
        f.by_id.insert("SP-1".into(), Err(FakeErr::Status(500)));
        let (r, w) = fetch(&f, &["SP-1"]);
        assert_eq!(r.missing, ids(&["SP-1"]));
        assert_eq!(w.len(), 1);
        assert!(
            w[0].starts_with("YouTrack: cannot fetch SP-1 ("),
            "{}",
            w[0]
        );
    }

    #[test]
    fn per_id_unauthorized_is_an_error() {
        let mut f = FakeYouTrack::new();
        f.by_id.insert("SP-1".into(), Err(FakeErr::Unauthorized));
        let mut w = Vec::new();
        let e = fetch_by_ids(&f, &ids(&["SP-1"]), &mut w).unwrap_err();
        assert_eq!(e.status(), Some(401));
    }

    #[test]
    fn alias_is_recorded_and_canonical_kept() {
        let mut f = FakeYouTrack::new();
        f.add_by_id("OLD-12", yt_issue("NEW-5", "moved", IN, "Open"));
        let (r, w) = fetch(&f, &["OLD-12"]);
        assert_eq!(r.aliases.get(&id("OLD-12")), Some(&id("NEW-5")));
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].id_readable, "NEW-5");
        assert!(r.missing.is_empty());
        assert!(w.is_empty());
    }

    #[test]
    fn issues_are_deduped_by_canonical_id() {
        let mut f = FakeYouTrack::new();
        f.add_by_id("OLD-12", yt_issue("NEW-5", "moved", IN, "Open"));
        f.add_by_id("OLD-13", yt_issue("NEW-5", "moved", IN, "Open"));
        let (r, _) = fetch(&f, &["OLD-12", "OLD-13", "OLD-12"]);
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.aliases.len(), 2);
        assert_eq!(f.calls_with("issue_by_id").len(), 2);
    }

    #[test]
    fn same_case_insensitive_id_is_not_an_alias() {
        let mut f = FakeYouTrack::new();
        f.add_by_id("SP-1", yt_issue("sp-1", "a", IN, "Open"));
        let (r, _) = fetch(&f, &["SP-1"]);
        assert!(r.aliases.is_empty());
        assert_eq!(r.issues.len(), 1);
    }

    #[test]
    fn more_than_fifty_per_id_fetches_warn() {
        let f = FakeYouTrack::new();
        let list: Vec<String> = (1..=60).map(|n| format!("SP-{n}")).collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        let (r, w) = fetch(&f, &refs);
        assert_eq!(f.calls_with("issue_by_id").len(), 50);
        assert_eq!(r.missing.len(), 60);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("too many"));
        assert_eq!(
            w[0],
            "YouTrack: too many issues to fetch individually; skipped 10"
        );
    }

    #[test]
    fn batch_unauthorized_is_an_error() {
        let mut f = FakeYouTrack::new();
        f.searches
            .insert("issue id: SP-1".into(), Err(FakeErr::Unauthorized));
        let mut w = Vec::new();
        let e = fetch_by_ids(&f, &ids(&["SP-1"]), &mut w).unwrap_err();
        assert_eq!(e.status(), Some(401));
        assert!(f.calls_with("issue_by_id").is_empty());
    }

    #[test]
    fn batch_other_error_is_silent() {
        let mut f = FakeYouTrack::new();
        f.searches
            .insert("issue id: SP-1".into(), Err(FakeErr::Status(500)));
        f.add_by_id("SP-1", yt_issue("SP-1", "a", IN, "Open"));
        let (r, w) = fetch(&f, &["SP-1"]);
        assert_eq!(r.issues.len(), 1);
        assert!(w.is_empty());
    }

    #[test]
    fn empty_ids_make_no_calls() {
        let f = FakeYouTrack::new();
        let (r, w) = fetch(&f, &[]);
        assert_eq!(r, FetchedById::default());
        assert!(w.is_empty());
        assert!(f.calls().is_empty());
    }

    // ---- fetch_referenced ----

    #[test]
    fn referenced_issues_have_referenced_role_and_are_not_in_window() {
        let mut f = FakeYouTrack::new();
        f.add_search(
            "issue id: SP-1 or issue id: OLD-2",
            vec![yt_issue("SP-1", "a", OUT_AFTER, "Open")],
        );
        f.add_by_id("OLD-2", yt_issue("NEW-2", "moved", IN, "Done"));
        let mut w = Vec::new();
        let (issues, aliases) =
            fetch_referenced(&f, &ids(&["SP-1", "OLD-2"]), &settings(), &mut w).unwrap();
        assert_eq!(issues.len(), 2);
        for i in &issues {
            assert_eq!(i.roles, roles(&[Role::Referenced]));
            assert!(!i.in_window);
        }
        assert_eq!(aliases.get(&id("OLD-2")), Some(&id("NEW-2")));
        assert!(w.is_empty());
    }

    #[test]
    fn referenced_caps_at_fifty_with_warning() {
        let f = FakeYouTrack::new();
        let list: Vec<String> = (1..=55).map(|n| format!("SP-{n}")).collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        let mut w = Vec::new();
        fetch_referenced(&f, &ids(&refs), &settings(), &mut w).unwrap();
        assert_eq!(
            w[0],
            "YouTrack: 55 issues referenced by MRs; fetching the first 50"
        );
        let asked: usize = f
            .search_queries()
            .iter()
            .map(|q| q.matches("issue id:").count())
            .sum();
        assert_eq!(asked, 50);
        assert!(f.search_queries().iter().all(|q| !q.contains("SP-51")));
    }

    #[test]
    fn referenced_exactly_fifty_does_not_warn() {
        let f = FakeYouTrack::new();
        let list: Vec<String> = (1..=50).map(|n| format!("SP-{n}")).collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        let mut w = Vec::new();
        fetch_referenced(&f, &ids(&refs), &settings(), &mut w).unwrap();
        assert!(w.is_empty());
    }

    #[test]
    fn referenced_unauthorized_is_an_error() {
        let mut f = FakeYouTrack::new();
        f.searches
            .insert("issue id: SP-1".into(), Err(FakeErr::Unauthorized));
        let mut w = Vec::new();
        assert!(fetch_referenced(&f, &ids(&["SP-1"]), &settings(), &mut w).is_err());
    }
}
