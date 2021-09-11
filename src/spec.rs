use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

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
}

impl ConfigFile {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<ConfigFile> {
        let conf_file = File::open(path.as_ref())?;
        let buf_reader = BufReader::new(conf_file);
        let conf = serde_yaml::from_reader(buf_reader)?;
        Ok(conf)
    }
}
