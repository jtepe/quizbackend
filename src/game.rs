use chrono::{DateTime, Utc};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::messages::{
    Answer, ErrorCode, GamePhase, GameState, Player, RevealReason, ServerBody, ServerMessage,
};
use crate::questions::StoredQuestion;

pub const QUESTION_DURATION: Duration = Duration::from_secs(15);

pub enum GameCmd {
    Join {
        player_id: String,
        name: String,
        out_tx: mpsc::Sender<ServerMessage>,
        request_id: Option<String>,
    },
    Submit {
        player_id: String,
        question_id: String,
        answer_id: String,
        request_id: Option<String>,
    },
    NextReady {
        player_id: String,
        question_id: String,
        request_id: Option<String>,
    },
    LobbyReturn,
    Disconnect {
        player_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct GameSummary {
    pub id: String,
    pub host_name: String,
    pub topic: String,
    pub question_count: u16,
    pub created_at: DateTime<Utc>,
}

struct PlayerSlot {
    id: String,
    name: String,
    score: u16,
    out_tx: Option<mpsc::Sender<ServerMessage>>,
    answer: Option<String>,
    is_ready_for_next: bool,
}

impl PlayerSlot {
    fn new(id: String, name: String, out_tx: mpsc::Sender<ServerMessage>) -> Self {
        Self {
            id,
            name,
            score: 0,
            out_tx: Some(out_tx),
            answer: None,
            is_ready_for_next: false,
        }
    }

    fn is_connected(&self) -> bool {
        self.out_tx.is_some()
    }

    fn to_player(&self, did_answer_correctly: Option<bool>) -> Player {
        Player {
            id: self.id.clone(),
            name: self.name.clone(),
            score: Some(self.score),
            is_ready_for_next: Some(self.is_ready_for_next),
            did_answer_correctly,
        }
    }
}

pub struct GameInit {
    pub id: String,
    pub topic: String,
    pub questions: Vec<StoredQuestion>,
    pub host_player_id: String,
    pub host_name: String,
    pub host_out_tx: mpsc::Sender<ServerMessage>,
    pub host_request_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

struct GameActor {
    id: String,
    topic: String,
    questions: Vec<StoredQuestion>,
    question_index: usize,
    phase: GamePhase,
    p1: PlayerSlot,
    p2: Option<PlayerSlot>,
    deadline: Option<Instant>,
    cmd_rx: mpsc::Receiver<GameCmd>,
    finished: bool,
}

pub fn spawn_game(init: GameInit) -> (mpsc::Sender<GameCmd>, GameSummary) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<GameCmd>(32);
    let summary = GameSummary {
        id: init.id.clone(),
        host_name: init.host_name.clone(),
        topic: init.topic.clone(),
        question_count: init.questions.len() as u16,
        created_at: init.created_at,
    };

    let p1 = PlayerSlot::new(
        init.host_player_id.clone(),
        init.host_name.clone(),
        init.host_out_tx.clone(),
    );
    let host_request_id = init.host_request_id.clone();

    let actor = GameActor {
        id: init.id.clone(),
        topic: init.topic.clone(),
        questions: init.questions,
        question_index: 0,
        phase: GamePhase::WaitingForPlayer,
        p1,
        p2: None,
        deadline: None,
        cmd_rx,
        finished: false,
    };

    tokio::spawn(async move {
        let mut actor = actor;
        // Send game.created to host
        let snapshot = actor.snapshot();
        actor.send_to(
            &actor.p1.id.clone(),
            ServerMessage {
                request_id: host_request_id,
                body: ServerBody::GameCreated { game: snapshot },
            },
        );
        actor.run().await;
    });

    (cmd_tx, summary)
}

impl GameActor {
    fn snapshot(&self) -> GameState {
        let mut players = vec![self.p1.to_player(None)];
        if let Some(p2) = &self.p2 {
            players.push(p2.to_player(None));
        }
        GameState {
            id: self.id.clone(),
            topic: self.topic.clone(),
            question_count: self.questions.len() as u16,
            question_index: self.question_index as u16,
            phase: self.phase.clone(),
            players,
        }
    }

    fn send_to(&self, player_id: &str, msg: ServerMessage) {
        let slot = if self.p1.id == player_id {
            Some(&self.p1)
        } else if let Some(p2) = &self.p2 {
            if p2.id == player_id { Some(p2) } else { None }
        } else {
            None
        };
        if let Some(slot) = slot {
            if let Some(tx) = &slot.out_tx {
                let _ = tx.try_send(msg);
            }
        }
    }

    fn broadcast(&self, msg: ServerMessage) {
        if let Some(tx) = &self.p1.out_tx {
            let _ = tx.try_send(msg.clone());
        }
        if let Some(p2) = &self.p2 {
            if let Some(tx) = &p2.out_tx {
                let _ = tx.try_send(msg);
            }
        }
    }

    fn current_question(&self) -> Option<&StoredQuestion> {
        self.questions.get(self.question_index)
    }

    fn players_full_payload(&self, reveal_correct: Option<&str>) -> Vec<Player> {
        let mut out = Vec::with_capacity(2);
        let p1_correct = reveal_correct.map(|cid| self.p1.answer.as_deref() == Some(cid));
        out.push(self.p1.to_player(p1_correct));
        if let Some(p2) = &self.p2 {
            let p2_correct = reveal_correct.map(|cid| p2.answer.as_deref() == Some(cid));
            out.push(p2.to_player(p2_correct));
        }
        out
    }

    fn send_error(
        &self,
        player_id: &str,
        request_id: Option<String>,
        code: ErrorCode,
        message: &str,
    ) {
        self.send_to(
            player_id,
            ServerMessage {
                request_id,
                body: ServerBody::Error {
                    code,
                    message: message.to_string(),
                },
            },
        );
    }

    async fn run(&mut self) {
        while !self.finished {
            let timer_active = self.phase == GamePhase::QuestionActive && self.deadline.is_some();
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(c) => self.handle_cmd(c),
                        None => break,
                    }
                }
                _ = async {
                    if let Some(deadline) = self.deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if timer_active => {
                    self.reveal(RevealReason::TimeExpired);
                }
            }

            if !self.p1.is_connected()
                && self.p2.as_ref().map(|p| !p.is_connected()).unwrap_or(true)
            {
                break;
            }
        }
    }

