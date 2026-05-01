use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod connection;
mod game;
mod ids;
mod lobby;
mod messages;
mod questions;

use connection::handle_connection;
use lobby::spawn_lobby;
use questions::QuestionRegistry;

const BIND_ADDR: &str = "127.0.0.1:9002";
const QUESTIONS_DIR: &str = "questions";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let registry = match QuestionRegistry::load_from_dir(&PathBuf::from(QUESTIONS_DIR)) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            error!("failed to load questions: {e}");
            return Err(e.into());
        }
    };

    let lobby_tx = spawn_lobby(registry);
    let listener = TcpListener::bind(BIND_ADDR).await?;
    info!("listening on {BIND_ADDR}");

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _)) => {
                        let lobby_tx = lobby_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, lobby_tx).await;
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                break;
            }
        }
    }

    Ok(())
}
