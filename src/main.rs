use std::{
    convert::Infallible,
    path::{Path, PathBuf},
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
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::Command,
    sync::broadcast::{channel, Sender},
    time::{sleep, Instant},
};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt as _};
use tower_cookies::{Cookie, CookieManagerLayer, Cookies};

pub type LogStream = Sender<String>;

struct AppState {
    stream_logs_toggle: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Config {
    login_key: String,
}
async fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let mut config_path = PathBuf::new();

    if let Ok(path) = std::env::var("DAGDASHMAN_CONFIG_PATH") {
        config_path.push(path);
    } else {
        config_path.push("/home/dagger/.config/dagdashman/config.toml");
    }

    if !config_path.exists() {
        // Define a default configuration
        // generate api key
        let login_key = "fake_api_key".to_string();
        let default_config = Config { login_key };

        // Serialize the default configuration to TOML
        let toml = toml::to_string(&default_config)?;

        // Ensure the config directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Create and write the default configuration file
        let mut file = fs::File::create(&config_path).await?;
        file.write_all(toml.as_bytes()).await?;

        // Optionally, set file permissions (Unix-like systems)
        #[cfg(unix)]
        {
            //fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).await?;
        }
    }

    // Now, read and parse the configuration file
    let config_contents = fs::read_to_string(config_path).await?;
    let config: Config = toml::from_str(&config_contents)?;

    Ok(config)
}

#[tokio::main]
async fn main() {
    let config;
    if let Ok(loaded_config) = load_config().await {
        config = loaded_config
    } else {
        panic!("Error loading config");
    }

    let (tx, _rx) = channel::<String>(10);

    let shared_state = Arc::new(Mutex::new(AppState {
        stream_logs_toggle: false,
    }));

    let app = Router::new()
        // No Auth
        .route("/styles.css", get(styles))
        .route("/script.js", get(script))
        .route("/login", get(handle_get_login))
        .route("/login", post(handle_post_login))
        // Authed
        .route("/", get(index))
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
        .layer(Extension(shared_state))
        .layer(Extension(config))
        .layer(CookieManagerLayer::new());

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

async fn index(cookies: Cookies) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        return Ok(IndexTemplate);
    }

    Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate;

async fn handle_get_login() -> impl IntoResponse {
    LoginTemplate
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

#[derive(Serialize, Deserialize)]
struct LoginRequest {
    login_key: String,
}

async fn handle_post_login(
    cookies: Cookies,
    Extension(config): Extension<Config>,
) -> impl IntoResponse {
    let payload_login_key = "fake_api_key";
    if payload_login_key == config.login_key {
        let cookie = Cookie::build(("auth_token", "session_value_here"))
            .http_only(true)
            .secure(true)
            .path("/");

        cookies.add(cookie.into());

        (StatusCode::OK, "Login Success")
    } else {
        (StatusCode::UNAUTHORIZED, "Invalid credentials")
    }
}

fn has_valid_auth_token(cookies: Cookies) -> bool {
    if let Some(auth_cookie) = cookies.get("auth_token") {
        return verified_auth_token(auth_cookie.value().to_string());
    }

    false
}

fn verified_auth_token(token: String) -> bool {
    if token == "session_value_here" {
        return true;
    }

    return false;
}

async fn handle_start_log_stream(
    cookies: Cookies,
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let mut shared_state_lock = shared_state.lock().unwrap();
        shared_state_lock.stream_logs_toggle = true;
        return Ok(StatusCode::OK);
    }
    Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
}

async fn handle_stop_log_stream(
    cookies: Cookies,
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let mut shared_state_lock = shared_state.lock().unwrap();
        shared_state_lock.stream_logs_toggle = false;
        return Ok(StatusCode::OK);
    } else {
        Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
    }
}

async fn handle_stream_logs(
    cookies: Cookies,
    Extension(tx): Extension<LogStream>,
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = tx.subscribe();

    let stream = BroadcastStream::new(rx);

    let is_authed = has_valid_auth_token(cookies);

    Sse::new(
        stream
            .map(move |msg| {
                if is_authed {
                    let shared_state = shared_state.lock().unwrap();
                    if shared_state.stream_logs_toggle {
                        let msg = msg.unwrap();
                        let event_data = format!("<div>{}</div>", msg);
                        Event::default().event("log-stream").data(event_data)
                    } else {
                        Event::default()
                    }
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

async fn handle_get_wield_version(
    cookies: Cookies,
    Extension(tx): Extension<LogStream>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let path_to_wield_binary = "/home/dagger/wield".to_string();

        if Path::new(&path_to_wield_binary).exists() {
            let output = Command::new("/home/dagger/wield")
                .arg("--version")
                .output()
                .await
                .expect(&format!(
                    "Should successfully run wield --version from path {}",
                    path_to_wield_binary
                ));

            let output_stdout = String::from_utf8(output.stdout).unwrap();

            if tx.send(output_stdout.clone()).is_err() {
                eprintln!("Error sending cmd output across channel.");
            };

            return (StatusCode::OK, format!("Version: {}", output_stdout));
        } else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Version: WIELD BINARY NOT FOUND".to_string(),
            );
        }
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized".to_string())
}

async fn handle_start_wield_service(cookies: Cookies) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("start")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        return StatusCode::OK;
    }

    StatusCode::UNAUTHORIZED
}

async fn handle_stop_wield_service(cookies: Cookies) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("stop")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        return StatusCode::OK;
    }

    StatusCode::UNAUTHORIZED
}

async fn handle_restart_wield_service(cookies: Cookies) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("restart")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        return StatusCode::OK;
    }
    StatusCode::UNAUTHORIZED
}

async fn handle_get_wield_status(cookies: Cookies) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
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

        return (
            StatusCode::OK,
            format!("Service Status: {}", split_output.last().unwrap()),
        );
    }
    (StatusCode::UNAUTHORIZED, "Unauthorized".to_string())
}

async fn handle_get_log_stream_status(
    cookies: Cookies,
    Extension(shared_state): Extension<Arc<Mutex<AppState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let shared_state_lock = shared_state.lock().unwrap();

        if shared_state_lock.stream_logs_toggle {
            return (StatusCode::OK, "Log Stream: active");
        } else {
            return (StatusCode::OK, "Log Stream: inactive");
        }
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized")
}
