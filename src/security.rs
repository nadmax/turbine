use crate::{Container, TurbineError, Result};
use nix::sys::resource::{setrlimit, Resource};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use users::{get_user_by_name, get_group_by_name};

pub struct SecurityManager {
    allowed_users: Vec<String>,
    restricted_paths: Vec<String>,
}

impl SecurityManager {
    pub fn new() -> Self {
        Self {
            allowed_users: vec!["turbine".to_string()],
            restricted_paths: vec![
                "/etc/passwd".to_string(),
                "/etc/shadow".to_string(),
                "/etc/group".to_string(),
                "/proc".to_string(),
                "/sys".to_string(),
            ],
        }
    }

    pub fn validate_container_security(&self, container: &Container) -> Result<()> {
        self.validate_user(&container.config.user)?;
        self.validate_volumes(&container.config.volumes)?;
        self.validate_resource_limits(&container.config.resources)?;
        self.validate_network_security(container)?;
        
        Ok(())
    }

    fn validate_user(&self, user: &Option<String>) -> Result<()> {
        if let Some(username) = user {
            if username == "root" {
                return Err(TurbineError::SecurityError(
                    "Running containers as root is not allowed".to_string()
                ));
            }

            if !self.allowed_users.contains(username) {
                return Err(TurbineError::SecurityError(
                    format!("User '{}' is not allowed to run containers", username)
                ));
            }

            if get_user_by_name(username).is_none() {
                return Err(TurbineError::SecurityError(
                    format!("User '{}' does not exist", username)
                ));
            }
        }
        
        Ok(())
    }

    fn validate_volumes(&self, volumes: &[crate::VolumeMount]) -> Result<()> {
        for volume in volumes {
            let host_path_str = volume.host_path.to_string_lossy();
            
            for restricted in &self.restricted_paths {
                if host_path_str.starts_with(restricted) {
                    return Err(TurbineError::SecurityError(
                        format!("Access to path '{}' is restricted", host_path_str)
                    ));
                }
            }

            if !volume.readonly && self.is_system_path(&volume.host_path) {
                return Err(TurbineError::SecurityError(
                    format!("Write access to system path '{}' is not allowed", host_path_str)
                ));
            }

            self.validate_path_permissions(&volume.host_path)?;
        }
        
        Ok(())
    }

    fn validate_resource_limits(&self, resources: &crate::ResourceLimits) -> Result<()> {
        if let Some(memory) = resources.memory_mb {
            if memory > 4096 {
                return Err(TurbineError::SecurityError(
                    "Memory limit cannot exceed 4GB".to_string()
                ));
            }
        }

        if let Some(cpu) = resources.cpu_quota {
            if cpu > 2.0 {
                return Err(TurbineError::SecurityError(
                    "CPU quota cannot exceed 2.0".to_string()
                ));
            }
        }

        if let Some(processes) = resources.max_processes {
            if processes > 1024 {
                return Err(TurbineError::SecurityError(
                    "Process limit cannot exceed 1024".to_string()
                ));
            }
        }
        
        Ok(())
    }

    fn validate_network_security(&self, container: &Container) -> Result<()> {
        for port in &container.config.ports {
            if port.host_port < 1024 && port.host_port != 8080 {
                return Err(TurbineError::SecurityError(
                    format!("Privileged port {} is not allowed", port.host_port)
                ));
            }

            if port.container_port < 1024 && port.container_port != 8080 {
                return Err(TurbineError::SecurityError(
                    format!("Privileged container port {} is not allowed", port.container_port)
                ));
            }
        }
        
        Ok(())
    }

    fn is_system_path(&self, path: &Path) -> bool {
        let system_paths = ["/etc", "/usr", "/lib", "/bin", "/sbin", "/boot"];
        let path_str = path.to_string_lossy();
        
        system_paths.iter().any(|&sys_path| path_str.starts_with(sys_path))
    }

