use std::{
    convert::Infallible,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
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
use chrono::{TimeZone, Utc};
use logwatcher::LogWatcher;
use passwords::PasswordGenerator;
use regex::{Regex, RegexSet};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::Command,
    sync::broadcast::{channel, Sender},
    time::{self, sleep, Instant},
};
use tokio_stream::Stream;

pub type LogStream = Sender<String>;

#[derive(Clone)]
struct SharedState {
    stream_logs_toggle: bool,
    last_known_modification: String,
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
    password: String,
    wield_binary_url: Option<String>,
    wield_binary_path: Option<String>,
    download_new_wield_binary_path: Option<String>,
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
        let pg = PasswordGenerator {
            length: 20,
            numbers: true,
            lowercase_letters: true,
            uppercase_letters: true,
            symbols: false,
            spaces: false,
            exclude_similar_characters: false,
            strict: true,
        };

        let new_password = pg
            .generate_one()
            .expect("Should succesfully generate a new password when creating a new config.");

        let default_config = Config {
            password: new_password,
            wield_binary_url: None,
            wield_binary_path: None,
            download_new_wield_binary_path: None,
        };

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
        last_known_modification: "".to_string(),
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
        .route("/wield/service/id", get(handle_get_wield_node_id))
        .route("/wield/service/version", get(handle_get_wield_version))
        .route("/wield/service/status", get(handle_get_wield_status))
        .route("/wield/service/start", post(handle_start_wield_service))
        .route("/wield/service/stop", post(handle_stop_wield_service))
        .route("/wield/service/restart", post(handle_restart_wield_service))
        .layer(Extension(tx.clone()))
        .layer(Extension(shared_state.clone()))
        .layer(Extension(config.clone()))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listener bound to port 3000");
    
