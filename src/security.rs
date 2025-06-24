use crate::{Container, TurbineError, Result};
use nix::sys::resource::{setrlimit, Resource};
use nix::unistd::{Uid, Gid};
use std::fs::{write, read_to_string};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct SecurityManager {
    allowed_users: Vec<String>,
    restricted_paths: Vec<String>,
    use_user_namespaces: bool,
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
            use_user_namespaces: true,
        }
    }

    pub fn new_rootless() -> Self {
        Self {
            allowed_users: vec![], // Empty means allow any user in rootless mode
            restricted_paths: vec![
                "/etc/passwd".to_string(),
                "/etc/shadow".to_string(),
                "/etc/group".to_string(),
                "/proc".to_string(),
                "/sys".to_string(),
            ],
            use_user_namespaces: true,
        }
    }

    pub fn validate_container_security(&self, container: &Container) -> Result<()> {
        if self.use_user_namespaces {
            self.validate_user_namespace_config(&container.config.user, container.config.uid, container.config.gid)?;
        } else {
            self.validate_user_traditional(&container.config.user)?;
        }

        self.validate_volumes(&container.config.volumes)?;
        self.validate_resource_limits(&container.config.resources)?;
        self.validate_network_security(container)?;

        Ok(())
    }

    fn validate_user_namespace_config(&self, user: &Option<String>, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
        // In user namespace mode, we're more permissive since we're isolated
        if let Some(username) = user {
            if username == "root" {
                // Root inside user namespace is fine - it's mapped to unprivileged user
                println!("Note: 'root' user will be mapped to current user via user namespace");
            }
        }

        // Validate UID/GID ranges for user namespace mapping
        if let Some(uid) = uid {
            if uid > 65535 {
                return Err(TurbineError::SecurityError(
                    "UID too large for user namespace mapping".to_string()
                ));
            }
        }

        if let Some(gid) = gid {
            if gid > 65535 {
                return Err(TurbineError::SecurityError(
                    "GID too large for user namespace mapping".to_string()
                ));
            }
        }

        Ok(())
    }

    fn validate_user_traditional(&self, user: &Option<String>) -> Result<()> {
        if let Some(username) = user {
            if username == "root" {
                return Err(TurbineError::SecurityError(
                    "Running containers as root is not allowed".to_string()
                ));
            }

            if !self.allowed_users.is_empty() && !self.allowed_users.contains(username) {
                return Err(TurbineError::SecurityError(
                    format!("User '{}' is not allowed to run containers", username)
                ));
            }
        }
        Ok(())
    }

    pub fn setup_user_namespace(&self, container: &Container) -> Result<()> {
        if !self.use_user_namespaces {
            return Ok(());
        }

        let current_uid = nix::unistd::getuid().as_raw();
        let current_gid = nix::unistd::getgid().as_raw();

        // Default container UID/GID if not specified
        let container_uid = container.config.uid.unwrap_or(0); // 0 = root inside container
        let container_gid = container.config.gid.unwrap_or(0);

        self.check_user_namespace_support()?;

        let flags = nix::sched::CloneFlags::CLONE_NEWUSER;

        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                self.setup_uid_gid_mappings(child, current_uid, current_gid, container_uid, container_gid)?;

                nix::sys::wait::waitpid(child, None)?;
                Ok(())
            }
            Ok(nix::unistd::ForkResult::Child) => {
                std::thread::sleep(std::time::Duration::from_millis(100));

                if container_uid != current_uid {
                    nix::unistd::setuid(Uid::from_raw(container_uid))
                    .map_err(|e| TurbineError::SecurityError(format!("Failed to set UID in namespace: {}", e)))?;
                }

                if container_gid != current_gid {
                    nix::unistd::setgid(Gid::from_raw(container_gid))
                    .map_err(|e| TurbineError::SecurityError(format!("Failed to set GID in namespace: {}", e)))?;
                }

                Ok(())
            }
            Err(e) => Err(TurbineError::SecurityError(format!("Failed to fork for user namespace: {}", e)))
        }
    }

    fn setup_uid_gid_mappings(&self, child_pid: nix::unistd::Pid, host_uid: u32, host_gid: u32, container_uid: u32, container_gid: u32) -> Result<()> {
        let pid = child_pid.as_raw();
        let uid_map = format!("{} {} 1", container_uid, host_uid);

        write(format!("/proc/{}/uid_map", pid), &uid_map)
            .map_err(|e| TurbineError::SecurityError(format!("Failed to write uid_map: {}", e)))?;
        write(format!("/proc/{}/setgroups", pid), "deny")
            .map_err(|e| TurbineError::SecurityError(format!("Failed to deny setgroups: {}", e)))?;

        let gid_map = format!("{} {} 1", container_gid, host_gid);

        write(format!("/proc/{}/gid_map", pid), &gid_map)
            .map_err(|e| TurbineError::SecurityError(format!("Failed to write gid_map: {}", e)))?;

        println!("User namespace mapping: container {}:{} -> host {}:{}",
                 container_uid, container_gid, host_uid, host_gid);

        Ok(())
    }

    fn check_user_namespace_support(&self) -> Result<()> {
        if Path::new("/proc/self/ns/user").exists() {
            if let Ok(content) = read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
                if content.trim() == "0" {
                    return Err(TurbineError::SecurityError(
                        "Unprivileged user namespaces are disabled. Enable with: echo 1 | sudo tee /proc/sys/kernel/unprivileged_userns_clone".to_string()
                    ));
                }
            }

            let current_user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());

            if let Ok(subuid) = read_to_string("/etc/subuid") {
                if !subuid.contains(&current_user) {
                    println!("Warning: No subordinate UIDs found for user {}. Consider adding entries to /etc/subuid", current_user);
                }
            }

            if let Ok(subgid) = read_to_string("/etc/subgid") {
                if !subgid.contains(&current_user) {
                    println!("Warning: No subordinate GIDs found for user {}. Consider adding entries to /etc/subgid", current_user);
                }
            }

            Ok(())
        } else {
            Err(TurbineError::SecurityError(
                "User namespaces not supported by kernel".to_string()
            ))
        }
    }

    pub fn create_rootless_environment(&self, container: &Container) -> Result<()> {
        self.setup_user_namespace(container)?;
        self.apply_resource_limits(&container.config.resources)?;
        self.setup_rootless_filesystem(container)?;

        Ok(())
    }

    fn setup_rootless_filesystem(&self, container: &Container) -> Result<()> {
        use std::fs;

        let dirs_to_create = ["proc", "sys", "dev", "tmp"];

        for dir in &dirs_to_create {
            let path = container.root_path.join(dir);
            if !path.exists() {
                fs::create_dir_all(&path)?;
            }

            let permissions = match *dir {
                "tmp" => fs::Permissions::from_mode(0o1777),
                "proc" | "sys" => fs::Permissions::from_mode(0o555),
                "dev" => fs::Permissions::from_mode(0o755),
                _ => fs::Permissions::from_mode(0o755),
            };

            fs::set_permissions(&path, permissions)?;
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

            if !volume.host_path.exists() {
                return Err(TurbineError::SecurityError(
                    format!("Host path '{}' does not exist", host_path_str)
                ));
            }

            let metadata = volume.host_path.metadata()
            .map_err(|e| TurbineError::SecurityError(format!("Cannot access path '{}': {}", host_path_str, e)))?;

            if !volume.readonly {
                if !self.can_write_to_path(&volume.host_path)? {
                    return Err(TurbineError::SecurityError(
                        format!("No write access to path '{}'", host_path_str)
                    ));
                }
            }
        }

        Ok(())
    }

    fn can_write_to_path(&self, path: &Path) -> Result<bool> {
        let test_file = path.join(".turbine_write_test");
        match std::fs::write(&test_file, b"test") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                Ok(true)
            }
            Err(_) => Ok(false)
        }
    }

    fn validate_resource_limits(&self, resources: &crate::ResourceLimits) -> Result<()> {
        if let Some(memory) = resources.memory_mb {
            if memory > 2048 {
                return Err(TurbineError::SecurityError(
                    "Memory limit cannot exceed 2GB in rootless mode".to_string()
                ));
            }
        }

        if let Some(cpu) = resources.cpu_quota {
            if cpu > 1.0 {
                return Err(TurbineError::SecurityError(
                    "CPU quota cannot exceed 1.0 in rootless mode".to_string()
                ));
            }
        }

        if let Some(processes) = resources.max_processes {
            if processes > 512 {
                return Err(TurbineError::SecurityError(
                    "Process limit cannot exceed 512 in rootless mode".to_string()
                ));
            }
        }

        Ok(())
    }

    fn validate_network_security(&self, container: &Container) -> Result<()> {
        for port in &container.config.ports {
            if port.host_port < 1024 {
                return Err(TurbineError::SecurityError(
                    format!("Privileged port {} is not allowed in rootless mode", port.host_port)
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
        let dangerous_vars = ["LD_PRELOAD", "LD_LIBRARY_PATH"];

        for var in &dangerous_vars {
            if let Some(value) = env.get(*var) {
                if value.contains("..") || value.contains("/etc") || value.contains("/usr") {
                    env.remove(*var);
                }
            }
        }

        env.insert("TURBINE_CONTAINER".to_string(), "true".to_string());
        env.insert("TURBINE_ROOTLESS".to_string(), "true".to_string());
        env.insert("HOME".to_string(), "/app".to_string());

        Ok(())
    }
}

impl Default for SecurityManager {
    fn default() -> Self {
        Self::new_rootless()
    }
}
