use crate::{Container, TurbineError, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;

pub struct FilesystemManager {
    base_path: PathBuf,
}

impl FilesystemManager {
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf(),
        }
    }

    pub fn create_container_root(&self, container: &Container) -> Result<()> {
        let root_path = &container.root_path;        
        if root_path.exists() {
            return Err(TurbineError::FilesystemError(
                format!("Container root already exists: {:?}", root_path)
            ));
        }

        fs::create_dir_all(root_path)?;
        
        let subdirs = ["bin", "etc", "lib", "tmp", "var", "proc", "sys", "dev", "app"];
        for subdir in &subdirs {
            let path = root_path.join(subdir);
            fs::create_dir_all(&path)?;
        }

        self.setup_basic_files(root_path)?;
        
        Ok(())
    }

    pub fn setup_volumes(&self, container: &Container) -> Result<()> {
        for volume in &container.config.volumes {
            if !volume.host_path.exists() {
                return Err(TurbineError::FilesystemError(
                    format!("Host path does not exist: {:?}", volume.host_path)
                ));
            }

            let target_path = container.root_path.join(
                volume.container_path.strip_prefix("/").unwrap_or(&volume.container_path)
            );
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            if volume.host_path.is_dir() {
                fs::create_dir_all(&target_path)?;
                self.bind_mount(&volume.host_path, &target_path, volume.readonly)?;
            } else {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                self.bind_mount(&volume.host_path, &target_path, volume.readonly)?;
            }
        }
        
        Ok(())
    }

    fn bind_mount(&self, source: &Path, target: &Path, readonly: bool) -> Result<()> {
        use std::process::Command;
        
        let mut cmd = Command::new("mount");
        cmd.arg("--bind")
           .arg(source)
           .arg(target);
        if readonly {
            cmd.arg("-o").arg("ro");
        }

        let output = cmd.output()?;        
        if !output.status.success() {
            return Err(TurbineError::FilesystemError(
                format!("Failed to bind mount {:?} to {:?}: {}", 
                    source, target, String::from_utf8_lossy(&output.stderr))
            ));
        }
        
        Ok(())
    }

    fn setup_basic_files(&self, root_path: &Path) -> Result<()> {
        let resolv_conf = root_path.join("etc/resolv.conf");
        fs::write(&resolv_conf, "nameserver 8.8.8.8\nnameserver 8.8.4.4\n")?;
        
        let passwd = root_path.join("etc/passwd");
        fs::write(&passwd, "turbine:x:1000:1000:Turbine User:/app:/bin/sh\n")?;
        
        let group = root_path.join("etc/group");
        fs::write(&group, "turbine:x:1000:\n")?;
        
        let hosts = root_path.join("etc/hosts");
        fs::write(&hosts, "127.0.0.1 localhost\n::1 localhost\n")?;
        
        Ok(())
    }

    pub fn cleanup_container(&self, container: &Container) -> Result<()> {
        if container.root_path.exists() {
            self.unmount_volumes(container)?;
            fs::remove_dir_all(&container.root_path)?;
        }

        Ok(())
    }

    fn unmount_volumes(&self, container: &Container) -> Result<()> {
        use std::process::Command;
        
        for volume in &container.config.volumes {
            let target_path = container.root_path.join(
                volume.container_path.strip_prefix("/").unwrap_or(&volume.container_path)
            );
            
            if target_path.exists() {
                let output = Command::new("umount")
                    .arg(&target_path)
                    .output()?;                    
                if !output.status.success() {
                    eprintln!("Warning: Failed to unmount {:?}: {}", 
                        target_path, String::from_utf8_lossy(&output.stderr));
                }
            }
        }
        
        Ok(())
    }

    pub fn create_working_directory(&self, container: &Container) -> Result<()> {
        if let Some(working_dir) = &container.config.working_dir {
            let work_path = container.root_path.join(
                working_dir.strip_prefix("/").unwrap_or(working_dir)
            );
            fs::create_dir_all(&work_path)?;
            
            let permissions = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&work_path, permissions)?;
        }
        
        Ok(())
    }
}

