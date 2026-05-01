use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::game::{spawn_game, GameCmd, GameInit, GameSummary};
use crate::ids::{next_game_id, ConnId};
use crate::messages::{ErrorCode, Game, ServerBody, ServerMessage};
use crate::questions::QuestionRegistry;

pub enum LobbyCmd {
    Subscribe {
        conn_id: ConnId,
        out_tx: mpsc::Sender<ServerMessage>,
    },
    Unsubscribe {
        conn_id: ConnId,
    },
    Create {
        conn_id: ConnId,
        host_player_id: String,
        host_name: String,
        host_out_tx: mpsc::Sender<ServerMessage>,
        topic: String,
        question_count: u16,
        request_id: Option<String>,
        // Reply with the spawned game's command sender so the connection can talk to the game.
        reply_tx: tokio::sync::oneshot::Sender<Option<mpsc::Sender<GameCmd>>>,
    },
    Join {
        conn_id: ConnId,
        player_id: String,
        name: String,
        out_tx: mpsc::Sender<ServerMessage>,
        game_id: String,
        request_id: Option<String>,
        reply_tx: tokio::sync::oneshot::Sender<Option<mpsc::Sender<GameCmd>>>,
    },
    GameFinished {
        game_id: String,
    },
    HostLeftWaiting {
        game_id: String,
    },
}

struct WaitingGame {
    summary: GameSummary,
    cmd_tx: mpsc::Sender<GameCmd>,
}

pub struct LobbyActor {
    registry: Arc<QuestionRegistry>,
    subscribers: HashMap<ConnId, mpsc::Sender<ServerMessage>>,
    waiting: HashMap<String, WaitingGame>,
    active: HashMap<String, mpsc::Sender<GameCmd>>,
    cmd_rx: mpsc::Receiver<LobbyCmd>,
}

pub fn spawn_lobby(registry: Arc<QuestionRegistry>) -> mpsc::Sender<LobbyCmd> {
    let (tx, rx) = mpsc::channel::<LobbyCmd>(64);
    let actor = LobbyActor {
        registry,
        subscribers: HashMap::new(),
        waiting: HashMap::new(),
        active: HashMap::new(),
        cmd_rx: rx,
    };
    tokio::spawn(async move {
        let mut actor = actor;
        actor.run().await;
    });
    tx
}

impl LobbyActor {
    async fn run(&mut self) {
        while let Some(cmd) = self.cmd_rx.recv().await {
            self.handle(cmd);
        }
    }

