use std::{collections::VecDeque, env, net::{IpAddr, Ipv4Addr, SocketAddr}, sync::Arc, time::{Duration, Instant}};
use anyhow::{Context, Result};
use axum::{extract::State, http::{header, StatusCode}, response::{Html, IntoResponse}, routing::{get, post}, Json, Router};
use chrono::{DateTime, Utc};
use reqwest::{redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Clone, Deserialize)]
struct Target { name: String, url: String }
#[derive(Deserialize)]
struct Config { targets: Vec<Target>, #[serde(default = "default_interval")] interval_seconds: u64 }
fn default_interval() -> u64 { 60 }
#[derive(Clone, Serialize)]
struct Check { at: DateTime<Utc>, ok: bool, status: Option<u16>, latency_ms: u64, error: Option<String> }
#[derive(Clone, Serialize)]
struct Monitor { name: String, url: String, history: VecDeque<Check> }
type Monitors = Arc<RwLock<Vec<Monitor>>>;
#[derive(Clone)]
struct App { monitors: Monitors, targets: Vec<Target>, client: reqwest::Client, running: Arc<tokio::sync::Mutex<()>> }

fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(!config.targets.is_empty() && config.targets.len() <= 25, "Configure 1–25 targets");
    anyhow::ensure!((15..=3600).contains(&config.interval_seconds), "interval_seconds must be 15–3600");
    for t in &config.targets {
        anyhow::ensure!(!t.name.trim().is_empty() && t.name.chars().count() <= 80, "Target names must contain 1–80 characters");
        let u = Url::parse(&t.url).context("invalid target URL")?;
        anyhow::ensure!(matches!(u.scheme(), "http" | "https") && u.host_str().is_some() && u.username().is_empty() && u.password().is_none() && u.fragment().is_none(), "Target must be an HTTP(S) URL without credentials or fragment");
    }
    Ok(())
}
async fn run_checks(app: &App) {
    let Ok(_guard) = app.running.try_lock() else { return; };
    let mut jobs = tokio::task::JoinSet::new();
    for (i, target) in app.targets.iter().enumerate() {
        let client = app.client.clone(); let url = target.url.clone();
        jobs.spawn(async move {
            let start = Instant::now();
            let result = client.get(url).send().await;
            let ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let (ok, status, error) = match result { Ok(r) => (r.status().is_success(), Some(r.status().as_u16()), None), Err(_) => (false, None, Some("Request failed or timed out".into())) };
            (i, Check { at: Utc::now(), ok, status, latency_ms: ms, error })
        });
    }
    while let Some(result) = jobs.join_next().await {
        if let Ok((i, check)) = result {
            let mut data = app.monitors.write().await;
            if let Some(m) = data.get_mut(i) { m.history.push_back(check); if m.history.len() > 120 { m.history.pop_front(); } }
        }
    }
}
async fn index() -> Html<&'static str> { Html(include_str!("../web/index.html")) }
async fn css() -> impl IntoResponse { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../web/style.css")) }
async fn js() -> impl IntoResponse { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../web/app.js")) }
async fn monitors(State(app): State<App>) -> Json<Vec<Monitor>> { Json(app.monitors.read().await.clone()) }
async fn refresh(State(app): State<App>) -> StatusCode { run_checks(&app).await; StatusCode::NO_CONTENT }
#[tokio::main]
async fn main() -> Result<()> {
    let path = env::var("TRACEBOARD_CONFIG").unwrap_or_else(|_| "targets.json".into());
    let config: Config = serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| format!("Cannot read {path}; copy targets.example.json to targets.json"))?)?;
    validate(&config)?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(Policy::limited(3)).build()?;
    let initial_monitors = config.targets.iter().map(|t| Monitor { name:t.name.clone(), url:t.url.clone(), history:VecDeque::new() }).collect();
    let app = App { monitors: Arc::new(RwLock::new(initial_monitors)), targets:config.targets, client, running:Arc::new(tokio::sync::Mutex::new(())) };
    let task = app.clone(); let interval = config.interval_seconds;
    tokio::spawn(async move { loop { run_checks(&task).await; tokio::time::sleep(Duration::from_secs(interval)).await; } });
    let router = Router::new().route("/",get(index)).route("/style.css",get(css)).route("/app.js",get(js)).route("/api/monitors",get(monitors)).route("/api/refresh",post(refresh)).with_state(app);
    let address=SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST),3000);
    let listener=tokio::net::TcpListener::bind(address).await?;
    println!("Traceboard: http://{address}");
    axum::serve(listener,router).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn configuration_rejects_bad_targets() {
        let mut c=Config { targets:vec![Target{name:"test".into(),url:"file:///tmp/test".into()}],interval_seconds:60};
        assert!(validate(&c).is_err()); c.targets[0].url="https://example.com/".into(); assert!(validate(&c).is_ok()); c.interval_seconds=1; assert!(validate(&c).is_err());
    }
}
