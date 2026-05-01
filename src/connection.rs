use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use crate::game::GameCmd;
use crate::ids::{next_conn_id, next_player_id, ConnId};
use crate::lobby::LobbyCmd;
use crate::messages::{ClientBody, ClientMessage, ErrorCode, ServerBody, ServerMessage};

const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq)]
enum SessionState {
    Greeting,
    Identifying,
    Idle,
    InGame,
}

pub async fn handle_connection(stream: TcpStream, lobby_tx: mpsc::Sender<LobbyCmd>) {
    let addr = match stream.peer_addr() {
        Ok(a) => a,
        Err(e) => {
            warn!("peer_addr failed: {e}");
            return;
        }
    };
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!("ws handshake failed for {addr}: {e}");
            return;
        }
    };
    info!("connection established: {addr}");

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMessage>(64);
    let conn_id: ConnId = next_conn_id();

    // Writer task: drain out_rx into the websocket sink.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    warn!("serialize: {e}");
                    continue;
                }
            };
            if ws_write.send(WsMessage::Text(json.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_write.close().await;
    });

    let mut state = SessionState::Greeting;
    let mut player_id: Option<String> = None;
    let mut current_game: Option<mpsc::Sender<GameCmd>> = None;
    let mut current_game_id: Option<String> = None;
    let mut display_name: Option<String> = None;

    while let Some(frame) = ws_read.next().await {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                debug!("ws read error from {addr}: {e}");
                break;
            }
        };
        let text = match frame {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_) | WsMessage::Frame(_) => continue,
        };
        let msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!("parse error from {addr}: {e}");
                let _ = out_tx
                    .send(ServerMessage {
                        request_id: None,
                        body: ServerBody::Error {
                            code: ErrorCode::InvalidSessionState,
                            message: format!("invalid message: {e}"),
                        },
                    })
                    .await;
                continue;
            }
        };
        let request_id = Some(msg.request_id.clone());
        match (state.clone(), msg.body) {
            (SessionState::Greeting, ClientBody::SessionHello { protocol_version, .. }) => {
                if protocol_version != PROTOCOL_VERSION {
                    let _ = out_tx
                        .send(ServerMessage {
                            request_id,
                            body: ServerBody::Error {
                                code: ErrorCode::InvalidSessionState,
                                message: "unsupported protocol version".into(),
                            },
                        })
                        .await;
                    continue;
                }
                let pid = next_player_id();
                player_id = Some(pid.clone());
                state = SessionState::Identifying;
                let _ = out_tx
                    .send(ServerMessage {
                        request_id,
                        body: ServerBody::SessionReady { player_id: pid },
                    })
                    .await;
            }
            (SessionState::Identifying, ClientBody::PlayerIdentify { display_name: name }) => {
                if name.trim().is_empty() {
                    let _ = out_tx
                        .send(ServerMessage {
                            request_id,
                            body: ServerBody::Error {
                                code: ErrorCode::InvalidName,
                                message: "display name required".into(),
                            },
                        })
                        .await;
                    continue;
                }
                display_name = Some(name);
                state = SessionState::Idle;
            }
            (SessionState::Idle, ClientBody::LobbySubscribe(_)) => {
                let _ = lobby_tx
                    .send(LobbyCmd::Subscribe {
                        conn_id,
                        out_tx: out_tx.clone(),
                    })
                    .await;
            }
            (SessionState::Idle, ClientBody::GameCreate { topic, question_count }) => {
                let pid = match &player_id {
                    Some(p) => p.clone(),
                    None => continue,
                };
                let name = display_name.clone().unwrap_or_else(|| "Player".into());
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let _ = lobby_tx
                    .send(LobbyCmd::Create {
                        conn_id,
                        host_player_id: pid,
                        host_name: name,
                        host_out_tx: out_tx.clone(),
                        topic,
                        question_count,
                        request_id: request_id.clone(),
                        reply_tx,
                    })
                    .await;
                if let Ok(Some(game_tx)) = reply_rx.await {
                    current_game = Some(game_tx);
                    state = SessionState::InGame;
                }
            }
            (SessionState::Idle, ClientBody::GameJoin { game_id }) => {
                let pid = match &player_id {
                    Some(p) => p.clone(),
                    None => continue,
                };
                let name = display_name.clone().unwrap_or_else(|| "Player".into());
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let _ = lobby_tx
                    .send(LobbyCmd::Join {
                        conn_id,
                        player_id: pid,
                        name,
                        out_tx: out_tx.clone(),
                        game_id: game_id.clone(),
                        request_id: request_id.clone(),
                        reply_tx,
                    })
                    .await;
                if let Ok(Some(game_tx)) = reply_rx.await {
                    current_game = Some(game_tx);
                    current_game_id = Some(game_id);
                    state = SessionState::InGame;
                }
            }
            (SessionState::InGame, ClientBody::AnswerSubmit { game_id: _, question_id, answer_id }) => {
                if let (Some(tx), Some(pid)) = (&current_game, &player_id) {
                    let _ = tx
                        .send(GameCmd::Submit {
                            player_id: pid.clone(),
                            question_id,
                            answer_id,
                            request_id: request_id.clone(),
                        })
                        .await;
                }
            }
            (SessionState::InGame, ClientBody::QuestionNextReady { game_id: _, question_id }) => {
                if let (Some(tx), Some(pid)) = (&current_game, &player_id) {
                    let _ = tx
                        .send(GameCmd::NextReady {
                            player_id: pid.clone(),
                            question_id,
                            request_id: request_id.clone(),
                        })
                        .await;
                }
            }
            (SessionState::InGame, ClientBody::LobbyReturn { game_id }) => {
                if let (Some(tx), Some(pid)) = (&current_game, &player_id) {
                    let _ = tx.send(GameCmd::LobbyReturn).await;
                    let _ = pid;
                    let _ = lobby_tx
                        .send(LobbyCmd::GameFinished { game_id: game_id.clone() })
                        .await;
                }
                current_game = None;
                current_game_id = None;
                state = SessionState::Idle;
            }
            (_, _) => {
                let _ = out_tx
                    .send(ServerMessage {
                        request_id,
                        body: ServerBody::Error {
                            code: ErrorCode::InvalidSessionState,
                            message: "message not allowed in current session state".into(),
                        },
                    })
                    .await;
            }
        }
    }

    // Connection closed: notify lobby (always) and game (if any).
    let _ = lobby_tx.send(LobbyCmd::Unsubscribe { conn_id }).await;
    if let Some(tx) = current_game {
        if let Some(pid) = &player_id {
            let _ = tx.send(GameCmd::Disconnect { player_id: pid.clone() }).await;
        }
    }
    if let Some(gid) = current_game_id {
        // Best-effort: in case we were the host of a still-waiting game, ask lobby to clean up.
        let _ = lobby_tx.send(LobbyCmd::HostLeftWaiting { game_id: gid }).await;
    }
    drop(out_tx);
    let _ = writer.await;
    info!("connection closed: {addr}");
}
