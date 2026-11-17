//! OSC over UDP: a receive thread feeding the main loop, and a sender.

use crate::Event;
use anyhow::{Context, Result, anyhow};
use rosc::{OscMessage, OscPacket, OscType};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

pub fn spawn_listener(port: u16, tx: Sender<Event>) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).with_context(|| {
        format!("cannot listen for OSC on UDP port {port}. Is open-stage-control (or another copy of MA3-OSC) still running?")
    })?;
    std::thread::Builder::new().name("osc-rx".into()).spawn(move || {
        let mut buf = vec![0u8; 65536];
        let mut last_err: Option<Instant> = None;
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, _from)) => match rosc::decoder::decode_udp(&buf[..n]) {
                    Ok((_, packet)) => {
                        let mut msgs = Vec::new();
                        flatten(packet, &mut msgs);
                        for (addr, args) in msgs {
                            if tx.send(Event::Osc { addr, args }).is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => debug!("undecodable OSC packet ({n} bytes): {e:?}"),
                },
                Err(e) => {
                    // e.g. Windows reports ICMP "port unreachable" here; never fatal.
                    if last_err.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
                        warn!("OSC receive error: {e}");
                        last_err = Some(Instant::now());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    })?;
    Ok(())
}

fn flatten(p: OscPacket, out: &mut Vec<(String, Vec<OscType>)>) {
    match p {
        OscPacket::Message(m) => out.push((m.addr, m.args)),
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

pub struct OscOut {
    sock: UdpSocket,
    target: SocketAddr,
    last_err: Option<Instant>,
}

impl OscOut {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        let target = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve MA host '{host}'"))?
            .next()
            .ok_or_else(|| anyhow!("cannot resolve MA host '{host}'"))?;
        let bind: SocketAddr = if target.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
        let sock = UdpSocket::bind(bind).context("cannot open UDP socket for sending OSC")?;
        Ok(OscOut { sock, target, last_err: None })
    }

    pub fn send(&mut self, addr: &str, args: Vec<OscType>) {
        let packet = OscPacket::Message(OscMessage { addr: addr.to_string(), args });
        let bytes = match rosc::encoder::encode(&packet) {
            Ok(b) => b,
            Err(e) => {
                warn!(addr, "cannot encode OSC: {e:?}");
                return;
            }
        };
        if let Err(e) = self.sock.send_to(&bytes, self.target)
            && self.last_err.is_none_or(|t| t.elapsed() > Duration::from_secs(10))
        {
            warn!(target = %self.target, "OSC send failed: {e}");
            self.last_err = Some(Instant::now());
        }
    }
}