    fn handle(&mut self, cmd: LobbyCmd) {
        match cmd {
            LobbyCmd::Subscribe { conn_id, out_tx } => {
                self.subscribers.insert(conn_id, out_tx.clone());
                let _ = out_tx.try_send(ServerMessage {
                    request_id: None,
                    body: ServerBody::LobbySnapshot { games: self.snapshot_games() },
                });
            }
            LobbyCmd::Unsubscribe { conn_id } => {
                self.subscribers.remove(&conn_id);
            }
            LobbyCmd::Create {
                conn_id,
                host_player_id,
                host_name,
                host_out_tx,
                topic,
                question_count,
                request_id,
                reply_tx,
            } => {
                self.subscribers.remove(&conn_id);
                let pool_size = match self.registry.pool_size(&topic) {
                    Some(s) => s,
                    None => {
                        let _ = host_out_tx.try_send(ServerMessage {
                            request_id: request_id.clone(),
                            body: ServerBody::Error {
                                code: ErrorCode::InvalidTopic,
                                message: format!("Unknown topic: {topic}"),
                            },
                        });
                        let _ = reply_tx.send(None);
                        return;
                    }
                };
                if (question_count as usize) == 0 || (question_count as usize) > pool_size {
                    let _ = host_out_tx.try_send(ServerMessage {
                        request_id,
                        body: ServerBody::Error {
                            code: ErrorCode::InvalidQuestionCount,
                            message: format!("questionCount must be between 1 and {pool_size}"),
                        },
                    });
                    let _ = reply_tx.send(None);
                    return;
                }
                let questions = self
                    .registry
                    .pick(&topic, question_count as usize)
                    .expect("validated above");
                let game_id = next_game_id();
                let init = GameInit {
                    id: game_id.clone(),
                    topic: topic.clone(),
                    questions,
                    host_player_id,
                    host_name,
                    host_out_tx,
                    host_request_id: request_id,
                    created_at: Utc::now(),
                };
                let (cmd_tx, summary) = spawn_game(init);
                self.waiting.insert(
                    game_id.clone(),
                    WaitingGame {
                        summary,
                        cmd_tx: cmd_tx.clone(),
                    },
                );
                let _ = reply_tx.send(Some(cmd_tx));
                self.broadcast_snapshot();
            }
            LobbyCmd::Join {
                conn_id,
                player_id,
                name,
                out_tx,
                game_id,
                request_id,
                reply_tx,
            } => {
                self.subscribers.remove(&conn_id);
                let waiting = match self.waiting.remove(&game_id) {
                    Some(w) => w,
                    None => {
                        let _ = out_tx.try_send(ServerMessage {
                            request_id,
                            body: ServerBody::Error {
                                code: ErrorCode::GameNotFound,
                                message: format!("Game {game_id} not found or already full."),
                            },
                        });
                        let _ = reply_tx.send(None);
                        return;
                    }
                };
                let game_cmd_tx = waiting.cmd_tx.clone();
                self.active.insert(game_id.clone(), game_cmd_tx.clone());
                let join_tx = game_cmd_tx.clone();
                tokio::spawn(async move {
                    let _ = join_tx
                        .send(GameCmd::Join {
                            player_id,
                            name,
                            out_tx,
                            request_id,
                        })
                        .await;
                });
                let _ = reply_tx.send(Some(game_cmd_tx));
                self.broadcast_snapshot();
            }
            LobbyCmd::GameFinished { game_id } => {
                self.active.remove(&game_id);
                if self.waiting.remove(&game_id).is_some() {
                    self.broadcast_snapshot();
                }
            }
            LobbyCmd::HostLeftWaiting { game_id } => {
                if self.waiting.remove(&game_id).is_some() {
                    self.broadcast_snapshot();
                }
            }
        }
    }

    fn snapshot_games(&self) -> Vec<Game> {
        let mut games: Vec<Game> = self
            .waiting
            .values()
            .map(|w| Game {
                id: w.summary.id.clone(),
                host_name: w.summary.host_name.clone(),
                topic: w.summary.topic.clone(),
                question_count: w.summary.question_count,
                created_at: w.summary.created_at,
            })
            .collect();
        games.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        games
    }

