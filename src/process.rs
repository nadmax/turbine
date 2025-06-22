use crate::{Container, TurbineError, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use tokio::process::Child;

pub struct ProcessManager {
    running_processes: HashMap<String, Child>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            running_processes: HashMap::new(),
        }
    }

    pub async fn start_container(&mut self, container: &Container) -> Result<u32> {
        let std_cmd = self.build_command(container)?;
        let mut cmd = tokio::process::Command::from(std_cmd);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn()
            .map_err(|e| TurbineError::ProcessError(format!("Failed to spawn process: {}", e)))?;
        let pid = child.id().ok_or_else(|| {
            TurbineError::ProcessError("Failed to get process ID".to_string())
        })?;

        self.running_processes.insert(container.id.clone(), child);

        Ok(pid)
    }

    fn build_command(&self, container: &Container) -> Result<std::process::Command> {
        let mut cmd = std::process::Command::new("unshare");

        cmd.args(&[
            "--pid",
            "--net",
            "--mount",
            "--uts",
            "--ipc",
            "--fork",
        ]);

        if let Some(user) = &container.config.user {
            cmd.arg("--user");
            cmd.arg(user);
        }

        if let Some(uid) = container.config.uid {
            unsafe {
                cmd.pre_exec(move || {
                    use nix::unistd::{setuid, Uid};

                    setuid(Uid::from_raw(uid))
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                    Ok(())
                });
            }
        }

        if let Some(gid) = container.config.gid {
            unsafe {
                cmd.pre_exec(move || {
                    use nix::unistd::{setgid, Gid};

                    setgid(Gid::from_raw(gid))
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                    Ok(())
                });
            }
        }

        if let Some(ref groups) = container.config.groups {
            let groups_clone = groups.clone();

            unsafe {
                cmd.pre_exec(move || {
                    use nix::unistd::{setgroups, Gid};

                    let gids: Vec<Gid> = groups_clone
                    .iter()
                    .map(|&g| Gid::from_raw(g)).collect();

                    setgroups(&gids)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                    Ok(())
                });
            }
        }

        cmd.arg("chroot");
        cmd.arg(&container.root_path);

        if let Some(working_dir) = &container.config.working_dir {
            cmd.current_dir(working_dir);
        }

        for (key, value) in &container.config.environment {
            cmd.env(key, value);
        }

        cmd.args(&container.config.command);

        Ok(cmd)
    }



    pub async fn stop_container(&mut self, container_id: &str, force: bool) -> Result<()> {
        if let Some(mut child) = self.running_processes.remove(container_id) {
            if force {
                child.kill().await
                    .map_err(|e| TurbineError::ProcessError(format!("Failed to kill process: {}", e)))?;
            } else {
                if let Some(pid) = child.id() {
                    self.send_signal(pid, Signal::SIGTERM)?;

                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                                         child.wait()
                    ).await
                        .map_err(|_| TurbineError::ProcessError("Process did not terminate gracefully".to_string()))?
                        .map_err(|e| TurbineError::ProcessError(format!("Failed to wait for process: {}", e)))?;
                }
            }
        }

        Ok(())
    }

    pub async fn restart_container(&mut self, container: &Container) -> Result<u32> {
        self.stop_container(&container.id, false).await?;
        self.start_container(container).await
    }

    pub fn pause_container(&self, container_id: &str) -> Result<()> {
        if let Some(child) = self.running_processes.get(container_id) {
            if let Some(pid) = child.id() {
                self.send_signal(pid, Signal::SIGSTOP)?;
            }
        }

        Ok(())
    }

    pub fn resume_container(&self, container_id: &str) -> Result<()> {
        if let Some(child) = self.running_processes.get(container_id) {
            if let Some(pid) = child.id() {
                self.send_signal(pid, Signal::SIGCONT)?;
            }
        }

        Ok(())
    }

    fn send_signal(&self, pid: u32, signal: Signal) -> Result<()> {
        let nix_pid = Pid::from_raw(pid as i32);

        signal::kill(nix_pid, signal)
            .map_err(|e| TurbineError::ProcessError(format!("Failed to send signal: {}", e)))?;

        Ok(())
    }

    pub fn is_running(&self, container_id: &str) -> bool {
        if let Some(child) = self.running_processes.get(container_id) {
            child.id().is_some()
        } else {
            false
        }
    }

    pub async fn get_container_logs(&mut self, container_id: &str) -> Result<(String, String)> {
        if let Some(child) = self.running_processes.get_mut(container_id) {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let child = self.running_processes.remove(container_id).unwrap();
                    let output = child.wait_with_output().await
                        .map_err(|e| TurbineError::ProcessError(format!("Failed to get output: {}", e)))?;
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                    Ok((stdout, stderr))
                }
                Ok(None) => {
                    Err(TurbineError::ProcessError("Container is still running".to_string()))
                }
                Err(e) => {
                    Err(TurbineError::ProcessError(format!("Failed to check process status: {}", e)))
                }
            }
        } else {
            Err(TurbineError::ProcessError("Container not found".to_string()))
        }
    }

    pub async fn execute_in_container(&self, container: &Container, command: Vec<String>) -> Result<String> {
        let mut cmd = tokio::process::Command::new("nsenter");
        if let Some(child) = self.running_processes.get(&container.id) {
            if let Some(pid) = child.id() {
                cmd.args(&[
                    "--target", &pid.to_string(),
                     "--pid", "--net", "--mount", "--uts", "--ipc",
                ]);
            }
        }

        cmd.arg("chroot");
        cmd.arg(&container.root_path);
        cmd.args(&command);

        let output = cmd.output().await
            .map_err(|e| TurbineError::ProcessError(format!("Failed to execute command: {}", e)))?;
        if !output.status.success() {
            return Err(TurbineError::ProcessError(
                format!("Command failed: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn get_running_containers(&self) -> Vec<String> {
        self.running_processes.keys().cloned().collect()
    }

    pub async fn cleanup_all(&mut self) -> Result<()> {
        let container_ids: Vec<String> = self.running_processes.keys().cloned().collect();

        for container_id in container_ids {
            self.stop_container(&container_id, true).await?;
        }

        Ok(())
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}
