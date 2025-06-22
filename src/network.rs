use crate::{Container, TurbineError, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;

pub struct NetworkManager {
    bridge_name: String,
    subnet: Ipv4Addr,
    allocated_ips: HashMap<String, Ipv4Addr>,
    port_mappings: HashMap<u16, String>,
}

impl NetworkManager {
    pub fn new(bridge_name: String) -> Self {
        Self {
            bridge_name,
            subnet: Ipv4Addr::new(172, 17, 0, 0),
            allocated_ips: HashMap::new(),
            port_mappings: HashMap::new(),
        }
    }

    pub fn setup_bridge(&self) -> Result<()> {
        if !self.bridge_exists()? {
            self.create_bridge()?;
            self.configure_bridge()?;
        }

        Ok(())
    }

    fn bridge_exists(&self) -> Result<bool> {
        let output = Command::new("ip")
            .args(&["link", "show", &self.bridge_name])
            .output()?;
        
        Ok(output.status.success())
    }

    fn create_bridge(&self) -> Result<()> {
        let output = Command::new("ip")
            .args(&["link", "add", "name", &self.bridge_name, "type", "bridge"])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to create bridge: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        let output = Command::new("ip")
            .args(&["link", "set", "dev", &self.bridge_name, "up"])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to bring up bridge: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn configure_bridge(&self) -> Result<()> {
        let bridge_ip = format!("{}/24", Ipv4Addr::new(172, 17, 0, 1));
        let output = Command::new("ip")
            .args(&["addr", "add", &bridge_ip, "dev", &self.bridge_name])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to configure bridge IP: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    pub fn setup_container_network(&mut self, container: &Container) -> Result<()> {
        let container_ip = self.allocate_ip(&container.id)?;        
        let veth_host = format!("veth-{}", &container.id[..8]);
        let veth_container = format!("veth-c-{}", &container.id[..8]);

        self.create_veth_pair(&veth_host, &veth_container)?;
        self.attach_to_bridge(&veth_host)?;
        self.configure_container_interface(&veth_container, container_ip)?;

        for port in &container.config.ports {
            if self.port_mappings.contains_key(&port.host_port) {
                return Err(TurbineError::NetworkError(
                    format!("Port {} is already in use", port.host_port)
                ));
            }
            
            self.setup_port_forwarding(port.host_port, container_ip, port.container_port)?;
            self.port_mappings.insert(port.host_port, container.id.clone());
        }

        Ok(())
    }

    fn allocate_ip(&mut self, container_id: &str) -> Result<Ipv4Addr> {
        let mut ip_suffix = 2u8;

        loop {
            let ip = Ipv4Addr::new(172, 17, 0, ip_suffix);            
            if !self.allocated_ips.values().any(|&allocated_ip| allocated_ip == ip) {
                self.allocated_ips.insert(container_id.to_string(), ip);
                return Ok(ip);
            }
            
            ip_suffix += 1;
            if ip_suffix == 255 {
                return Err(TurbineError::NetworkError(
                    "No available IP addresses in subnet".to_string()
                ));
            }
        }
    }

    fn create_veth_pair(&self, host_name: &str, container_name: &str) -> Result<()> {
        let output = Command::new("ip")
            .args(&["link", "add", host_name, "type", "veth", "peer", "name", container_name])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to create veth pair: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn attach_to_bridge(&self, interface: &str) -> Result<()> {
        let output = Command::new("ip")
            .args(&["link", "set", interface, "master", &self.bridge_name])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to attach to bridge: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        let output = Command::new("ip")
            .args(&["link", "set", "dev", interface, "up"])
            .output()?;

        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to bring up interface: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn configure_container_interface(&self, interface: &str, ip: Ipv4Addr) -> Result<()> {
        let ip_with_mask = format!("{}/24", ip);
        let output = Command::new("ip")
            .args(&["addr", "add", &ip_with_mask, "dev", interface])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to configure container interface: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn setup_port_forwarding(&self, host_port: u16, container_ip: Ipv4Addr, container_port: u16) -> Result<()> {
        let rule = format!(
            "DNAT --to-destination {}:{}",
            container_ip, container_port
        );
        let output = Command::new("iptables")
            .args(&[
                "-t", "nat",
                "-A", "PREROUTING",
                "-p", "tcp",
                "--dport", &host_port.to_string(),
                "-j", &rule
            ])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to setup port forwarding: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    pub fn cleanup_container_network(&mut self, container: &Container) -> Result<()> {
        let veth_host = format!("veth-{}", &container.id[..8]);        
        let _ = Command::new("ip")
            .args(&["link", "del", &veth_host])
            .output();

        for port in &container.config.ports {
            self.cleanup_port_forwarding(port.host_port)?;
            self.port_mappings.remove(&port.host_port);
        }

        self.allocated_ips.remove(&container.id);
        
        Ok(())
    }

    fn cleanup_port_forwarding(&self, host_port: u16) -> Result<()> {
        let output = Command::new("iptables")
            .args(&[
                "-t", "nat",
                "-D", "PREROUTING",
                "-p", "tcp",
                "--dport", &host_port.to_string(),
                "-j", "DNAT"
            ])
            .output()?;
        if !output.status.success() {
            eprintln!("Warning: Failed to cleanup port forwarding for port {}: {}", 
                host_port, String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    pub fn cleanup_bridge(&self) -> Result<()> {
        if self.bridge_exists()? {
            let output = Command::new("ip")
                .args(&["link", "del", &self.bridge_name])
                .output()?;
            if !output.status.success() {
                return Err(TurbineError::NetworkError(
                    format!("Failed to cleanup bridge: {}", String::from_utf8_lossy(&output.stderr))
                ));
            }
        }
        
        Ok(())
    }
}