    fn handle_cmd(&mut self, cmd: GameCmd) {
        match cmd {
            GameCmd::Join {
                player_id,
                name,
                out_tx,
                request_id,
            } => self.handle_join(player_id, name, out_tx, request_id),
            GameCmd::Submit {
                player_id,
                question_id,
                answer_id,
                request_id,
            } => {
                self.handle_submit(player_id, question_id, answer_id, request_id);
            }
            GameCmd::NextReady {
                player_id,
                question_id,
                request_id,
            } => {
                self.handle_next_ready(player_id, question_id, request_id);
            }
            GameCmd::LobbyReturn => {
                self.finished = true;
            }
            GameCmd::Disconnect { player_id } => self.handle_disconnect(player_id),
        }
    }

    fn handle_join(
        &mut self,
        player_id: String,
        name: String,
        out_tx: mpsc::Sender<ServerMessage>,
        request_id: Option<String>,
    ) {
        if self.phase != GamePhase::WaitingForPlayer || self.p2.is_some() {
            let _ = out_tx.try_send(ServerMessage {
                request_id,
                body: ServerBody::Error {
                    code: ErrorCode::GameFull,
                    message: "That game already has two players.".into(),
                },
            });
            return;
        }
        let p2 = PlayerSlot::new(player_id.clone(), name.clone(), out_tx);
        self.p2 = Some(p2);
        self.phase = GamePhase::QuestionActive;

        // game.joined to joiner
        let snapshot = self.snapshot();
        self.send_to(
            &player_id,
            ServerMessage {
                request_id,
                body: ServerBody::GameJoined { game: snapshot },
            },
        );

        // game.player_joined broadcast
        self.broadcast(ServerMessage {
            request_id: None,
            body: ServerBody::PlayerJoined {
                game_id: self.id.clone(),
                player: Player {
                    id: player_id,
                    name,
                    score: None,
                    is_ready_for_next: None,
                    did_answer_correctly: None,
                },
            },
        });

        self.start_question();
    }

