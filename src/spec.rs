use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::unix::Signal;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum OnExit {
    Restart {
        #[serde(default = "defaults::restart_delay")]
        restart_delay: u64,
    },
    Shutdown,
}

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

    #[serde(default = "defaults::on_exit")]
    pub on_exit: OnExit,
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

    use super::OnExit;

    pub fn stop_timeout() -> Duration {
        Duration::from_secs(10)
    }

    pub fn stop_signal() -> Signal {
        Signal::Sigint
    }

    pub fn on_exit() -> OnExit {
        OnExit::Restart {
            restart_delay: restart_delay(),
        }
    }

    pub fn restart_delay() -> u64 {
        0
    }
}
