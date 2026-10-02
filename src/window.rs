//! Time window computation (DESIGN section 4).
//!
//! Generic over `Tz: TimeZone`: production uses `chrono::Local`, tests use
//! `chrono_tz::Europe::Berlin` so DST behaviour is deterministic.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
    Weekday,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meeting {
    pub weekday: Weekday,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRequest {
    /// `Utc::now()` or `--now`.
    pub now: DateTime<Utc>,
    /// `--since`.
    pub since: Option<DateTime<Utc>>,
    /// `--until`.
    pub until: Option<DateTime<Utc>>,
    /// `--previous` count (0 = current window).
    pub previous: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOrigin {
    Meeting {
        meeting: Meeting,
        ended_at: DateTime<Utc>,
        previous: u32,
    },
    /// `--since` was given.
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub origin: WindowOrigin,
}

impl Window {
    /// Inclusive at both ends.
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        self.start <= t && t <= self.end
    }
}

/// The two `String` fields of `SinceNotBefore*` are local times formatted
/// `%Y-%m-%d %H:%M`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WindowError {
    #[error("no meetings configured; add [[meetings]] to the config or pass --since")]
    NoMeetings,
    #[error("--since ({0}) must be before --until ({1})")]
    SinceNotBeforeUntil(String, String),
    #[error("--since ({0}) must be before now ({1})")]
    SinceNotBeforeNow(String, String),
    #[error(
        "invalid date/time \"{0}\"; accepted: 2026-09-29T10:00, \"2026-09-29 10:00\", 2026-09-29, or RFC 3339 (2026-09-29T10:00:00+02:00)"
    )]
    BadDateTime(String),
}

/// Resolve a local wall-clock time to an instant.
///
/// Single -> that instant; Ambiguous -> the earliest; DST gap -> +1h, then
/// 15-minute steps (up to 16), and finally the naive time taken as UTC.
pub fn resolve_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> DateTime<Tz> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => t,
        LocalResult::Ambiguous(a, b) => a.min(b),
        LocalResult::None => {
            if let Some(t) = tz
                .from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()
            {
                return t;
            }
            for k in 1..=16 {
                if let Some(t) = tz
                    .from_local_datetime(&(naive + Duration::minutes(15 * k)))
                    .earliest()
                {
                    return t;
                }
            }
            // Cannot happen with real tz data.
            tz.from_utc_datetime(&naive)
        }
    }
}

/// Meeting end instants that are `<= now_utc`, newest first, deduplicated by
/// end instant. Always yields at least `needed` candidates for sane input
/// (one meeting per week is enough).
pub fn finished_meeting_ends<Tz: TimeZone>(
    tz: &Tz,
    meetings: &[Meeting],
    now_utc: DateTime<Utc>,
    needed: usize,
) -> Vec<(DateTime<Utc>, Meeting)> {
    let today = now_utc.with_timezone(tz).date_naive();
    let max_back = 7 * needed as i64 + 7;
    let mut cands = Vec::new();
    for d in 0..=max_back {
        let date = today - Duration::days(d);
        for m in meetings.iter().filter(|m| m.weekday == date.weekday()) {
            let end_utc = resolve_local(tz, date.and_time(m.end)).with_timezone(&Utc);
            if end_utc <= now_utc {
                cands.push((end_utc, *m));
            }
        }
    }
    cands.sort_by_key(|c| std::cmp::Reverse(c.0));
    cands.dedup_by_key(|c| c.0);
    cands
}