    fn start_question(&mut self) {
        self.p1.answer = None;
        self.p1.is_ready_for_next = false;
        if let Some(p2) = &mut self.p2 {
            p2.answer = None;
            p2.is_ready_for_next = false;
        }
        self.phase = GamePhase::QuestionActive;
        let now = Utc::now();
        let ends_at_utc = now + chrono::Duration::from_std(QUESTION_DURATION).unwrap();
        self.deadline = Some(Instant::now() + QUESTION_DURATION);

        let question = match self.current_question() {
            Some(q) => q.question.clone(),
            None => {
                self.deadline = None;
                return;
            }
        };

        self.broadcast(ServerMessage {
            request_id: None,
            body: ServerBody::QuestionStarted {
                game_id: self.id.clone(),
                question_index: self.question_index as u16,
                question_count: self.questions.len() as u16,
                question_ends_at: ends_at_utc,
                question,
                players: self.players_full_payload(None),
            },
        });

        self.maybe_auto_reveal_on_disconnect();
    }

    fn handle_submit(
        &mut self,
        player_id: String,
        question_id: String,
        answer_id: String,
        request_id: Option<String>,
    ) {
        if self.phase != GamePhase::QuestionActive {
            self.send_error(
                &player_id,
                request_id,
                ErrorCode::QuestionTimeout,
                "Question is no longer accepting answers.",
            );
            return;
        }
        let current_qid = match self.current_question() {
            Some(q) => q.question.id.clone(),
            None => {
                self.send_error(
                    &player_id,
                    request_id,
                    ErrorCode::InvalidGamePhase,
                    "No active question.",
                );
                return;
            }
        };
        if current_qid != question_id {
            self.send_error(
                &player_id,
                request_id,
                ErrorCode::InvalidGamePhase,
                "Stale questionId.",
            );
            return;
        }

        let already_locked = if self.p1.id == player_id {
            self.p1.answer.is_some()
        } else if let Some(p2) = &self.p2 {
            if p2.id == player_id {
                p2.answer.is_some()
            } else {
                false
            }
        } else {
            false
        };

        if already_locked {
            self.send_error(
                &player_id,
                request_id.clone(),
                ErrorCode::AnswerAlreadyLocked,
                "Answer already locked.",
            );
            return;
        }

        let is_member =
            self.p1.id == player_id || self.p2.as_ref().map(|p| p.id == player_id).unwrap_or(false);
        if !is_member {
            self.send_error(
                &player_id,
                request_id,
                ErrorCode::NotAGameMember,
                "Not a member of this game.",
            );
            return;
        }

        if self.p1.id == player_id {
            self.p1.answer = Some(answer_id.clone());
        } else if let Some(p2) = self.p2.as_mut() {
            p2.answer = Some(answer_id.clone());
        }

        // answer.accepted to submitter
        self.send_to(
            &player_id,
            ServerMessage {
                request_id,
                body: ServerBody::AnswerAccepted(Answer {
                    game_id: self.id.clone(),
                    question_id: question_id.clone(),
                    player_id: player_id.clone(),
                    locked: Some(true),
                }),
            },
        );
        // answer.locked broadcast
        self.broadcast(ServerMessage {
            request_id: None,
            body: ServerBody::AnswerLocked(Answer {
                game_id: self.id.clone(),
                question_id,
                player_id,
                locked: None,
            }),
        });

        // Reveal if both have answered, or if opponent is disconnected.
        let p1_done = self.p1.answer.is_some() || !self.p1.is_connected();
        let p2_done = match &self.p2 {
            Some(p) => p.answer.is_some() || !p.is_connected(),
            None => true,
        };
        if p1_done && p2_done {
            self.reveal(RevealReason::BothAnswered);
        }
    }

    fn reveal(&mut self, reason: RevealReason) {
        if self.phase != GamePhase::QuestionActive {
            return;
        }
        let correct_id = match self.current_question() {
            Some(q) => q.correct_answer_id.clone(),
            None => return,
        };
        let qid = self.current_question().unwrap().question.id.clone();

        // Score
        if self.p1.answer.as_deref() == Some(correct_id.as_str()) {
            self.p1.score += 1;
        }
        if let Some(p2) = self.p2.as_mut() {
            if p2.answer.as_deref() == Some(correct_id.as_str()) {
                p2.score += 1;
            }
        }

        self.phase = GamePhase::AnswerReveal;
        self.deadline = None;
        // For disconnected players in reveal/waiting_for_next, treat as ready immediately.
        if !self.p1.is_connected() {
            self.p1.is_ready_for_next = true;
        }
        if let Some(p2) = self.p2.as_mut() {
            if !p2.is_connected() {
                p2.is_ready_for_next = true;
            }
        }

        let players = self.players_full_payload(Some(&correct_id));

        self.broadcast(ServerMessage {
            request_id: None,
            body: ServerBody::QuestionRevealed {
                game_id: self.id.clone(),
                question_id: qid,
                correct_answer_id: correct_id,
                players,
                reason,
            },
        });

        // If both already ready (e.g. both disconnected), advance.
        self.maybe_advance_after_ready();
    }

