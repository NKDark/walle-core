use crate::{
    config::{WebSocketClient, WebSocketServer},
    error::{WalleError, WalleResult},
    event::{Event, MetaDetailEvent, MetaTypes},
    util::{AuthReqHeaderExt, Echo, GetSelf, ProtocolItem},
    ActionHandler, EventHandler, OneBot,
};
use crate::{
    obc::{
        ws_util::{try_connect, upgrade_websocket},
        AppOBC, EchoMap,
    },
    util::ContentType,
};

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::http::{header::USER_AGENT, Request};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::WebSocketStream;
use tracing::{info, warn};

impl<A, R> AppOBC<A, R>
where
    A: ProtocolItem,
    R: ProtocolItem,
{
    pub(crate) async fn ws<E, AH, EH>(
        &self,
        ob: &Arc<OneBot<AH, EH>>,
        config: Vec<WebSocketClient>,
        tasks: &mut Vec<JoinHandle<()>>,
    ) -> WalleResult<()>
    where
        E: ProtocolItem + GetSelf + Clone,
        AH: ActionHandler<E, A, R> + Send + Sync + 'static,
        EH: EventHandler<E, A, R> + Send + Sync + 'static,
    {
        for wsc in config {
            info!(target: super::OBC, "Start try connect to {}", wsc.url);
            let ob = ob.clone();
            let echo_map = self.echos.clone();
            let mut signal_rx = ob.get_signal_rx()?;
            let bot_map = self.get_bot_map().clone();
            tasks.push(tokio::spawn(async move {
                while signal_rx.try_recv().is_err() {
                    let ob = ob.clone();
                    let echo_map = echo_map.clone();
                    let req = Request::builder()
                        .header(
                            USER_AGENT,
                            format!("OneBot/12 Walle-App/{}", crate::VERSION),
                        )
                        .header_auth_token(&wsc.access_token);
                    match try_connect(&wsc, req).await {
                        Some(ws_stream) => {
                            ws_loop(ob, ws_stream, echo_map, bot_map.clone()).await;
                            warn!(target: crate::WALLE_CORE, "Disconnected from {}", wsc.url);
                        }
                        None => {
                            tokio::time::sleep(std::time::Duration::from_secs(
                                wsc.reconnect_interval as u64,
                            ))
                            .await;
                        }
                    }
                }
            }));
        }
        Ok(())
    }
    pub(crate) async fn wsr<E, AH, EH>(
        &self,
        ob: &Arc<OneBot<AH, EH>>,
        config: Vec<WebSocketServer>,
        tasks: &mut Vec<JoinHandle<()>>,
    ) -> WalleResult<()>
    where
        E: ProtocolItem + GetSelf + Clone,
        AH: ActionHandler<E, A, R> + Send + Sync + 'static,
        EH: EventHandler<E, A, R> + Send + Sync + 'static,
    {
        for wss in config {
            let addr = std::net::SocketAddr::new(wss.host, wss.port);
            let tcp_listener = TcpListener::bind(&addr).await.map_err(WalleError::IO)?;
            info!(
                target: super::OBC,
                "Websocket server listening on ws://{}", addr
            );
            let ob = ob.clone();
            let mut signal_rx = ob.get_signal_rx()?;
            let echo_map = self.echos.clone();
            let bot_map = self.get_bot_map().clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = signal_rx.recv() => {
                            info!(target: super::OBC, "Stop listening on ws://{}", addr);
                            break;
                        }
                        Ok((stream, _)) = tcp_listener.accept() => {
                            if let Some((ws_stream, _implt)) =
                                upgrade_websocket(&wss.access_token, &wss.path, stream)
                                    .await
                            {
                                let ob = ob.clone();
                                tokio::spawn(ws_loop(ob.clone(), ws_stream, echo_map.clone(), bot_map.clone()));
                            }
                        }
                    }
                }
            }));
        }
        Ok(())
    }
}

