use crate::{Container, TurbineError, Result, PortMapping};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Command;
use std::fs;

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
            subnet: Ipv4Addr::new(10, 88, 0, 0),
            prefix: 24,
        }
    }
}

pub struct NetworkManager {
    bridge_name: String,
    network_config: NetworkConfig,
    allocated_ips: HashMap<String, Vec<IpAddr>>,
    port_mappings: HashMap<u16, String>,
    container_ports: HashMap<String, Vec<PortMapping>>,
    netns_dir: String,
    use_slirp4netns: bool,
}

impl NetworkManager {
    pub fn new(bridge_name: String) -> Self {
        Self {
            bridge_name,
            network_config: NetworkConfig::default(),
            allocated_ips: HashMap::new(),
            port_mappings: HashMap::new(),
            container_ports: HashMap::new(),
            netns_dir: "/tmp/turbine-netns".to_string(),
            use_slirp4netns: Self::check_slirp4netns_available(),
        }
    }

    pub fn with_config(bridge_name: String, config: NetworkConfig) -> Self {
        Self {
            bridge_name,
            network_config: config,
            allocated_ips: HashMap::new(),
            port_mappings: HashMap::new(),
            container_ports: HashMap::new(),
            netns_dir: "/tmp/turbine-netns".to_string(),
            use_slirp4netns: Self::check_slirp4netns_available(),
        }
    }

    fn check_slirp4netns_available() -> bool {
        Command::new("which")
        .arg("slirp4netns")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
    }

    pub fn setup_rootless_networking(&self) -> Result<()> {
        fs::create_dir_all(&self.netns_dir)?;

        if self.use_slirp4netns {
            println!("Using slirp4netns for rootless networking");
            Ok(())
        } else {
            self.setup_user_namespace_networking()
        }
    }

    fn setup_user_namespace_networking(&self) -> Result<()> {
        if !self.check_user_namespace_support()? {
            return Err(TurbineError::NetworkError(
                "User namespaces not supported. Consider installing slirp4netns for rootless networking.".to_string()
            ));
        }

        let netns_path = format!("{}/{}", self.netns_dir, self.bridge_name);
        let output = Command::new("unshare")
        .args(&["--net", "--mount-proc", "/bin/sh", "-c", &format!("mount --bind /proc/self/ns/net {}", netns_path)])
        .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to create network namespace: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        self.setup_namespaced_bridge(&netns_path)?;

        Ok(())
    }

    fn check_user_namespace_support(&self) -> Result<bool> {
        match fs::read_to_string("/proc/sys/user/max_user_namespaces") {
            Ok(content) => {
                let max_ns: i32 = content.trim().parse().unwrap_or(0);
                Ok(max_ns > 0)
            }
            Err(_) => Ok(false)
        }
    }

