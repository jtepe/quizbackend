# quizbackend

WebSocket backend for the [webquiz](../webquiz) two-player quiz game. The server is authoritative for lobby, game phase, question payloads, timing, and scoring; the frontend only sends user intents.

See [`protocol.md`](./protocol.md) for the full wire protocol.

## Layout

```
src/
  main.rs         # entrypoint: tracing init, TCP accept loop, ctrl-c shutdown
  messages.rs     # serde wire types (ClientMessage, ServerMessage, GameState, ...)
  questions.rs    # loads questions/*.json into a registry; random subset selection
  ids.rs          # atomic counters for player_/game_/conn ids
  connection.rs   # per-WebSocket task: session state machine, routes to actors
  lobby.rs        # singleton lobby actor: subscribers, waiting games, snapshots
  game.rs         # per-game actor: phase machine, timer, scoring, reveal, results
questions/        # topic JSON files (Science, Nerd Stuff, Geography)
```

Architecture: one task per WebSocket connection, one singleton lobby actor, one task per active game. State is owned by exactly one task; communication is via `tokio::sync::mpsc`. No shared mutable state.

## Run

```sh
cargo run
```

Listens on `127.0.0.1:9002`. Set `RUST_LOG=debug` for verbose logs (default: `info`).

Point the frontend at this server via its `VITE_WS_URL` env var, e.g. `VITE_WS_URL=ws://127.0.0.1:9002`.

## Test

```sh
cargo test
```

Covers serialization round-trips, the question loader, lobby behavior, and the game-actor state machine (scoring, reveal timing, disconnect handling, error paths). Timer-based tests use `tokio::time::pause()` and run instantly.

## Configuration

Hard-coded for now:

| | |
|---|---|
| Bind address | `127.0.0.1:9002` |
| Questions dir | `./questions/` |
| Question time limit | 15s |
| Protocol version | 1 |
