use std::{convert::Infallible, path::Path, time::Duration};

use askama::Template;
use axum::{
    http::{Response, StatusCode},
    response::{sse::Event, IntoResponse, Sse},
    routing::get,
    Extension, Router,
};
use logwatcher::LogWatcher;
use tokio::{
    sync::broadcast::{channel, Sender},
    time::sleep,
};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt as _};

pub type LogStream = Sender<String>;

#[tokio::main]
async fn main() {
    let (tx, _rx) = channel::<String>(10);

    let app = Router::new()
        .route("/", get(index))
        .route("/styles.css", get(styles))
        .route("/script.js", get(script))
        .route("/stream-logs", get(handle_stream_logs))
        .layer(Extension(tx.clone()));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    tokio::spawn(async move {
        let path = "/home/dagger/config.log".to_string();
        loop {
            if Path::new(&path).exists() {
                let mut log_watcher = LogWatcher::register(&path).unwrap();
                let tx = tx.clone();

                // A simple subscriber to keep the logwatcher tx sender open.
                // Without this the SSE  handle_stream_logs stream doesn't pick up any messages.
                let mut rx = tx.subscribe();
                tokio::spawn(async move { while let Ok(_msg) = rx.recv().await {} });

                log_watcher.watch(&mut move |line: String| {
                    if tx.send(line).is_err() {
                        eprintln!("Error sending sse data across channel");
                    }
                    logwatcher::LogWatcherAction::None
                });
            } else {
                eprintln!("File does not exist, re-checking in 2 seconds.");
                sleep(Duration::from_millis(2000)).await;
            }
        }
    });

    axum::serve(listener, app).await.unwrap();
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate;

async fn index() -> impl IntoResponse {
    IndexTemplate
}

async fn styles() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/css")
        .body(include_str!("../templates/styles.css").to_owned())
        .unwrap()
}

async fn script() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/css")
        .body(include_str!("../templates/script.js").to_owned())
        .unwrap()
}

async fn handle_stream_logs(
    Extension(tx): Extension<LogStream>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = tx.subscribe();

    let stream = BroadcastStream::new(rx);

    Sse::new(
        stream
            .map(|msg| {
                let msg = msg.unwrap();
                let event_data = format!("<div>{}</div>", msg);
                Event::default().data(event_data)
            })
            .map(Ok),
    )
    .keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(600))
            .text("keep-alive-text"),
    )
}