    fn setup_namespaced_bridge(&self, netns_path: &str) -> Result<()> {
        let setup_script = format!(r#"
        # Create bridge
        ip link add name {} type bridge
        ip link set dev {} up

        # Configure bridge IP
        {}

        # Setup loopback
        ip link set dev lo up
        "#,
        self.bridge_name,
        self.bridge_name,
        self.get_bridge_ip_command()
        );

        let output = Command::new("nsenter")
        .args(&[&format!("--net={}", netns_path), "/bin/sh", "-c", &setup_script])
        .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to setup namespaced bridge: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    fn get_bridge_ip_command(&self) -> String {
        match &self.network_config {
            NetworkConfig::IPv4 { subnet, prefix } => {
                let octets = subnet.octets();
                format!("ip addr add {}.{}.{}.1/{} dev {}",
                    octets[0], octets[1], octets[2], prefix, self.bridge_name)
            }
            NetworkConfig::IPv6 { subnet, prefix } => {
                let segments = subnet.segments();
                format!("ip addr add {:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:1/{} dev {}",
                    segments[0], segments[1], segments[2], segments[3],
                    segments[4], segments[5], segments[6], prefix, self.bridge_name)
            }
            NetworkConfig::DualStack { ipv4_subnet, ipv4_prefix, ipv6_subnet, ipv6_prefix } => {
                let octets = ipv4_subnet.octets();
                let segments = ipv6_subnet.segments();
                format!("ip addr add {}.{}.{}.1/{} dev {} && ip addr add {:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:1/{} dev {}",
                    octets[0], octets[1], octets[2], ipv4_prefix, self.bridge_name,
                    segments[0], segments[1], segments[2], segments[3],
                    segments[4], segments[5], segments[6], ipv6_prefix, self.bridge_name)
            }
        }
    }

    pub fn setup_container_network(&mut self, container: &Container) -> Result<()> {
        self.container_ports.insert(container.id.clone(), container.config.ports.clone());

        if self.use_slirp4netns {
            self.setup_slirp4netns_network(container)
        } else {
            self.setup_user_namespace_network(container)
        }
    }

    fn setup_slirp4netns_network(&mut self, container: &Container) -> Result<()> {
        let _container_ips = self.allocate_ips(&container.id)?;
        let mut port_args = Vec::new();

        for port in &container.config.ports {
            if self.port_mappings.contains_key(&port.host_port) {
                return Err(TurbineError::NetworkError(
                    format!("Port {} is already in use", port.host_port)
                ));
            }

            port_args.push(format!("--configure"));
            port_args.push(format!("tcp:{}-tcp:{}", port.host_port, port.container_port));
            self.port_mappings.insert(port.host_port, container.id.clone());
        }

        println!("Container {} will use slirp4netns with ports: {:?}",
                 &container.id[..8], container.config.ports);

        Ok(())
    }

    fn setup_user_namespace_network(&mut self, container: &Container) -> Result<()> {
        let container_ips = self.allocate_ips(&container.id)?;
        let veth_host = format!("veth-{}", &container.id[..8]);
        let veth_container = format!("veth-c-{}", &container.id[..8]);
        let netns_path = format!("{}/{}", self.netns_dir, self.bridge_name);
        let create_veth_script = format!(r#"
        ip link add {} type veth peer name {}
        ip link set {} master {}
        ip link set {} up
        ip link set {} up
        "#, veth_host, veth_container, veth_host, self.bridge_name, veth_host, self.bridge_name);
        let output = Command::new("nsenter")
        .args(&[&format!("--net={}", netns_path), "/bin/sh", "-c", &create_veth_script])
        .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to create veth pair in namespace: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        for ip in &container_ips {
            self.configure_container_interface_in_namespace(&netns_path, &veth_container, *ip)?;
        }

        Ok(())
    }

    fn configure_container_interface_in_namespace(&self, netns_path: &str, interface: &str, ip: IpAddr) -> Result<()> {
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

        let output = Command::new("nsenter")
        .args(&[
            &format!("--net={}", netns_path),
              "ip", family, "addr", "add", &ip_with_mask, "dev", interface
        ])
        .output()?;
        if !output.status.success() {
            return Err(TurbineError::NetworkError(
                format!("Failed to configure container interface: {}", String::from_utf8_lossy(&output.stderr))
            ));
        }

        Ok(())
    }

    pub fn start_slirp4netns(&self, container_pid: u32, container_id: &str) -> Result<std::process::Child> {
        if !self.use_slirp4netns {
            return Err(TurbineError::NetworkError(
                "slirp4netns not available".to_string()
            ));
        }

        let mut cmd = Command::new("slirp4netns");
        cmd.args(&[
            "--configure",
            "--mtu=65520",
            "--disable-host-loopback",
            &container_pid.to_string(),
                 "tap0"
        ]);

        if let Some(ports) = self.get_container_ports(container_id) {
            for port in ports {
                cmd.args(&[
                    "--port-forward",
                    &format!("tcp:{}-tcp:{}", port.host_port, port.container_port)
                ]);
            }
        }

        let child = cmd.spawn()?;

        Ok(child)
    }

    fn get_container_ports(&self, container_id: &str) -> Option<&Vec<PortMapping>> {
        self.container_ports.get(container_id)
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

    pub fn cleanup_container_network(&mut self, container: &Container) -> Result<()> {
        if self.use_slirp4netns {
            self.port_mappings.retain(|_, id| id != &container.id);
        } else {
            let veth_host = format!("veth-{}", &container.id[..8]);
            let netns_path = format!("{}/{}", self.netns_dir, self.bridge_name);

            let _ = Command::new("nsenter")
            .args(&[&format!("--net={}", netns_path), "ip", "link", "del", &veth_host])
            .output();
        }

        self.allocated_ips.remove(&container.id);
        self.container_ports.remove(&container.id);
        Ok(())
    }

    pub fn cleanup_networking(&self) -> Result<()> {
        if !self.use_slirp4netns {
            let netns_path = format!("{}/{}", self.netns_dir, self.bridge_name);
            let _ = fs::remove_file(netns_path);
        }
        Ok(())
    }

    pub fn get_container_ips(&self, container_id: &str) -> Option<&Vec<IpAddr>> {
        self.allocated_ips.get(container_id)
    }

    pub fn network_config(&self) -> &NetworkConfig {
        &self.network_config
    }

    pub fn is_using_slirp4netns(&self) -> bool {
        self.use_slirp4netns
    }

    pub fn setup_bridge(&self) -> Result<()> {
        if self.use_slirp4netns {
            println!("Using slirp4netns - no bridge setup required");
            Ok(())
        } else {
            self.setup_rootless_networking()
        }
    }

    pub fn cleanup_bridge(&self) -> Result<()> {
        if !self.use_slirp4netns {
            let netns_path = format!("{}/{}", self.netns_dir, self.bridge_name);
            let cleanup_script = format!("ip link del {}", self.bridge_name);
            let _ = Command::new("nsenter")
            .args(&[&format!("--net={}", netns_path), "/bin/sh", "-c", &cleanup_script])
            .output();

            let _ = fs::remove_file(netns_path);
        }
        Ok(())
    }
}
