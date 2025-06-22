use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub name: String,
    pub image: String,
    pub command: Vec<String>,
    pub working_dir: Option<String>,
    pub environment: HashMap<String, String>,
    pub ports: Vec<PortMapping>,
    pub volumes: Vec<VolumeMount>,
    pub resources: ResourceLimits,
    pub network: NetworkConfig,
    pub user: Option<String>,
    pub restart_policy: RestartPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub memory_mb: Option<u64>,
    pub cpu_quota: Option<f64>,
    pub disk_mb: Option<u64>,
    pub max_processes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bridge: Option<String>,
    pub dns: Vec<String>,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure,
    UnlessStopped,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            image: String::new(),
            command: vec!["/bin/sh".to_string()],
            working_dir: Some("/app".to_string()),
            environment: HashMap::new(),
            ports: Vec::new(),
            volumes: Vec::new(),
            resources: ResourceLimits::default(),
            network: NetworkConfig::default(),
            user: None,
            restart_policy: RestartPolicy::Never,
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory_mb: Some(512),
            cpu_quota: Some(1.0),
            disk_mb: Some(1024),
            max_processes: Some(256),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bridge: None,
            dns: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
            hostname: None,
        }
    }
}

impl ContainerConfig {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: ContainerConfig = serde_json::from_str(&content)?;

        Ok(config)
    }

    pub fn to_file(&self, path: &str) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(self)?;

        std::fs::write(path, content)?;

        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.name.is_empty() {
            return Err(anyhow::anyhow!("Container name cannot be empty"));
        }

        if self.image.is_empty() {
            return Err(anyhow::anyhow!("Container image cannot be empty"));
        }

        for port in &self.ports {
            if port.host_port == 0 || port.container_port == 0 {
                return Err(anyhow::anyhow!("Invalid port mapping"));
            }
        }

        for volume in &self.volumes {
            if !volume.host_path.exists() {
                return Err(anyhow::anyhow!("Host path does not exist: {:?}", volume.host_path));
            }
        }

        Ok(())
    }

    pub fn set_web_defaults(&mut self, port: u16) {
        self.ports.push(PortMapping {
            host_port: port,
            container_port: 8080,
            protocol: "tcp".to_string(),
        });

        self.environment.insert("PORT".to_string(), "8080".to_string());
        self.environment.insert("NODE_ENV".to_string(), "production".to_string());        
        self.restart_policy = RestartPolicy::Always;
        if self.resources.memory_mb.is_none() {
            self.resources.memory_mb = Some(256);
        }
        
        if self.resources.cpu_quota.is_none() {
            self.resources.cpu_quota = Some(0.5);
        }
    }
}

