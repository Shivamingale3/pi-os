//! SQLite: the rolling 24 h trend and incidents (7 days). WAL, so the API thread can read
//! while the loop writes. The loop owns the only writing connection.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};

use crate::error::Result;
use crate::rules::{Opened, Severity};
use crate::types::Snapshot;

pub const TREND_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
pub const INCIDENT_KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    ts     INTEGER NOT NULL,
    kind   TEXT    NOT NULL,
    user   TEXT    NOT NULL,
    src    TEXT    NOT NULL,
    detail TEXT    NOT NULL,
    UNIQUE (ts, kind, user, src, detail)
);
CREATE INDEX IF NOT EXISTS events_ts ON events (ts);

CREATE TABLE IF NOT EXISTS alerts (
    id       INTEGER PRIMARY KEY,
    ts       INTEGER NOT NULL,
    kind     TEXT    NOT NULL,
    severity TEXT    NOT NULL,
    text     TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trend (
    ts     INTEGER NOT NULL,
    metric TEXT    NOT NULL,
    value  REAL    NOT NULL
);
CREATE INDEX IF NOT EXISTS trend_metric_ts ON trend (metric, ts);

CREATE TABLE IF NOT EXISTS incidents (
    id             INTEGER PRIMARY KEY,
    key            TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    subject        TEXT    NOT NULL,
    title          TEXT    NOT NULL,
    severity       TEXT    NOT NULL,
    threshold      REAL,
    started_at     INTEGER NOT NULL,
    peak_at        INTEGER,
    peak_value     REAL,
    ended_at       INTEGER,
    end_reason     TEXT,
    start_snapshot TEXT    NOT NULL,
    peak_snapshot  TEXT,
    end_snapshot   TEXT
);
CREATE INDEX IF NOT EXISTS incidents_started ON incidents (started_at);
";

// ---------- trend metrics ----------

fn net_sum(s: &Snapshot, rx: bool) -> Option<f64> {
    Some(
        s.network
            .interfaces
            .iter()
            .filter(|i| !i.name.starts_with("tailscale")) // VPN traffic also passes through eth/wl
            .map(|i| {
                if rx {
                    i.rx_bytes_per_sec
                } else {
                    i.tx_bytes_per_sec
                }
            })
            .sum(),
    )
}

type Reader = fn(&Snapshot) -> Option<f64>;

/// What the Trend screen can graph. `None` (e.g. no sensor) → no row, never a fake 0.
pub const TREND_METRICS: [(&str, Reader); 7] = [
    ("cpu.percent", |s| Some(s.cpu.percent)),
    ("cpu.tempC", |s| s.cpu.temp_c),
    ("mem.usedPercent", |s| Some(s.memory.used_percent)),
    ("disk.usedPercent", |s| Some(s.disk.used_percent)),
    ("load.1m", |s| Some(s.host.load[0])),
    ("net.rxBytesPerSec", |s| net_sum(s, true)),
    ("net.txBytesPerSec", |s| net_sum(s, false)),
];

pub fn is_trend_metric(name: &str) -> bool {
    TREND_METRICS.iter().any(|(m, _)| *m == name)
}

// ---------- writer ----------

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(2))?;
        // NORMAL + WAL: no fsync per commit; a power cut can lose the last minute, never corrupt.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// One row per available trend metric, one transaction.
    pub fn write_trend(&mut self, s: &Snapshot) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt =
                tx.prepare_cached("INSERT INTO trend (ts, metric, value) VALUES (?1, ?2, ?3)")?;
            for (metric, read) in TREND_METRICS {
                if let Some(v) = read(s).filter(|v| v.is_finite()) {
                    stmt.execute(params![s.ts, metric, v])?;
                    n += 1;
                }
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn prune_trend(&self, now: i64) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM trend WHERE ts < ?1", [now - TREND_WINDOW_MS])?)
    }

    pub fn insert_incident(&self, o: &Opened) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO incidents (key, kind, subject, title, severity, threshold, started_at,
                                    peak_at, peak_value, start_snapshot, peak_snapshot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                o.key,
                o.kind.as_str(),
                o.subject,
                o.title,
                o.severity.as_str(),
                o.threshold,
                o.started_at,
                o.peak_at,
                o.peak_value,
                serde_json::to_string(&o.start_snapshot)?,
                serde_json::to_string(&o.peak_snapshot)?,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_severity(&self, id: i64, severity: Severity) -> Result<()> {
        self.conn.execute(
            "UPDATE incidents SET severity = ?2 WHERE id = ?1",
            params![id, severity.as_str()],
        )?;
        Ok(())
    }

    pub fn update_peak(&self, id: i64, at: i64, value: f64, snapshot: &Snapshot) -> Result<()> {
        self.conn.execute(
            "UPDATE incidents SET peak_at = ?2, peak_value = ?3, peak_snapshot = ?4 WHERE id = ?1",
            params![id, at, value, serde_json::to_string(snapshot)?],
        )?;
        Ok(())
    }

    /// Folds the WAL back into the main file (on shutdown), so a stopped agent leaves one file.
    /// Adds events (duplicates ignored); returns the ones that were new.
    /// A boot once marked "power_lost" that turned out to be clean (an older, stricter check).
    pub fn unmark_power_lost(&self, ts: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM events WHERE kind = 'power_lost' AND ts = ?1",
            [ts],
        )?)
    }

    pub fn insert_events(
        &self,
        list: &[crate::events::Event],
    ) -> Result<Vec<crate::events::Event>> {
        let mut new = Vec::new();
        let mut st = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO events (ts, kind, user, src, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for e in list {
            if st.execute(rusqlite::params![e.ts, e.kind, e.user, e.from, e.detail])? > 0 {
                new.push(e.clone());
            }
        }
        Ok(new)
    }

    /// Queues one phone alert (the outbox pios web reads).
    pub fn insert_alert(&self, ts: i64, kind: &str, severity: &str, text: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO alerts (ts, kind, severity, text) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts, kind, severity, text],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn prune_alerts(&self, now: i64) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM alerts WHERE ts < ?1", [now - INCIDENT_KEEP_MS])?)
    }

    pub fn prune_events(&self, now: i64) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM events WHERE ts < ?1", [now - INCIDENT_KEEP_MS])?)
    }

    pub fn get_kv(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn close_incident(
        &self,
        id: i64,
        ended_at: i64,
        reason: &str,
        snapshot: &Snapshot,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE incidents SET ended_at = ?2, end_reason = ?3, end_snapshot = ?4 WHERE id = ?1",
            params![id, ended_at, reason, serde_json::to_string(snapshot)?],
        )?;
        Ok(())
    }

    /// Incidents left open by a crash or power cut: end them at `now`, no end snapshot.
    pub fn close_dangling(&self, now: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE incidents SET ended_at = ?1, end_reason = 'agent stopped unexpectedly' WHERE ended_at IS NULL",
            [now],
        )?)
    }

    pub fn prune_incidents(&self, now: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM incidents WHERE ended_at IS NOT NULL AND ended_at < ?1",
            [now - INCIDENT_KEEP_MS],
        )?)
    }
}

