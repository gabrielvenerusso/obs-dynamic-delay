//! RTMP server that OBS publishes to.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rml_rtmp::handshake::PeerType;
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;

use crate::EngineMsg;
use crate::flv::Kind;
use crate::rtmp_io;

/// OBS sends media many times a second: a publisher silent for this long is gone
/// (network dropped, OBS killed) even if the OS has not closed the socket yet.
/// Without this a half-open connection keeps the slot and the reconnecting OBS
/// is rejected as a "second publisher".
const PUBLISH_IDLE: Duration = Duration::from_secs(10);
/// Same for a client that connected but never published.
const IDLE: Duration = Duration::from_secs(30);

pub async fn serve(listen: String, tx: UnboundedSender<EngineMsg>) -> Result<()> {
    let listener = TcpListener::bind(&listen).await?;
    log::info!("waiting for OBS on rtmp://{listen}/live");
    let busy = Arc::new(AtomicBool::new(false));
    let ids = AtomicU64::new(1);
    loop {
        let (sock, peer) = listener.accept().await?;
        let id = ids.fetch_add(1, Ordering::Relaxed);
        let (tx, busy) = (tx.clone(), busy.clone());
        tokio::spawn(async move {
            log::info!("[ingest {id}] connection from {peer}");
            let mut publishing = false;
            if let Err(e) = handle(sock, id, &tx, &busy, &mut publishing).await {
                log::warn!("[ingest {id}] {e:#}");
            }
            if publishing {
                busy.store(false, Ordering::SeqCst);
                let _ = tx.send(EngineMsg::PublishEnd { session: id });
            }
            log::info!("[ingest {id}] closed");
        });
    }
}

async fn handle(
    mut sock: TcpStream,
    id: u64,
    tx: &UnboundedSender<EngineMsg>,
    busy: &AtomicBool,
    publishing: &mut bool,
) -> Result<()> {
    sock.set_nodelay(true)?;
    let leftover = tokio::time::timeout(IDLE, rtmp_io::handshake(&mut sock, PeerType::Server))
        .await
        .context("timeout during the RTMP handshake")??;
    let (mut session, initial) = ServerSession::new(ServerSessionConfig::new())?;
    let mut pending: VecDeque<ServerSessionResult> = initial.into();
    pending.extend(session.handle_input(&leftover)?);

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        while let Some(result) = pending.pop_front() {
            match result {
                ServerSessionResult::OutboundResponse(p) => sock.write_all(&p.bytes).await?,
                ServerSessionResult::RaisedEvent(ev) => {
                    handle_event(ev, id, &mut session, &mut pending, tx, busy, publishing)?
                }
                ServerSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        let idle = if *publishing { PUBLISH_IDLE } else { IDLE };
        let n = tokio::time::timeout(idle, sock.read(&mut buf))
            .await
            .with_context(|| format!("nothing received for {} s, dropping the connection", idle.as_secs()))??;
        if n == 0 {
            return Ok(());
        }
        pending.extend(session.handle_input(&buf[..n])?);
    }
}

fn handle_event(
    ev: ServerSessionEvent,
    id: u64,
    session: &mut ServerSession,
    pending: &mut VecDeque<ServerSessionResult>,
    tx: &UnboundedSender<EngineMsg>,
    busy: &AtomicBool,
    publishing: &mut bool,
) -> Result<()> {
    match ev {
        ServerSessionEvent::ConnectionRequested { request_id, app_name } => {
            log::info!("[ingest {id}] connect app={app_name}");
            pending.extend(session.accept_request(request_id)?);
        }
        ServerSessionEvent::PublishStreamRequested { request_id, stream_key, .. } => {
            if busy.swap(true, Ordering::SeqCst) {
                log::warn!("[ingest {id}] rejecting second publisher");
                pending.extend(session.reject_request(
                    request_id,
                    "NetStream.Publish.BadName",
                    "another stream is already publishing",
                )?);
            } else {
                log::info!("[ingest {id}] OBS started publishing");
                *publishing = true;
                pending.extend(session.accept_request(request_id)?);
                let _ = tx.send(EngineMsg::PublishStart { session: id, key: stream_key });
            }
        }
        ServerSessionEvent::PublishStreamFinished { .. } => {
            if *publishing {
                log::info!("[ingest {id}] OBS stopped publishing");
                *publishing = false;
                busy.store(false, Ordering::SeqCst);
                let _ = tx.send(EngineMsg::PublishEnd { session: id });
            }
        }
        ServerSessionEvent::StreamMetadataChanged { metadata, .. } if *publishing => {
            let _ = tx.send(EngineMsg::Metadata(metadata));
        }
        ServerSessionEvent::AudioDataReceived { data, timestamp, .. } if *publishing => {
            let _ = tx.send(EngineMsg::Media {
                session: id,
                kind: Kind::Audio,
                ts: timestamp.value,
                data,
                arrival: Instant::now(),
            });
        }
        ServerSessionEvent::VideoDataReceived { data, timestamp, .. } if *publishing => {
            let _ = tx.send(EngineMsg::Media {
                session: id,
                kind: Kind::Video,
                ts: timestamp.value,
                data,
                arrival: Instant::now(),
            });
        }
        _ => {}
    }
    Ok(())
}
