use std::{collections::VecDeque, env, net::{IpAddr, Ipv4Addr, SocketAddr}, sync::Arc, time::{Duration, Instant}};
use anyhow::{Context, Result};
use axum::{extract::State, http::{header, StatusCode}, response::{Html, IntoResponse}, routing::{get, post}, Json, Router};
use chrono::{DateTime, Utc};
use reqwest::{redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};
use std::sync::Mutex;
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
struct App { monitors: Monitors, targets: Vec<Target>, client: reqwest::Client, running: Arc<tokio::sync::Mutex<()>>, db: Arc<Mutex<Connection>>, telegram: Option<(String, String)> }

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
fn open_db(path: &str, targets: &[Target]) -> Result<(Arc<Mutex<Connection>>, Vec<Monitor>)> {
    let conn = Connection::open(path).context("opening history database")?;
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS checks (id INTEGER PRIMARY KEY, url TEXT NOT NULL, at TEXT NOT NULL, ok INTEGER NOT NULL, status INTEGER, latency_ms INTEGER NOT NULL, error TEXT); CREATE INDEX IF NOT EXISTS checks_url_id ON checks(url,id);")?;
    let mut monitors = Vec::new();
    for target in targets {
        let mut statement=conn.prepare("SELECT at,ok,status,latency_ms,error FROM (SELECT at,ok,status,latency_ms,error,id FROM checks WHERE url=?1 ORDER BY id DESC LIMIT 120) ORDER BY id")?;
        let history=statement.query_map([&target.url], |r| {
            let at: String = r.get(0)?;
            let parsed=DateTime::parse_from_rfc3339(&at).map_err(|e|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,Box::new(e)))?;
            Ok(Check { at:parsed.with_timezone(&Utc), ok:r.get(1)?, status:r.get(2)?, latency_ms:r.get(3)?, error:r.get(4)? })
        })?.collect::<rusqlite::Result<VecDeque<_>>>()?;
        monitors.push(Monitor { name:target.name.clone(), url:target.url.clone(), history });
    }
    Ok((Arc::new(Mutex::new(conn)),monitors))
}
fn save_check(app: &App, url: &str, c: &Check) -> Result<()> {
    let mut db=app.db.lock().map_err(|_|anyhow::anyhow!("database lock poisoned"))?;
    let tx=db.transaction()?;
    tx.execute("INSERT INTO checks(url,at,ok,status,latency_ms,error) VALUES(?1,?2,?3,?4,?5,?6)",params![url,c.at.to_rfc3339(),c.ok,c.status,c.latency_ms,c.error])?;
    tx.execute("DELETE FROM checks WHERE url=?1 AND id NOT IN (SELECT id FROM checks WHERE url=?1 ORDER BY id DESC LIMIT 120)",[url])?;
    tx.commit()?; Ok(())
}
async fn notify(app: &App, name: &str, check: &Check) {
    if let Some((token,chat)) = &app.telegram {
        let endpoint=format!("https://api.telegram.org/bot{}/sendMessage",token);
        let text=format!("Traceboard: {} — {}{}",name,if check.ok {"RECOVERED"} else {"DOWN"}, check.status.map(|s|format!(" (HTTP {s})")).unwrap_or_default());
        let result=app.client.post(endpoint).json(&serde_json::json!({"chat_id":chat,"text":text})).send().await;
        match result { Ok(r) if r.status().is_success()=>{}, _=>eprintln!("Telegram notification failed; check token and chat ID") }
    }
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
            let target = &app.targets[i];
            if let Err(e)=save_check(app,&target.url,&check) { eprintln!("Cannot save check: {e:#}"); continue; }
            let changed = {
                let mut data=app.monitors.write().await;
                if let Some(m)=data.get_mut(i) {
                    let previous=m.history.back().map(|c|c.ok);
                    m.history.push_back(check.clone()); if m.history.len()>120 {m.history.pop_front();}
                    previous.is_some_and(|ok|ok!=check.ok)
                } else { false }
            };
            if changed {notify(app,&target.name,&check).await; }
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
    let (db,initial_monitors)=open_db(&env::var("TRACEBOARD_DB").unwrap_or_else(|_| "traceboard.sqlite".into()), &config.targets)?;
    let telegram=match (env::var("TELEGRAM_BOT_TOKEN"),env::var("TELEGRAM_CHAT_ID")) {
        (Ok(token),Ok(chat)) if !token.is_empty() && !chat.is_empty() => Some((token,chat)),
        (Err(env::VarError::NotPresent),Err(env::VarError::NotPresent)) => None,
        _ => anyhow::bail!("Set both TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID, or neither")
    };
    let app = App { monitors: Arc::new(RwLock::new(initial_monitors)), targets:config.targets, client, running:Arc::new(tokio::sync::Mutex::new(())), db, telegram };
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
    #[test] fn history_survives_reopening() -> Result<()> {
        let path=std::env::temp_dir().join(format!("traceboard-{}-{}.sqlite",std::process::id(),Utc::now().timestamp_nanos_opt().unwrap_or_default()));
        let file=path.to_string_lossy().to_string();
        let targets=vec![Target {name:"Example".into(),url:"https://example.com".into()}];
        let (db, monitors)=open_db(&file,&targets)?;
        let app=App {monitors:Arc::new(RwLock::new(monitors)), targets:targets.clone(), client:reqwest::Client::new(), running:Arc::new(tokio::sync::Mutex::new(())), db,telegram:None};
        let check=Check {at:Utc::now(),ok:true,status:Some(200),latency_ms:42,error:None};
        save_check(&app,&targets[0].url,&check)?; drop(app);
        let (db, loaded)=open_db(&file,&targets)?; assert_eq!(loaded[0].history.len(),1);assert_eq!(loaded[0].history[0].status,Some(200));drop(db);
        let _=std::fs::remove_file(path); Ok(())
    }

}
