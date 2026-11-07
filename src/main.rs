mod outbox;
mod startup;
mod telegram;
use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use reqwest::{redirect::Policy, Url};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    env,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
static CHECK_WRITE_WARNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Deserialize, Default)]
struct Target {
    name: String,
    url: String,
    #[serde(default)]
    expected_statuses: Option<Vec<u16>>,
    #[serde(default)]
    latency_warning_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Config {
    targets: Vec<Target>,
    #[serde(default = "default_interval")]
    interval_seconds: u64,
    #[serde(default = "default_threshold")]
    failure_threshold: usize,
}
fn default_interval() -> u64 {
    60
}
fn default_threshold() -> usize {
    3
}
#[derive(Clone, Serialize)]
struct Check {
    at: DateTime<Utc>,
    ok: bool,
    status: Option<u16>,
    latency_ms: u64,
    error: Option<String>,
    warning: Option<String>,
}
#[derive(Clone, Serialize)]
struct Monitor {
    name: String,
    url: String,
    history: VecDeque<Check>,
    health: Option<bool>,
}
type Monitors = Arc<RwLock<Vec<Monitor>>>;
const MAX_CONFIG_BYTES: usize = 128 * 1024;

fn read_config(reader: impl Read) -> Result<Config> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_CONFIG_BYTES,
        "Configuration exceeds 128 KiB"
    );
    let config = serde_json::from_slice(&bytes)?;
    validate(&config)?;
    Ok(config)
}
#[derive(Clone)]
struct App {
    monitors: Monitors,
    targets: Vec<Target>,
    client: reqwest::Client,
    running: Arc<tokio::sync::Mutex<()>>,
    store: Arc<outbox::Store>,
    failure_threshold: usize,
}

fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(
        !config.targets.is_empty() && config.targets.len() <= 25,
        "Configure 1–25 targets"
    );
    anyhow::ensure!(
        (15..=3600).contains(&config.interval_seconds),
        "interval_seconds must be 15–3600"
    );
    anyhow::ensure!(
        (1..=10).contains(&config.failure_threshold),
        "failure_threshold must be 1–10"
    );
    let mut urls = std::collections::HashSet::new();
    for t in &config.targets {
        anyhow::ensure!(
            t.url.len() <= 2048,
            "Target URLs must not exceed 2048 bytes"
        );
        if let Some(codes) = &t.expected_statuses {
            anyhow::ensure!(
                !codes.is_empty()
                    && codes.len() <= 50
                    && codes.iter().all(|v| (100..=599).contains(v)),
                "expected_statuses must contain 1–50 HTTP codes"
            );
        }
        if let Some(ms) = t.latency_warning_ms {
            anyhow::ensure!(
                (1..=60000).contains(&ms),
                "latency_warning_ms must be 1–60000"
            );
        }
        anyhow::ensure!(
            !t.name.trim().is_empty() && t.name.chars().count() <= 80,
            "Target names must contain 1–80 characters"
        );
        anyhow::ensure!(urls.insert(&t.url), "Target URLs must be unique");
        let u = Url::parse(&t.url).context("invalid target URL")?;
        anyhow::ensure!(
            matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none(),
            "Target must be an HTTP(S) URL without credentials or fragment"
        );
    }
    Ok(())
}
fn open_db(path: &str, targets: &[Target], threshold: usize) -> Result<(Connection, Vec<Monitor>)> {
    let conn = Connection::open(path).context("opening history database")?;
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS checks (id INTEGER PRIMARY KEY, url TEXT NOT NULL, at TEXT NOT NULL, ok INTEGER NOT NULL, status INTEGER, latency_ms INTEGER NOT NULL, error TEXT); CREATE INDEX IF NOT EXISTS checks_url_id ON checks(url,id);")?;
    let has_warning = {
        let mut q = conn.prepare("PRAGMA table_info(checks)")?;
        let names = q
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        names.iter().any(|n| n == "warning")
    };
    if !has_warning {
        conn.execute("ALTER TABLE checks ADD COLUMN warning TEXT", [])?;
    }
    let mut monitors = Vec::new();
    for target in targets {
        let mut statement=conn.prepare("SELECT at,ok,status,latency_ms,error,warning FROM (SELECT at,ok,status,latency_ms,error,warning,id FROM checks WHERE url=?1 ORDER BY id DESC LIMIT 120) ORDER BY id")?;
        let history = statement
            .query_map([&target.url], |r| {
                let at: String = r.get(0)?;
                let parsed = DateTime::parse_from_rfc3339(&at).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(Check {
                    at: parsed.with_timezone(&Utc),
                    ok: r.get(1)?,
                    status: r.get(2)?,
                    latency_ms: r.get(3)?,
                    error: r.get(4)?,
                    warning: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<VecDeque<_>>>()?;
        let health = health_from_history(&history, threshold);
        monitors.push(Monitor {
            name: target.name.clone(),
            url: target.url.clone(),
            history,
            health,
        });
    }
    Ok((conn, monitors))
}
fn health_from_history(history: &VecDeque<Check>, threshold: usize) -> Option<bool> {
    let mut health = None;
    let mut consecutive = 0;
    for check in history {
        if health.is_none() {
            health = Some(check.ok);
            consecutive = if check.ok { 0 } else { 1 };
            continue;
        }
        if check.ok {
            health = Some(true);
            consecutive = 0;
        } else {
            consecutive += 1;
            if consecutive >= threshold {
                health = Some(false);
            }
        }
    }
    health
}
fn classify(
    target: &Target,
    status: Option<u16>,
    latency: u64,
) -> (bool, Option<String>, Option<String>) {
    let ok = status.is_some_and(|code| match &target.expected_statuses {
        Some(codes) => codes.contains(&code),
        None => (200..300).contains(&code),
    });
    let error = if status.is_none() {
        Some("Request failed or timed out / Ошибка запроса или таймаут".into())
    } else if !ok {
        Some("Unexpected HTTP status / Неожиданный HTTP-статус".into())
    } else {
        None
    };
    let warning = if ok
        && target
            .latency_warning_ms
            .is_some_and(|limit| latency > limit)
    {
        Some("Slow response headers / Медленное получение заголовков".into())
    } else {
        None
    };
    (ok, error, warning)
}
async fn run_checks(app: &App) {
    let Ok(_guard) = app.running.try_lock() else {
        return;
    };
    let mut jobs = tokio::task::JoinSet::new();
    for (i, target) in app.targets.iter().enumerate() {
        let client = app.client.clone();
        let target = target.clone();
        let url = target.url.clone();
        jobs.spawn(async move {
            let start = Instant::now();
            let result = client.get(url).send().await;
            let ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let status = result.ok().map(|r| r.status().as_u16());
            let (ok, error, warning) = classify(&target, status, ms);
            (
                i,
                Check {
                    at: Utc::now(),
                    ok,
                    status,
                    latency_ms: ms,
                    error,
                    warning,
                },
            )
        });
    }
    while let Some(result) = jobs.join_next().await {
        if let Ok((i, check)) = result {
            let target = &app.targets[i];
            let Some(mut monitor) = app.monitors.read().await.get(i).cloned() else {
                continue;
            };
            let previous = monitor.health;
            monitor.history.push_back(check.clone());
            if monitor.history.len() > 120 {
                monitor.history.pop_front();
            }
            monitor.health = health_from_history(&monitor.history, app.failure_threshold);
            let changed = previous.is_some_and(|ok| Some(ok) != monitor.health);
            let text = changed.then(|| {
                format!(
                    "Traceboard: {} — {}{}\nChecked: {}",
                    target.name,
                    if monitor.health == Some(true) {
                        "RECOVERED"
                    } else {
                        "DOWN"
                    },
                    check
                        .status
                        .map(|status| format!(" (HTTP {status})"))
                        .unwrap_or_default(),
                    check.at.to_rfc3339()
                )
            });
            if app.store.record(&target.url, check, text).await.is_err() {
                if !CHECK_WRITE_WARNING.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "Cannot save check and notification intent; previous history retained"
                    );
                }
                continue;
            }
            if let Some(current) = app.monitors.write().await.get_mut(i) {
                *current = monitor;
            }
        }
    }
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
async fn monitors(State(app): State<App>) -> Json<Vec<Monitor>> {
    Json(app.monitors.read().await.clone())
}
async fn refresh(State(app): State<App>) -> StatusCode {
    run_checks(&app).await;
    StatusCode::NO_CONTENT
}
async fn notifications(State(app): State<App>) -> Json<outbox::Overview> {
    Json(app.store.status())
}
#[tokio::main]
async fn main() -> Result<()> {
    let path = env::var("TRACEBOARD_CONFIG").unwrap_or_else(|_| "targets.json".into());
    let file = std::fs::File::open(&path).with_context(|| {
        format!("Cannot read {path}; copy targets.example.json to targets.json")
    })?;
    let config = read_config(file)?;
    let telegram = telegram::Telegram::from_env()?;
    if env::args().any(|a| a == "--check") {
        return startup::check(
            Some(std::path::Path::new(
                &env::var("TRACEBOARD_DB").unwrap_or_else(|_| "traceboard.sqlite".into()),
            )),
            Some(3000),
        );
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(Policy::limited(3))
        .build()?;
    let (db, initial_monitors) = open_db(
        &env::var("TRACEBOARD_DB").unwrap_or_else(|_| "traceboard.sqlite".into()),
        &config.targets,
        config.failure_threshold,
    )?;
    let store = outbox::Store::start(db, telegram.as_ref().map(|t| t.destination.clone()))?;
    if let Some(telegram) = telegram {
        tokio::spawn(telegram::run(store.clone(), telegram));
    }
    let app = App {
        monitors: Arc::new(RwLock::new(initial_monitors)),
        targets: config.targets,
        client,
        running: Arc::new(tokio::sync::Mutex::new(())),
        store,
        failure_threshold: config.failure_threshold,
    };
    let task = app.clone();
    let interval = config.interval_seconds;
    tokio::spawn(async move {
        loop {
            run_checks(&task).await;
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
    let router = Router::new()
        .route("/", get(index))
        .route("/style.css", get(css))
        .route("/app.js", get(js))
        .route("/api/monitors", get(monitors))
        .route("/api/notifications", get(notifications))
        .route("/api/refresh", post(refresh))
        .with_state(app);
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("Local port unavailable; stop the other instance / Локальный порт занят")?;
    println!("Traceboard: http://{address}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_read_is_bounded_before_parsing() {
        let large = std::io::Cursor::new(vec![b' '; MAX_CONFIG_BYTES + 1]);
        assert!(read_config(large).is_err());
        let valid = br#"{"targets":[{"name":"local","url":"http://localhost"}]}"#;
        assert!(read_config(&valid[..]).is_ok());
    }
    #[test]
    fn configuration_rejects_bad_targets() {
        let mut c = Config {
            targets: vec![Target {
                name: "test".into(),
                url: "file:///tmp/test".into(),
                ..Default::default()
            }],
            interval_seconds: 60,
            failure_threshold: 3,
        };
        assert!(validate(&c).is_err());
        c.targets[0].url = "https://example.com/".into();
        assert!(validate(&c).is_ok());
        c.interval_seconds = 1;
        assert!(validate(&c).is_err());
    }
    #[tokio::test]
    async fn history_survives_reopening() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "traceboard-{}-{}.sqlite",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let file = path.to_string_lossy().to_string();
        let targets = vec![Target {
            name: "Example".into(),
            url: "https://example.com".into(),
            ..Default::default()
        }];
        let (db, _) = open_db(&file, &targets, 3)?;
        let store = outbox::Store::start(db, None)?;
        let check = Check {
            at: Utc::now(),
            ok: true,
            status: Some(200),
            latency_ms: 42,
            error: None,
            warning: None,
        };
        store.record(&targets[0].url, check, None).await?;
        drop(store);
        let (db, loaded) = open_db(&file, &targets, 3)?;
        assert_eq!(loaded[0].history.len(), 1);
        assert_eq!(loaded[0].history[0].status, Some(200));
        drop(db);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn transient_failures_do_not_trigger_outage() {
        let check = |ok| Check {
            at: Utc::now(),
            ok,
            status: None,
            latency_ms: 1,
            error: None,
            warning: None,
        };
        let mut history = VecDeque::from(vec![check(true), check(false), check(false)]);
        assert_eq!(health_from_history(&history, 3), Some(true));
        history.push_back(check(false));
        assert_eq!(health_from_history(&history, 3), Some(false));
        history.push_back(check(true));
        assert_eq!(health_from_history(&history, 3), Some(true));
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    #[test]
    fn explicit_codes_and_latency_do_not_change_health() {
        let mut t = Target {
            name: "local".into(),
            url: "http://localhost".into(),
            ..Default::default()
        };
        assert!(classify(&t, Some(204), 5).0);
        assert!(!classify(&t, Some(404), 5).0);
        t.expected_statuses = Some(vec![404]);
        t.latency_warning_ms = Some(10);
        let (ok, _, warning) = classify(&t, Some(404), 11);
        assert!(ok);
        assert!(warning.is_some());
        assert!(!classify(&t, None, 1).0);
    }
    #[test]
    fn config_legacy_and_reject_empty_codes() {
        let c: Config = serde_json::from_str(
            r#"{"targets":[{"name":"x","url":"http://localhost"}],"interval_seconds":60}"#,
        )
        .expect("fixture");
        assert!(validate(&c).is_ok());
        let mut c = c;
        c.targets[0].expected_statuses = Some(vec![]);
        assert!(validate(&c).is_err());
    }
}
