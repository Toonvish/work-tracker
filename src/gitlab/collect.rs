//! Pure collection logic over `GitLabApi`: identity, three concurrent queries, inclusion rules, extras.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::ApiError;
use crate::gitlab::{
    EVENT_PAGE_CAP, GitLabApi, GlEvent, GlMergeRequest, GlUser, MR_PAGE_CAP, MrQuery,
};
use crate::http::GITLAB;
use crate::model::{MergeRequest, MrState, Role};
use crate::window::{Window, padded_dates};

/// Most MRs fetched for events whose MR is not in the A/B listings.
const EXTRAS_CAP: usize = 200;
/// `iids[]` per `project_merge_requests` request.
const EXTRAS_CHUNK: usize = 20;

#[derive(Debug, Clone)]
pub struct GitLabOutcome {
    pub user: GlUser,
    pub mrs: Vec<MergeRequest>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventHit {
    Mr { key: (u64, u64), role: Role },
    Push { project_id: u64, branch: String },
}

/// Classifies one event (5.5). Issue notes and unknown events give `None`.
pub fn classify(ev: &GlEvent) -> Option<EventHit> {
    let project_id = ev.project_id?;
    match ev.target_type.as_deref() {
        Some("MergeRequest") => {
            let iid = ev.target_iid?;
            let role = match ev.action_name.as_str() {
                "approved" => Role::Approver,
                "opened" | "created" => Role::Author,
                "accepted" | "merged" => Role::Merger,
                _ => Role::Participant,
            };
            return Some(EventHit::Mr {
                key: (project_id, iid),
                role,
            });
        }
        Some("Note" | "DiffNote" | "DiscussionNote") => {
            let note = ev.note.as_ref()?;
            if note.noteable_type.as_deref() != Some("MergeRequest") {
                return None;
            }
            return Some(EventHit::Mr {
                key: (project_id, note.noteable_iid?),
                role: Role::Commenter,
            });
        }
        _ => {}
    }
    let push = ev.push_data.as_ref()?;
    if push.ref_type.as_deref() == Some("branch") && ev.action_name.starts_with("pushed") {
        return Some(EventHit::Push {
            project_id,
            branch: push.git_ref.clone()?,
        });
    }
    None
}

/// `references.full` up to the last `!`; fallback: the `web_url` path before `/-/merge_requests`.
pub fn project_path(gl: &GlMergeRequest) -> String {
    if let Some(full) = gl.references.as_ref().map(|r| r.full.as_str())
        && let Some((path, _)) = full.rsplit_once('!')
        && !path.is_empty()
    {
        return path.to_string();
    }
    let Ok(url) = reqwest::Url::parse(&gl.web_url) else {
        return String::new();
    };
    let path = url.path().trim_start_matches('/');
    let end = path
        .find("/-/merge_requests")
        .or_else(|| path.find("/merge_requests"))
        .unwrap_or(path.len());
    path[..end].to_string()
}

/// Converts the API DTO to the domain type (`issue_ids` stays empty).
pub fn to_model(gl: &GlMergeRequest, roles: BTreeSet<Role>) -> MergeRequest {
    MergeRequest {
        project_id: gl.project_id,
        iid: gl.iid,
        project_path: project_path(gl),
        title: gl.title.clone(),
        description: gl.description.clone().unwrap_or_default(),
        source_branch: gl.source_branch.clone(),
        state: MrState::from_gitlab(&gl.state, gl.draft),
        web_url: gl.web_url.clone(),
        author: gl.author.username.clone(),
        created_at: gl.created_at,
        updated_at: gl.updated_at,
        roles,
        issue_ids: Vec::new(),
    }
}

fn resolve_user(api: &dyn GitLabApi, username: Option<&str>) -> Result<GlUser, ApiError> {
    match username {
        None => api.current_user(),
        Some(u) => api
            .users_by_username(u)?
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::Other {
                service: GITLAB,
                message: format!("user \"{u}\" not found"),
            }),
    }
}