fn fmt_local<Tz: TimeZone>(tz: &Tz, t: DateTime<Utc>) -> String {
    t.with_timezone(tz)
        .naive_local()
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

pub fn compute<Tz: TimeZone>(
    tz: &Tz,
    meetings: &[Meeting],
    req: &WindowRequest,
) -> Result<Window, WindowError> {
    if let Some(since) = req.since {
        let end = match req.until {
            Some(until) => {
                if since >= until {
                    return Err(WindowError::SinceNotBeforeUntil(
                        fmt_local(tz, since),
                        fmt_local(tz, until),
                    ));
                }
                until
            }
            None => {
                if since >= req.now {
                    return Err(WindowError::SinceNotBeforeNow(
                        fmt_local(tz, since),
                        fmt_local(tz, req.now),
                    ));
                }
                req.now
            }
        };
        return Ok(Window {
            start: since,
            end,
            origin: WindowOrigin::Explicit,
        });
    }
    if meetings.is_empty() {
        return Err(WindowError::NoMeetings);
    }
    let anchor = req.until.unwrap_or(req.now);
    let k = req.previous as usize;
    let cands = finished_meeting_ends(tz, meetings, anchor, k + 1);
    // Cannot be short for validated meetings; do not panic on odd input.
    let Some(&(start, meeting)) = cands.get(k) else {
        return Err(WindowError::NoMeetings);
    };
    let end = if k == 0 { anchor } else { cands[k - 1].0 };
    Ok(Window {
        start,
        end,
        origin: WindowOrigin::Meeting {
            meeting,
            ended_at: start,
            previous: req.previous,
        },
    })
}

/// Parse `--since/--until/--now` input (DESIGN 4.4).
pub fn parse_user_datetime<Tz: TimeZone>(tz: &Tz, s: &str) -> Result<DateTime<Utc>, WindowError> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(resolve_local(tz, n).with_timezone(&Utc));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(resolve_local(tz, d.and_time(NaiveTime::MIN)).with_timezone(&Utc));
    }
    Err(WindowError::BadDateTime(s.to_string()))
}

pub const PAD_DAYS: i64 = 2;

/// (UTC date of `w.start` - PAD_DAYS, UTC date of `w.end` + PAD_DAYS).
pub fn padded_dates(w: &Window) -> (NaiveDate, NaiveDate) {
    (
        w.start.date_naive() - Duration::days(PAD_DAYS),
        w.end.date_naive() + Duration::days(PAD_DAYS),
    )
}

