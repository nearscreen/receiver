//! Telling the network we are here.
//!
//! The receiver answers mDNS itself, so nothing from Apple has to be installed
//! for a phone to find this computer. Multicast on the local link is the only
//! traffic this produces — nothing leaves the network.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// The service the phone browses for.
pub const SERVICE_TYPE: &str = "_nearscreen._tcp.local.";

/// How long we wait for the goodbye packet to go out when shutting down.
const GOODBYE_TIMEOUT: Duration = Duration::from_millis(500);

/// Which interfaces the announcement goes out on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Interfaces {
    /// Every network this computer is on.
    #[default]
    All,
    /// Only the interface that carries this address — for a computer on
    /// several networks at once.
    Only(IpAddr),
    /// Loopback only: this machine can find itself, nothing else can. Useful
    /// for trying the receiver out without announcing it to a real network.
    Loopback,
}

impl Interfaces {
    /// Parses the `--mdns-interface` value: an address, or `loopback`.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("loopback") {
            return Ok(Interfaces::Loopback);
        }
        let address: IpAddr = value
            .parse()
            .with_context(|| format!("{value:?} is neither an IP address nor \"loopback\""))?;
        Ok(Interfaces::Only(address))
    }

    /// The interfaces to keep, as the mDNS responder names them.
    fn kinds(&self) -> Option<Vec<IfKind>> {
        match self {
            Interfaces::All => None,
            Interfaces::Only(address) => Some(vec![IfKind::Addr(*address)]),
            Interfaces::Loopback => Some(vec![IfKind::LoopbackV4, IfKind::LoopbackV6]),
        }
    }
}

/// A live `_nearscreen._tcp` announcement. Dropping it takes the receiver off
/// the network again.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Starts announcing `instance_name` on `port`. Addresses are picked up
    /// from the interfaces themselves and follow the machine moving between
    /// networks.
    pub fn start(instance_name: &str, port: u16, interfaces: &Interfaces) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("cannot start the mDNS responder")?;
        if let Some(kinds) = interfaces.kinds() {
            // Selections apply in order, so this leaves exactly `kinds` on.
            daemon
                .disable_interface(IfKind::All)
                .context("cannot narrow the mDNS responder to one interface")?;
            daemon
                .enable_interface(kinds.clone())
                .context("cannot enable the chosen interface")?;
        }

        let instance_name = display_name(instance_name);
        let host = format!("{}.local.", host_label(&instance_name));
        let mut service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host,
            (), // Addresses are filled in from the interfaces below.
            port,
            &[("v", "1")][..],
        )
        .context("cannot describe the service")?
        .enable_addr_auto();
        if let Some(kinds) = interfaces.kinds() {
            service.set_interfaces(kinds);
        }

        let fullname = service.get_fullname().to_string();
        daemon
            .register(service)
            .context("cannot announce the receiver on the network")?;
        info!("announcing \"{instance_name}\" as {SERVICE_TYPE} on port {port}");
        debug!("mDNS interfaces: {interfaces:?}, host {host}");
        Ok(Self { daemon, fullname })
    }

    /// The instance name as it appears on the network.
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        // Say goodbye properly, so phones drop us from their list at once
        // instead of waiting for the record to expire.
        match self.daemon.unregister(&self.fullname) {
            Ok(status) => {
                let _ = status.recv_timeout(GOODBYE_TIMEOUT);
            }
            Err(e) => warn!("cannot withdraw the announcement: {e}"),
        }
        if let Err(e) = self.daemon.shutdown() {
            warn!("cannot stop the mDNS responder: {e}");
        }
    }
}

/// What to call ourselves on the network — never empty.
fn display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Nearscreen receiver".to_string()
    } else {
        name.to_string()
    }
}

/// A host label a DNS name can actually carry: letters, digits and hyphens.
fn host_label(name: &str) -> String {
    let mut label: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    label.truncate(63); // The longest a DNS label may be.
    let label = label.trim_matches('-').to_string();
    if label.is_empty() {
        "nearscreen".to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_values_are_understood() {
        assert_eq!(Interfaces::parse("loopback").unwrap(), Interfaces::Loopback);
        assert_eq!(Interfaces::parse("LoopBack").unwrap(), Interfaces::Loopback);
        assert_eq!(
            Interfaces::parse("192.168.1.7").unwrap(),
            Interfaces::Only("192.168.1.7".parse().unwrap())
        );
        assert!(Interfaces::parse("eth0").is_err());
    }

    #[test]
    fn host_labels_survive_real_computer_names() {
        assert_eq!(host_label("MacBook Иры"), "MacBook");
        assert_eq!(host_label("Ira's PC"), "Ira-s-PC");
        assert_eq!(host_label("home-pc"), "home-pc");
        assert_eq!(host_label("..."), "nearscreen");
        assert!(host_label(&"x".repeat(100)).len() <= 63);
    }

    #[test]
    fn a_nameless_computer_still_gets_a_name() {
        assert_eq!(display_name("   "), "Nearscreen receiver");
        assert_eq!(display_name(" Home PC "), "Home PC");
    }
}
