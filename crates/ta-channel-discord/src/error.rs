//! Discord channel errors.

/// Errors specific to the Discord channel implementation.
#[derive(Debug, thiserror::Error)]
pub enum DiscordChannelError {
    #[error("Discord API error (HTTP {status}): {message}")]
    ApiError { status: u16, message: String },

    #[error("environment variable '{var}' not set — set it to your Discord bot token")]
    MissingToken { var: String },

    #[error(
        "missing config field '{field}' — add it to your .ta/config.yaml discord channel config"
    )]
    MissingConfig { field: String },

    #[error("HTTP request to Discord API failed: {0}")]
    HttpError(String),

    #[error("failed to parse Discord API response: {0}")]
    ParseError(String),

    #[error("access denied: user '{user}' is not in allowed_users or allowed_roles")]
    AccessDenied { user: String },
}