    let tx1 = tx.clone();
    tokio::spawn(async move {
        let path = "/home/dagger/config.log".to_string();
        loop {
            if Path::new(&path).exists() {
                let mut log_watcher = LogWatcher::register(&path).unwrap();
                let tx = tx1.clone();

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

    // Wield binary handler
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let url = if let Some(url) = config.wield_binary_url {
            url
        } else {
            "https://shdw-drive.genesysgo.net/4xdLyZZJzL883AbiZvgyWKf2q55gcZiMgMkDNQMnyFJC/wield-latest".to_string()
        };
        let shared_state = shared_state.clone();
        let tx = tx2.clone();

        loop {
            let last_known_modification = shared_state
                .lock()
                .unwrap()
                .last_known_modification
                .to_string();
            let new_last_modified = get_last_modified(&url).await;
            let shared_state = shared_state.clone();

            if new_last_modified != last_known_modification {
                let wield_binary_new_path =
                    if let Some(url) = config.download_new_wield_binary_path.clone() {
                        url
                    } else {
                        "/home/dagger/new-wield-binary/wield".to_string()
                    };
                if let Ok(_) = download_new_wield_binary(&url, &wield_binary_new_path).await {
                    println!("Downloaded new binary");
                    let tx = tx.clone();
                    let _ = tx.send("toast-event -- Downloaded a new wield binary".to_string());
                }

                tokio::spawn(async move {
                    let shared_state = shared_state.clone();
                    let mut shared_state = shared_state.lock().unwrap();
                    shared_state.last_known_modification = new_last_modified;
                });
            }

            sleep(Duration::from_millis(5000)).await;
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
    password: String,
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
    let payload_password = login_form.password.to_string();
    if payload_password == config.password {
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
    Extension(tx): Extension<LogStream>,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let mut shared_state_lock = shared_state.lock().unwrap();
        shared_state_lock.stream_logs_toggle = true;

        let tx = tx.clone();
        let _ = tx.send("toast-event -- Started log stream".to_string());
        return Ok(StatusCode::OK);
    }
    Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
}

async fn handle_stop_log_stream(
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
    Extension(shared_state): Extension<Arc<Mutex<SharedState>>>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let mut shared_state_lock = shared_state.lock().unwrap();
        shared_state_lock.stream_logs_toggle = false;
        let tx = tx.clone();

        let _ = tx.send("toast-event -- Stopped log stream".to_string());
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
                if is_authed {
                    if let Some(uptime_metrics) = parse_uptime_metrics_entry(msg.as_ref()) {
                        let event_data = format!("is_up: {}", uptime_metrics.is_up.to_string());
                        let event = Event::default().event("metric-is-up").data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;

                        let start_date_time =
                            Utc.timestamp_millis_opt(uptime_metrics.start_ts as i64);
                        let event_data = format!("start_ts_date: {:?}", start_date_time.unwrap());
                        let event = Event::default().event("metric-start-ts").data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;

                        let event_data = format!(
                            "current_uptime_ms: {}",
                            uptime_metrics.current_uptime_ms.to_string()
                        );
                        let event = Event::default()
                            .event("metric-current-uptime-ms")
                            .data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;

                        let event_data = format!(
                            "uptime_added_ms: {}",
                            uptime_metrics.uptime_added_ms.to_string()
                        );
                        let event = Event::default()
                            .event("metric-uptime-added-ms")
                            .data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;

                        let last_successfull_sync_date_time =
                            Utc.timestamp_millis_opt(uptime_metrics.last_successful_sync_ts as i64);
                        let event_data = format!(
                            "last_successful_sync_ts_date: {:?}",
                            last_successfull_sync_date_time.unwrap()
                        );
                        let event = Event::default()
                            .event("metric-last-successful-sync-ts")
                            .data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;
                    }
                    

                    let split_msg: Vec<&str> = msg.split(" -- ").collect();

                    if split_msg.len() > 1 {
                        let prefix = split_msg[0];
                        if prefix == "toast-event" {
                            let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis();
                            let toast_message = format!("<div _=\"on load wait 2s then remove me\" class=\"toast\" id=\"toast-{}\" onclick=\"removeMe(this)\">{}</div>", now, split_msg[1].clone());
                            let event = Event::default().event("toast-event").data(toast_message);
                            let _ = tx_new_clone.send(Ok(event)).await;
                        }
                    }


                    let shared_state = shared_state.lock().unwrap().clone();
                    if shared_state.stream_logs_toggle {
                        let msg = msg.clone();
                        let event_data = format!("<div>{}</div>", msg);
                        let event = Event::default().event("log-stream").data(event_data);
                        let _ = tx_new_clone.send(Ok(event)).await;

                    }
                }
            }
            .await;
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
    Extension(config): Extension<Config>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let path_to_wield_binary = if let Some(path) = config.wield_binary_path.clone() {
            path
        } else {
            "/home/dagger/wield".to_string()
        };
        let path_to_updated_wield_binary = if let Some(path) = config.wield_binary_path {
            path
        } else {
            "/home/dagger/new-wield-binary/wield".to_string()
        };

        if Path::new(&path_to_wield_binary).exists() {
            let output = Command::new(path_to_wield_binary.clone())
                .arg("--version")
                .output()
                .await
                .expect(&format!(
                    "Should successfully run wield --version from path {}",
                    path_to_wield_binary
                ));

            let output_stdout = String::from_utf8(output.stdout).unwrap();

            let mut response = output_stdout.to_string();

            if Path::new(&path_to_updated_wield_binary).exists() {
                let output = Command::new(path_to_updated_wield_binary)
                    .arg("--version")
                    .output()
                    .await
                    .expect(&format!(
                        "Should successfully run wield --version from path {}",
                        path_to_wield_binary
                    ));

                response = format!(
                    "{} {}: {}",
                    output_stdout,
                    "**NEW VERSION UPDATE AVAILABLE",
                    String::from_utf8(output.stdout).unwrap()
                );
            }

            return (StatusCode::OK, format!("Version: {}", response));
        } else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Version: WIELD BINARY NOT FOUND".to_string(),
            );
        }
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized".to_string())
}

async fn handle_get_wield_node_id(
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        let path_to_shdw_keygen_binary = "/home/dagger/shdw-keygen".to_string();

        if Path::new(&path_to_shdw_keygen_binary).exists() {
            let output = Command::new("/home/dagger/shdw-keygen")
                .arg("pubkey")
                .arg("/home/dagger/id.json")
                .output()
                .await
                .expect(&format!(
                    "Should successfully run wield --version from path {}",
                    path_to_shdw_keygen_binary
                ));

            let output_stdout = String::from_utf8(output.stdout).unwrap();

            if tx.send(output_stdout.clone()).is_err() {
                eprintln!("Error sending cmd output across channel.");
            };

            let beginning = &output_stdout[0..6];
            let length = output_stdout.len();
            let ending = &output_stdout[length - 6..length];
            return (
                StatusCode::OK,
                format!("Node ID: {}...{}", beginning, ending),
            );
        } else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Version: shdw-keygen BINARY NOT FOUND".to_string(),
            );
        }
    }

    (StatusCode::UNAUTHORIZED, "Unauthorized".to_string())
}

