use crate::config::ConfigError;
use crate::window::WindowError;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{service} rejected the token (401 Unauthorized) at {url} — check {hint}")]
    Unauthorized {
        service: &'static str,
        url: String,
        hint: &'static str,
    },
    #[error("{service}: access denied (403) for {url}{hint}")]
    Forbidden {
        service: &'static str,
        url: String,
        /// "" or "; <advice>"
        hint: String,
    },
    #[error("{service}: not found (404): {url}")]
    NotFound { service: &'static str, url: String },
    #[error("{service}: HTTP {status} for {url}: {body}")]
    Http {
        service: &'static str,
        status: u16,
        url: String,
        /// First 300 bytes, char-safe.
        body: String,
    },
    #[error("{service}: request to {url} failed: {}", root_cause(.source))]
    Network {
        service: &'static str,
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{service}: unexpected response from {url}: {source}")]
    Decode {
        service: &'static str,
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{service}: {message}")]
    Other {
        service: &'static str,
        message: String,
    },
}

impl ApiError {
    /// Some(401|403|404|status of Http); None for Network/Decode/Other.
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Unauthorized { .. } => Some(401),
            ApiError::Forbidden { .. } => Some(403),
            ApiError::NotFound { .. } => Some(404),
            ApiError::Http { status, .. } => Some(*status),
            ApiError::Network { .. } | ApiError::Decode { .. } | ApiError::Other { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub err: anyhow::Error,
}

impl CliError {
    /// Usage or configuration error (exit code 2).
    pub fn usage(err: impl Into<anyhow::Error>) -> Self {
        CliError {
            code: 2,
            err: err.into(),
        }
    }

    /// Runtime failure (exit code 1).
    pub fn runtime(err: impl Into<anyhow::Error>) -> Self {
        CliError {
            code: 1,
            err: err.into(),
        }
    }
}

impl From<ConfigError> for CliError {
    fn from(e: ConfigError) -> Self {
        CliError {
            code: e.exit_code(),
            err: e.into(),
        }
    }
}

impl From<WindowError> for CliError {
    fn from(e: WindowError) -> Self {
        CliError {
            code: 2,
            err: e.into(),
        }
    }
}

/// Innermost message of a `reqwest::Error`; its own Display only says
/// "error sending request for url (...)" and hides e.g. "Connection refused".
fn root_cause(err: &reqwest::Error) -> String {
    let mut cur: &dyn std::error::Error = err;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_values() {
        let u = || "http://x".to_string();
        assert_eq!(
            ApiError::Unauthorized {
                service: "GitLab",
                url: u(),
                hint: "h"
            }
            .status(),
            Some(401)
        );
        assert_eq!(
            ApiError::Forbidden {
                service: "GitLab",
                url: u(),
                hint: String::new()
            }
            .status(),
            Some(403)
        );
        assert_eq!(
            ApiError::NotFound {
                service: "GitLab",
                url: u()
            }
            .status(),
            Some(404)
        );
        assert_eq!(
            ApiError::Http {
                service: "GitLab",
                status: 500,
                url: u(),
                body: String::new()
            }
            .status(),
            Some(500)
        );
        assert_eq!(
            ApiError::Other {
                service: "GitLab",
                message: "m".into()
            }
            .status(),
            None
        );
    }

    #[test]
    fn messages() {
        let e = ApiError::Unauthorized {
            service: "YouTrack",
            url: "http://y/api".into(),
            hint: "[youtrack].token or YOUTRACK_TOKEN",
        };
        assert_eq!(
            e.to_string(),
            "YouTrack rejected the token (401 Unauthorized) at http://y/api — check [youtrack].token or YOUTRACK_TOKEN"
        );
        let e = ApiError::Forbidden {
            service: "GitLab",
            url: "http://g".into(),
            hint: "; nope".into(),
        };
        assert_eq!(
            e.to_string(),
            "GitLab: access denied (403) for http://g; nope"
        );
        let e = ApiError::NotFound {
            service: "GitLab",
            url: "http://g".into(),
        };
        assert_eq!(e.to_string(), "GitLab: not found (404): http://g");
        let e = ApiError::Http {
            service: "GitLab",
            status: 500,
            url: "http://g".into(),
            body: "boom".into(),
        };
        assert_eq!(e.to_string(), "GitLab: HTTP 500 for http://g: boom");
        let e = ApiError::Other {
            service: "GitLab",
            message: "user \"eric\" not found".into(),
        };
        assert_eq!(e.to_string(), "GitLab: user \"eric\" not found");
    }

    #[test]
    fn cli_error_codes() {
        assert_eq!(CliError::usage(anyhow::anyhow!("x")).code, 2);
        assert_eq!(CliError::runtime(anyhow::anyhow!("x")).code, 1);
        let e: CliError = ConfigError::NoConfigDir.into();
        assert_eq!(e.code, 2);
        let e: CliError = ConfigError::AlreadyExists("/x".into()).into();
        assert_eq!(e.code, 1);
        let e: CliError = WindowError::NoMeetings.into();
        assert_eq!(e.code, 2);
    }
}