/// `Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)`
pub fn format_window_line<Tz: TimeZone>(tz: &Tz, w: &Window, now: DateTime<Utc>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let now_year = now.with_timezone(tz).year();
    let stamp = |t: DateTime<Utc>| {
        let l = t.with_timezone(tz);
        let fmt = if l.year() == now_year {
            "%a %d %b %H:%M"
        } else {
            "%Y %a %d %b %H:%M"
        };
        l.format(fmt).to_string()
    };
    let dur = w.end - w.start;
    let duration = if dur >= Duration::days(1) {
        format!("{}d {}h", dur.num_days(), dur.num_hours() % 24)
    } else {
        format!("{}h {}m", dur.num_hours(), dur.num_minutes() % 60)
    };
    let origin = match w.origin {
        WindowOrigin::Explicit => "explicit range".to_string(),
        WindowOrigin::Meeting {
            meeting, previous, ..
        } => {
            let since = format!(
                "since {} {}\u{2013}{} meeting",
                meeting.weekday,
                meeting.start.format("%H:%M"),
                meeting.end.format("%H:%M")
            );
            if previous == 0 {
                since
            } else {
                format!("previous \u{00D7}{previous}, {since}")
            }
        }
    };
    format!(
        "Window: {} \u{2192} {}  ({duration}, {origin})",
        stamp(w.start),
        stamp(w.end)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Berlin;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    /// Berlin local time -> UTC instant.
    fn lt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        resolve_local(
            &Berlin,
            NaiveDate::from_ymd_opt(y, mo, d)
                .unwrap()
                .and_hms_opt(h, mi, 0)
                .unwrap(),
        )
        .with_timezone(&Utc)
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn mm() -> Vec<Meeting> {
        vec![
            Meeting {
                weekday: Weekday::Tue,
                start: t(9, 0),
                end: t(10, 0),
            },
            Meeting {
                weekday: Weekday::Thu,
                start: t(14, 0),
                end: t(15, 0),
            },
        ]
    }

    fn tue_only() -> Vec<Meeting> {
        vec![mm()[0]]
    }

    fn sun_gap() -> Vec<Meeting> {
        vec![Meeting {
            weekday: Weekday::Sun,
            start: t(2, 0),
            end: t(2, 30),
        }]
    }

    fn req(now: DateTime<Utc>) -> WindowRequest {
        WindowRequest {
            now,
            since: None,
            until: None,
            previous: 0,
        }
    }

    fn run(meetings: &[Meeting], r: &WindowRequest) -> Result<Window, WindowError> {
        compute(&Berlin, meetings, r)
    }

    fn check(w: Result<Window, WindowError>, start: DateTime<Utc>, end: DateTime<Utc>) -> Window {
        let w = w.unwrap();
        assert_eq!(w.start, start, "start");
        assert_eq!(w.end, end, "end");
        w
    }

    // ---- DESIGN 4.5 table ----

    #[test]
    fn case_01_wed_after_tue_meeting() {
        let now = lt(2026, 9, 30, 11, 0);
        let w = check(run(&mm(), &req(now)), lt(2026, 9, 29, 10, 0), now);
        assert!(matches!(
            w.origin,
            WindowOrigin::Meeting { previous: 0, meeting, .. } if meeting.weekday == Weekday::Tue
        ));
    }

    #[test]
    fn case_02_tue_before_meeting() {
        let now = lt(2026, 9, 29, 8, 30);
        check(run(&mm(), &req(now)), lt(2026, 9, 24, 15, 0), now);
    }

    #[test]
    fn case_03_tue_meeting_running() {
        let now = lt(2026, 9, 29, 9, 30);
        check(run(&mm(), &req(now)), lt(2026, 9, 24, 15, 0), now);
    }

    #[test]
    fn case_04_tue_just_after_meeting() {
        let now = lt(2026, 9, 29, 10, 30);
        check(run(&mm(), &req(now)), lt(2026, 9, 29, 10, 0), now);
    }

    #[test]
    fn case_05_thu_meeting_running() {
        let now = lt(2026, 10, 1, 14, 30);
        check(run(&mm(), &req(now)), lt(2026, 9, 29, 10, 0), now);
    }

    #[test]
    fn case_06_exactly_at_meeting_end_is_zero_length() {
        let now = lt(2026, 10, 1, 15, 0);
        let w = check(run(&mm(), &req(now)), lt(2026, 10, 1, 15, 0), now);
        assert_eq!(w.start, w.end);
    }

    #[test]
    fn case_07_monday_week_wrap() {
        let now = lt(2026, 9, 28, 12, 0);
        check(run(&mm(), &req(now)), lt(2026, 9, 24, 15, 0), now);
    }

    #[test]
    fn case_08_sunday_week_wrap() {
        let now = lt(2026, 10, 4, 18, 0);
        check(run(&mm(), &req(now)), lt(2026, 10, 1, 15, 0), now);
    }

    #[test]
    fn monday_morning_looks_back_to_thursday() {
        let now = lt(2026, 9, 28, 8, 0);
        check(run(&mm(), &req(now)), lt(2026, 9, 24, 15, 0), now);
    }

    #[test]
    fn sunday_late_evening_looks_back_to_thursday() {
        // Sun 23:30 Berlin = 21:30Z
        let now = lt(2026, 10, 4, 23, 30);
        assert_eq!(now, utc(2026, 10, 4, 21, 30));
        check(run(&mm(), &req(now)), lt(2026, 10, 1, 15, 0), now);
    }

    #[test]
    fn meeting_late_on_local_day_before_utc_midnight() {
        // Now = Sun 00:30 Berlin = Sat 22:30Z; the Sat 23:00-23:45 meeting
        // (21:45Z) is the most recent finished one.
        let meetings = [Meeting {
            weekday: Weekday::Sat,
            start: t(23, 0),
            end: t(23, 45),
        }];
        let now = lt(2026, 10, 4, 0, 30);
        assert_eq!(now, utc(2026, 10, 3, 22, 30));
        check(run(&meetings, &req(now)), lt(2026, 10, 3, 23, 45), now);
    }

    #[test]
    fn local_date_ahead_of_utc_date_is_used() {
        // Now = Sun 00:30 Berlin = Sat 22:30Z. The Sun 00:00-00:15 meeting
        // ended at Sat 22:15Z. Taking `today` as the UTC date (Sat) would
        // skip it and fall back a week.
        let meetings = [Meeting {
            weekday: Weekday::Sun,
            start: t(0, 0),
            end: t(0, 15),
        }];
        let now = lt(2026, 10, 4, 0, 30);
        let w = check(run(&meetings, &req(now)), lt(2026, 10, 4, 0, 15), now);
        assert_eq!(w.start, utc(2026, 10, 3, 22, 15));
    }

    #[test]
    fn case_09_single_meeting_full_week() {
        let now = lt(2026, 9, 29, 9, 30);
        check(run(&tue_only(), &req(now)), lt(2026, 9, 22, 10, 0), now);
    }

    #[test]
    fn case_10_single_meeting_day_after() {
        let now = lt(2026, 9, 30, 11, 0);
        check(run(&tue_only(), &req(now)), lt(2026, 9, 29, 10, 0), now);
    }

    #[test]
    fn case_11_previous_1() {
        let now = lt(2026, 9, 30, 11, 0);
        let r = WindowRequest {
            previous: 1,
            ..req(now)
        };
        let w = check(
            run(&mm(), &r),
            lt(2026, 9, 24, 15, 0),
            lt(2026, 9, 29, 10, 0),
        );
        assert!(matches!(
            w.origin,
            WindowOrigin::Meeting { previous: 1, .. }
        ));
    }

    #[test]
    fn case_12_previous_2() {
        let now = lt(2026, 9, 30, 11, 0);
        let r = WindowRequest {
            previous: 2,
            ..req(now)
        };
        check(
            run(&mm(), &r),
            lt(2026, 9, 22, 10, 0),
            lt(2026, 9, 24, 15, 0),
        );
    }

    #[test]
    fn case_13_dst_end_in_between() {
        let now = lt(2026, 10, 27, 8, 30);
        let w = check(run(&mm(), &req(now)), lt(2026, 10, 22, 15, 0), now);
        assert_eq!(w.start, utc(2026, 10, 22, 13, 0));
        assert_eq!(w.end, utc(2026, 10, 27, 7, 30));
    }

    #[test]
    fn case_14_dst_start_in_between() {
        let now = lt(2026, 3, 31, 8, 30);
        let w = check(run(&mm(), &req(now)), lt(2026, 3, 26, 15, 0), now);
        assert_eq!(w.start, utc(2026, 3, 26, 14, 0));
        assert_eq!(w.end, utc(2026, 3, 31, 6, 30));
    }

    #[test]
    fn case_15_meeting_end_in_dst_gap() {
        let now = lt(2026, 3, 29, 12, 0);
        let w = check(run(&sun_gap(), &req(now)), lt(2026, 3, 29, 3, 30), now);
        assert_eq!(w.start, utc(2026, 3, 29, 1, 30));
    }

    #[test]
    fn case_16_meeting_end_ambiguous() {
        let now = lt(2026, 10, 25, 12, 0);
        let w = run(&sun_gap(), &req(now)).unwrap();
        assert_eq!(w.start, utc(2026, 10, 25, 0, 30));
        assert_eq!(w.end, now);
    }

    #[test]
    fn case_17_explicit_since() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            since: Some(lt(2026, 9, 28, 8, 0)),
            ..req(now)
        };
        let w = check(run(&mm(), &r), lt(2026, 9, 28, 8, 0), now);
        assert_eq!(w.origin, WindowOrigin::Explicit);
    }

    #[test]
    fn case_18_since_and_until() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            since: Some(parse_user_datetime(&Berlin, "2026-09-28").unwrap()),
            until: Some(parse_user_datetime(&Berlin, "2026-09-29T12:00").unwrap()),
            ..req(now)
        };
        check(
            run(&mm(), &r),
            lt(2026, 9, 28, 0, 0),
            lt(2026, 9, 29, 12, 0),
        );
    }

    #[test]
    fn case_19_until_alone_is_anchor() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            until: Some(lt(2026, 9, 29, 8, 30)),
            ..req(now)
        };
        check(
            run(&mm(), &r),
            lt(2026, 9, 24, 15, 0),
            lt(2026, 9, 29, 8, 30),
        );
    }

    #[test]
    fn case_20_since_after_until() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            since: Some(lt(2026, 9, 29, 12, 0)),
            until: Some(lt(2026, 9, 29, 8, 0)),
            ..req(now)
        };
        let e = run(&mm(), &r).unwrap_err();
        assert_eq!(
            e,
            WindowError::SinceNotBeforeUntil("2026-09-29 12:00".into(), "2026-09-29 08:00".into())
        );
        assert_eq!(
            e.to_string(),
            "--since (2026-09-29 12:00) must be before --until (2026-09-29 08:00)"
        );
    }

    #[test]
    fn case_21_no_meetings_no_since() {
        let e = run(&[], &req(lt(2026, 10, 2, 12, 0))).unwrap_err();
        assert_eq!(e, WindowError::NoMeetings);
        assert_eq!(
            e.to_string(),
            "no meetings configured; add [[meetings]] to the config or pass --since"
        );
    }

    #[test]
    fn case_22_no_meetings_with_since() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            since: Some(lt(2026, 9, 28, 8, 0)),
            ..req(now)
        };
        let w = check(run(&[], &r), lt(2026, 9, 28, 8, 0), now);
        assert_eq!(w.origin, WindowOrigin::Explicit);
    }

    #[test]
    fn case_23_since_not_before_now() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            since: Some(lt(2026, 10, 3, 0, 0)),
            ..req(now)
        };
        let e = run(&mm(), &r).unwrap_err();
        assert_eq!(
            e,
            WindowError::SinceNotBeforeNow("2026-10-03 00:00".into(), "2026-10-02 12:00".into())
        );
        assert!(e.to_string().contains("must be before now"));
        assert!(!e.to_string().contains("--until"));
    }

    #[test]
    fn case_24_previous_with_until() {
        let now = lt(2026, 10, 2, 12, 0);
        let r = WindowRequest {
            previous: 1,
            until: Some(lt(2026, 9, 30, 11, 0)),
            ..req(now)
        };
        check(
            run(&mm(), &r),
            lt(2026, 9, 24, 15, 0),
            lt(2026, 9, 29, 10, 0),
        );
    }

    // ---- resolve_local ----

    #[test]
    fn resolve_local_single() {
        let n = NaiveDate::from_ymd_opt(2026, 9, 29)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        assert_eq!(
            resolve_local(&Berlin, n).with_timezone(&Utc),
            utc(2026, 9, 29, 8, 0)
        );
    }

    #[test]
    fn resolve_local_gap_moves_forward_one_hour() {
        let n = NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let r = resolve_local(&Berlin, n);
        assert_eq!(r.naive_local(), n + Duration::hours(1));
        assert_eq!(r.with_timezone(&Utc), utc(2026, 3, 29, 1, 30));
    }

    #[test]
    fn resolve_local_ambiguous_is_earliest() {
        let n = NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(
            resolve_local(&Berlin, n).with_timezone(&Utc),
            utc(2026, 10, 25, 0, 30)
        );
    }

    // ---- Window::contains ----

    #[test]
    fn contains_is_inclusive_at_both_ends() {
        let w = Window {
            start: utc(2026, 9, 29, 8, 0),
            end: utc(2026, 9, 30, 9, 0),
            origin: WindowOrigin::Explicit,
        };
        assert!(w.contains(w.start));
        assert!(w.contains(w.end));
        assert!(w.contains(utc(2026, 9, 29, 12, 0)));
        assert!(!w.contains(w.start - Duration::seconds(1)));
        assert!(!w.contains(w.end + Duration::seconds(1)));
    }

    // ---- finished_meeting_ends ----

    #[test]
    fn finished_meeting_ends_has_enough_candidates() {
        let now = lt(2026, 9, 30, 11, 0);
        for meetings in [mm(), tue_only()] {
            for k in 0..=3usize {
                let c = finished_meeting_ends(&Berlin, &meetings, now, k + 1);
                assert!(c.len() > k, "k={k}: got {}", c.len());
                assert!(c.windows(2).all(|p| p[0].0 > p[1].0), "strictly descending");
                assert!(c.iter().all(|x| x.0 <= now));
            }
        }
    }

    #[test]
    fn finished_meeting_ends_dedups_same_end_instant() {
        let a = Meeting {
            weekday: Weekday::Tue,
            start: t(9, 0),
            end: t(10, 0),
        };
        let b = Meeting {
            weekday: Weekday::Tue,
            start: t(9, 30),
            end: t(10, 0),
        };
        let c = finished_meeting_ends(&Berlin, &[a, b], lt(2026, 9, 30, 11, 0), 1);
        assert_eq!(
            c.iter().filter(|x| x.0 == lt(2026, 9, 29, 10, 0)).count(),
            1
        );
    }

    // ---- parse_user_datetime ----

    #[test]
    fn parse_rfc3339_with_offset() {
        assert_eq!(
            parse_user_datetime(&Berlin, "2026-09-29T10:00:00+02:00").unwrap(),
            utc(2026, 9, 29, 8, 0)
        );
        assert_eq!(
            parse_user_datetime(&Berlin, "2026-09-29T08:00:00Z").unwrap(),
            utc(2026, 9, 29, 8, 0)
        );
    }

    #[test]
    fn parse_naive_forms() {
        let want = utc(2026, 9, 29, 8, 0);
        for s in [
            "2026-09-29T10:00:00",
            "2026-09-29T10:00",
            "2026-09-29 10:00:00",
            "2026-09-29 10:00",
        ] {
            assert_eq!(parse_user_datetime(&Berlin, s).unwrap(), want, "{s}");
        }
    }

    #[test]
    fn parse_date_only_is_local_midnight() {
        assert_eq!(
            parse_user_datetime(&Berlin, "2026-09-29").unwrap(),
            utc(2026, 9, 28, 22, 0)
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            parse_user_datetime(&Berlin, "  2026-09-29 10:00\n").unwrap(),
            utc(2026, 9, 29, 8, 0)
        );
    }

    #[test]
    fn parse_garbage_is_bad_datetime() {
        for s in ["yesterday", "2026-13-01", ""] {
            assert_eq!(
                parse_user_datetime(&Berlin, s).unwrap_err(),
                WindowError::BadDateTime(s.to_string())
            );
        }
        let e = parse_user_datetime(&Berlin, "yesterday").unwrap_err();
        assert!(e.to_string().starts_with("invalid date/time \"yesterday\""));
    }

    #[test]
    fn parse_naive_in_dst_gap_resolves_forward() {
        assert_eq!(
            parse_user_datetime(&Berlin, "2026-03-29T02:30").unwrap(),
            utc(2026, 3, 29, 1, 30)
        );
    }

    // ---- padded_dates ----

    #[test]
    fn padded_dates_pads_two_days() {
        let w = Window {
            start: utc(2026, 9, 29, 8, 0),
            end: utc(2026, 9, 30, 9, 0),
            origin: WindowOrigin::Explicit,
        };
        assert_eq!(
            padded_dates(&w),
            (
                NaiveDate::from_ymd_opt(2026, 9, 27).unwrap(),
                NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()
            )
        );
    }

    #[test]
    fn padded_dates_uses_utc_date() {
        let w = Window {
            start: utc(2026, 9, 29, 0, 30),
            end: utc(2026, 9, 29, 5, 0),
            origin: WindowOrigin::Explicit,
        };
        assert_eq!(
            padded_dates(&w).0,
            NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
        );
    }

    // ---- format_window_line ----

    fn win(start: DateTime<Utc>, end: DateTime<Utc>, origin: WindowOrigin) -> Window {
        Window { start, end, origin }
    }

    fn tue_origin(previous: u32) -> WindowOrigin {
        WindowOrigin::Meeting {
            meeting: mm()[0],
            ended_at: lt(2026, 9, 29, 10, 0),
            previous,
        }
    }

    #[test]
    fn line_current_meeting() {
        let now = lt(2026, 9, 30, 11, 0);
        let w = win(lt(2026, 9, 29, 10, 0), now, tue_origin(0));
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: Tue 29 Sep 10:00 \u{2192} Wed 30 Sep 11:00  (1d 1h, since Tue 09:00\u{2013}10:00 meeting)"
        );
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: Tue 29 Sep 10:00 → Wed 30 Sep 11:00  (1d 1h, since Tue 09:00–10:00 meeting)"
        );
    }

    #[test]
    fn line_previous_origin() {
        let now = lt(2026, 9, 30, 11, 0);
        let w = win(
            lt(2026, 9, 24, 15, 0),
            lt(2026, 9, 29, 10, 0),
            tue_origin(1),
        );
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: Thu 24 Sep 15:00 → Tue 29 Sep 10:00  (4d 19h, previous ×1, since Tue 09:00–10:00 meeting)"
        );
    }

    #[test]
    fn line_explicit_range() {
        let now = lt(2026, 10, 2, 12, 0);
        let w = win(lt(2026, 9, 28, 8, 0), now, WindowOrigin::Explicit);
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: Mon 28 Sep 08:00 → Fri 02 Oct 12:00  (4d 4h, explicit range)"
        );
    }

    #[test]
    fn line_zero_length() {
        let now = lt(2026, 10, 1, 15, 0);
        let w = win(now, now, WindowOrigin::Explicit);
        assert!(format_window_line(&Berlin, &w, now).contains("(0h 0m, explicit range)"));
    }

    #[test]
    fn line_under_a_day() {
        let now = lt(2026, 9, 29, 10, 30);
        let w = win(lt(2026, 9, 29, 10, 0), now, tue_origin(0));
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: Tue 29 Sep 10:00 → Tue 29 Sep 10:30  (0h 30m, since Tue 09:00–10:00 meeting)"
        );
    }

    #[test]
    fn line_year_prefix_when_year_differs_from_now() {
        let now = lt(2027, 1, 5, 12, 0);
        let w = win(lt(2026, 12, 31, 15, 0), now, WindowOrigin::Explicit);
        assert_eq!(
            format_window_line(&Berlin, &w, now),
            "Window: 2026 Thu 31 Dec 15:00 → Tue 05 Jan 12:00  (4d 21h, explicit range)"
        );
    }
}
