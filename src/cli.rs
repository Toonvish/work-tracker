use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "work-tracker", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Config file (default: $XDG_CONFIG_HOME/work-tracker/config.toml)
    #[arg(short = 'c', long = "config", global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Window start (overrides meetings)
    #[arg(long, global = true, value_name = "DATETIME")]
    pub since: Option<String>,

    /// Window end (default: now)
    #[arg(long, global = true, value_name = "DATETIME")]
    pub until: Option<String>,

    /// Pretend the current time is DATETIME (testing)
    #[arg(long, global = true, value_name = "DATETIME")]
    pub now: Option<String>,

    /// Shift back one meeting interval; repeat for more (-pp)
    #[arg(
        short = 'p',
        long = "previous",
        global = true,
        action = ArgAction::Count,
        conflicts_with = "since"
    )]
    pub previous: u8,

    /// Static table instead of the interactive list
    #[arg(long, conflicts_with = "json")]
    pub plain: bool,

    /// Dump merged data as JSON
    #[arg(long)]
    pub json: bool,

    /// Skip GitLab
    #[arg(long)]
    pub no_gitlab: bool,

    /// Skip YouTrack
    #[arg(long)]
    pub no_youtrack: bool,

    /// Log HTTP requests (method, URL, status, item count) to stderr
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a config template (mode 0600)
    Init {
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
    /// Print the resolved time window only
    Window,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn previous_counts() {
        let cli = Cli::try_parse_from(["work-tracker", "-pp"]).unwrap();
        assert_eq!(cli.previous, 2);
        let cli = Cli::try_parse_from(["work-tracker"]).unwrap();
        assert_eq!(cli.previous, 0);
    }

    #[test]
    fn previous_conflicts_with_since() {
        assert!(
            Cli::try_parse_from(["work-tracker", "--previous", "--since", "2026-09-28"]).is_err()
        );
    }

    #[test]
    fn plain_conflicts_with_json() {
        assert!(Cli::try_parse_from(["work-tracker", "--plain", "--json"]).is_err());
    }

    #[test]
    fn init_force_parses() {
        let cli = Cli::try_parse_from(["work-tracker", "init", "--force"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Init { force: true })));
        let cli = Cli::try_parse_from(["work-tracker", "init"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Init { force: false })));
    }

    #[test]
    fn global_args_after_subcommand() {
        let cli = Cli::try_parse_from([
            "work-tracker",
            "window",
            "--now",
            "2026-09-30T11:00",
            "-c",
            "/x",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Command::Window)));
        assert_eq!(cli.now.as_deref(), Some("2026-09-30T11:00"));
        assert_eq!(cli.config.as_deref(), Some(std::path::Path::new("/x")));
    }
}