    fn validate_path_permissions(&self, path: &Path) -> Result<()> {
        let metadata = path.metadata()
            .map_err(|e| TurbineError::SecurityError(format!("Cannot access path {:?}: {}", path, e)))?;
        let permissions = metadata.permissions();
        let mode = permissions.mode();

        if mode & 0o002 != 0 {
            return Err(TurbineError::SecurityError(
                format!("Path {:?} is world-writable, which is not allowed", path)
            ));
        }
        
        Ok(())
    }

    pub fn apply_resource_limits(&self, resources: &crate::ResourceLimits) -> Result<()> {
        if let Some(memory_mb) = resources.memory_mb {
            let memory_bytes = (memory_mb * 1024 * 1024) as u64;
            setrlimit(Resource::RLIMIT_AS, memory_bytes, memory_bytes)
                .map_err(|e| TurbineError::SecurityError(format!("Failed to set memory limit: {}", e)))?;
        }

        if let Some(max_processes) = resources.max_processes {
            setrlimit(Resource::RLIMIT_NPROC, max_processes as u64, max_processes as u64)
                .map_err(|e| TurbineError::SecurityError(format!("Failed to set process limit: {}", e)))?;
        }

        if let Some(disk_mb) = resources.disk_mb {
            let disk_bytes = (disk_mb * 1024 * 1024) as u64;
            setrlimit(Resource::RLIMIT_FSIZE, disk_bytes, disk_bytes)
                .map_err(|e| TurbineError::SecurityError(format!("Failed to set disk limit: {}", e)))?;
        }
        
        Ok(())
    }

    pub fn setup_container_user(&self, container: &Container) -> Result<()> {
        if let Some(username) = &container.config.user {
            let user = get_user_by_name(username)
                .ok_or_else(|| TurbineError::SecurityError(format!("User '{}' not found", username)))?;

            nix::unistd::setuid(user.uid())
                .map_err(|e| TurbineError::SecurityError(format!("Failed to set UID: {}", e)))?;

            if let Some(group) = get_group_by_name(username) {
                nix::unistd::setgid(group.gid())
                    .map_err(|e| TurbineError::SecurityError(format!("Failed to set GID: {}", e)))?;
            }
        }
        
        Ok(())
    }

    pub fn create_secure_environment(&self, container: &Container) -> Result<()> {
        self.setup_container_user(container)?;
        self.apply_resource_limits(&container.config.resources)?;
        self.setup_secure_filesystem(container)?;
        
        Ok(())
    }

    fn setup_secure_filesystem(&self, container: &Container) -> Result<()> {
        use std::fs;
        
        let sensitive_dirs = ["proc", "sys", "dev"];
        
        for dir in &sensitive_dirs {
            let path = container.root_path.join(dir);
            if path.exists() {
                let permissions = fs::Permissions::from_mode(0o555);
                fs::set_permissions(&path, permissions)?;
            }
        }

        let tmp_path = container.root_path.join("tmp");
        if tmp_path.exists() {
            let permissions = fs::Permissions::from_mode(0o1777);
            fs::set_permissions(&tmp_path, permissions)?;
        }
        
        Ok(())
    }

    pub fn validate_image_security(&self, image_path: &str) -> Result<()> {
        if image_path.contains("..") {
            return Err(TurbineError::SecurityError(
                "Image path contains directory traversal".to_string()
            ));
        }

        if !image_path.starts_with("/") && !image_path.starts_with("./") {
            return Err(TurbineError::SecurityError(
                "Image path must be absolute or relative to current directory".to_string()
            ));
        }
        
        Ok(())
    }

    pub fn sanitize_environment(&self, env: &mut std::collections::HashMap<String, String>) -> Result<()> {
        let dangerous_vars = ["LD_PRELOAD", "LD_LIBRARY_PATH", "PATH"];
        
        for var in &dangerous_vars {
            if let Some(value) = env.get(*var) {
                if value.contains("..") || value.contains("/etc") || value.contains("/usr") {
                    env.remove(*var);
                }
            }
        }

        env.insert("TURBINE_CONTAINER".to_string(), "true".to_string());
        env.insert("HOME".to_string(), "/app".to_string());
        
        Ok(())
    }
}

impl Default for SecurityManager {
    fn default() -> Self {
        Self::new()
    }
}