async fn ws_loop<E, A, R, AH, EH>(
    ob: Arc<OneBot<AH, EH>>,
    mut ws_stream: WebSocketStream<TcpStream>,
    echo_map: EchoMap<R>,
    bot_map: Arc<super::BotMap<A>>,
    // implt: String,
) where
    E: ProtocolItem + GetSelf + Clone,
    A: ProtocolItem,
    R: ProtocolItem,
    AH: ActionHandler<E, A, R> + Send + Sync + 'static,
    EH: EventHandler<E, A, R> + Send + Sync + 'static,
{
    let (seq, mut action_rx) = bot_map.new_connect();
    let mut signal_rx = ob.get_signal_rx().unwrap(); //todo
    let mut implt = None;
    loop {
        tokio::select! {
            _ = signal_rx.recv() => break,
            Some(action) = action_rx.recv() => {
                if ws_stream.send(action.to_ws_msg(&ContentType::Json)).await.is_err() { //todo
                    break;
                }
            },
            Some(msg) = ws_stream.next() => {
                match msg {
                    Ok(msg) => if ws_recv(
                        msg,
                        &ob,
                        &mut ws_stream,
                        &echo_map,
                        &seq,
                        &mut implt,
                        &bot_map,
                    ).await {
                        break;
                    },
                    Err(_) => {
                        break;
                    }
                }
            }
        }
    }
    ws_stream.send(WsMsg::Close(None)).await.ok();
    bot_map.connect_closs(&seq);
}

async fn ws_recv<E, A, R, AH, EH>(
    msg: WsMsg,
    ob: &Arc<OneBot<AH, EH>>,
    ws_stream: &mut WebSocketStream<TcpStream>,
    echo_map: &EchoMap<R>,
    seq: &usize,
    implt: &mut Option<String>,
    bot_map: &Arc<super::BotMap<A>>,
) -> bool
where
    E: ProtocolItem + Clone + GetSelf,
    A: ProtocolItem,
    R: ProtocolItem,
    AH: ActionHandler<E, A, R> + Send + Sync + 'static,
    EH: EventHandler<E, A, R> + Send + Sync + 'static,
{
    #[derive(Debug, Deserialize, Serialize)]
    #[serde(untagged)]
    enum ReceiveItem<E, R> {
        Event(E),
        Resp(Echo<R>),
    }

    let handle_ok = |item: Result<ReceiveItem<E, R>, String>| async move {
        match item {
            Ok(ReceiveItem::Event(event)) => {
                let ob = ob.clone();
                tokio::spawn(async move { ob.handle_event(event).await });
            }
            Ok(ReceiveItem::Resp(resp)) => {
                let (r, echos) = resp.unpack();
                if let Some((_, tx)) = echo_map.remove(&echos) {
                    tx.send(r).ok();
                }
            }
            Err(s) => warn!(target: super::OBC, "serde failed: {}", s),
        }
    };

    let meta_process = |meta: Result<Event, String>| async move {
        if let Ok(event) = meta.and_then(|e: Event| {
            <MetaDetailEvent as TryFrom<Event>>::try_from(e).map_err(|e| e.to_string())
        }) {
            match event.detail_type {
                MetaTypes::Connect(c) => *implt = Some(c.version.implt),
                MetaTypes::StatusUpdate(s) => {
                    if let Some(some_implt) = implt {
                        bot_map.connect_update(seq, s.status.bots, some_implt)
                    }
                }
                _ => {}
            }
        }
    };

    match msg {
        WsMsg::Text(text) => {
            meta_process(ProtocolItem::json_decode(&text)).await;
            handle_ok(ProtocolItem::json_decode(&text)).await;
        }
        WsMsg::Binary(b) => {
            meta_process(ProtocolItem::rmp_decode(&b)).await;
            handle_ok(ProtocolItem::rmp_decode(&b)).await;
        }
        WsMsg::Ping(b) => {
            if ws_stream.send(WsMsg::Pong(b)).await.is_err() {
                return true;
            }
        }
        WsMsg::Pong(_) => {}
        WsMsg::Close(_) => return true,
        WsMsg::Frame(_) => unreachable!(),
    }
    false
}