    fn handle_next_ready(
        &mut self,
        player_id: String,
        question_id: String,
        request_id: Option<String>,
    ) {
        if self.phase != GamePhase::AnswerReveal && self.phase != GamePhase::WaitingForNext {
            self.send_error(
                &player_id,
                request_id,
                ErrorCode::InvalidGamePhase,
                "Not in reveal phase.",
            );
            return;
        }
        let current_qid = match self.current_question() {
            Some(q) => q.question.id.clone(),
            None => return,
        };
        if current_qid != question_id {
            self.send_error(
                &player_id,
                request_id,
                ErrorCode::InvalidGamePhase,
                "Stale questionId.",
            );
            return;
        }

        if self.p1.id == player_id {
            self.p1.is_ready_for_next = true;
        } else if let Some(p2) = self.p2.as_mut() {
            if p2.id == player_id {
                p2.is_ready_for_next = true;
            }
        }

        let p1_ready = self.p1.is_ready_for_next || !self.p1.is_connected();
        let p2_ready = self
            .p2
            .as_ref()
            .map(|p| p.is_ready_for_next || !p.is_connected())
            .unwrap_or(true);

        if p1_ready && p2_ready {
            self.advance();
        } else {
            self.phase = GamePhase::WaitingForNext;
            self.broadcast(ServerMessage {
                request_id: None,
                body: ServerBody::NextWaiting {
                    game_id: self.id.clone(),
                    question_id: current_qid,
                    players: self.players_full_payload(None),
                },
            });
        }
    }

    fn maybe_advance_after_ready(&mut self) {
        if self.phase != GamePhase::AnswerReveal && self.phase != GamePhase::WaitingForNext {
            return;
        }
        let p1_ready = self.p1.is_ready_for_next || !self.p1.is_connected();
        let p2_ready = self
            .p2
            .as_ref()
            .map(|p| p.is_ready_for_next || !p.is_connected())
            .unwrap_or(true);
        if p1_ready && p2_ready {
            self.advance();
        }
    }

    fn advance(&mut self) {
        self.question_index += 1;
        if self.question_index >= self.questions.len() {
            self.emit_results();
        } else {
            self.start_question();
        }
    }

    fn emit_results(&mut self) {
        self.phase = GamePhase::Results;
        let p1 = self.p1.to_player(None);
        let p2_opt = self.p2.as_ref().map(|p| p.to_player(None));

        let (winner_id, winner_label) = match &p2_opt {
            Some(p2) => {
                let s1 = p1.score.unwrap_or(0);
                let s2 = p2.score.unwrap_or(0);
                if s1 > s2 {
                    (Some(p1.id.clone()), format!("{} wins", p1.name))
                } else if s2 > s1 {
                    (Some(p2.id.clone()), format!("{} wins", p2.name))
                } else {
                    (None, "Draw game".to_string())
                }
            }
            None => (Some(p1.id.clone()), format!("{} wins", p1.name)),
        };

        let mut players = vec![p1];
        if let Some(p2) = p2_opt {
            players.push(p2);
        }

        self.broadcast(ServerMessage {
            request_id: None,
            body: ServerBody::GameResults {
                game_id: self.id.clone(),
                players,
                winner_player_id: winner_id,
                winner_label,
            },
        });
    }

