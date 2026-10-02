use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("GITLAB_TOKEN")
        .env_remove("YOUTRACK_TOKEN")
        .output()
        .expect("failed to run work-tracker")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn init_creates_config_then_refuses_then_force() {
    let home = TempDir::new().unwrap();
    let cfg = home.path().join("wt/config.toml");
    let cfg_s = cfg.to_str().unwrap();

    let out = run(&home, &["--config", cfg_s, "init"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(cfg.exists());
    #[cfg(unix)]
    assert_eq!(mode_of(&cfg), 0o600);
    let so = stdout(&out);
    assert!(so.contains("Created"), "{so}");
    assert!(so.contains(cfg_s), "{so}");
    assert!(so.contains("(mode 0600)"), "{so}");

    let out = run(&home, &["--config", cfg_s, "init"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));

    let out = run(&home, &["--config", cfg_s, "init", "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    #[cfg(unix)]
    assert_eq!(mode_of(&cfg), 0o600);
}

#[test]
fn init_uses_xdg_config_home_by_default() {
    let home = TempDir::new().unwrap();
    let out = run(&home, &["init"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(home.path().join("work-tracker/config.toml").exists());
}

#[test]
fn usage_error_exits_2() {
    let home = TempDir::new().unwrap();
    let out = run(&home, &["--plain", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}

fn run_berlin(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("TZ", "Europe/Berlin")
        .env_remove("GITLAB_TOKEN")
        .env_remove("YOUTRACK_TOKEN")
        .output()
        .expect("failed to run work-tracker")
}

fn init_cfg(home: &TempDir) -> std::path::PathBuf {
    let cfg = home.path().join("wt/config.toml");
    let out = run(home, &["--config", cfg.to_str().unwrap(), "init"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    cfg
}

#[test]
fn window_prints_three_lines_for_default_meetings() {
    let home = TempDir::new().unwrap();
    let cfg = init_cfg(&home);
    let out = run_berlin(
        &home,
        &[
            "window",
            "--config",
            cfg.to_str().unwrap(),
            "--now",
            "2026-09-30T11:00",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let so = stdout(&out);
    assert_eq!(so.lines().count(), 3, "{so}");
    assert!(
        so.contains("start: 2026-09-29T10:00:00+02:00 (2026-09-29T08:00:00Z)"),
        "{so}"
    );
    assert!(
        so.contains("end:   2026-09-30T11:00:00+02:00 (2026-09-30T09:00:00Z)"),
        "{so}"
    );
    assert!(
        so.starts_with(
            "Window: Tue 29 Sep 10:00 \u{2192} Wed 30 Sep 11:00  (1d 1h, since Tue 09:00\u{2013}10:00 meeting)"
        ),
        "{so}"
    );
}

#[test]
fn window_without_config_but_with_since_works() {
    let home = TempDir::new().unwrap();
    let missing = home.path().join("nope.toml");
    let out = run_berlin(
        &home,
        &[
            "window",
            "--config",
            missing.to_str().unwrap(),
            "--since",
            "2026-09-28T08:00",
            "--now",
            "2026-10-02T12:00",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("explicit range"), "{}", stdout(&out));
}

#[test]
fn window_without_config_and_without_since_exits_2() {
    let home = TempDir::new().unwrap();
    let missing = home.path().join("nope.toml");
    let out = run_berlin(&home, &["window", "--config", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("No config file"), "{}", stderr(&out));
}

#[test]
fn window_since_after_now_exits_2() {
    let home = TempDir::new().unwrap();
    let cfg = init_cfg(&home);
    let out = run_berlin(
        &home,
        &[
            "window",
            "--config",
            cfg.to_str().unwrap(),
            "--since",
            "2026-10-03",
            "--now",
            "2026-10-02T12:00",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("must be before now"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn report_with_template_tokens_exits_2_with_token_message() {
    let home = TempDir::new().unwrap();
    let cfg = init_cfg(&home);
    for extra in [&[][..], &["--plain"][..], &["--json"][..]] {
        let mut args = vec!["--config", cfg.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = run_berlin(&home, &args);
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("GitLab token missing"),
            "{}",
            stderr(&out)
        );
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    }
}

#[test]
fn report_without_any_source_exits_2() {
    let home = TempDir::new().unwrap();
    let cfg = init_cfg(&home);
    let out = run_berlin(
        &home,
        &[
            "--config",
            cfg.to_str().unwrap(),
            "--no-gitlab",
            "--no-youtrack",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("no data source"), "{}", stderr(&out));
}

#[test]
fn report_without_config_exits_2() {
    let home = TempDir::new().unwrap();
    let missing = home.path().join("nope.toml");
    let out = run_berlin(&home, &["--config", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("No config file"), "{}", stderr(&out));
}