async fn handle_start_wield_service(
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("start")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        let tx = tx.clone();
        let _ = tx.send("toast-event -- Started wield node".to_string());
        return StatusCode::OK;
    }

    StatusCode::UNAUTHORIZED
}

async fn handle_stop_wield_service(
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("stop")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        let tx = tx.clone();
        let _ = tx.send("toast-event -- Stopped wield node".to_string());
        return StatusCode::OK;
    }

    StatusCode::UNAUTHORIZED
}

async fn handle_restart_wield_service(
    cookies: PrivateCookieJar,
    Extension(tx): Extension<LogStream>,
) -> impl IntoResponse {
    if has_valid_auth_token(cookies) {
        Command::new("systemctl")
            .arg("restart")
            .arg("wield")
            .output()
            .await
            .expect("Should successfully run shell command");

        let tx = tx.clone();
        let _ = tx.send("toast-event -- Restarted wield node".to_string());
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
    is_up: bool,
    start_ts: u64,
    current_uptime_ms: u64,
    uptime_added_ms: u64,
    last_successful_sync_ts: u64,
}

fn parse_uptime_metrics_entry(entry: &str) -> Option<UptimeMetrics> {
    // Step 1: Check for the presence of fields using RegexSet
    let set = RegexSet::new(&[
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
    let is_up_re = Regex::new(r"is_up=(?P<is_up>true|false)").unwrap();
    let start_ts_re = Regex::new(r"start_ts=(?P<start_ts>\d+)").unwrap();
    let current_uptime_ms_re = Regex::new(r"current_uptime_ms=(?P<current_uptime_ms>\d+)").unwrap();
    let uptime_added_ms_re = Regex::new(r"uptime_added_ms=(?P<uptime_added_ms>\d+)").unwrap();
    let last_successful_sync_ts_re =
        Regex::new(r"last_successful_sync_ts=(?P<last_successful_sync_ts>\d+)").unwrap();

    // Extracting each field
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
        is_up,
        start_ts,
        current_uptime_ms,
        uptime_added_ms,
        last_successful_sync_ts,
    })
}

async fn get_last_modified(url: &str) -> String {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let response = client.head(url).send().await.unwrap();

    if response.status().is_success() {
        if let Some(last_modified) = response.headers().get("Last-Modified") {
            let last_modified_str = last_modified.to_str().unwrap();
            return last_modified_str.to_string();
        }
    }

    // If the request failed or the Last-Modified header is not present, handle accordingly
    "".to_string()
}

async fn backup_current_version(current_binary_path: &str, backup_path: &str) -> Result<(), ()> {
    // Ensure the backup directory exists
    if let Some(parent) = Path::new(backup_path).parent() {
        if let Err(_) = fs::create_dir_all(parent).await {
            return Err(());
        }
    }

    // Copy the current binary to the backup location
    if let Err(_) = fs::copy(current_binary_path, backup_path).await {
        return Err(());
    }

    Ok(())
}

async fn download_new_wield_binary(binary_url: &str, download_path: &str) -> Result<(), ()> {
    let response = reqwest::get(binary_url).await.unwrap();
    let content = response.bytes().await.unwrap();

    let wield_binary_new_path = download_path;
    if let Some(parent) = Path::new(wield_binary_new_path).parent() {
        if let Err(_) = fs::create_dir_all(parent).await {
            return Err(());
        }
    }

    let mut f = fs::File::create(wield_binary_new_path).await.unwrap();
    f.write_all(&content).await.unwrap();

    let mut perms = fs::metadata(wield_binary_new_path)
        .await
        .unwrap()
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(wield_binary_new_path, perms)
        .await
        .unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uptime_metrics_entry() {
        let log_entry = r#"2024-02-12T04:51:13.833282570+00:00 [INFO] dagger_logger::metrics - datapoint: uptime_metrics node_id="AzrAW1uLjrV9nuDZyCD1nmoRovbyPyXWYWQ12goZedhn" is_up=true start_ts=1707713462233 current_uptime_ms=11599 uptime_added_ms=5000 last_successful_sync_ts=1707713473740"#;
        let parsed_metrics = parse_uptime_metrics_entry(log_entry);

        assert!(
            parsed_metrics.is_some(),
            "The parser should successfully extract metrics."
        );

        let metrics = parsed_metrics.unwrap();

        assert_eq!(metrics.is_up, true);
        assert_eq!(metrics.start_ts, 1707713462233);
        assert_eq!(metrics.current_uptime_ms, 11599);
        assert_eq!(metrics.uptime_added_ms, 5000);
        assert_eq!(metrics.last_successful_sync_ts, 1707713473740);
    }
}
