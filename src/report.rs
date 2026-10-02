//! Known-project set, alias rewriting and grouping of MRs under issues (DESIGN 7.3-7.5).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::model::{Issue, IssueId, MergeRequest};

/// Combines the admin project list and the configured projects (DESIGN 7.3 table).
/// When there is no admin list but YouTrack succeeded, the projects of the
/// already fetched issues join the configured ones, so the denylist fallback
/// is only used when nothing at all is known.
pub fn known_projects(
    admin: Option<&BTreeSet<String>>,
    configured: &[String],
    issues: &[Issue],
    youtrack_ok: bool,
) -> Option<BTreeSet<String>> {
    if admin.is_none() && youtrack_ok {
        let set: BTreeSet<String> = configured
            .iter()
            .cloned()
            .chain(issues.iter().map(|i| i.id.project.clone()))
            .collect();
        return if set.is_empty() { None } else { Some(set) };
    }
    match (admin, configured.is_empty()) {
        (Some(a), true) => Some(a.clone()),
        (Some(a), false) => Some(
            configured
                .iter()
                .filter(|c| a.contains(*c))
                .cloned()
                .collect(),
        ),
        (None, false) => Some(configured.iter().cloned().collect()),
        (None, true) => None,
    }
}

/// Replaces alias keys with their canonical ID, then dedupes keeping the first occurrence.
pub fn apply_aliases(mrs: &mut [MergeRequest], aliases: &BTreeMap<IssueId, IssueId>) {
    for mr in mrs.iter_mut() {
        let mut seen: HashSet<IssueId> = HashSet::new();
        let mut out: Vec<IssueId> = Vec::with_capacity(mr.issue_ids.len());
        for id in mr.issue_ids.drain(..) {
            let id = aliases.get(&id).cloned().unwrap_or(id);
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        mr.issue_ids = out;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// `mrs` may be empty.
    Issue {
        issue: Issue,
        mrs: Vec<MergeRequest>,
    },
    /// No referenced issue is present in the issue list.
    OrphanMr(MergeRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Counts {
    pub issues: usize,
    /// Distinct MRs.
    pub mrs: usize,
    pub linked_groups: usize,
    pub orphan_mrs: usize,
    pub issues_without_mr: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub entries: Vec<Entry>,
    pub counts: Counts,
}

impl Entry {
    fn sort_key(&self) -> DateTime<Utc> {
        match self {
            Entry::Issue { issue, mrs } => mrs
                .iter()
                .map(|m| m.updated_at)
                .fold(issue.updated, |a, b| a.max(b)),
            Entry::OrphanMr(mr) => mr.updated_at,
        }
    }

    fn display_ref(&self) -> String {
        match self {
            Entry::Issue { issue, .. } => issue.id.to_string(),
            Entry::OrphanMr(mr) => format!("{}!{}", mr.project_path, mr.iid),
        }
    }
}

/// Groups MRs under the issues they reference and sorts the result.
pub fn build(mrs: Vec<MergeRequest>, issues: Vec<Issue>) -> Report {
    // First issue with a given ID wins.
    let mut index: HashMap<IssueId, usize> = HashMap::new();
    let mut groups: Vec<(Issue, Vec<MergeRequest>)> = Vec::new();
    for issue in issues {
        if !index.contains_key(&issue.id) {
            index.insert(issue.id.clone(), groups.len());
            groups.push((issue, Vec::new()));
        }
    }

    let mut orphans: Vec<MergeRequest> = Vec::new();
    for mr in mrs {
        let mut targets: Vec<usize> = mr
            .issue_ids
            .iter()
            .filter_map(|id| index.get(id).copied())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            orphans.push(mr);
        } else {
            for t in targets {
                groups[t].1.push(mr.clone());
            }
        }
    }

    let mut entries: Vec<Entry> = Vec::new();
    for (issue, mut mrs) in groups {
        if !issue.in_window && mrs.is_empty() {
            continue;
        }
        mrs.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        entries.push(Entry::Issue { issue, mrs });
    }
    entries.extend(orphans.into_iter().map(Entry::OrphanMr));

    entries.sort_by(|a, b| {
        b.sort_key()
            .cmp(&a.sort_key())
            .then_with(|| a.display_ref().cmp(&b.display_ref()))
    });

    let mut distinct: HashSet<(u64, u64)> = HashSet::new();
    let mut counts = Counts {
        issues: 0,
        mrs: 0,
        linked_groups: 0,
        orphan_mrs: 0,
        issues_without_mr: 0,
    };
    for e in &entries {
        match e {
            Entry::Issue { mrs, .. } => {
                counts.issues += 1;
                if mrs.is_empty() {
                    counts.issues_without_mr += 1;
                } else {
                    counts.linked_groups += 1;
                }
                distinct.extend(mrs.iter().map(MergeRequest::key));
            }
            Entry::OrphanMr(mr) => {
                counts.orphan_mrs += 1;
                distinct.insert(mr.key());
            }
        }
    }
    counts.mrs = distinct.len();

    Report { entries, counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MrState, Role};
    use chrono::TimeZone;

    fn id(p: &str, n: u32) -> IssueId {
        IssueId {
            project: p.to_string(),
            number: n,
        }
    }

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, h, 0, 0).unwrap()
    }

    fn mr(iid: u64, ids: &[IssueId], updated: DateTime<Utc>) -> MergeRequest {
        MergeRequest {
            project_id: 10,
            iid,
            project_path: "group/proj".into(),
            title: format!("MR {iid}"),
            description: String::new(),
            source_branch: "b".into(),
            state: MrState::Opened,
            web_url: format!("https://gl/group/proj/-/merge_requests/{iid}"),
            author: "eric".into(),
            created_at: at(0),
            updated_at: updated,
            roles: BTreeSet::from([Role::Author]),
            issue_ids: ids.to_vec(),
        }
    }

    fn issue(i: IssueId, updated: DateTime<Utc>, in_window: bool) -> Issue {
        Issue {
            summary: format!("Issue {i}"),
            web_url: format!("https://yt/issue/{i}"),
            id: i,
            state: "Open".into(),
            resolved: false,
            updated,
            roles: BTreeSet::from([Role::Assignee]),
            in_window,
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn known_projects_table() {
        let a = set(&["SP", "MS", "P"]);
        let c = vec!["SP".to_string(), "XX".to_string()];
        assert_eq!(known_projects(Some(&a), &[], &[], true), Some(a.clone()));
        assert_eq!(known_projects(Some(&a), &c, &[], true), Some(set(&["SP"])));
        assert_eq!(
            known_projects(None, &c, &[], false),
            Some(set(&["SP", "XX"]))
        );
        assert_eq!(known_projects(None, &[], &[], false), None);
        assert_eq!(known_projects(None, &[], &[], true), None);
    }

    #[test]
    fn known_projects_without_admin_uses_fetched_issue_projects() {
        let issues = vec![
            issue(id("SP", 1), at(1), true),
            issue(id("MS", 2), at(1), true),
            issue(id("SP", 3), at(1), true),
        ];
        assert_eq!(
            known_projects(None, &[], &issues, true),
            Some(set(&["MS", "SP"]))
        );
        let c = vec!["XX".to_string()];
        assert_eq!(
            known_projects(None, &c, &issues, true),
            Some(set(&["MS", "SP", "XX"]))
        );
        // YouTrack failed or disabled: fetched issues (none) cannot help.
        assert_eq!(known_projects(None, &[], &issues, false), None);
        // With an admin list, fetched issues are irrelevant.
        let a = set(&["SP"]);
        assert_eq!(known_projects(Some(&a), &[], &issues, true), Some(a));
    }

    #[test]
    fn aliases_replace_and_dedupe() {
        let aliases = BTreeMap::from([(id("OLD", 12), id("NEW", 5))]);
        let mut mrs = vec![
            mr(1, &[id("OLD", 12)], at(1)),
            mr(2, &[id("OLD", 12), id("NEW", 5)], at(1)),
            mr(3, &[id("NEW", 5), id("OLD", 12), id("SP", 1)], at(1)),
        ];
        apply_aliases(&mut mrs, &aliases);
        assert_eq!(mrs[0].issue_ids, vec![id("NEW", 5)]);
        assert_eq!(mrs[1].issue_ids, vec![id("NEW", 5)]);
        assert_eq!(mrs[2].issue_ids, vec![id("NEW", 5), id("SP", 1)]);
    }

    #[test]
    fn linked_mr_joins_issue_group() {
        let r = build(
            vec![mr(1, &[id("SP", 1)], at(5))],
            vec![issue(id("SP", 1), at(3), true)],
        );
        assert_eq!(r.entries.len(), 1);
        match &r.entries[0] {
            Entry::Issue { issue, mrs } => {
                assert_eq!(issue.id, id("SP", 1));
                assert_eq!(mrs.len(), 1);
                assert_eq!(mrs[0].iid, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            r.counts,
            Counts {
                issues: 1,
                mrs: 1,
                linked_groups: 1,
                orphan_mrs: 0,
                issues_without_mr: 0
            }
        );
    }

    #[test]
    fn mr_with_two_issues_in_both_groups_counted_once() {
        let r = build(
            vec![mr(1, &[id("SP", 1), id("SP", 2)], at(5))],
            vec![
                issue(id("SP", 1), at(3), true),
                issue(id("SP", 2), at(3), true),
            ],
        );
        let with_mr = r
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Issue { mrs, .. } if mrs.len() == 1))
            .count();
        assert_eq!(with_mr, 2);
        assert_eq!(r.counts.mrs, 1);
        assert_eq!(r.counts.linked_groups, 2);
        assert_eq!(r.counts.issues, 2);
        assert_eq!(r.counts.orphan_mrs, 0);
    }

    #[test]
    fn mr_without_refs_is_orphan() {
        let r = build(vec![mr(1, &[], at(5))], vec![]);
        assert!(matches!(&r.entries[0], Entry::OrphanMr(m) if m.iid == 1));
        assert_eq!(r.counts.orphan_mrs, 1);
        assert_eq!(r.counts.mrs, 1);
        assert_eq!(r.counts.issues, 0);
    }

    #[test]
    fn ref_to_unfetched_issue_is_orphan_keeping_ids() {
        let r = build(
            vec![mr(1, &[id("SP", 99)], at(5))],
            vec![issue(id("SP", 1), at(3), true)],
        );
        let orphan = r
            .entries
            .iter()
            .find_map(|e| match e {
                Entry::OrphanMr(m) => Some(m),
                _ => None,
            })
            .expect("orphan");
        assert_eq!(orphan.issue_ids, vec![id("SP", 99)]);
        assert_eq!(r.counts.issues_without_mr, 1);
        assert_eq!(r.counts.orphan_mrs, 1);
    }

    #[test]
    fn in_window_issue_without_mr_kept() {
        let r = build(vec![], vec![issue(id("SP", 1), at(3), true)]);
        assert_eq!(
            r.entries,
            vec![Entry::Issue {
                issue: issue(id("SP", 1), at(3), true),
                mrs: vec![]
            }]
        );
        assert_eq!(r.counts.issues_without_mr, 1);
        assert_eq!(r.counts.linked_groups, 0);
        assert_eq!(r.counts.mrs, 0);
    }

    #[test]
    fn referenced_issue_without_mr_dropped() {
        let r = build(
            vec![],
            vec![
                issue(id("SP", 1), at(3), false),
                issue(id("SP", 2), at(4), true),
            ],
        );
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.counts.issues, 1);
        assert_eq!(r.counts.issues_without_mr, 1);
    }

    #[test]
    fn referenced_issue_with_mr_kept() {
        let r = build(
            vec![mr(1, &[id("SP", 1)], at(5))],
            vec![issue(id("SP", 1), at(3), false)],
        );
        assert_eq!(r.counts.issues, 1);
        assert_eq!(r.counts.linked_groups, 1);
    }

    #[test]
    fn sorted_by_max_updated_desc_with_ref_tiebreak() {
        let r = build(
            vec![
                mr(7, &[id("SP", 2)], at(9)),  // lifts SP-2 to 9
                mr(3, &[], at(6)),             // orphan group/proj!3 at 6
                mr(2, &[], at(6)),             // orphan group/proj!2 at 6
                mr(8, &[id("SP", 12)], at(2)), // SP-12 stays at its own 6
            ],
            vec![
                issue(id("SP", 2), at(1), true),
                issue(id("SP", 12), at(6), true),
                issue(id("SP", 5), at(4), true),
            ],
        );
        let refs: Vec<String> = r.entries.iter().map(Entry::display_ref).collect();
        assert_eq!(
            refs,
            vec![
                "SP-2",
                "SP-12", // ties at 6: "SP-12" < "group/proj!2" (uppercase sorts first)
                "group/proj!2",
                "group/proj!3",
                "SP-5"
            ]
        );
    }

    #[test]
    fn mrs_inside_group_sorted_desc() {
        let r = build(
            vec![
                mr(1, &[id("SP", 1)], at(2)),
                mr(2, &[id("SP", 1)], at(8)),
                mr(3, &[id("SP", 1)], at(5)),
            ],
            vec![issue(id("SP", 1), at(1), true)],
        );
        match &r.entries[0] {
            Entry::Issue { mrs, .. } => {
                let iids: Vec<u64> = mrs.iter().map(|m| m.iid).collect();
                assert_eq!(iids, vec![2, 3, 1]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn counts_mixed() {
        let r = build(
            vec![
                mr(1, &[id("SP", 1), id("SP", 2)], at(5)),
                mr(2, &[id("SP", 1)], at(6)),
                mr(3, &[], at(7)),
                mr(4, &[id("ZZ", 1)], at(8)),
            ],
            vec![
                issue(id("SP", 1), at(1), true),
                issue(id("SP", 2), at(1), false),
                issue(id("SP", 3), at(1), true),
                issue(id("SP", 4), at(1), false), // dropped
            ],
        );
        assert_eq!(
            r.counts,
            Counts {
                issues: 3,
                mrs: 4,
                linked_groups: 2,
                orphan_mrs: 2,
                issues_without_mr: 1
            }
        );
        assert_eq!(r.entries.len(), 5);
    }
}
