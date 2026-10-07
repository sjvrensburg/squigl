//! Where a phone can reach this machine: its LAN addresses, and the overlay
//! networks (Tailscale, ZeroTier, Nebula) it is on. A phone on a different network
//! can still pair when both are on the same overlay, through the overlay's address;
//! squigl does not run an overlay itself (the phone end is a browser, which cannot
//! join one, so the phone needs the overlay's app either way).

use std::net::{IpAddr, Ipv4Addr};

/// Which network an address is on, as a person would name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    WiFi,
    Wired,
    /// A LAN address on an interface whose kind the name does not say.
    Local,
    Tailscale,
    ZeroTier,
    Nebula,
}

impl Network {
    pub fn label(self) -> &'static str {
        match self {
            Network::WiFi => "Wi-Fi",
            Network::Wired => "Wired network",
            Network::Local => "Local network",
            Network::Tailscale => "Tailscale",
            Network::ZeroTier => "ZeroTier",
            Network::Nebula => "Nebula",
        }
    }

    pub fn is_overlay(self) -> bool {
        matches!(
            self,
            Network::Tailscale | Network::ZeroTier | Network::Nebula
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub ip: IpAddr,
    pub network: Network,
    /// The interface's name, for the log.
    pub interface: String,
}

impl Address {
    /// An address given outright (`--webrtc-bind`), with no interface to name it.
    pub fn given(ip: IpAddr) -> Self {
        Self {
            ip,
            network: Network::Local,
            interface: String::new(),
        }
    }
}

/// This machine's addresses a phone could pair at: the LAN ones (the one the
/// default route leaves by first), then the overlays'. IPv4 only, with loopback,
/// link-local and the virtual bridges of containers and VMs left out.
pub fn addresses() -> Vec<Address> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(e) => {
            log::warn!("listing network interfaces: {e}");
            Vec::new()
        }
    };
    let route = crate::detect_lan_ip().ok();
    let mut found: Vec<Address> = interfaces
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) => classify(&i.name, ip).map(|network| Address {
                ip: IpAddr::V4(ip),
                network,
                interface: i.name,
            }),
            IpAddr::V6(_) => None,
        })
        .collect();
    // The LAN before the overlays (even when the route out is an overlay's: an
    // exit node), the route out first within each; otherwise as listed.
    found.sort_by_key(|a| (a.network.is_overlay(), Some(a.ip) != route));
    found.dedup_by_key(|a| a.ip);
    found
}

/// What network `ip` on interface `name` is, or `None` if it is not one to offer.
/// Names are matched case-insensitively, for Linux's (`tailscale0`, `zt…`,
/// `nebula1`, `wlp…`) and Windows' adapter names ("Tailscale", "ZeroTier One […]",
/// "Wi-Fi").
pub fn classify(name: &str, ip: Ipv4Addr) -> Option<Network> {
    let lower = name.to_ascii_lowercase();
    if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast() {
        return None;
    }
    const VIRTUAL: &[&str] = &[
        "docker",
        "br-",
        "veth",
        "virbr",
        "vmnet",
        "vboxnet",
        "podman",
        "cni",
        "flannel",
        "lxc",
        "lxd",
        "incus",
        "vethernet",
        "hyper-v",
        "virtualbox",
        "vmware",
        "wsl",
    ];
    if VIRTUAL.iter().any(|v| lower.starts_with(v)) {
        return None;
    }
    if lower.contains("tailscale") {
        return Some(Network::Tailscale);
    }
    if lower.starts_with("zt") || lower.contains("zerotier") {
        return Some(Network::ZeroTier);
    }
    if lower.contains("nebula") {
        return Some(Network::Nebula);
    }
    // Tailscale's range (carrier-grade NAT space) on an interface not named for it.
    let [a, b, ..] = ip.octets();
    if a == 100 && (64..128).contains(&b) {
        return Some(Network::Tailscale);
    }
    if !ip.is_private() {
        return None;
    }
    Some(
        if lower.starts_with("wl") || lower.contains("wi-fi") || lower.contains("wireless") {
            Network::WiFi
        } else if lower.starts_with("en") || lower.starts_with("eth") || lower.contains("ethernet")
        {
            Network::Wired
        } else {
            Network::Local
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn interfaces_are_named_for_people() {
        let cases = [
            ("wlp0s20f3", "10.201.136.19", Some(Network::WiFi)),
            ("Wi-Fi", "192.168.1.20", Some(Network::WiFi)),
            ("enx00e04cdea7f5", "10.101.1.162", Some(Network::Wired)),
            ("eth0", "172.20.0.4", Some(Network::Wired)),
            ("Ethernet 2", "192.168.0.7", Some(Network::Wired)),
            ("bond0", "192.168.0.8", Some(Network::Local)),
            ("tailscale0", "100.72.115.112", Some(Network::Tailscale)),
            ("Tailscale", "100.101.1.2", Some(Network::Tailscale)),
            ("utun4", "100.64.0.9", Some(Network::Tailscale)),
            ("ztabcdef12", "10.147.17.3", Some(Network::ZeroTier)),
            (
                "ZeroTier One [8056c2e21c000001]",
                "10.147.17.3",
                Some(Network::ZeroTier),
            ),
            ("nebula1", "192.168.100.5", Some(Network::Nebula)),
            ("docker0", "172.17.0.1", None),
            ("br-5f2a", "172.18.0.1", None),
            ("virbr0", "192.168.122.1", None),
            ("vEthernet (WSL)", "172.29.0.1", None),
            ("lo", "127.0.0.1", None),
            ("wlan0", "169.254.3.4", None),
            ("eth0", "203.0.113.5", None),
        ];
        for (name, addr, want) in cases {
            assert_eq!(classify(name, ip(addr)), want, "{name} {addr}");
        }
    }

    #[test]
    fn the_overlays_come_after_the_lan() {
        let found = addresses();
        let first_overlay = found.iter().position(|a| a.network.is_overlay());
        if let Some(i) = first_overlay {
            assert!(
                found[i..].iter().all(|a| a.network.is_overlay()),
                "{found:?}"
            );
        }
    }
}
