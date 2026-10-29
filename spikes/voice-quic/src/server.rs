//! Selective forwarding server: every valid datagram goes to every other
//! member of the room, unchanged. In echo mode it goes back to the sender,
//! which is what the probe measures against.

use crate::tls::{self, BoxError};
use itson_protocol::voice::VoicePacket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

type Room = Arc<Mutex<HashMap<u32, quinn::Connection>>>;

fn lock(room: &Room) -> MutexGuard<'_, HashMap<u32, quinn::Connection>> {
    room.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub async fn run(listen: SocketAddr, data_dir: PathBuf, echo: bool) -> Result<(), BoxError> {
    let identity = tls::load_or_create(&data_dir)?;
    let fingerprint = tls::fingerprint(&identity.cert);
    std::fs::write(
        data_dir.join("voice-cert.sha256"),
        format!("{fingerprint}\n"),
    )?;
    let endpoint = quinn::Endpoint::server(tls::server_config(identity)?, listen)?;
    println!(
        "listening on udp {} in {} mode",
        endpoint.local_addr()?,
        if echo { "echo" } else { "forward" }
    );
    println!("certificate sha256 {fingerprint}");

    let room: Room = Arc::new(Mutex::new(HashMap::new()));
    let next_session = AtomicU32::new(1);
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let session = next_session.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(member(incoming, room.clone(), session, echo));
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    endpoint.close(0u32.into(), b"server stopping");
    endpoint.wait_idle().await;
    Ok(())
}

async fn member(incoming: quinn::Incoming, room: Room, session: u32, echo: bool) {
    let connection = match incoming.await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("handshake failed: {e}");
            return;
        }
    };
    if let Err(e) = tls::send_session(&connection, session).await {
        eprintln!("session {session}: could not announce session: {e}");
        return;
    }
    lock(&room).insert(session, connection.clone());
    println!("session {session} joined");

    let (mut forwarded, mut rejected) = (0u64, 0u64);
    let reason = loop {
        let datagram = match connection.read_datagram().await {
            Ok(d) => d,
            Err(e) => break e,
        };
        // Only forward well-formed packets that carry the sender's own session.
        match VoicePacket::decode(&datagram) {
            Ok(p) if p.header.speaker == session => {}
            _ => {
                rejected += 1;
                continue;
            }
        }
        if echo {
            if connection.send_datagram(datagram).is_ok() {
                forwarded += 1;
            }
            continue;
        }
        let peers: Vec<quinn::Connection> = lock(&room)
            .iter()
            .filter(|(s, _)| **s != session)
            .map(|(_, c)| c.clone())
            .collect();
        for peer in peers {
            if peer.send_datagram(datagram.clone()).is_ok() {
                forwarded += 1;
            }
        }
    };
    lock(&room).remove(&session);
    println!("session {session} left ({reason}); forwarded {forwarded}, rejected {rejected}");
}