    fn handle_disconnect(&mut self, player_id: String) {
        if self.p1.id == player_id {
            self.p1.out_tx = None;
        } else if let Some(p2) = self.p2.as_mut() {
            if p2.id == player_id {
                p2.out_tx = None;
            }
        }

        match self.phase {
            GamePhase::WaitingForPlayer => {
                // Host left; lobby actor handles cleanup; we just exit.
                self.finished = true;
            }
            GamePhase::QuestionActive => {
                self.maybe_auto_reveal_on_disconnect();
            }
            GamePhase::AnswerReveal | GamePhase::WaitingForNext => {
                // Mark disconnected as ready and advance if other side is also ready.
                if !self.p1.is_connected() {
                    self.p1.is_ready_for_next = true;
                }
                if let Some(p2) = self.p2.as_mut() {
                    if !p2.is_connected() {
                        p2.is_ready_for_next = true;
                    }
                }
                self.maybe_advance_after_ready();
            }
            GamePhase::Results => {}
        }
    }

    fn maybe_auto_reveal_on_disconnect(&mut self) {
        if self.phase != GamePhase::QuestionActive {
            return;
        }
        let p1_done = self.p1.answer.is_some() || !self.p1.is_connected();
        let p2_done = match &self.p2 {
            Some(p) => p.answer.is_some() || !p.is_connected(),
            None => return,
        };
        if p1_done && p2_done {
            // If neither answered, treat as time_expired; if at least one answered, both_answered.
            let any_answer = self.p1.answer.is_some()
                || self.p2.as_ref().and_then(|p| p.answer.as_ref()).is_some();
            let reason = if any_answer {
                RevealReason::BothAnswered
            } else {
                RevealReason::TimeExpired
            };
            self.reveal(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::QuestionOption;

    fn make_q(id: &str, correct: &str) -> StoredQuestion {
        StoredQuestion {
            question: crate::messages::Question {
                id: id.into(),
                topic: "Test".into(),
                prompt: "?".into(),
                options: vec![
                    QuestionOption {
                        id: "a".into(),
                        label: "A".into(),
                        text: "A".into(),
                    },
                    QuestionOption {
                        id: "b".into(),
                        label: "B".into(),
                        text: "B".into(),
                    },
                ],
            },
            correct_answer_id: correct.into(),
        }
    }

    async fn drain_until<F: FnMut(&ServerBody) -> bool>(
        rx: &mut mpsc::Receiver<ServerMessage>,
        mut pred: F,
    ) -> ServerMessage {
        loop {
            let msg = rx.recv().await.expect("channel closed");
            if pred(&msg.body) {
                return msg;
            }
        }
    }

    fn setup(
        qs: Vec<StoredQuestion>,
    ) -> (
        mpsc::Sender<GameCmd>,
        mpsc::Receiver<ServerMessage>,
        mpsc::Receiver<ServerMessage>,
        String,
        String,
    ) {
        let (host_tx, host_rx) = mpsc::channel(64);
        let (joiner_tx, joiner_rx) = mpsc::channel(64);
        let p1_id = "player_1".to_string();
        let p2_id = "player_2".to_string();
        let init = GameInit {
            id: "game_1".into(),
            topic: "Test".into(),
            questions: qs,
            host_player_id: p1_id.clone(),
            host_name: "Mira".into(),
            host_out_tx: host_tx,
            host_request_id: Some("req_create".into()),
            created_at: Utc::now(),
        };
        let (cmd_tx, _summary) = spawn_game(init);
        let p2 = p2_id.clone();
        let cmd_tx_clone = cmd_tx.clone();
        // Send Join synchronously via blocking_send replacement: just send asynchronously below
        tokio::spawn(async move {
            cmd_tx_clone
                .send(GameCmd::Join {
                    player_id: p2,
                    name: "Jonah".into(),
                    out_tx: joiner_tx,
                    request_id: Some("req_join".into()),
                })
                .await
                .ok();
        });
        (cmd_tx, host_rx, joiner_rx, p1_id, p2_id)
    }

    #[tokio::test]
    async fn both_correct_scores_plus_one() {
        let qs = vec![make_q("q1", "a"), make_q("q2", "b")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;

        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();

        let msg = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        match msg.body {
            ServerBody::QuestionRevealed {
                reason, players, ..
            } => {
                assert!(matches!(reason, RevealReason::BothAnswered));
                assert_eq!(players[0].score, Some(1));
                assert_eq!(players[1].score, Some(1));
                assert_eq!(players[0].did_answer_correctly, Some(true));
                assert_eq!(players[1].did_answer_correctly, Some(true));
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn one_correct_one_wrong() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1,
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2,
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        if let ServerBody::QuestionRevealed { players, .. } = msg.body {
            assert_eq!(players[0].score, Some(1));
            assert_eq!(players[1].score, Some(0));
        }
    }

    #[tokio::test]
    async fn timer_expires_with_partial_answer() {
        tokio::time::pause();
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, _p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1,
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        // Drain accepted/locked
        tokio::time::advance(QUESTION_DURATION + Duration::from_secs(1)).await;
        let msg = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        if let ServerBody::QuestionRevealed {
            reason, players, ..
        } = msg.body
        {
            assert!(matches!(reason, RevealReason::TimeExpired));
            assert_eq!(players[0].did_answer_correctly, Some(true));
            assert_eq!(players[1].did_answer_correctly, Some(false));
        }
    }

    #[tokio::test]
    async fn both_wrong_zero_scores() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1,
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2,
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        if let ServerBody::QuestionRevealed { players, .. } = msg.body {
            assert_eq!(players[0].score, Some(0));
            assert_eq!(players[1].score, Some(0));
        }
    }

    #[tokio::test]
    async fn disconnect_during_active_with_other_answer_reveals() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Disconnect { player_id: p2 })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        if let ServerBody::QuestionRevealed { players, .. } = msg.body {
            assert_eq!(players[0].score, Some(1));
            assert_eq!(players[1].score, Some(0));
        }
    }

    #[tokio::test]
    async fn disconnect_during_reveal_advances_after_other_ready() {
        let qs = vec![make_q("q1", "a"), make_q("q2", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Disconnect { player_id: p2 })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::NextReady {
                player_id: p1,
                question_id: "q1".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
    }

    #[tokio::test]
    async fn last_question_emits_results_with_winner() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2.clone(),
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::NextReady {
                player_id: p1.clone(),
                question_id: "q1".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::NextReady {
                player_id: p2.clone(),
                question_id: "q1".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| matches!(b, ServerBody::GameResults { .. })).await;
        if let ServerBody::GameResults {
            winner_player_id,
            winner_label,
            ..
        } = msg.body
        {
            assert_eq!(winner_player_id.as_deref(), Some(p1.as_str()));
            assert_eq!(winner_label, "Mira wins");
        }
    }

    #[tokio::test]
    async fn results_tie_has_null_winner() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p2.clone(),
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::NextReady {
                player_id: p1,
                question_id: "q1".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::NextReady {
                player_id: p2,
                question_id: "q1".into(),
                request_id: None,
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| matches!(b, ServerBody::GameResults { .. })).await;
        if let ServerBody::GameResults {
            winner_player_id,
            winner_label,
            ..
        } = msg.body
        {
            assert!(winner_player_id.is_none());
            assert_eq!(winner_label, "Draw game");
        }
    }

    #[tokio::test]
    async fn answer_after_timeout_returns_error() {
        tokio::time::pause();
        let qs = vec![make_q("q1", "a"), make_q("q2", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, _p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        tokio::time::advance(QUESTION_DURATION + Duration::from_secs(1)).await;
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionRevealed { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1,
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: Some("late".into()),
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| matches!(b, ServerBody::Error { .. })).await;
        if let ServerBody::Error { code, .. } = msg.body {
            assert!(matches!(code, ErrorCode::QuestionTimeout));
        }
    }

    #[tokio::test]
    async fn double_submit_returns_error() {
        let qs = vec![make_q("q1", "a")];
        let (cmd_tx, mut hrx, mut jrx, p1, _p2) = setup(qs);
        let _ = drain_until(&mut hrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        let _ = drain_until(&mut jrx, |b| {
            matches!(b, ServerBody::QuestionStarted { .. })
        })
        .await;
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1.clone(),
                question_id: "q1".into(),
                answer_id: "a".into(),
                request_id: None,
            })
            .await
            .unwrap();
        cmd_tx
            .send(GameCmd::Submit {
                player_id: p1,
                question_id: "q1".into(),
                answer_id: "b".into(),
                request_id: Some("dup".into()),
            })
            .await
            .unwrap();
        let msg = drain_until(&mut hrx, |b| matches!(b, ServerBody::Error { .. })).await;
        if let ServerBody::Error { code, .. } = msg.body {
            assert!(matches!(code, ErrorCode::AnswerAlreadyLocked));
        }
    }
}
