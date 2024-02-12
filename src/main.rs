use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use askama::Template;
use axum::{
    extract::FromRef,
    http::{Response, StatusCode},
    response::{sse::Event, IntoResponse, Sse},
    routing::{get, post, put},
    Extension, Form, Router,
};
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar};
use logwatcher::LogWatcher;
use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::Command,
    sync::broadcast::{channel, Sender},
    time::{sleep, Instant},
};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt as _};

pub type LogStream = Sender<String>;

#[derive(Clone)]
struct SharedState {
    stream_logs_toggle: bool,
}

#[derive(Clone)]
struct AppState {
    key: Key,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.key.clone()
    }
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

    let shared_state = Arc::new(Mutex::new(SharedState {
        stream_logs_toggle: false,
    }));

    let app_state = AppState {
        key: Key::generate(),
    };

    let app = Router::new()
        // No Auth
        .route("/styles.css", get(styles))
        .route("/script.js", get(script))
        .route("/login", get(handle_get_login).post(handle_post_login))
        .route("/logout", put(handle_logout))
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
        .with_state(app_state);

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

async fn index(cookies: PrivateCookieJar) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        return Ok(IndexTemplate);
    }

    Err((StatusCode::UNAUTHORIZED, LoginTemplate))
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

#[derive(Template)]
#[template(path = "index-content.html")]
struct IndexContentTemplate;

#[derive(Serialize, Deserialize)]
struct LoginForm {
    password: String,
}

async fn handle_post_login(
    Extension(config): Extension<Config>,
    cookies: PrivateCookieJar,
    Form(login_form): Form<LoginForm>,
) -> impl IntoResponse {
    let payload_login_key = login_form.password.to_string();
    if payload_login_key == config.login_key {
        let cookie = Cookie::build(("auth_token", "session_value_here"))
            .http_only(true)
            .secure(true)
            .path("/");

        return Ok((cookies.add(cookie), IndexContentTemplate));
    } else {
        Err((StatusCode::UNAUTHORIZED, "Invalid credentials"))
    }
}

fn has_valid_auth_token(cookies: PrivateCookieJar) -> bool {
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

async fn handle_logout(cookies: PrivateCookieJar) -> impl IntoResponse {
    (
        StatusCode::OK,
        cookies.remove(Cookie::from("auth_token")),
        LoginTemplate,
    )
}

async fn handle_start_log_stream(
    cookies: PrivateCookieJar,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let mut shared_state_lock = shared_state.lock().unwrap();
        shared_state_lock.stream_logs_toggle = true;
        return Ok(StatusCode::OK);
    }
    Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
}

async fn handle_stop_log_stream(
    cookies: PrivateCookieJar,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
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
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx_new, rx_new) = tokio::sync::mpsc::channel(100);

    let mut rx = tx.subscribe();
    let is_authed = has_valid_auth_token(cookies);
    tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            let tx_new_clone = tx_new.clone();
            let shared_state = shared_state.clone();
            let is_authed = is_authed.clone();
            async move {
                // This is where you process each message
                // For example, you could send multiple events based on the message content
                if is_authed {
                    if let Some(uptime_metrics) = parse_uptime_metrics_entry(msg.as_ref()) {
                        println!("UPTIME METRICS: {:?}", uptime_metrics);
                        let event_data = format!("is_up: {}", uptime_metrics.is_up.to_string());
                        let event = Event::default().event("metric-is-up").data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;
                    }

                    if let Some(node_id) = parse_node_id(msg.as_ref()) {
                        println!("Node ID: {:?}", node_id);
                    }

                    let shared_state = shared_state.lock().unwrap().clone();
                    if shared_state.stream_logs_toggle {
                        let msg = msg;
                        let event_data = format!("<div>{}</div>", msg);
                        let event = Event::default().event("log-stream").data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;
                    }
                }
            }.await;
        }
    });

    let event_stream = tokio_stream::wrappers::ReceiverStream::new(rx_new);
    Sse::new(event_stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive-text"), // Some browsers or proxies need non-empty messages
    )
}

