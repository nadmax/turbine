use crate::{Container, TurbineError, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Command;

#[derive(Debug, Clone)]
pub enum NetworkConfig {
    IPv4 { subnet: Ipv4Addr, prefix: u8 },
    IPv6 { subnet: Ipv6Addr, prefix: u8 },
    DualStack {
        ipv4_subnet: Ipv4Addr,
        ipv4_prefix: u8,
        ipv6_subnet: Ipv6Addr,
        ipv6_prefix: u8
    },
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig::IPv4 {
            subnet: Ipv4Addr::new(172, 17, 0, 0),
            prefix: 24,
        }
    }
}

pub struct NetworkManager {
    bridge_name: String,
    network_config: NetworkConfig,
    allocated_ips: HashMap<String, Vec<IpAddr>>,
    port_mappings: HashMap<u16, String>,
}

impl NetworkManager {
    pub fn new(bridge_name: String) -> Self {
        Self {
            bridge_name,
            network_config: NetworkConfig::default(),
            allocated_ips: HashMap::new(),
            port_mappings: HashMap::new(),
        }
    }

    pub fn with_config(bridge_name: String, config: NetworkConfig) -> Self {
        Self {
            bridge_name,
            network_config: config,
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
        match &self.network_config {
            NetworkConfig::IPv4 { subnet, prefix } => {
                let octets = subnet.octets();
                let bridge_ip = format!("{}.{}.{}.1/{}", octets[0], octets[1], octets[2], prefix);
                self.add_bridge_address(&bridge_ip)?;
            }
            NetworkConfig::IPv6 { subnet, prefix } => {
                let segments = subnet.segments();
                let bridge_ip = format!("{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:1/{}",
                                        segments[0], segments[1], segments[2], segments[3],
                                        segments[4], segments[5], segments[6], prefix);
                self.add_bridge_address(&bridge_ip)?;
            }
            NetworkConfig::DualStack { ipv4_subnet, ipv4_prefix, ipv6_subnet, ipv6_prefix } => {
                let octets = ipv4_subnet.octets();
                let ipv4_bridge_ip = format!("{}.{}.{}.1/{}", octets[0], octets[1], octets[2], ipv4_prefix);
                self.add_bridge_address(&ipv4_bridge_ip)?;

                let segments = ipv6_subnet.segments();
                let ipv6_bridge_ip = format!("{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:1/{}",
                                             segments[0], segments[1], segments[2], segments[3],
                                             segments[4], segments[5], segments[6], ipv6_prefix);
                self.add_bridge_address(&ipv6_bridge_ip)?;
            }
        }

        Ok(())
    }

    fn add_bridge_address(&self, ip_with_prefix: &str) -> Result<()> {
        let output = Command::new("ip")
            .args(&["addr", "add", ip_with_prefix, "dev", &self.bridge_name])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to configure bridge IP {}: {}",
                    ip_with_prefix, String::from_utf8_lossy(&output.stderr))
            ));
        }
        Ok(())
    }

    pub fn setup_container_network(&mut self, container: &Container) -> Result<()> {
        let container_ips = self.allocate_ips(&container.id)?;
        let veth_host = format!("veth-{}", &container.id[..8]);
        let veth_container = format!("veth-c-{}", &container.id[..8]);

        self.create_veth_pair(&veth_host, &veth_container)?;
        self.attach_to_bridge(&veth_host)?;

        for ip in &container_ips {
            self.configure_container_interface(&veth_container, *ip)?;
        }

        for port in &container.config.ports {
            if self.port_mappings.contains_key(&port.host_port) {
                return Err(TurbineError::NetworkError(
                    format!("Port {} is already in use", port.host_port)
                ));
            }

            let target_ip = container_ips.iter()
                .find(|ip| ip.is_ipv4())
                .or_else(|| container_ips.first())
                .ok_or_else(|| TurbineError::NetworkError("No IP allocated for container".to_string()))?;

            self.setup_port_forwarding(port.host_port, *target_ip, port.container_port)?;
            self.port_mappings.insert(port.host_port, container.id.clone());
        }

        Ok(())
    }

    fn allocate_ips(&mut self, container_id: &str) -> Result<Vec<IpAddr>> {
        if let Some(existing_ips) = self.allocated_ips.get(container_id) {
            return Ok(existing_ips.clone());
        }

        let mut ips = Vec::new();

        match &self.network_config {
            NetworkConfig::IPv4 { subnet, .. } => {
                let ipv4 = self.allocate_ipv4(container_id, *subnet)?;

                ips.push(IpAddr::V4(ipv4));
            }
            NetworkConfig::IPv6 { subnet, .. } => {
                let ipv6 = self.allocate_ipv6(container_id, *subnet)?;

                ips.push(IpAddr::V6(ipv6));
            }
            NetworkConfig::DualStack { ipv4_subnet, ipv6_subnet, .. } => {
                let ipv4 = self.allocate_ipv4(container_id, *ipv4_subnet)?;
                let ipv6 = self.allocate_ipv6(container_id, *ipv6_subnet)?;

                ips.push(IpAddr::V4(ipv4));
                ips.push(IpAddr::V6(ipv6));
            }
        }

        self.allocated_ips.insert(container_id.to_string(), ips.clone());
        Ok(ips)
    }

    fn allocate_ipv4(&self, container_id: &str, subnet: Ipv4Addr) -> Result<Ipv4Addr> {
        if let Some(existing_ips) = self.allocated_ips.get(container_id) {
            if let Some(ipv4) = existing_ips.iter().find_map(|ip| match ip {
                IpAddr::V4(v4) => Some(*v4),
                                                             _ => None,
            }) {
                return Ok(ipv4);
            }
        }

        let mut ip_suffix = 2u8;
        let octets = subnet.octets();

        loop {
            let ip = Ipv4Addr::new(octets[0], octets[1], octets[2], ip_suffix);
            let ip_addr = IpAddr::V4(ip);

            if !self.allocated_ips.values().any(|ips| ips.contains(&ip_addr)) {
                return Ok(ip);
            }

            ip_suffix += 1;
            if ip_suffix == 255 {
                return Err(TurbineError::NetworkError(
                    "No available IPv4 addresses in subnet".to_string()
                ));
            }
        }
    }

    fn allocate_ipv6(&self, container_id: &str, subnet: Ipv6Addr) -> Result<Ipv6Addr> {
        if let Some(existing_ips) = self.allocated_ips.get(container_id) {
            if let Some(ipv6) = existing_ips.iter().find_map(|ip| match ip {
                IpAddr::V6(v6) => Some(v6),
                _ => None,
            }) {
                return Ok(*ipv6);
            }
        }

        let mut host_suffix = 2u16;
        let segments = subnet.segments();

        loop {
            let ip = Ipv6Addr::new(
                segments[0], segments[1], segments[2], segments[3],
                segments[4], segments[5], segments[6], host_suffix
            );
            let ip_addr = IpAddr::V6(ip);
            if !self.allocated_ips.values().any(|ips| ips.contains(&ip_addr)) {
                return Ok(ip);
            }

            host_suffix += 1;
            if host_suffix == 0xFFFF {
                return Err(TurbineError::NetworkError(
                    format!("No available IPv6 addresses in subnet for container {}", container_id)
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

    fn configure_container_interface(&self, interface: &str, ip: IpAddr) -> Result<()> {
        let (ip_with_mask, family) = match ip {
            IpAddr::V4(ipv4) => {
                let prefix = match &self.network_config {
                    NetworkConfig::IPv4 { prefix, .. } => prefix,
                    NetworkConfig::DualStack { ipv4_prefix, .. } => ipv4_prefix,
                    _ => &24u8,
                };
                (format!("{}/{}", ipv4, prefix), "-4")
            }
            IpAddr::V6(ipv6) => {
                let prefix = match &self.network_config {
                    NetworkConfig::IPv6 { prefix, .. } => prefix,
                    NetworkConfig::DualStack { ipv6_prefix, .. } => ipv6_prefix,
                    _ => &64u8,
                };
                (format!("{}/{}", ipv6, prefix), "-6")
            }
        };

        let output = Command::new("ip")
            .args(&[family, "addr", "add", &ip_with_mask, "dev", interface])
            .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to configure container interface: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn setup_port_forwarding(&self, host_port: u16, container_ip: IpAddr, container_port: u16) -> Result<()> {
        let (iptables_cmd, rule) = match container_ip {
            IpAddr::V4(ipv4) => (
                "iptables",
                format!("DNAT --to-destination {}:{}", ipv4, container_port)
            ),
            IpAddr::V6(ipv6) => (
                "ip6tables",
                format!("DNAT --to-destination [{}]:{}", ipv6, container_port)
            ),
        };

        let output = Command::new(iptables_cmd)
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
        for (cmd, family) in [("iptables", "IPv4"), ("ip6tables", "IPv6")] {
            let output = Command::new(cmd)
                .args(&[
                    "-t", "nat",
                    "-D", "PREROUTING",
                    "-p", "tcp",
                    "--dport", &host_port.to_string(),
                    "-j", "DNAT"
                ])
                .output()?;
            if !output.status.success() {
                eprintln!(
                    "Warning: Failed to cleanup {} port forwarding for port {}: {}",
                    family, host_port, String::from_utf8_lossy(&output.stderr)
                );
            }
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

    pub fn get_container_ips(&self, container_id: &str) -> Option<&Vec<IpAddr>> {
        self.allocated_ips.get(container_id)
    }

    pub fn network_config(&self) -> &NetworkConfig {
        &self.network_config
    }
}