    fn broadcast_snapshot(&self) {
        let games = self.snapshot_games();
        for tx in self.subscribers.values() {
            let _ = tx.try_send(ServerMessage {
                request_id: None,
                body: ServerBody::LobbySnapshot { games: games.clone() },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn registry() -> Arc<QuestionRegistry> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("questions");
        Arc::new(QuestionRegistry::load_from_dir(&dir).expect("loads"))
    }

    async fn drain_until<F: FnMut(&ServerBody) -> bool>(rx: &mut mpsc::Receiver<ServerMessage>, mut pred: F) -> ServerMessage {
        loop {
            let msg = rx.recv().await.expect("channel closed");
            if pred(&msg.body) { return msg; }
        }
    }

    #[tokio::test]
    async fn subscribe_returns_initial_snapshot_to_caller() {
        let lobby = spawn_lobby(registry());
        let (tx_a, mut rx_a) = mpsc::channel(16);
        let (tx_b, mut rx_b) = mpsc::channel(16);
        lobby.send(LobbyCmd::Subscribe { conn_id: 1, out_tx: tx_a }).await.unwrap();
        let msg = rx_a.recv().await.unwrap();
        assert!(matches!(msg.body, ServerBody::LobbySnapshot { .. }));
        // B has not subscribed; should not receive anything.
        lobby.send(LobbyCmd::Subscribe { conn_id: 2, out_tx: tx_b }).await.unwrap();
        let msg = rx_b.recv().await.unwrap();
        assert!(matches!(msg.body, ServerBody::LobbySnapshot { .. }));
    }

    #[tokio::test]
    async fn create_broadcasts_to_subscribers() {
        let lobby = spawn_lobby(registry());
        let (sub_tx, mut sub_rx) = mpsc::channel(16);
        lobby.send(LobbyCmd::Subscribe { conn_id: 1, out_tx: sub_tx }).await.unwrap();
        let _ = sub_rx.recv().await; // initial snapshot

        let (host_tx, mut host_rx) = mpsc::channel(16);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        lobby.send(LobbyCmd::Create {
            conn_id: 2,
            host_player_id: "player_1".into(),
            host_name: "Mira".into(),
            host_out_tx: host_tx,
            topic: "Science".into(),
            question_count: 3,
            request_id: Some("req".into()),
            reply_tx,
        }).await.unwrap();
        let game_cmd = reply_rx.await.unwrap();
        assert!(game_cmd.is_some());
        // Subscriber receives an updated snapshot
        let msg = drain_until(&mut sub_rx, |b| matches!(b, ServerBody::LobbySnapshot { games } if !games.is_empty())).await;
        if let ServerBody::LobbySnapshot { games } = msg.body {
            assert_eq!(games.len(), 1);
            assert_eq!(games[0].topic, "Science");
        }
        // Host gets game.created
        let msg = drain_until(&mut host_rx, |b| matches!(b, ServerBody::GameCreated { .. })).await;
        assert!(matches!(msg.body, ServerBody::GameCreated { .. }));
    }

    #[tokio::test]
    async fn host_left_waiting_removes_and_rebroadcasts() {
        let lobby = spawn_lobby(registry());
        let (sub_tx, mut sub_rx) = mpsc::channel(16);
        lobby.send(LobbyCmd::Subscribe { conn_id: 1, out_tx: sub_tx }).await.unwrap();
        let _ = sub_rx.recv().await;

        let (host_tx, _host_rx) = mpsc::channel(16);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        lobby.send(LobbyCmd::Create {
            conn_id: 2,
            host_player_id: "player_1".into(),
            host_name: "Mira".into(),
            host_out_tx: host_tx,
            topic: "Science".into(),
            question_count: 3,
            request_id: None,
            reply_tx,
        }).await.unwrap();
        let _ = reply_rx.await.unwrap();
        // First broadcast (after create) — wait for the snapshot with one game.
        let msg = drain_until(&mut sub_rx, |b| matches!(b, ServerBody::LobbySnapshot { games } if games.len() == 1)).await;
        let game_id = match msg.body {
            ServerBody::LobbySnapshot { games } => games[0].id.clone(),
            _ => unreachable!(),
        };
        lobby.send(LobbyCmd::HostLeftWaiting { game_id }).await.unwrap();
        let msg = drain_until(&mut sub_rx, |b| matches!(b, ServerBody::LobbySnapshot { games } if games.is_empty())).await;
        assert!(matches!(msg.body, ServerBody::LobbySnapshot { .. }));
    }

    #[tokio::test]
    async fn invalid_topic_returns_error() {
        let lobby = spawn_lobby(registry());
        let (host_tx, mut host_rx) = mpsc::channel(16);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        lobby.send(LobbyCmd::Create {
            conn_id: 1,
            host_player_id: "player_1".into(),
            host_name: "Mira".into(),
            host_out_tx: host_tx,
            topic: "Sherlock Holmes".into(),
            question_count: 3,
            request_id: Some("req".into()),
            reply_tx,
        }).await.unwrap();
        let reply = reply_rx.await.unwrap();
        assert!(reply.is_none());
        let msg = host_rx.recv().await.unwrap();
        if let ServerBody::Error { code, .. } = msg.body {
            assert!(matches!(code, ErrorCode::InvalidTopic));
        } else {
            panic!("expected error");
        }
    }

    #[tokio::test]
    async fn invalid_question_count_returns_error() {
        let lobby = spawn_lobby(registry());
        let (host_tx, mut host_rx) = mpsc::channel(16);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        lobby.send(LobbyCmd::Create {
            conn_id: 1,
            host_player_id: "player_1".into(),
            host_name: "Mira".into(),
            host_out_tx: host_tx,
            topic: "Science".into(),
            question_count: 9999,
            request_id: Some("req".into()),
            reply_tx,
        }).await.unwrap();
        let reply = reply_rx.await.unwrap();
        assert!(reply.is_none());
        let msg = host_rx.recv().await.unwrap();
        if let ServerBody::Error { code, .. } = msg.body {
            assert!(matches!(code, ErrorCode::InvalidQuestionCount));
        } else {
            panic!("expected error");
        }
    }
}