fn warn_truncated(warnings: &mut Vec<String>, what: &str, cap: u32, truncated: bool) {
    if truncated {
        warnings.push(format!(
            "{what}: stopped after {cap} pages; results may be incomplete"
        ));
    }
}

fn add(map: &mut BTreeMap<(u64, u64), MergeRequest>, mr: MergeRequest) {
    match map.get_mut(&mr.key()) {
        Some(existing) => existing.roles.extend(mr.roles),
        None => {
            map.insert(mr.key(), mr);
        }
    }
}

/// Inclusion rules 5.4. An empty result means the MR is not part of the report.
fn roles_for(
    gl: &GlMergeRequest,
    uid: u64,
    window: &Window,
    ev: Option<&BTreeSet<Role>>,
    pushes: &BTreeSet<(u64, String)>,
) -> BTreeSet<Role> {
    let has_ev = ev.is_some_and(|s| !s.is_empty());
    let mut roles = BTreeSet::new();
    if gl.author.id == uid
        && (window.contains(gl.created_at)
            || window.contains(gl.updated_at)
            || has_ev
            || pushes.contains(&(gl.project_id, gl.source_branch.clone())))
    {
        roles.insert(Role::Author);
    }
    if gl.reviewers.iter().any(|r| r.id == uid) && (window.contains(gl.updated_at) || has_ev) {
        roles.insert(Role::Reviewer);
    }
    if let Some(ev) = ev {
        roles.extend(ev.iter().copied());
    }
    roles
}

pub fn collect(
    api: &dyn GitLabApi,
    username: Option<&str>,
    window: &Window,
) -> Result<GitLabOutcome, ApiError> {
    let mut warnings = Vec::new();
    let user = resolve_user(api, username)?;
    let uid = user.id;

    // The three listings only depend on the user id, so they run concurrently.
    // Errors keep the deterministic precedence authored → reviewer → events.
    let (after, before) = padded_dates(window);
    let (authored, reviewing, events) = std::thread::scope(|s| {
        let a = s.spawn(|| {
            api.merge_requests(&MrQuery {
                author_id: Some(uid),
                reviewer_id: None,
                updated_after: window.start,
            })
        });
        let b = s.spawn(|| {
            api.merge_requests(&MrQuery {
                author_id: None,
                reviewer_id: Some(uid),
                updated_after: window.start,
            })
        });
        let c = s.spawn(|| api.user_events(uid, after, before));
        (
            a.join().expect("authored MR thread panicked"),
            b.join().expect("reviewer MR thread panicked"),
            c.join().expect("events thread panicked"),
        )
    });
    let (authored, reviewing, events) = (authored?, reviewing?, events?);
    warn_truncated(
        &mut warnings,
        "GitLab authored MRs",
        MR_PAGE_CAP,
        authored.truncated,
    );
    warn_truncated(
        &mut warnings,
        "GitLab reviewer MRs",
        MR_PAGE_CAP,
        reviewing.truncated,
    );
    warn_truncated(
        &mut warnings,
        "GitLab events",
        EVENT_PAGE_CAP,
        events.truncated,
    );

    // Attribute in-window events to MRs (and pushes to branches).
    let mut ev_roles: BTreeMap<(u64, u64), BTreeSet<Role>> = BTreeMap::new();
    let mut pushes: BTreeSet<(u64, String)> = BTreeSet::new();
    for ev in events
        .items
        .iter()
        .filter(|e| window.contains(e.created_at))
    {
        match classify(ev) {
            Some(EventHit::Mr { key, role }) => {
                ev_roles.entry(key).or_default().insert(role);
            }
            Some(EventHit::Push { project_id, branch }) => {
                pushes.insert((project_id, branch));
            }
            None => {}
        }
    }

    let mut found: BTreeMap<(u64, u64), MergeRequest> = BTreeMap::new();
    let mut fetched: BTreeSet<(u64, u64)> = BTreeSet::new();
    for gl in authored.items.iter().chain(reviewing.items.iter()) {
        let key = (gl.project_id, gl.iid);
        fetched.insert(key);
        let roles = roles_for(gl, uid, window, ev_roles.get(&key), &pushes);
        if !roles.is_empty() {
            add(&mut found, to_model(gl, roles));
        }
    }

    fetch_extras(api, &ev_roles, &fetched, &mut found, &mut warnings);

    Ok(GitLabOutcome {
        user,
        mrs: found.into_values().collect(),
        warnings,
    })
}

