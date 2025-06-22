use crate::{ContainerConfig, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainerState {
    Created,
    Running,
    Stopped,
    Paused,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub config: ContainerConfig,
    pub state: ContainerState,
    pub pid: Option<u32>,
    pub root_path: PathBuf,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Container {
    pub fn new(config: ContainerConfig) -> Result<Self> {
        let id = Uuid::new_v4().to_string();
        let root_path = PathBuf::from(format!("/tmp/turbine/{}", id));

        Ok(Container {
            id,
            config,
            state: ContainerState::Created,
            pid: None,
            root_path,
            created_at: chrono::Utc::now(),
           started_at: None,
           stopped_at: None,
        })
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, ContainerState::Running)
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self.state, ContainerState::Stopped)
    }

    pub fn set_state(&mut self, state: ContainerState) {
        match state {
            ContainerState::Running => {
                self.started_at = Some(chrono::Utc::now());
                self.stopped_at = None;
            }
            ContainerState::Stopped => {
                self.stopped_at = Some(chrono::Utc::now());
                self.pid = None;
            }
            _ => {}
        }
        self.state = state;
    }

    pub fn set_pid(&mut self, pid: u32) {
        self.pid = Some(pid);
    }

    // Convenience methods for accessing user/group info
    pub fn get_user(&self) -> Option<&String> {
        self.config.user.as_ref()
    }

    pub fn get_uid(&self) -> Option<u32> {
        self.config.uid
    }

    pub fn get_gid(&self) -> Option<u32> {
        self.config.gid
    }

    pub fn get_groups(&self) -> Option<&Vec<u32>> {
        self.config.groups.as_ref()
    }
}

pub struct ContainerRegistry {
    containers: HashMap<String, Container>,
}

impl ContainerRegistry {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
        }
    }

    pub fn register(&mut self, container: Container) -> Result<()> {
        let id = container.id.clone();

        self.containers.insert(id, container);

        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Container> {
        self.containers.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Container> {
        self.containers.get_mut(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<Container> {
        self.containers.remove(id)
    }

    pub fn list(&self) -> Vec<&Container> {
        self.containers.values().collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Container> {
        self.containers.values().find(|c| c.config.name == name)
    }

    pub fn find_running(&self) -> Vec<&Container> {
        self.containers.values().filter(|c| c.is_running()).collect()
    }
}

impl Default for ContainerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