async fn handle_get_wield_version(
    cookies: PrivateCookieJar,
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

async fn handle_start_wield_service(cookies: PrivateCookieJar) -> impl IntoResponse {
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

async fn handle_stop_wield_service(cookies: PrivateCookieJar) -> impl IntoResponse {
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

async fn handle_restart_wield_service(cookies: PrivateCookieJar) -> impl IntoResponse {
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

async fn handle_get_wield_status(cookies: PrivateCookieJar) -> impl IntoResponse {
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
    cookies: PrivateCookieJar,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
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

#[derive(Debug)]
struct UptimeMetrics {
    node_id: String,
    is_up: bool,
    start_ts: u64,
    current_uptime_ms: u64,
    uptime_added_ms: u64,
    last_successful_sync_ts: u64,
}

fn parse_uptime_metrics_entry(entry: &str) -> Option<UptimeMetrics> {
    // Step 1: Check for the presence of fields using RegexSet
    let set = RegexSet::new(&[
        r#"node_id="(?P<node_id>[^\"]+)""#,
        r"is_up=(true|false)",
        r"start_ts=\d+",
        r"current_uptime_ms=\d+",
        r"uptime_added_ms=\d+",
        r"last_successful_sync_ts=\d+",
    ])
    .unwrap();

    let matches = set.matches(entry);

    // Ensure all fields are present
    if matches.iter().count() < 6 {
        return None; // Not all fields are present, so exit early
    }

    // Step 2: Extract values for each field
    let node_id_re = Regex::new(r#"node_id="(?P<node_id>[^"]+)""#).unwrap();
    let is_up_re = Regex::new(r"is_up=(?P<is_up>true|false)").unwrap();
    let start_ts_re = Regex::new(r"start_ts=(?P<start_ts>\d+)").unwrap();
    let current_uptime_ms_re = Regex::new(r"current_uptime_ms=(?P<current_uptime_ms>\d+)").unwrap();
    let uptime_added_ms_re = Regex::new(r"uptime_added_ms=(?P<uptime_added_ms>\d+)").unwrap();
    let last_successful_sync_ts_re =
        Regex::new(r"last_successful_sync_ts=(?P<last_successful_sync_ts>\d+)").unwrap();

    // Extracting each field
    let node_id = node_id_re
        .captures(entry)?
        .name("node_id")?
        .as_str()
        .to_string();
    let is_up = is_up_re
        .captures(entry)?
        .name("is_up")?
        .as_str()
        .parse()
        .ok()?;
    let start_ts = start_ts_re
        .captures(entry)?
        .name("start_ts")?
        .as_str()
        .parse()
        .ok()?;
    let current_uptime_ms = current_uptime_ms_re
        .captures(entry)?
        .name("current_uptime_ms")?
        .as_str()
        .parse()
        .ok()?;
    let uptime_added_ms = uptime_added_ms_re
        .captures(entry)?
        .name("uptime_added_ms")?
        .as_str()
        .parse()
        .ok()?;
    let last_successful_sync_ts = last_successful_sync_ts_re
        .captures(entry)?
        .name("last_successful_sync_ts")?
        .as_str()
        .parse()
        .ok()?;

    // Construct and return the UptimeMetrics struct if all fields were successfully extracted
    Some(UptimeMetrics {
        node_id,
        is_up,
        start_ts,
        current_uptime_ms,
        uptime_added_ms,
        last_successful_sync_ts,
    })
}

fn parse_node_id(entry: &str) -> Option<String> {
    let re = Regex::new(r#"node_id="(?P<node_id>[^\"]+)""#).unwrap();
    let caps = re.captures(entry)?;

    caps.name("node_id")
        .map(|match_| match_.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_node_id() {
        let log_entry = r#"2024-02-12T04:51:13.833282570+00:00 [INFO] dagger_logger::metrics - datapoint: uptime_metrics node_id="AzrAW1uLjrV9nuDZyCD1nmoRovbyPyXWYWQ12goZedhn" is_up=true start_ts=1707713462233 current_uptime_ms=11599 uptime_added_ms=5000 last_successful_sync_ts=1707713473740"#;
        let parsed_metrics = parse_node_id(log_entry);

        assert!(
            parsed_metrics.is_some(),
            "The parser should successfully extract node id."
        );

        let metrics = parsed_metrics.unwrap();

        assert_eq!(metrics, "AzrAW1uLjrV9nuDZyCD1nmoRovbyPyXWYWQ12goZedhn");
    }

    #[test]
    fn test_parse_uptime_metrics_entry() {
        let log_entry = r#"2024-02-12T04:51:13.833282570+00:00 [INFO] dagger_logger::metrics - datapoint: uptime_metrics node_id="AzrAW1uLjrV9nuDZyCD1nmoRovbyPyXWYWQ12goZedhn" is_up=true start_ts=1707713462233 current_uptime_ms=11599 uptime_added_ms=5000 last_successful_sync_ts=1707713473740"#;
        let parsed_metrics = parse_uptime_metrics_entry(log_entry);

        assert!(
            parsed_metrics.is_some(),
            "The parser should successfully extract metrics."
        );

        let metrics = parsed_metrics.unwrap();

        assert_eq!(
            metrics.node_id,
            "AzrAW1uLjrV9nuDZyCD1nmoRovbyPyXWYWQ12goZedhn"
        );
        assert_eq!(metrics.is_up, true);
        assert_eq!(metrics.start_ts, 1707713462233);
        assert_eq!(metrics.current_uptime_ms, 11599);
        assert_eq!(metrics.uptime_added_ms, 5000);
        assert_eq!(metrics.last_successful_sync_ts, 1707713473740);
    }
}