// ---------- readers (any connection) ----------

pub fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    Ok(conn)
}

/// `[[ts, value], …]` oldest first.
pub fn trend(conn: &Connection, metric: &str, since: i64) -> Result<Vec<(i64, f64)>> {
    let mut stmt = conn
        .prepare_cached("SELECT ts, value FROM trend WHERE metric = ?1 AND ts >= ?2 ORDER BY ts")?;
    let rows = stmt.query_map(params![metric, since], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

const SUMMARY_COLS: &str = "id, kind, subject, title, severity, threshold, started_at, peak_at, peak_value, ended_at, end_reason";

fn summary(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "kind": r.get::<_, String>(1)?,
        "subject": r.get::<_, String>(2)?,
        "title": r.get::<_, String>(3)?,
        "severity": r.get::<_, String>(4)?,
        "threshold": r.get::<_, Option<f64>>(5)?,
        "startedAt": r.get::<_, i64>(6)?,
        "peakAt": r.get::<_, Option<i64>>(7)?,
        "peakValue": r.get::<_, Option<f64>>(8)?,
        "endedAt": r.get::<_, Option<i64>>(9)?,
        "endReason": r.get::<_, Option<String>>(10)?,
    }))
}

/// Newest first, without snapshots.
/// Outbox rows after `after` (oldest first, at most 200), and the newest id.
pub fn alerts_after(conn: &Connection, after: i64) -> Result<Vec<crate::alerts::Alert>> {
    let mut st = conn.prepare_cached(
        "SELECT id, ts, kind, severity, text FROM alerts WHERE id > ?1 ORDER BY id LIMIT 200",
    )?;
    let rows = st.query_map([after], |r| {
        Ok(crate::alerts::Alert {
            id: r.get(0)?,
            ts: r.get(1)?,
            kind: r.get(2)?,
            severity: r.get(3)?,
            text: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn kv(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

/// The last 7 days of events, newest first, repeats merged.
pub fn list_events(conn: &Connection, since: i64) -> Result<Vec<crate::events::Event>> {
    let mut st = conn.prepare_cached(
        "SELECT ts, kind, user, src, detail FROM events WHERE ts >= ?1 ORDER BY ts DESC",
    )?;
    let rows = st.query_map([since], |r| {
        Ok(crate::events::Event {
            ts: r.get(0)?,
            kind: r.get(1)?,
            user: r.get(2)?,
            from: r.get(3)?,
            detail: r.get(4)?,
            count: 1,
        })
    })?;
    let list: Vec<crate::events::Event> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(crate::events::merge(list))
}

pub fn list_incidents(conn: &Connection, since: i64) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT {SUMMARY_COLS} FROM incidents WHERE started_at >= ?1 OR ended_at IS NULL ORDER BY started_at DESC"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([since], summary)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One incident with its 3 snapshots (start, peak, end; peak/end may be null).
pub fn get_incident(conn: &Connection, id: i64) -> Result<Option<Value>> {
    let sql = format!(
        "SELECT {SUMMARY_COLS}, start_snapshot, peak_snapshot, end_snapshot FROM incidents WHERE id = ?1"
    );
    let row = conn
        .query_row(&sql, [id], |r| {
            Ok((
                summary(r)?,
                r.get::<_, String>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
            ))
        })
        .optional()?;
    let Some((mut v, start, peak, end)) = row else {
        return Ok(None);
    };
    let parse = |t: Option<String>| -> Result<Value> {
        Ok(match t {
            Some(t) => serde_json::from_str(&t)?,
            None => Value::Null,
        })
    };
    v["snapshots"] =
        json!({ "start": parse(Some(start))?, "peak": parse(peak)?, "end": parse(end)? });
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Kind, tests::snap};

    fn opened(s: &Snapshot) -> Opened {
        Opened {
            key: "metric:cpu.percent".into(),
            kind: Kind::Metric,
            subject: "cpu.percent".into(),
            title: "CPU over 80%".into(),
            severity: Severity::Warn,
            threshold: Some(80.0),
            started_at: s.ts,
            start_snapshot: s.clone(),
            peak_value: Some(s.cpu.percent),
            peak_at: s.ts,
            peak_snapshot: s.clone(),
        }
    }

    #[test]
    fn trend_write_read_prune() {
        let mut st = Store::memory().unwrap();
        let day = TREND_WINDOW_MS;
        let mut old = snap(1_000, 5.0);
        old.cpu.temp_c = None; // no sensor → no row
        assert_eq!(st.write_trend(&old).unwrap(), 6);
        st.write_trend(&snap(day + 2_000, 7.0)).unwrap();
        assert_eq!(
            trend(st.conn(), "cpu.percent", 0).unwrap(),
            [(1_000, 5.0), (day + 2_000, 7.0)]
        );
        assert_eq!(
            trend(st.conn(), "cpu.tempC", 0).unwrap(),
            [(day + 2_000, 40.0)]
        );
        // at now = day + 2_000, the row at 1_000 is older than 24 h
        assert_eq!(st.prune_trend(day + 2_000).unwrap(), 6);
        assert_eq!(trend(st.conn(), "cpu.percent", 0).unwrap().len(), 1);
    }

    #[test]
    fn incident_lifecycle() {
        let st = Store::memory().unwrap();
        let id = st.insert_incident(&opened(&snap(1_000, 85.0))).unwrap();
        st.update_peak(id, 2_000, 97.0, &snap(2_000, 97.0)).unwrap();
        st.update_severity(id, Severity::Critical).unwrap();

        let list = list_incidents(st.conn(), 0).unwrap();
        assert_eq!(list[0]["endedAt"], Value::Null);
        assert_eq!(list[0]["severity"], "critical");

        st.close_incident(id, 3_000, "recovered", &snap(3_000, 50.0))
            .unwrap();
        let v = get_incident(st.conn(), id).unwrap().unwrap();
        assert_eq!(v["peakValue"], 97.0);
        assert_eq!(v["endedAt"], 3_000);
        assert_eq!(v["snapshots"]["start"]["cpu"]["percent"], 85.0);
        assert_eq!(v["snapshots"]["peak"]["cpu"]["percent"], 97.0);
        assert_eq!(v["snapshots"]["end"]["cpu"]["percent"], 50.0);
        assert!(get_incident(st.conn(), id + 1).unwrap().is_none());
    }

    #[test]
    fn dangling_and_prune() {
        let st = Store::memory().unwrap();
        let id = st.insert_incident(&opened(&snap(1_000, 85.0))).unwrap();
        assert_eq!(st.close_dangling(5_000).unwrap(), 1);
        let v = get_incident(st.conn(), id).unwrap().unwrap();
        assert_eq!(v["endReason"], "agent stopped unexpectedly");
        assert_eq!(v["snapshots"]["end"], Value::Null);
        assert_eq!(st.prune_incidents(5_000 + INCIDENT_KEEP_MS).unwrap(), 0);
        assert_eq!(st.prune_incidents(5_001 + INCIDENT_KEEP_MS).unwrap(), 1);
    }
}
