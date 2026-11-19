//! Finding PlayStation consoles on the home network and waking them from Rest mode.
//!
//! A console answers a short text request sent over UDP to its discovery port (PS5: 9302,
//! PS4: 987) with its state and details. The same port takes a wake-up request, which must
//! carry the credential the console handed out when this PC was paired with it.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Ps4,
    Ps5,
}

impl Model {
    pub fn port(self) -> u16 {
        match self {
            Model::Ps4 => 987,
            Model::Ps5 => 9302,
        }
    }

    fn protocol_version(self) -> &'static str {
        match self {
            Model::Ps4 => "00020020",
            Model::Ps5 => "00030010",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerState {
    /// Switched on.
    On,
    /// Rest mode: can be woken over the network.
    Standby,
}

/// One console's answer to a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Console {
    pub addr: IpAddr,
    pub model: Model,
    pub state: PowerState,
    pub host_id: String,
    pub host_name: String,
    pub system_version: String,
    /// TCP port for pairing and session requests.
    pub request_port: u16,
    /// The game or app running, if any: (title id, name).
    pub running: Option<(String, String)>,
}

/// The search request for one console model.
pub fn search_request(model: Model) -> Vec<u8> {
    format!(
        "SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{}\n",
        model.protocol_version()
    )
    .into_bytes()
}

/// The wake-up request. `credential` is the pairing key the console gave this PC
/// (hex text), sent as a decimal number.
pub fn wake_request(model: Model, credential_hex: &str) -> Result<Vec<u8>> {
    let credential = u64::from_str_radix(credential_hex.trim(), 16)
        .with_context(|| format!("pairing credential {credential_hex:?} is not hex"))?;
    Ok(format!(
        "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:w\napp-type:r\n\
         user-credential:{credential}\ndevice-discovery-protocol-version:{}\n",
        model.protocol_version()
    )
    .into_bytes())
}

/// Read a console's answer. `None` if it isn't one.
pub fn parse_reply(addr: IpAddr, text: &str) -> Option<Console> {
    let mut lines = text.lines();
    let status = lines.next()?.trim();
    let state = if status.starts_with("HTTP/1.1 200") {
        PowerState::On
    } else if status.starts_with("HTTP/1.1 620") {
        PowerState::Standby
    } else {
        return None;
    };
    let fields: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let get = |k: &str| fields.get(k).cloned().unwrap_or_default();
    let model = match get("host-type").to_ascii_uppercase().as_str() {
        "PS5" => Model::Ps5,
        "PS4" => Model::Ps4,
        _ => return None,
    };
    let running = match (fields.get("running-app-titleid"), fields.get("running-app-name")) {
        (Some(id), Some(name)) if !id.is_empty() => Some((id.clone(), name.clone())),
        _ => None,
    };
    Some(Console {
        addr,
        model,
        state,
        host_id: get("host-id"),
        host_name: get("host-name"),
        system_version: get("system-version"),
        request_port: get("host-request-port").parse().unwrap_or(997),
        running,
    })
}

/// Look for consoles: a broadcast on every local IPv4 network, plus a direct request to each
/// address of those networks (Windows' firewall can drop replies to a broadcast, because
/// they come back from an address the request wasn't sent to).
pub fn search(timeout: Duration) -> Result<Vec<Console>> {
    let mut found: HashMap<IpAddr, Console> = HashMap::new();
    for local in local_ipv4_networks() {
        let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(local.addr), 0))
            .with_context(|| format!("UDP socket on {}", local.addr))?;
        sock.set_broadcast(true)?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        for model in [Model::Ps5, Model::Ps4] {
            let req = search_request(model);
            let _ = sock.send_to(&req, (local.broadcast(), model.port()));
            for host in local.hosts() {
                if host != local.addr {
                    let _ = sock.send_to(&req, (host, model.port()));
                }
            }
        }
        let end = Instant::now() + timeout;
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                if let Some(c) = parse_reply(from.ip(), &String::from_utf8_lossy(&buf[..n])) {
                    found.insert(c.addr, c);
                }
            }
        }
    }
    Ok(found.into_values().collect())
}

