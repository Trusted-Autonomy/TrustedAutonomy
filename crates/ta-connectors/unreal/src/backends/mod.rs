pub mod flopperam;
pub mod kvick;
pub mod special_agent;

pub use flopperam::FlopperamBackend;
pub use kvick::KvickBackend;
pub use special_agent::SpecialAgentBackend;

use crate::{backend::UnrealBackend, config::UnrealConnectorConfig, error::UnrealConnectorError};

/// Instantiate the configured backend.
pub fn make_backend(
    config: &UnrealConnectorConfig,
) -> Result<Box<dyn UnrealBackend>, UnrealConnectorError> {
    match config.backend.as_str() {
        "kvick" => Ok(Box::new(KvickBackend::new(config))),
        "flopperam" => Ok(Box::new(FlopperamBackend::new(config))),
        "special-agent" => Ok(Box::new(SpecialAgentBackend::new(config))),
        other => Err(UnrealConnectorError::UnsupportedBackend(other.to_string())),
    }
}
