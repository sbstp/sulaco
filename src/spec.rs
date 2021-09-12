use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::unix::Signal;

#[derive(Debug, Deserialize)]
pub struct ConfigFile {
    /// Map of service name to service spec.
    pub services: HashMap<Arc<String>, ServiceSpec>,
}

#[derive(Debug, Deserialize)]
pub struct ServiceSpec {
    /// Path to service binary
    pub cmd: String,

    /// Arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Additional environment variables for this service.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory of service
    pub workdir: Option<String>,

    /// Timeout before sending SIGKILL
    #[serde(default = "defaults::stop_timeout")]
    pub stop_timeout: Duration,

    /// Signal to send to terminate service
    #[serde(default = "defaults::stop_signal")]
    pub stop_signal: Signal,

    /// Restart service
    #[serde(default = "defaults::restart")]
    pub restart: bool,

    /// Delay before restarting service
    #[serde(default = "defaults::restart_delay")]
    pub restart_delay: Duration,
}

impl ConfigFile {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<ConfigFile> {
        let conf_file = File::open(path.as_ref())?;
        let buf_reader = BufReader::new(conf_file);
        let conf = serde_yaml::from_reader(buf_reader)?;
        Ok(conf)
    }
}

mod defaults {
    use std::time::Duration;

    use crate::unix::Signal;

    pub fn stop_timeout() -> Duration {
        Duration::from_secs(10)
    }

    pub fn stop_signal() -> Signal {
        Signal::Sigint
    }

    pub fn restart() -> bool {
        true
    }

    pub fn restart_delay() -> Duration {
        Duration::from_secs(1)
    }
}
