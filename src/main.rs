use std::{
    convert::Infallible,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use askama::Template;
use axum::{
    http::{Response, StatusCode},
    response::{sse::Event, IntoResponse, Sse},
    routing::{get, post, put},
    Extension, Router,
};
use logwatcher::LogWatcher;
use tokio::{
    process::Command, sync::broadcast::{channel, Sender}, time::{sleep, Instant}
};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt as _};

pub type LogStream = Sender<String>;

struct AppState {
    stream_logs_toggle: bool,
}

#[tokio::main]
async fn main() {
    let (tx, _rx) = channel::<String>(10);

    let shared_state = Arc::new(Mutex::new(AppState {
        stream_logs_toggle: false,
    }));

    let app = Router::new()
        .route("/", get(index))
        .route("/styles.css", get(styles))
        .route("/script.js", get(script))
        .route("/start-log-stream", put(handle_start_log_stream))
        .route("/stop-log-stream", put(handle_stop_log_stream))
        .route("/stream-logs", get(handle_stream_logs))
        .route("/log-stream/status", get(handle_get_log_stream_status))
        .route("/wield/service/version", get(handle_get_wield_version))
        .route("/wield/service/status", get(handle_get_wield_status))
        .route("/wield/service/start", post(handle_start_wield_service))
        .route("/wield/service/stop", post(handle_stop_wield_service))
        .route("/wield/service/restart", post(handle_restart_wield_service))
        .layer(Extension(tx.clone()))
        .layer(Extension(shared_state));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listener bound to port 3000");
    tokio::spawn(async move {
        let path = "/home/dagger/config.log".to_string();
        loop {
            if Path::new(&path).exists() {
                let mut log_watcher = LogWatcher::register(&path).unwrap();
                let tx = tx.clone();

                // simple subscriber used to ensure a constant reciever for this channel
                // without this, the log_watcher tx.send doesn't make it to the
                // sse handler subscriber.
                let mut rx = tx.subscribe();
                tokio::spawn(async move { while let Ok(_msg) = rx.recv().await {} });
                let mut batch = Vec::new();
                let batch_send_interval = Duration::from_secs(1); // Adjust as needed
                let mut last_sent = Instant::now();

                log_watcher.watch(&mut move |line: String| {
                    batch.push(line);
                    if last_sent.elapsed() >= batch_send_interval {
                        if let Err(e) = tx.send(batch.join("\n")) {
                            eprintln!("Error sending sse data across channel: {}", e);
                        }
                        batch.clear();
                        last_sent = Instant::now();
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
        .header("Content-Type", "text/javascript")
        .body(include_str!("../templates/script.js").to_owned())
        .unwrap()
}

async fn handle_start_log_stream(
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    let mut shared_state_lock = shared_state.lock().unwrap();
    shared_state_lock.stream_logs_toggle = true;
    StatusCode::OK
}

async fn handle_stop_log_stream(
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    let mut shared_state_lock = shared_state.lock().unwrap();
    shared_state_lock.stream_logs_toggle = false;
    StatusCode::OK
}

async fn handle_stream_logs(
    Extension(tx): Extension<LogStream>,
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = tx.subscribe();

    let stream = BroadcastStream::new(rx);

    Sse::new(
        stream
            .map(move |msg| {
                let shared_state = shared_state.lock().unwrap();
                if shared_state.stream_logs_toggle {
                    let msg = msg.unwrap();
                    let event_data = format!("<div>{}</div>", msg);
                    Event::default().event("log-stream").data(event_data)
                } else {
                    Event::default()
                }
            })
            .map(Ok),
    )
    .keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(600))
            .text("keep-alive-text"),
    )
}
async fn handle_get_wield_version(Extension(tx): Extension<LogStream>) -> String {
    let output = Command::new("/home/dagger/wield")
        .arg("--version")
        .output()
        .await
        .expect("Should successfully run shell command");
    
    let output_stdout = String::from_utf8(output.stdout).unwrap();

    if tx.send(output_stdout.clone()).is_err() {
        eprintln!("Error sending cmd output across channel.");
    };

    format!("Version: {}", output_stdout)
}

async fn handle_start_wield_service() -> impl IntoResponse {
    Command::new("systemctl")
        .arg("start")
        .arg("wield")
        .output()
        .await
        .expect("Should successfully run shell command");
    
    StatusCode::OK
}

async fn handle_stop_wield_service() -> impl IntoResponse {
    Command::new("systemctl")
        .arg("stop")
        .arg("wield")
        .output()
        .await
        .expect("Should successfully run shell command");
    
    StatusCode::OK
}

async fn handle_restart_wield_service() -> impl IntoResponse {
    Command::new("systemctl")
        .arg("restart")
        .arg("wield")
        .output()
        .await
        .expect("Should successfully run shell command");
    
    StatusCode::OK
}

async fn handle_get_wield_status() -> impl IntoResponse {
    let output = Command::new("systemctl")
        .arg("show")
        .arg("-p")
        .arg("ActiveState")
        .arg("wield")
        .output()
        .await
        .expect("Should successfully run shell command");
    
    let output_stdout = String::from_utf8(output.stdout).unwrap();
    

    let split_output: Vec<&str> = output_stdout.split("=").collect();

    format!("Service Status: {}", split_output.last().unwrap())
}

async fn handle_get_log_stream_status(
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> &'static str {
    let shared_state_lock = shared_state.lock().unwrap();
    
    if shared_state_lock.stream_logs_toggle {
        "Log Stream: active"
    } else {
        "Log Stream: inactive"
    }
}