/// Wake a console in Rest mode (it answers searches again once it's on).
pub fn wake(console: IpAddr, model: Model, credential_hex: &str) -> Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("UDP socket")?;
    sock.send_to(&wake_request(model, credential_hex)?, (console, model.port()))
        .context("send wake-up")?;
    Ok(())
}

/// A local IPv4 network small enough to ask every address on it.
struct LocalNet {
    addr: Ipv4Addr,
    prefix: u8,
}

impl LocalNet {
    fn broadcast(&self) -> Ipv4Addr {
        let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
        Ipv4Addr::from(u32::from(self.addr) | !mask)
    }

    fn hosts(&self) -> impl Iterator<Item = Ipv4Addr> {
        let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
        let base = u32::from(self.addr) & mask;
        let count = if self.prefix >= 22 { (!mask).saturating_sub(1) } else { 0 };
        (1..=count).map(move |i| Ipv4Addr::from(base + i))
    }
}

/// This PC's private IPv4 networks (home LANs), skipping virtual adapters' link-local ones.
fn local_ipv4_networks() -> Vec<LocalNet> {
    // The address the OS would use to reach the internet is the home network's.
    let mut nets = Vec::new();
    if let Ok(s) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        if s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                if a.ip().is_private() {
                    nets.push(LocalNet {
                        addr: *a.ip(),
                        prefix: 24,
                    });
                }
            }
        }
    }
    nets
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 50));

    #[test]
    fn reads_a_ps5_that_is_on() {
        let reply = "HTTP/1.1 200 Ok\nhost-id:AABBCCDDEEFF\nhost-type:PS5\nhost-name:Living room\n\
                     host-request-port:997\ndevice-discovery-protocol-version:00030010\n\
                     system-version:09060000\nrunning-app-name:Grand Theft Auto VI\n\
                     running-app-titleid:PPSA00000\n";
        let c = parse_reply(ADDR, reply).expect("a console");
        assert_eq!(c.model, Model::Ps5);
        assert_eq!(c.state, PowerState::On);
        assert_eq!(c.host_name, "Living room");
        assert_eq!(c.request_port, 997);
        assert_eq!(
            c.running,
            Some(("PPSA00000".into(), "Grand Theft Auto VI".into()))
        );
    }

    #[test]
    fn reads_a_console_in_rest_mode_and_ignores_others() {
        let reply = "HTTP/1.1 620 Server Standby\nhost-id:112233445566\nhost-type:PS4\n\
                     host-name:Bedroom\nhost-request-port:997\n";
        let c = parse_reply(ADDR, reply).expect("a console");
        assert_eq!((c.model, c.state), (Model::Ps4, PowerState::Standby));
        assert_eq!(c.running, None);
        assert!(parse_reply(ADDR, "HTTP/1.1 404 Not Found\n").is_none());
        assert!(parse_reply(ADDR, "HTTP/1.1 200 Ok\nhost-type:TV\n").is_none());
        assert!(parse_reply(ADDR, "").is_none());
    }

    #[test]
    fn requests_use_each_models_protocol_version() {
        let s = String::from_utf8(search_request(Model::Ps5)).unwrap();
        assert_eq!(s, "SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n");
        let w = String::from_utf8(wake_request(Model::Ps4, "0000beef").unwrap()).unwrap();
        assert!(w.starts_with("WAKEUP * HTTP/1.1\n"));
        assert!(w.contains("user-credential:48879\n"), "hex credential sent as decimal");
        assert!(w.ends_with("device-discovery-protocol-version:00020020\n"));
        assert!(wake_request(Model::Ps5, "not hex").is_err());
    }

    #[test]
    fn local_network_maths() {
        let n = LocalNet {
            addr: Ipv4Addr::new(192, 168, 0, 84),
            prefix: 24,
        };
        assert_eq!(n.broadcast(), Ipv4Addr::new(192, 168, 0, 255));
        let hosts: Vec<_> = n.hosts().collect();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 0, 254));
    }
}
