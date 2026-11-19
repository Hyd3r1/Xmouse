//! PlayStation Remote Play test tool (development only).
//!
//!   cargo run --example ps_spike -- find
//!   cargo run --example ps_spike -- wake <console-ip> <ps5|ps4> <pairing-credential-hex>

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use xmouse::ps::discovery::{self, Model};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("find") => {
            let consoles = discovery::search(Duration::from_secs(3))?;
            if consoles.is_empty() {
                println!(
                    "No PlayStation answered. It needs to be on or in Rest mode with \
                     \"Stay connected to the internet\" on, Remote Play enabled, and on this network."
                );
            }
            for c in consoles {
                println!(
                    "{:?} \"{}\" at {} — {:?}, system {}, id {}{}",
                    c.model,
                    c.host_name,
                    c.addr,
                    c.state,
                    c.system_version,
                    c.host_id,
                    c.running
                        .map(|(id, name)| format!(", playing {name} ({id})"))
                        .unwrap_or_default()
                );
            }
        }
        Some("wake") => {
            let ip: IpAddr = args.get(1).context("console IP")?.parse()?;
            let model = match args.get(2).map(String::as_str) {
                Some("ps5") => Model::Ps5,
                Some("ps4") => Model::Ps4,
                _ => bail!("model: ps5 or ps4"),
            };
            discovery::wake(ip, model, args.get(3).context("pairing credential")?)?;
            println!("Wake-up sent to {ip}.");
        }
        _ => println!("usage: ps_spike find | wake <ip> <ps5|ps4> <credential-hex>"),
    }
    Ok(())
}
