use crate::Check;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tokio::sync::{mpsc, oneshot};

pub const CAPACITY: i64 = 256;
const RETAIN_TERMINAL: i64 = 64;
const WORK_QUEUE: usize = 32;

#[derive(Clone, Default, Serialize)]
pub struct Overview {
    pub enabled: bool,
    pub storage_error: bool,
    pub sender_stopped: bool,
    pub pending: u64,
    pub sending: u64,
    pub delivered: u64,
    pub failed: u64,
    pub uncertain: u64,
    pub paused: u64,
    pub overflow: u64,
    pub capacity: i64,
}
#[derive(Clone)]
pub struct Delivery {
    pub id: i64,
    pub text: String,
    pub attempt: u32,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered(i64),
    RateLimited(u64),
    Rejected,
    Unknown,
}
struct Record {
    url: String,
    check: Check,
    text: Option<String>,
}
enum Job {
    Record(Record, oneshot::Sender<Result<()>>),
    Claim(i64, oneshot::Sender<Result<Option<Delivery>>>),
    Finish(Delivery, Outcome, i64, oneshot::Sender<Result<()>>),
}
pub struct Store {
    sender: mpsc::Sender<Job>,
    overview: Arc<RwLock<Overview>>,
}

fn setup(db: &Connection) -> Result<()> {
    db.busy_timeout(std::time::Duration::from_millis(750))?;
    db.execute_batch("PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS notification_outbox (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            check_id INTEGER NOT NULL UNIQUE,
            destination TEXT NOT NULL,
            text TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('pending','sending','delivered','failed','uncertain')),
            attempts INTEGER NOT NULL DEFAULT 0,
            next_at INTEGER NOT NULL DEFAULT 0,
            reason TEXT,
            message_id INTEGER
        );
        CREATE INDEX IF NOT EXISTS outbox_pending ON notification_outbox(destination,state,id);
        CREATE TABLE IF NOT EXISTS notification_stats (singleton INTEGER PRIMARY KEY CHECK(singleton=1),overflow INTEGER NOT NULL);
        INSERT OR IGNORE INTO notification_stats VALUES(1,0);
        UPDATE notification_outbox SET state='uncertain',reason='interrupted' WHERE state='sending';")?;
    Ok(())
}
fn overview(db: &Connection, destination: Option<&str>) -> Result<Overview> {
    let mut value = Overview {
        enabled: destination.is_some(),
        capacity: CAPACITY,
        ..Default::default()
    };
    let mut statement =
        db.prepare("SELECT state,count(*) FROM notification_outbox GROUP BY state")?;
    for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (state, count) = row?;
        match state.as_str() {
            "pending" => value.pending = count,
            "sending" => value.sending = count,
            "delivered" => value.delivered = count,
            "failed" => value.failed = count,
            "uncertain" => value.uncertain = count,
            _ => anyhow::bail!("Invalid notification state"),
        }
    }
    value.paused = db.query_row("SELECT count(*) FROM notification_outbox WHERE state='pending' AND (?1 IS NULL OR destination<>?1)", [destination], |r| r.get(0))?;
    value.overflow = db.query_row(
        "SELECT overflow FROM notification_stats WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(value)
}
fn record(db: &mut Connection, entry: Record, destination: Option<&str>) -> Result<()> {
    let tx = db.transaction()?;
    let c = &entry.check;
    tx.execute("INSERT INTO checks(url,at,ok,status,latency_ms,error,warning) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![entry.url,c.at.to_rfc3339(),c.ok,c.status,c.latency_ms,c.error,c.warning])?;
    let check_id = tx.last_insert_rowid();
    if let (Some(text), Some(destination)) = (entry.text, destination) {
        anyhow::ensure!(text.chars().count() <= 300, "Notification text too long");
        // Only known terminal records are disposable. Unknown deliveries are never retried or pruned automatically.
        tx.execute("DELETE FROM notification_outbox WHERE id IN (SELECT id FROM notification_outbox WHERE state IN ('delivered','failed') ORDER BY id DESC LIMIT -1 OFFSET ?1)", [RETAIN_TERMINAL])?;
        let mut count: i64 =
            tx.query_row("SELECT count(*) FROM notification_outbox", [], |r| r.get(0))?;
        if count >= CAPACITY {
            count -= tx.execute("DELETE FROM notification_outbox WHERE id=(SELECT id FROM notification_outbox WHERE state IN ('delivered','failed') ORDER BY id LIMIT 1)", [])? as i64;
        }
        if count < CAPACITY {
            tx.execute("INSERT INTO notification_outbox(check_id,destination,text,state) VALUES(?1,?2,?3,'pending')", params![check_id,destination,text])?;
        } else {
            tx.execute("UPDATE notification_stats SET overflow=min(overflow+1,9223372036854775807) WHERE singleton=1", [])?;
        }
    }
    tx.execute("DELETE FROM checks WHERE url=?1 AND id NOT IN (SELECT id FROM checks WHERE url=?1 ORDER BY id DESC LIMIT 120)", [&entry.url])?;
    tx.commit()?;
    Ok(())
}
fn claim(db: &mut Connection, destination: Option<&str>, now: i64) -> Result<Option<Delivery>> {
    let Some(destination) = destination else {
        return Ok(None);
    };
    let tx = db.transaction()?;
    // FIFO includes the head's cooldown, so recovery cannot overtake its down notification.
    let sending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM notification_outbox WHERE state='sending')",
        [],
        |r| r.get(0),
    )?;
    if sending {
        return Ok(None);
    }
    let next = tx.query_row("SELECT id,text,attempts,next_at FROM notification_outbox WHERE state='pending' AND destination=?1 ORDER BY id LIMIT 1", [destination], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, String>(1)?,r.get::<_, u32>(2)?,r.get::<_, i64>(3)?))).optional()?;
    let Some((id, text, attempts, next_at)) = next else {
        return Ok(None);
    };
    if next_at > now {
        return Ok(None);
    }
    anyhow::ensure!(attempts < 3, "Invalid notification attempts");
    tx.execute("UPDATE notification_outbox SET state='sending',attempts=attempts+1 WHERE id=?1 AND state='pending'", [id])?;
    tx.commit()?;
    Ok(Some(Delivery {
        id,
        text,
        attempt: attempts + 1,
    }))
}
fn finish(db: &Connection, delivery: &Delivery, outcome: Outcome, now: i64) -> Result<()> {
    let (state, reason, next_at, message_id) = match outcome {
        Outcome::Delivered(id) => ("delivered", None, 0, Some(id)),
        Outcome::RateLimited(wait) if delivery.attempt < 3 => (
            "pending",
            Some("rate_limited"),
            now.saturating_add(wait.min(3600) as i64),
            None,
        ),
        Outcome::RateLimited(_) => ("failed", Some("retry_exhausted"), 0, None),
        Outcome::Rejected => ("failed", Some("rejected"), 0, None),
        Outcome::Unknown => ("uncertain", Some("unknown_response"), 0, None),
    };
    // Repeating a completion cannot alter a newer claim or resend a delivered notification.
    db.execute("UPDATE notification_outbox SET state=?1,reason=?2,next_at=?3,message_id=?4 WHERE id=?5 AND state='sending' AND attempts=?6", params![state,reason,next_at,message_id,delivery.id,delivery.attempt])?;
    Ok(())
}
impl Store {
    pub fn start(mut db: Connection, destination: Option<String>) -> Result<Arc<Self>> {
        setup(&db)?;
        let initial = overview(&db, destination.as_deref())?;
        let status = Arc::new(RwLock::new(initial));
        let worker_status = status.clone();
        let (sender, mut receiver) = mpsc::channel(WORK_QUEUE);
        std::thread::Builder::new()
            .name("traceboard-sqlite".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Record(entry, reply) => {
                            let result = record(&mut db, entry, destination.as_deref());
                            update_status(
                                &db,
                                destination.as_deref(),
                                &worker_status,
                                result.is_err(),
                            );
                            let _ = reply.send(result);
                        }
                        Job::Claim(now, reply) => {
                            let result = claim(&mut db, destination.as_deref(), now);
                            update_status(
                                &db,
                                destination.as_deref(),
                                &worker_status,
                                result.is_err(),
                            );
                            let _ = reply.send(result);
                        }
                        Job::Finish(delivery, outcome, now, reply) => {
                            let result = finish(&db, &delivery, outcome, now);
                            update_status(
                                &db,
                                destination.as_deref(),
                                &worker_status,
                                result.is_err(),
                            );
                            let _ = reply.send(result);
                        }
                    }
                }
            })?;
        Ok(Arc::new(Self {
            sender,
            overview: status,
        }))
    }
    pub fn status(&self) -> Overview {
        self.overview
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Overview {
                storage_error: true,
                capacity: CAPACITY,
                ..Default::default()
            })
    }
    pub fn stop_sender(&self) {
        if let Ok(mut status) = self.overview.write() {
            status.sender_stopped = true;
        }
    }
    pub async fn record(&self, url: &str, check: Check, text: Option<String>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Job::Record(
                Record {
                    url: url.into(),
                    check,
                    text,
                },
                reply,
            ))
            .map_err(|_| anyhow::anyhow!("Storage queue unavailable"))?;
        result.await.context("Storage worker stopped")?
    }
    pub async fn claim(&self, now: i64) -> Result<Option<Delivery>> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Job::Claim(now, reply))
            .map_err(|_| anyhow::anyhow!("Storage queue unavailable"))?;
        result.await.context("Storage worker stopped")?
    }
    pub async fn finish(&self, delivery: Delivery, outcome: Outcome, now: i64) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Job::Finish(delivery, outcome, now, reply))
            .map_err(|_| anyhow::anyhow!("Storage queue unavailable"))?;
        result.await.context("Storage worker stopped")?
    }
}
fn update_status(
    db: &Connection,
    destination: Option<&str>,
    status: &RwLock<Overview>,
    error: bool,
) {
    if let Ok(mut current) = status.write() {
        match overview(db, destination) {
            Ok(mut value) => {
                value.storage_error = error;
                value.sender_stopped = current.sender_stopped;
                *current = value;
            }
            Err(_) => current.storage_error = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE checks(id INTEGER PRIMARY KEY AUTOINCREMENT,url TEXT NOT NULL,at TEXT NOT NULL,ok INTEGER NOT NULL,status INTEGER,latency_ms INTEGER NOT NULL,error TEXT,warning TEXT);")?;
        setup(&db)?;
        Ok(db)
    }
    fn entry() -> Record {
        Record {
            url: "http://localhost".into(),
            check: Check {
                at: chrono::Utc::now(),
                ok: false,
                status: Some(500),
                latency_ms: 1,
                error: None,
                warning: None,
            },
            text: Some("outage".into()),
        }
    }
    #[test]
    fn check_and_intent_are_atomic_and_bounded() -> Result<()> {
        let mut db = database()?;
        db.execute_batch("CREATE TRIGGER fail_outbox BEFORE INSERT ON notification_outbox BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        assert!(record(&mut db, entry(), Some("destination")).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM checks", [], |r| r.get::<_, i64>(0))?,
            0
        );
        db.execute("DROP TRIGGER fail_outbox", [])?;
        for _ in 0..CAPACITY + 3 {
            record(&mut db, entry(), Some("destination"))?;
        }
        let status = overview(&db, Some("destination"))?;
        assert_eq!(status.pending, CAPACITY as u64);
        assert_eq!(status.overflow, 3);
        assert_eq!(
            db.query_row("SELECT count(*) FROM checks", [], |r| r.get::<_, i64>(0))?,
            120
        );
        Ok(())
    }
    #[test]
    fn uncertain_is_not_retried_and_only_matching_destination_sends() -> Result<()> {
        let mut db = database()?;
        record(&mut db, entry(), Some("original"))?;
        assert!(claim(&mut db, Some("changed"), 0)?.is_none());
        assert_eq!(overview(&db, Some("changed"))?.paused, 1);
        let delivery = claim(&mut db, Some("original"), 0)?.expect("queued event");
        setup(&db)?; // Opening after an interruption never changes sending back to pending.
        assert!(claim(&mut db, Some("original"), 10)?.is_none());
        finish(&db, &delivery, Outcome::Delivered(1), 10)?;
        assert_eq!(overview(&db, Some("original"))?.uncertain, 1);
        Ok(())
    }
    #[test]
    fn confirmed_rate_limit_obeys_fifo_and_attempt_limit() -> Result<()> {
        let mut db = database()?;
        record(&mut db, entry(), Some("dest"))?;
        record(&mut db, entry(), Some("dest"))?;
        let first = claim(&mut db, Some("dest"), 0)?.expect("first");
        assert!(claim(&mut db, Some("dest"), 0)?.is_none());
        finish(&db, &first, Outcome::RateLimited(3), 0)?;
        finish(&db, &first, Outcome::Unknown, 0)?; // Stale completion must not overwrite the retry.
        assert!(claim(&mut db, Some("dest"), 2)?.is_none());
        for at in [3, 6] {
            let delivery = claim(&mut db, Some("dest"), at)?.expect("retry");
            assert_eq!(delivery.id, first.id);
            finish(&db, &delivery, Outcome::RateLimited(3), at)?;
        }
        assert_eq!(overview(&db, Some("dest"))?.failed, 1);
        let second = claim(&mut db, Some("dest"), 7)?.expect("next event");
        assert_ne!(second.id, first.id);
        finish(&db, &second, Outcome::Delivered(12), 7)?;
        assert!(claim(&mut db, Some("dest"), 8)?.is_none());
        Ok(())
    }
}
