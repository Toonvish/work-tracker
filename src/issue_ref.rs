//! Issue-ID extraction from free text (DESIGN 7.1-7.3).

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::model::{IssueId, MergeRequest};
use crate::text::truncate_bytes;

static RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[a-z][a-z0-9]{0,19}-[0-9]{1,7}").unwrap());

static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").unwrap());

/// Project names that are never real YouTrack projects (used when no known set exists).
const DENYLIST: &[&str] = &[
    "UTF", "ISO", "SHA", "RFC", "CVE", "X86", "X64", "ARM", "AES", "RSA", "HTTP", "TLS", "SSL",
    "IPV", "GPL", "MD", "RC", "V", "WIN", "MR", "PR",
];

/// True for project names that are never real YouTrack projects: the
/// denylist, plus version-like prefixes such as `V2` (from `v2-3`).
fn is_denied(project: &str) -> bool {
    DENYLIST.contains(&project)
        || project
            .strip_prefix('V')
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Removes URLs from `text` (they hold project paths like `bar-2`), except
/// that the part after `/issue/` of a YouTrack issue link is kept.
fn strip_urls(text: &str) -> String {
    URL_RE
        .replace_all(text, |caps: &regex::Captures| {
            let url = &caps[0];
            match url.split_once("/issue/") {
                Some((_, tail)) => format!(" {tail}"),
                None => " ".to_string(),
            }
        })
        .into_owned()
}

/// Raw extraction: boundary rules only, no project filter. Deduped, first-occurrence order.
pub fn extract_issue_ids(text: &str) -> Vec<IssueId> {
    let mut out: Vec<IssueId> = Vec::new();
    for m in RE.find_iter(text) {
        let before = text[..m.start()].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let after = text[m.end()..].chars().next();
        if after.is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(id) = m.as_str().parse::<IssueId>() else {
            continue; // zero or otherwise invalid
        };
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Keeps only IDs of known projects, or applies the denylist when `known` is `None`.
pub fn filter_known(ids: Vec<IssueId>, known: Option<&BTreeSet<String>>) -> Vec<IssueId> {
    ids.into_iter()
        .filter(|id| match known {
            Some(set) => set.contains(&id.project),
            None => !is_denied(&id.project),
        })
        .collect()
}

/// Extracts the description text outside fenced code blocks.
fn strip_fences(desc: &str) -> String {
    let mut in_fence = false;
    let mut out = String::new();
    for line in desc.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Issue IDs referenced by an MR: title, then source branch, then description.
pub fn mr_issue_ids(mr: &MergeRequest, known: Option<&BTreeSet<String>>) -> Vec<IssueId> {
    let desc = strip_fences(truncate_bytes(&mr.description, 65_536));
    let mut seen: HashSet<IssueId> = HashSet::new();
    let mut ids: Vec<IssueId> = Vec::new();
    for src in [mr.title.as_str(), mr.source_branch.as_str(), desc.as_str()] {
        for id in extract_issue_ids(&strip_urls(src)) {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    filter_known(ids, known)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MrState;
    use chrono::{TimeZone, Utc};

    fn id(p: &str, n: u32) -> IssueId {
        IssueId {
            project: p.to_string(),
            number: n,
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn mr(title: &str, branch: &str, desc: &str) -> MergeRequest {
        let t = Utc.with_ymd_and_hms(2026, 9, 30, 12, 0, 0).unwrap();
        MergeRequest {
            project_id: 1,
            iid: 1,
            project_path: "g/p".into(),
            title: title.into(),
            description: desc.into(),
            source_branch: branch.into(),
            state: MrState::Opened,
            web_url: "https://gl/g/p/-/merge_requests/1".into(),
            author: "eric".into(),
            created_at: t,
            updated_at: t,
            roles: BTreeSet::new(),
            issue_ids: vec![],
        }
    }

    #[test]
    fn positive_cases() {
        let cases: &[(&str, IssueId)] = &[
            ("SP-123 fix", id("SP", 123)),
            ("fix sp-45", id("SP", 45)),
            ("feature/SP-123-login", id("SP", 123)),
            ("sp-123_fix", id("SP", 123)),
            ("bugfix-SP-12", id("SP", 12)),
            ("feature_SP-123", id("SP", 123)),
            ("eric_sp-12-fix", id("SP", 12)),
            ("1234_SP-5", id("SP", 5)),
            ("Resolve \"MS-7: x\"", id("MS", 7)),
            ("(P-7)", id("P", 7)),
            ("https://youtrack.uponu.com/issue/SP-9", id("SP", 9)),
        ];
        for (text, want) in cases {
            assert_eq!(extract_issue_ids(text), vec![want.clone()], "{text:?}");
        }
    }

    #[test]
    fn multiple_ids_dedup_keep_order() {
        assert_eq!(
            extract_issue_ids("SP-2 and MS-1, again sp-2 then SP-3"),
            vec![id("SP", 2), id("MS", 1), id("SP", 3)]
        );
        assert_eq!(
            extract_issue_ids("fix-12-SP-3"),
            vec![id("FIX", 12), id("SP", 3)]
        );
    }

    #[test]
    fn negative_cases() {
        for text in [
            "3f2a-4b1c-9d",
            "a1b2-3cd4",
            "SP-0",
            "SP-12abc",
            "SP-12345678",
            "",
        ] {
            assert!(extract_issue_ids(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn glued_prefix_is_part_of_match() {
        let raw = extract_issue_ids("x1SP-2");
        assert_eq!(raw, vec![id("X1SP", 2)]);
        assert!(filter_known(raw, Some(&set(&["SP"]))).is_empty());
    }

    #[test]
    fn filter_with_known_set() {
        let ids = extract_issue_ids("SP-1 UTF-8 ISO-8601");
        let got = filter_known(ids, Some(&set(&["SP"])));
        assert_eq!(got, vec![id("SP", 1)]);
    }

    #[test]
    fn filter_with_denylist() {
        let ids = extract_issue_ids("UTF-8 sha-256 SP-1");
        assert_eq!(filter_known(ids, None), vec![id("SP", 1)]);
    }

    #[test]
    fn version_prefixes_denied_without_known_set() {
        for text in ["release v2-3", "v10-1"] {
            assert!(mr_issue_ids(&mr(text, "b", ""), None).is_empty(), "{text}");
        }
        // A real project that merely starts with V is kept.
        assert_eq!(mr_issue_ids(&mr("VX-3", "b", ""), None), vec![id("VX", 3)]);
    }

    #[test]
    fn noise_is_dropped_with_known_set() {
        let known = set(&["SP"]);
        for text in [
            "deadbeef-1234-5678-9abc",
            "python3-12",
            "release v2-3",
            "abc1234-5",
        ] {
            let m = mr(text, "b", "");
            assert!(mr_issue_ids(&m, Some(&known)).is_empty(), "{text}");
            let m = mr("t", "b", text);
            assert!(mr_issue_ids(&m, Some(&known)).is_empty(), "{text}");
        }
    }

    #[test]
    fn urls_in_description_are_skipped() {
        let desc = "see https://gitlab.paar-it.de/foo/bar-2/-/merge_requests/3 and SP-4";
        assert_eq!(mr_issue_ids(&mr("t", "b", desc), None), vec![id("SP", 4)]);
        let desc = "https://gitlab.paar-it.de/foo/bar-2/-/merge_requests/3";
        assert!(mr_issue_ids(&mr("t", "b", desc), None).is_empty());
        assert!(mr_issue_ids(&mr("t", "b", desc), Some(&set(&["BAR"]))).is_empty());
    }

    #[test]
    fn youtrack_issue_urls_are_kept() {
        let desc = "closes (https://youtrack.uponu.com/issue/SP-9) and \
                    [x](https://youtrack.uponu.com/issue/MS-2?foo=1)";
        assert_eq!(
            mr_issue_ids(&mr("t", "b", desc), Some(&set(&["SP", "MS"]))),
            vec![id("SP", 9), id("MS", 2)]
        );
    }

    #[test]
    fn fenced_code_ignored() {
        let desc = "SP-1 intro\n```\nSP-2 in code\n```\nafter SP-3\n  ~~~\nSP-4\n~~~\nend";
        let got = mr_issue_ids(&mr("t", "b", desc), Some(&set(&["SP"])));
        assert_eq!(got, vec![id("SP", 1), id("SP", 3)]);
    }

    #[test]
    fn id_after_cap_not_found() {
        let mut desc = "a ".repeat(32_768); // exactly 65_536 bytes
        desc.push_str("SP-9");
        let got = mr_issue_ids(&mr("t", "b", &desc), Some(&set(&["SP"])));
        assert!(got.is_empty());
        let mut early = "SP-8 ".to_string();
        early.push_str(&desc);
        let got = mr_issue_ids(&mr("t", "b", &early), Some(&set(&["SP"])));
        assert_eq!(got, vec![id("SP", 8)]);
    }

    #[test]
    fn multibyte_at_cap_does_not_panic() {
        let mut desc = " ".repeat(65_535);
        desc.push('€'); // straddles byte 65_536
        desc.push_str(" SP-1");
        assert!(mr_issue_ids(&mr("t", "b", &desc), None).is_empty());
        let mut desc = "x ".repeat(32_766);
        desc.push_str("SP-5 ");
        while desc.len() < 65_534 {
            desc.push(' ');
        }
        desc.push('😀');
        desc.push_str("tail");
        assert_eq!(mr_issue_ids(&mr("t", "b", &desc), None), vec![id("SP", 5)]);
    }

    #[test]
    fn source_order_title_branch_description() {
        let got = mr_issue_ids(
            &mr("MS-3 title", "feature/SP-2-x", "desc SP-1 and ms-3"),
            Some(&set(&["SP", "MS"])),
        );
        assert_eq!(got, vec![id("MS", 3), id("SP", 2), id("SP", 1)]);
    }

    #[test]
    fn mr_filter_known_and_none() {
        let m = mr("SP-1 UTF-8 ISO-8601", "b", "sha-256");
        assert_eq!(mr_issue_ids(&m, Some(&set(&["SP"]))), vec![id("SP", 1)]);
        assert_eq!(mr_issue_ids(&m, None), vec![id("SP", 1)]);
        let m = mr("SP-1 UTF-8", "b", "sha-256 AB-2");
        assert_eq!(mr_issue_ids(&m, None), vec![id("SP", 1), id("AB", 2)]);
    }
}
