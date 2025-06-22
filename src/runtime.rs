use crate::{
    Container, ContainerConfig, ContainerRegistry, ContainerState,
    TurbineError, Result, 
    filesystem::FilesystemManager,
    network::NetworkManager,
    process::ProcessManager,
    security::SecurityManager,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct TurbineRuntime {
    registry: Arc<RwLock<ContainerRegistry>>,
    filesystem: FilesystemManager,
    network: Arc<RwLock<NetworkManager>>,
    process: Arc<RwLock<ProcessManager>>,
    security: SecurityManager,
    base_path: PathBuf,
}

impl TurbineRuntime {
    pub fn new<P: AsRef<std::path::Path>>(base_path: P) -> Self {
        let base_path = base_path.as_ref().to_path_buf();

        Self {
            registry: Arc::new(RwLock::new(ContainerRegistry::new())),
            filesystem: FilesystemManager::new(&base_path),
            network: Arc::new(RwLock::new(NetworkManager::new("turbine0".to_string()))),
            process: Arc::new(RwLock::new(ProcessManager::new())),
            security: SecurityManager::new(),
            base_path,
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        std::fs::create_dir_all(&self.base_path)?;

        let network = self.network.read().await;

        network.setup_bridge()?;

        Ok(())
    }

    pub async fn create_container(&self, mut config: ContainerConfig) -> Result<String> {
        self.security.validate_container_security(&Container::new(config.clone())?)?;
        self.security.sanitize_environment(&mut config.environment)?;
        self.security.validate_image_security(&config.image)?;

        config.validate()?;

        let container = Container::new(config)?;
        let container_id = container.id.clone();

        self.filesystem.create_container_root(&container)?;
        self.filesystem.setup_volumes(&container)?;
        self.filesystem.create_working_directory(&container)?;

        let mut network = self.network.write().await;

        network.setup_container_network(&container)?;
        drop(network);

        let mut registry = self.registry.write().await;

        registry.register(container)?;

        Ok(container_id)
    }

    pub async fn start_container(&self, container_id: &str) -> Result<()> {
        let mut registry = self.registry.write().await;
        let container = registry.get_mut(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if container.is_running() {
            return Err(TurbineError::ContainerError("Container is already running".to_string()));
        }

        self.security.create_secure_environment(container)?;
 
        let mut process = self.process.write().await;
        let pid = process.start_container(container).await?;

        container.set_pid(pid);
        container.set_state(ContainerState::Running);

        Ok(())
    }

    pub async fn stop_container(&self, container_id: &str, force: bool) -> Result<()> {
        let mut registry = self.registry.write().await;
        let container = registry.get_mut(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !container.is_running() {
            return Err(TurbineError::ContainerError("Container is not running".to_string()));
        }

        let mut process = self.process.write().await;

        process.stop_container(container_id, force).await?;
        container.set_state(ContainerState::Stopped);

        Ok(())
    }

    pub async fn restart_container(&self, container_id: &str) -> Result<()> {
        self.stop_container(container_id, false).await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        self.start_container(container_id).await
    }

    pub async fn pause_container(&self, container_id: &str) -> Result<()> {
        let mut registry = self.registry.write().await;
        let container = registry.get_mut(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !container.is_running() {
            return Err(TurbineError::ContainerError("Container is not running".to_string()));
        }

        let process = self.process.read().await;

        process.pause_container(container_id)?;
        container.set_state(ContainerState::Paused);

        Ok(())
    }

    pub async fn resume_container(&self, container_id: &str) -> Result<()> {
        let mut registry = self.registry.write().await;
        let container = registry.get_mut(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !matches!(container.state, ContainerState::Paused) {
            return Err(TurbineError::ContainerError("Container is not paused".to_string()));
        }

        let process = self.process.read().await;

        process.resume_container(container_id)?;
        container.set_state(ContainerState::Running);

        Ok(())
    }

    pub async fn remove_container(&self, container_id: &str, force: bool) -> Result<()> {
        let registry_read = self.registry.read().await;
        let container = registry_read.get(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if container.is_running() && !force {
            return Err(TurbineError::ContainerError(
                "Container is running. Use force=true to remove running container".to_string()
            ));
        }

        let container_clone = container.clone();

        drop(registry_read);

        if container_clone.is_running() {
            self.stop_container(container_id, true).await?;
        }

        let mut network = self.network.write().await;

        network.cleanup_container_network(&container_clone)?;
        drop(network);

        self.filesystem.cleanup_container(&container_clone)?;

        let mut registry = self.registry.write().await;

        registry.remove(container_id);

        Ok(())
    }

    pub async fn list_containers(&self) -> Result<Vec<Container>> {
        let registry = self.registry.read().await;

        Ok(registry.list().into_iter().cloned().collect())
    }

    pub async fn get_container(&self, container_id: &str) -> Result<Container> {
        let registry = self.registry.read().await;

        registry.get(container_id)
            .cloned()
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))
    }

    pub async fn get_container_logs(&self, container_id: &str) -> Result<(String, String)> {
        let registry = self.registry.read().await;
        let container = registry.get(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !container.is_running() {
            return Err(TurbineError::ContainerError("Container is not running".to_string()));
        }
        
        drop(registry);

        let mut process = self.process.write().await;

        process.get_container_logs(container_id).await
    }

    pub async fn execute_in_container(&self, container_id: &str, command: Vec<String>) -> Result<String> {
        let registry = self.registry.read().await;
        let container = registry.get(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !container.is_running() {
            return Err(TurbineError::ContainerError("Container is not running".to_string()));
        }

        let container_clone = container.clone();

        drop(registry);

        let process = self.process.read().await;

        process.execute_in_container(&container_clone, command).await
    }

    pub async fn get_running_containers(&self) -> Result<Vec<String>> {
        let process = self.process.read().await;

        Ok(process.get_running_containers())
    }

    pub async fn create_web_container(&self, name: String, image: String, port: u16) -> Result<String> {
        let mut config = ContainerConfig::default();

        config.name = name;
        config.image = image;
        config.set_web_defaults(port);

        self.create_container(config).await
    }

    pub async fn deploy_web_app(&self, name: String, image: String, port: u16) -> Result<String> {
        let container_id = self.create_web_container(name, image, port).await?;

        self.start_container(&container_id).await?;

        Ok(container_id)
    }

    pub async fn get_container_stats(&self, container_id: &str) -> Result<ContainerStats> {
        let registry = self.registry.read().await;
        let container = registry.get(container_id)
            .ok_or_else(|| TurbineError::ContainerError("Container not found".to_string()))?;
        if !container.is_running() {
            return Err(TurbineError::ContainerError("Container is not running".to_string()));
        }

        let stats = ContainerStats {
            container_id: container_id.to_string(),
            memory_usage: self.get_memory_usage(container.pid.unwrap_or(0)).await?,
            cpu_usage: self.get_cpu_usage(container.pid.unwrap_or(0)).await?,
            network_rx: 0,
            network_tx: 0,
            uptime: container.started_at
                .map(|start| chrono::Utc::now().signed_duration_since(start).num_seconds())
                .unwrap_or(0),
        };

        Ok(stats)
    }

    async fn get_memory_usage(&self, pid: u32) -> Result<u64> {
        let stat_path = format!("/proc/{}/status", pid);
        let content = tokio::fs::read_to_string(&stat_path).await?;
        
        for line in content.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    return Ok(parts[1].parse::<u64>().unwrap_or(0) * 1024);
                }
            }
        }
        
        Ok(0)
    }

    async fn get_cpu_usage(&self, pid: u32) -> Result<f64> {
        let stat_path = format!("/proc/{}/stat", pid);
        let content = tokio::fs::read_to_string(&stat_path).await?;        
        let parts: Vec<&str> = content.split_whitespace().collect();
        if parts.len() >= 15 {
            let utime: u64 = parts[13].parse().unwrap_or(0);
            let stime: u64 = parts[14].parse().unwrap_or(0);
            let total_time = utime + stime;
            
            return Ok(total_time as f64 / 100.0);
        }
        
        Ok(0.0)
    }

    pub async fn cleanup(&self) -> Result<()> {
        let mut process = self.process.write().await;

        process.cleanup_all().await?;
        drop(process);

        let containers = self.list_containers().await?;

        for container in containers {
            if container.is_running() {
                self.remove_container(&container.id, true).await?;
            } else {
                self.filesystem.cleanup_container(&container)?;
            }
        }

        let network = self.network.read().await;

        network.cleanup_bridge()?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ContainerStats {
    pub container_id: String,
    pub memory_usage: u64,
    pub cpu_usage: f64,
    pub network_rx: u64,
    pub network_tx: u64,
    pub uptime: i64,
}
