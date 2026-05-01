use std::sync::atomic::{AtomicU64, Ordering};

static PLAYER_COUNTER: AtomicU64 = AtomicU64::new(0);
static GAME_COUNTER: AtomicU64 = AtomicU64::new(0);
static CONN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn next_player_id() -> String {
    let n = PLAYER_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("player_{n}")
}

pub fn next_game_id() -> String {
    let n = GAME_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("game_{n}")
}

pub type ConnId = u64;

pub fn next_conn_id() -> ConnId {
    CONN_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
}