/// 5.6: MRs known only from events are fetched per project. Failures only warn.
fn fetch_extras(
    api: &dyn GitLabApi,
    ev_roles: &BTreeMap<(u64, u64), BTreeSet<Role>>,
    fetched: &BTreeSet<(u64, u64)>,
    found: &mut BTreeMap<(u64, u64), MergeRequest>,
    warnings: &mut Vec<String>,
) {
    let missing: Vec<(u64, u64)> = ev_roles
        .keys()
        .filter(|k| !fetched.contains(*k))
        .copied()
        .collect();
    if missing.len() > EXTRAS_CAP {
        warnings.push(format!(
            "GitLab: {} MRs are known only from events; fetching only the first {EXTRAS_CAP}",
            missing.len()
        ));
    }
    let mut by_project: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (pid, iid) in missing.into_iter().take(EXTRAS_CAP) {
        by_project.entry(pid).or_default().push(iid);
    }
    for (pid, iids) in by_project {
        for (n, chunk) in iids.chunks(EXTRAS_CHUNK).enumerate() {
            match api.project_merge_requests(pid, chunk) {
                Ok(list) => {
                    for gl in list.iter().filter(|m| chunk.contains(&m.iid)) {
                        if let Some(roles) = ev_roles.get(&(pid, gl.iid)) {
                            add(found, to_model(gl, roles.clone()));
                        }
                    }
                }
                Err(e) => {
                    let skipped = iids.len() - n * EXTRAS_CHUNK;
                    let reason = match e.status() {
                        Some(s) => s.to_string(),
                        None => e.to_string(),
                    };
                    warnings.push(format!(
                        "GitLab: cannot read project {pid} ({reason}); skipping {skipped} MRs"
                    ));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitlab::GlUserRef;
    use crate::gitlab::fakes::{FakeGitLab, ev_mr, ev_note, ev_push, mr, ts};
    use crate::http::FakeErr;
    use crate::window::WindowOrigin;

    fn win() -> Window {
        Window {
            start: ts("2026-09-29T08:00:00Z"),
            end: ts("2026-09-30T09:00:00Z"),
            origin: WindowOrigin::Explicit,
        }
    }

    const IN: &str = "2026-09-29T12:00:00Z";
    const OUT_AFTER: &str = "2026-10-01T12:00:00Z";
    const OUT_BEFORE: &str = "2026-09-28T12:00:00Z";

    fn run(fake: &FakeGitLab) -> GitLabOutcome {
        collect(fake, None, &win()).unwrap()
    }

    fn roles(o: &GitLabOutcome, iid: u64) -> Vec<Role> {
        o.mrs
            .iter()
            .find(|m| m.iid == iid)
            .unwrap_or_else(|| panic!("MR {iid} missing"))
            .roles
            .iter()
            .copied()
            .collect()
    }

    // ---- fixtures / DTOs ----

    #[test]
    fn user_fixture_deserializes() {
        let u: GlUser =
            serde_json::from_str(include_str!("../../tests/fixtures/gitlab_user.json")).unwrap();
        assert_eq!(
            u,
            GlUser {
                id: 7,
                username: "eric".into()
            }
        );
    }

    #[test]
    fn mr_fixture_deserializes_and_converts() {
        let v: Vec<GlMergeRequest> = serde_json::from_str(include_str!(
            "../../tests/fixtures/gitlab_merge_requests.json"
        ))
        .unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].reviewers.len(), 1);
        assert_eq!(v[0].reviewers[0].username, "anna");
        assert!(v[1].reviewers.is_empty());
        assert!(v[1].description.is_none());
        let m0 = to_model(&v[0], BTreeSet::from([Role::Author]));
        assert_eq!(m0.project_path, "acme/platform/backend");
        assert_eq!(m0.state, MrState::Opened);
        assert_eq!(m0.iid, 17);
        assert_eq!(m0.source_branch, "feature/SP-123-login");
        assert_eq!(m0.author, "eric");
        assert!(m0.description.contains("SP-123"));
        assert!(m0.issue_ids.is_empty());
        let m1 = to_model(&v[1], BTreeSet::new());
        assert_eq!(m1.state, MrState::Draft);
        assert_eq!(m1.project_path, "acme/platform/backend");
        assert_eq!(m1.description, "");
        assert_eq!(
            m1.web_url,
            "https://gitlab.example.com/acme/platform/backend/-/merge_requests/18"
        );
    }

    #[test]
    fn project_path_falls_back_to_web_url() {
        let v: Vec<GlMergeRequest> = serde_json::from_str(include_str!(
            "../../tests/fixtures/gitlab_merge_requests.json"
        ))
        .unwrap();
        let mut gl = v[0].clone();
        gl.references = None;
        assert_eq!(project_path(&gl), "acme/platform/backend");
        gl.web_url = "https://gitlab.example.com/a/b/c/merge_requests/3".into();
        assert_eq!(project_path(&gl), "a/b/c");
        gl.web_url = "not a url".into();
        assert_eq!(project_path(&gl), "");
    }

    #[test]
    fn events_fixture_deserializes_and_classifies() {
        let v: Vec<GlEvent> =
            serde_json::from_str(include_str!("../../tests/fixtures/gitlab_events.json")).unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!(
            classify(&v[0]),
            Some(EventHit::Mr {
                key: (42, 21),
                role: Role::Approver
            })
        );
        assert_eq!(
            classify(&v[1]),
            Some(EventHit::Mr {
                key: (42, 22),
                role: Role::Commenter
            })
        );
        assert_eq!(
            classify(&v[2]),
            Some(EventHit::Push {
                project_id: 42,
                branch: "feature/SP-123-login".into()
            })
        );
        assert_eq!(classify(&v[3]), None);
    }

    // ---- classify ----

    #[test]
    fn classify_mr_actions() {
        let t = ts(IN);
        let role = |a: &str| match classify(&ev_mr(1, 5, a, t)) {
            Some(EventHit::Mr { key, role }) => {
                assert_eq!(key, (1, 5));
                role
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(role("approved"), Role::Approver);
        assert_eq!(role("opened"), Role::Author);
        assert_eq!(role("created"), Role::Author);
        assert_eq!(role("accepted"), Role::Merger);
        assert_eq!(role("merged"), Role::Merger);
        assert_eq!(role("closed"), Role::Participant);
        assert_eq!(role("reopened"), Role::Participant);
        assert_eq!(role("updated"), Role::Participant);
    }

    #[test]
    fn classify_notes_use_noteable_iid() {
        for tt in ["Note", "DiffNote", "DiscussionNote"] {
            let mut ev = ev_note(3, tt, "MergeRequest", 12, ts(IN));
            ev.target_iid = Some(999); // must be ignored
            assert_eq!(
                classify(&ev),
                Some(EventHit::Mr {
                    key: (3, 12),
                    role: Role::Commenter
                }),
                "{tt}"
            );
        }
        assert_eq!(classify(&ev_note(3, "Note", "Issue", 12, ts(IN))), None);
        assert_eq!(classify(&ev_note(3, "Note", "Commit", 12, ts(IN))), None);
        let mut ev = ev_note(3, "Note", "MergeRequest", 12, ts(IN));
        ev.note.as_mut().unwrap().noteable_iid = None;
        assert_eq!(classify(&ev), None);
    }

    #[test]
    fn classify_push() {
        assert_eq!(
            classify(&ev_push(3, "feat/x", ts(IN))),
            Some(EventHit::Push {
                project_id: 3,
                branch: "feat/x".into()
            })
        );
        let mut ev = ev_push(3, "v1", ts(IN));
        ev.push_data.as_mut().unwrap().ref_type = Some("tag".into());
        assert_eq!(classify(&ev), None);
        let mut ev = ev_push(3, "gone", ts(IN));
        ev.action_name = "deleted".into();
        assert_eq!(classify(&ev), None);
    }

    // ---- inclusion rules ----

    #[test]
    fn authored_created_or_updated_in_window_is_author() {
        let mut f = FakeGitLab::new();
        let mut created = mr(1, 1, "a", "b1", "", ts(OUT_AFTER));
        created.created_at = ts(IN);
        let updated = mr(1, 2, "b", "b2", "", ts(IN));
        f.authored = Ok(vec![created, updated]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 2);
        assert_eq!(roles(&o, 1), vec![Role::Author]);
        assert_eq!(roles(&o, 2), vec![Role::Author]);
        assert!(o.warnings.is_empty());
        assert_eq!(o.user.username, "eric");
    }

    #[test]
    fn authored_updated_after_window_without_events_is_excluded() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 1, "a", "b1", "", ts(OUT_AFTER));
        m.created_at = ts(OUT_BEFORE);
        f.authored = Ok(vec![m]);
        assert!(run(&f).mrs.is_empty());
    }

    #[test]
    fn authored_with_push_to_source_branch_is_included() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 1, "a", "feat/x", "", ts(OUT_AFTER));
        m.created_at = ts(OUT_BEFORE);
        let mut other = mr(1, 2, "b", "feat/y", "", ts(OUT_AFTER));
        other.created_at = ts(OUT_BEFORE);
        f.authored = Ok(vec![m, other]);
        f.events = Ok(vec![ev_push(1, "feat/x", ts(IN))]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 1);
        assert_eq!(o.mrs[0].iid, 1);
        assert_eq!(roles(&o, 1), vec![Role::Author]);
    }

    #[test]
    fn push_in_other_project_does_not_match() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 1, "a", "feat/x", "", ts(OUT_AFTER));
        m.created_at = ts(OUT_BEFORE);
        f.authored = Ok(vec![m]);
        f.events = Ok(vec![ev_push(2, "feat/x", ts(IN))]);
        assert!(run(&f).mrs.is_empty());
    }

    #[test]
    fn reviewer_updated_in_window_is_reviewer() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 5, "r", "b", "", ts(IN));
        m.author = GlUserRef {
            id: 99,
            username: "other".into(),
        };
        m.reviewers = vec![GlUserRef {
            id: 7,
            username: "eric".into(),
        }];
        let mut stale = m.clone();
        stale.iid = 6;
        stale.updated_at = ts(OUT_AFTER);
        f.reviewer = Ok(vec![m, stale]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 1);
        assert_eq!(roles(&o, 5), vec![Role::Reviewer]);
        assert_eq!(o.mrs[0].author, "other");
    }

    #[test]
    fn events_make_stale_reviewer_mr_included_with_roles() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 5, "r", "b", "", ts(OUT_AFTER));
        m.author = GlUserRef {
            id: 99,
            username: "other".into(),
        };
        m.reviewers = vec![GlUserRef {
            id: 7,
            username: "eric".into(),
        }];
        f.reviewer = Ok(vec![m]);
        f.events = Ok(vec![ev_mr(1, 5, "approved", ts(IN))]);
        let o = run(&f);
        assert_eq!(roles(&o, 5), vec![Role::Reviewer, Role::Approver]);
        assert!(f.calls().iter().all(|c| !c.starts_with("project_mrs")));
    }

    #[test]
    fn event_only_mr_is_fetched_with_event_roles() {
        let mut f = FakeGitLab::new();
        let mut m = mr(4, 8, "foreign", "b", "", ts(OUT_AFTER));
        m.author = GlUserRef {
            id: 99,
            username: "other".into(),
        };
        f.project_mrs
            .insert(4, Ok(vec![m, mr(4, 9, "x", "b", "", ts(IN))]));
        f.events = Ok(vec![
            ev_mr(4, 8, "approved", ts(IN)),
            ev_note(4, "DiffNote", "MergeRequest", 8, ts(IN)),
        ]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 1);
        assert_eq!(roles(&o, 8), vec![Role::Approver, Role::Commenter]);
        assert_eq!(o.mrs[0].project_path, "grp/proj4");
        assert!(f.calls().contains(&"project_mrs 4 8".to_string()));
    }

    #[test]
    fn events_outside_window_are_ignored() {
        let mut f = FakeGitLab::new();
        f.project_mrs
            .insert(4, Ok(vec![mr(4, 8, "x", "b", "", ts(IN))]));
        f.events = Ok(vec![
            ev_mr(4, 8, "approved", ts(OUT_BEFORE)),
            ev_mr(4, 8, "approved", ts(OUT_AFTER)),
            ev_push(4, "b", ts(OUT_AFTER)),
        ]);
        let o = run(&f);
        assert!(o.mrs.is_empty());
        assert!(f.calls().iter().all(|c| !c.starts_with("project_mrs")));
    }

    #[test]
    fn window_bounds_are_inclusive_for_events() {
        let mut f = FakeGitLab::new();
        f.project_mrs
            .insert(4, Ok(vec![mr(4, 8, "x", "b", "", ts(IN))]));
        f.events = Ok(vec![ev_mr(4, 8, "approved", win().end)]);
        assert_eq!(run(&f).mrs.len(), 1);
    }

    #[test]
    fn dedup_merges_roles_across_a_b_and_events() {
        let mut f = FakeGitLab::new();
        let mut m = mr(1, 1, "both", "b", "", ts(IN));
        m.reviewers = vec![GlUserRef {
            id: 7,
            username: "eric".into(),
        }];
        f.authored = Ok(vec![m.clone()]);
        f.reviewer = Ok(vec![m]);
        f.events = Ok(vec![ev_note(1, "Note", "MergeRequest", 1, ts(IN))]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 1);
        assert_eq!(
            roles(&o, 1),
            vec![Role::Author, Role::Reviewer, Role::Commenter]
        );
    }

    #[test]
    fn same_iid_in_different_projects_is_not_merged() {
        let mut f = FakeGitLab::new();
        f.authored = Ok(vec![
            mr(1, 1, "a", "b", "", ts(IN)),
            mr(2, 1, "b", "b", "", ts(IN)),
        ]);
        assert_eq!(run(&f).mrs.len(), 2);
    }

    // ---- identity / errors ----

    #[test]
    fn configured_username_uses_lookup() {
        let mut f = FakeGitLab::new();
        f.users_by_name = vec![GlUser {
            id: 55,
            username: "bob".into(),
        }];
        let o = collect(&f, Some("bob"), &win()).unwrap();
        assert_eq!(o.user.id, 55);
        let calls = f.calls();
        assert_eq!(calls[0], "users_by_username bob");
        assert!(!calls.contains(&"current_user".to_string()));
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("merge_requests author 55"))
        );
        assert!(calls.iter().any(|c| c.starts_with("user_events 55")));
    }

    #[test]
    fn unknown_username_is_an_error() {
        let f = FakeGitLab::new();
        let e = collect(&f, Some("nobody"), &win()).unwrap_err();
        assert!(matches!(e, ApiError::Other { .. }));
        assert!(e.to_string().contains("not found"));
        assert!(e.to_string().contains("\"nobody\""));
    }

    #[test]
    fn unauthorized_current_user_fails() {
        let mut f = FakeGitLab::new();
        f.user = Err(FakeErr::Unauthorized);
        let e = collect(&f, None, &win()).unwrap_err();
        assert!(matches!(e, ApiError::Unauthorized { .. }));
    }

    #[test]
    fn errors_in_a_b_c_fail_the_source() {
        let mut f = FakeGitLab::new();
        f.authored = Err(FakeErr::Status(500));
        assert_eq!(collect(&f, None, &win()).unwrap_err().status(), Some(500));
        let mut f = FakeGitLab::new();
        f.reviewer = Err(FakeErr::Forbidden);
        assert_eq!(collect(&f, None, &win()).unwrap_err().status(), Some(403));
        let mut f = FakeGitLab::new();
        f.events = Err(FakeErr::NotFound);
        assert_eq!(collect(&f, None, &win()).unwrap_err().status(), Some(404));
    }

    #[test]
    fn forbidden_extra_project_is_a_warning() {
        let mut f = FakeGitLab::new();
        f.project_mrs.insert(4, Err(FakeErr::Forbidden));
        f.events = Ok(vec![
            ev_mr(4, 8, "approved", ts(IN)),
            ev_mr(4, 9, "approved", ts(IN)),
            ev_mr(4, 10, "approved", ts(IN)),
        ]);
        let o = run(&f);
        assert!(o.mrs.is_empty());
        assert_eq!(
            o.warnings,
            vec!["GitLab: cannot read project 4 (403); skipping 3 MRs".to_string()]
        );
    }

    #[test]
    fn extras_error_does_not_hide_other_projects() {
        let mut f = FakeGitLab::new();
        f.project_mrs.insert(4, Err(FakeErr::NotFound));
        f.project_mrs
            .insert(5, Ok(vec![mr(5, 1, "ok", "b", "", ts(IN))]));
        f.events = Ok(vec![
            ev_mr(4, 8, "approved", ts(IN)),
            ev_mr(5, 1, "approved", ts(IN)),
        ]);
        let o = run(&f);
        assert_eq!(o.mrs.len(), 1);
        assert_eq!(o.mrs[0].project_id, 5);
        assert_eq!(o.warnings.len(), 1);
        assert!(o.warnings[0].contains("project 4 (404); skipping 1 MRs"));
    }

    #[test]
    fn extras_are_chunked_by_20_and_capped_at_200() {
        let mut f = FakeGitLab::new();
        f.events = Ok((1..=45).map(|i| ev_mr(4, i, "approved", ts(IN))).collect());
        run(&f);
        let calls: Vec<String> = f
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("project_mrs"))
            .collect();
        assert_eq!(calls.len(), 3);

        let f = {
            let mut f = FakeGitLab::new();
            f.events = Ok((1..=230).map(|i| ev_mr(4, i, "approved", ts(IN))).collect());
            f
        };
        let o = run(&f);
        let fetched: usize = f
            .calls()
            .iter()
            .filter(|c| c.starts_with("project_mrs"))
            .map(|c| c.split(' ').nth(2).unwrap().split(',').count())
            .sum();
        assert_eq!(fetched, 200);
        assert!(o.warnings.iter().any(|w| w.contains("first 200")));
    }

    // ---- padding / truncation ----

    #[test]
    fn events_query_uses_padded_dates() {
        let f = FakeGitLab::new();
        run(&f);
        assert!(
            f.calls()
                .contains(&"user_events 7 2026-09-27 2026-10-02".to_string()),
            "{:?}",
            f.calls()
        );
        assert!(
            f.calls()
                .contains(&"merge_requests author 7 2026-09-29T08:00:00Z".to_string())
        );
        assert!(
            f.calls()
                .contains(&"merge_requests reviewer 7 2026-09-29T08:00:00Z".to_string())
        );
    }

    #[test]
    fn truncated_listings_warn() {
        let mut f = FakeGitLab::new();
        f.truncate = true;
        let o = run(&f);
        assert_eq!(o.warnings.len(), 3);
        for what in [
            "GitLab authored MRs",
            "GitLab reviewer MRs",
            "GitLab events",
        ] {
            assert!(
                o.warnings.iter().any(|w| w.starts_with(what)
                    && w.contains("stopped after")
                    && w.ends_with("results may be incomplete")),
                "{what}: {:?}",
                o.warnings
            );
        }
        assert!(o.warnings[0].contains("stopped after 20 pages"));
        assert!(o.warnings[2].contains("stopped after 30 pages"));
    }
}
