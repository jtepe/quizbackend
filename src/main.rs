use futures_util::{TryStreamExt, future, stream::StreamExt};
use std::error::Error;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

mod messages;
use messages::*;

fn parse() -> Result<(), Box<dyn Error>> {
    let message = ServerMessage {
        request_id: Some("request-4".to_string()),
        body: ServerBody::GameResults {
            game_id: "game-1".to_string(),
            players: vec![
                Player {
                    id: "player-1".to_string(),
                    name: "Mirah".to_string(),
                    score: Some(2),
                    is_ready_for_next: Some(false),
                    did_answer_correctly: None,
                },
                Player {
                    id: "player-2".to_string(),
                    name: "Jonah".to_string(),
                    score: Some(2),
                    is_ready_for_next: Some(false),
                    did_answer_correctly: None,
                },
            ],
            winner_player_id: None,
            winner_label: "Draw game".to_string(),
        },
    };
    println!("=== JSON ===");
    println!("{}", serde_json::to_string_pretty(&message)?);

    println!("\n=== TOML ===");
    println!("{}", toml::to_string_pretty(&message)?);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let listener = TcpListener::bind(("localhost", 9002)).await?;
    info!("listening on localhost:9002");

    while let Ok((stream, _)) = listener.accept().await {
        info!("accepted connection");
        accept_connection(stream).await;
    }

    Ok(())
}

async fn accept_connection(stream: TcpStream) {
    let addr = stream.peer_addr().expect("streams have a peer address");
    info!("peer address = {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .expect("successful websocket handshake");

    info!("new websocket connection: {}", addr);

    let (write, read) = ws_stream.split();
    read.try_filter(|msg| future::ready(msg.is_text()))
        .forward(write)
        .await
        .expect("messages are forwarded");
}
