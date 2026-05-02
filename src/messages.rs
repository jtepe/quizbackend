use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClientMessage {
    pub request_id: String,
    #[serde(flatten)]
    pub body: ClientBody,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(tag = "type", content = "payload", rename_all_fields = "camelCase")]
pub enum ClientBody {
    #[serde(rename = "session.hello")]
    SessionHello {
        protocol_version: u16,
        client_version: String,
        resume_token: Option<String>,
    },
    #[serde(rename = "player.identify")]
    PlayerIdentify { display_name: String },
    #[serde(rename = "lobby.subscribe")]
    LobbySubscribe(EmptyPayload),
    #[serde(rename = "game.create")]
    GameCreate { topic: String, question_count: u16 },
    #[serde(rename = "game.join")]
    GameJoin { game_id: String },
    #[serde(rename = "answer.submit")]
    AnswerSubmit {
        game_id: String,
        question_id: String,
        answer_id: String,
    },
    #[serde(rename = "question.next.ready")]
    QuestionNextReady {
        game_id: String,
        question_id: String,
    },
    #[serde(rename = "lobby.return")]
    LobbyReturn { game_id: String },
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerMessage {
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub body: ServerBody,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(tag = "type", content = "payload", rename_all_fields = "camelCase")]
pub enum ServerBody {
    #[serde(rename = "session.ready")]
    SessionReady { player_id: String },
    #[serde(rename = "lobby.snapshot")]
    LobbySnapshot { games: Vec<Game> },
    #[serde(rename = "game.created")]
    GameCreated { game: GameState },
    #[serde(rename = "game.joined")]
    GameJoined { game: GameState },
    #[serde(rename = "game.player_joined")]
    PlayerJoined { game_id: String, player: Player },
    #[serde(rename = "question.started")]
    QuestionStarted {
        game_id: String,
        question_index: u16,
        question_count: u16,
        question_ends_at: DateTime<Utc>,
        question: Question,
        players: Vec<Player>,
    },
    #[serde(rename = "answer.accepted")]
    AnswerAccepted(Answer),
    #[serde(rename = "answer.locked")]
    AnswerLocked(Answer),
    #[serde(rename = "question.revealed")]
    QuestionRevealed {
        game_id: String,
        question_id: String,
        correct_answer_id: String,
        players: Vec<Player>,
        reason: RevealReason,
    },
    #[serde(rename = "question.next.waiting")]
    NextWaiting {
        game_id: String,
        question_id: String,
        players: Vec<Player>,
    },
    #[serde(rename = "game.results")]
    GameResults {
        game_id: String,
        players: Vec<Player>,
        winner_player_id: Option<String>,
        winner_label: String,
    },
    #[serde(rename = "error")]
    Error { code: ErrorCode, message: String },
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub host_name: String,
    pub topic: String,
    pub question_count: u16,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GameState {
    pub id: String,
    pub topic: String,
    pub question_count: u16,
    pub question_index: u16,
    pub phase: GamePhase,
    pub players: Vec<Player>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ready_for_next: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_answer_correctly: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GamePhase {
    WaitingForPlayer,
    QuestionActive,
    AnswerReveal,
    WaitingForNext,
    Results,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub topic: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    pub text: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub game_id: String,
    pub question_id: String,
    pub player_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RevealReason {
    BothAnswered,
    TimeExpired,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidName,
    LobbyNotSubscribed,
    GameNotFound,
    GameFull,
    InvalidQuestionCount,
    InvalidGamePhase,
    InvalidSessionState,
    InvalidTopic,
    AnswerAlreadyLocked,
    QuestionTimeout,
    NotAGameMember,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct EmptyPayload {}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_client(json: &str) {
        let parsed: ClientMessage = serde_json::from_str(json).expect("parse");
        let back = serde_json::to_string(&parsed).expect("serialize");
        let _: ClientMessage = serde_json::from_str(&back).expect("reparse");
    }

    fn roundtrip_server(json: &str) {
        let parsed: ServerMessage = serde_json::from_str(json).expect("parse");
        let back = serde_json::to_string(&parsed).expect("serialize");
        let _: ServerMessage = serde_json::from_str(&back).expect("reparse");
    }

    #[test]
    fn client_message_roundtrips() {
        roundtrip_client(
            r#"{"type":"session.hello","requestId":"r1","payload":{"protocolVersion":1,"clientVersion":"x","resumeToken":null}}"#,
        );
        roundtrip_client(
            r#"{"type":"player.identify","requestId":"r2","payload":{"displayName":"Mira"}}"#,
        );
        roundtrip_client(r#"{"type":"lobby.subscribe","requestId":"r3","payload":{}}"#);
        roundtrip_client(
            r#"{"type":"game.create","requestId":"r4","payload":{"topic":"Science","questionCount":5}}"#,
        );
        roundtrip_client(r#"{"type":"game.join","requestId":"r5","payload":{"gameId":"game_1"}}"#);
        roundtrip_client(
            r#"{"type":"answer.submit","requestId":"r6","payload":{"gameId":"game_1","questionId":"q1","answerId":"a"}}"#,
        );
        roundtrip_client(
            r#"{"type":"question.next.ready","requestId":"r7","payload":{"gameId":"game_1","questionId":"q1"}}"#,
        );
        roundtrip_client(
            r#"{"type":"lobby.return","requestId":"r8","payload":{"gameId":"game_1"}}"#,
        );
    }

    #[test]
    fn server_message_roundtrips() {
        roundtrip_server(
            r#"{"requestId":"r1","type":"session.ready","payload":{"playerId":"player_1"}}"#,
        );
        roundtrip_server(r#"{"requestId":null,"type":"lobby.snapshot","payload":{"games":[]}}"#);
        roundtrip_server(
            r#"{"requestId":null,"type":"error","payload":{"code":"INVALID_TOPIC","message":"x"}}"#,
        );
    }

    #[test]
    fn timestamps_serialize_with_z_suffix() {
        let now: DateTime<Utc> = Utc::now();
        let s = serde_json::to_string(&now).unwrap();
        assert!(s.ends_with("Z\""), "expected Z suffix, got {s}");
    }

    #[test]
    fn game_phase_serializes_to_protocol_strings() {
        let v = serde_json::to_string(&GamePhase::WaitingForPlayer).unwrap();
        assert_eq!(v, "\"waiting_for_player\"");
        let v = serde_json::to_string(&GamePhase::QuestionActive).unwrap();
        assert_eq!(v, "\"question_active\"");
    }
}
