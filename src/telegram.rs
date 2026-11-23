//! Sending phone alerts with a Telegram bot, from `pios web` (the only PiOS process with network).
//!
//! The bot token comes from a systemd credential (`LoadCredential=telegram:/etc/pios/telegram`):
//! line 1 the token, optional line 2 the chat ID. Without a chat ID, the chat that last messaged
//! the bot is used and remembered. The token goes to curl through its stdin (`--config -`), never
//! on a command line, and is never logged.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::alerts::{self, Alert, Status};
use crate::error::{Result, err};

/// A burst of alerts becomes one message: wait until the newest is this old (or the oldest is
/// a minute old) before sending.
const SETTLE_MS: i64 = 10_000;
const BURST_MAX_MS: i64 = 60_000;

pub struct Telegram {
    pub token: String,
    pub chat: Option<String>,
}

/// The credential file: token, then optionally the chat ID. Empty = alerts off.
pub fn parse_credential(text: &str) -> Option<Telegram> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let token = lines.next()?.to_string();
    // "123456:ABC-…": digits, a colon, then the secret
    let (id, secret) = token.split_once(':')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || secret.len() < 20 {
        return None;
    }
    let chat = lines
        .next()
        .filter(|c| {
            c.trim_start_matches('-')
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .map(String::from);
    Some(Telegram { token, chat })
}

/// One string for curl's config syntax: quoted, with \ " and newlines escaped.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Runs curl with `config` on stdin; returns (HTTP code, body).
fn curl(config: &str) -> Result<(u16, String)> {
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--max-time",
            "15",
            "--config",
            "-",
            "--write-out",
            "\n%{response_code}",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| err(format!("curl: {e}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| err("curl: no stdin"))?
        .write_all(config.as_bytes())?;
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// Telegram's reply → Ok, or its description (no token in it).
fn check(code: u16, body: &str) -> Result<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if code == 200 && v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        return Ok(v);
    }
    let why = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            if code == 0 {
                "no connection".into()
            } else {
                format!("HTTP {code}")
            }
        });
    Err(err(why))
}

impl Telegram {
    pub fn send(&self, chat: &str, text: &str) -> Result<()> {
        let cfg = format!(
            "url = {}\ndata-urlencode = {}\ndata-urlencode = {}\n",
            quote(&format!(
                "https://api.telegram.org/bot{}/sendMessage",
                self.token
            )),
            quote(&format!("chat_id={chat}")),
            quote(&format!("text={text}")),
        );
        let (code, body) = curl(&cfg)?;
        check(code, &body).map(|_| ())
    }

    /// The chat that last wrote to the bot (send it any message once).
    pub fn find_chat(&self) -> Result<Option<String>> {
        let cfg = format!(
            "url = {}\n",
            quote(&format!(
                "https://api.telegram.org/bot{}/getUpdates",
                self.token
            ))
        );
        let (code, body) = curl(&cfg)?;
        let v = check(code, &body)?;
        Ok(last_chat(&v))
    }
}

/// The newest message's chat ID in a getUpdates reply.
pub fn last_chat(v: &serde_json::Value) -> Option<String> {
    v.get("result")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|u| {
            let m = u.get("message").or_else(|| u.get("my_chat_member"))?;
            m.get("chat")?.get("id")?.as_i64()
        })
        .map(|id| id.to_string())
}

/// Wait for a burst to finish? (newest under 10 s old, unless the oldest has waited a minute)
pub fn settling(list: &[Alert], now: i64) -> bool {
    let (Some(first), Some(last)) = (list.first(), list.last()) else {
        return false;
    };
    now - last.ts < SETTLE_MS && now - first.ts < BURST_MAX_MS
}

/// What `pios web` keeps between rounds (its StateDirectory): the outbox cursor and the chat.
pub struct State {
    dir: Option<PathBuf>,
    pub cursor: i64,
    pub chat: Option<String>,
}

impl State {
    pub fn load(dir: Option<PathBuf>) -> State {
        let read = |n: &str| {
            dir.as_ref()
                .and_then(|d| std::fs::read_to_string(d.join(n)).ok())
        };
        State {
            cursor: read("cursor")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(-1),
            chat: read("chat")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            dir,
        }
    }
    pub fn save(&self) {
        if let Some(d) = &self.dir {
            let _ = std::fs::write(d.join("cursor"), self.cursor.to_string());
            if let Some(c) = &self.chat {
                let _ = std::fs::write(d.join("chat"), c);
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct Pending {
    alerts: Vec<Alert>,
    last: i64,
}

/// One round: learn the chat if needed, read what's due from the agent, send it as one message.
/// Returns the status to report (written next to the web results).
pub fn round(
    cred: Option<&Telegram>,
    st: &mut State,
    agent: &Path,
    host: &str,
    now: i64,
) -> Status {
    let Some(tg) = cred else {
        return Status::default(); // not set up
    };
    let mut status = Status {
        configured: true,
        ..Status::default()
    };
    if st.chat.is_none() {
        st.chat = tg.chat.clone();
    }
    if st.chat.is_none() {
        match tg.find_chat() {
            Ok(Some(c)) => {
                st.chat = Some(c);
                st.save();
            }
            Ok(None) => {
                status.error = Some("send your bot any message once".into());
                return status;
            }
            Err(e) => {
                status.error = Some(e.to_string());
                return status;
            }
        }
    }
    status.chat = true;
    let path = format!("/alerts?after={}", st.cursor);
    let pending: Pending = match crate::http::get(agent, &path, Duration::from_secs(3))
        .ok()
        .filter(|(c, _)| *c == 200)
        .and_then(|(_, b)| serde_json::from_str(&b).ok())
    {
        Some(p) => p,
        None => {
            status.error = Some("the PiOS agent isn't answering".into());
            return status;
        }
    };
    if pending.alerts.is_empty() {
        if pending.last != st.cursor {
            st.cursor = pending.last;
            st.save();
        }
        return status;
    }
    if settling(&pending.alerts, now) {
        return status;
    }
    let chat = st.chat.clone().unwrap_or_default();
    match tg.send(&chat, &alerts::message(host, &pending.alerts)) {
        Ok(()) => {
            st.cursor = pending.last;
            st.save();
            status.last_sent = Some(now);
        }
        Err(e) => status.error = Some(e.to_string()),
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_file() {
        let t =
            parse_credential("123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw\n-100200300\n").unwrap();
        assert_eq!(t.chat.as_deref(), Some("-100200300"));
        assert!(parse_credential("# nothing yet\n\n").is_none());
        assert!(parse_credential("not-a-token").is_none());
        assert!(parse_credential("123:short").is_none());
        let no_chat =
            parse_credential("123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw\nnope").unwrap();
        assert_eq!(no_chat.chat, None);
    }

    #[test]
    fn curl_config_quoting() {
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn chat_from_updates() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"ok":true,"result":[{"update_id":1,"message":{"chat":{"id":111}}},{"update_id":2,"message":{"chat":{"id":222}}}]}"#,
        )
        .unwrap();
        assert_eq!(last_chat(&v).as_deref(), Some("222"));
        assert_eq!(last_chat(&serde_json::json!({"ok":true,"result":[]})), None);
        assert!(
            check(401, r#"{"ok":false,"description":"Unauthorized"}"#)
                .unwrap_err()
                .to_string()
                .contains("Unauthorized")
        );
    }

    #[test]
    fn bursts_wait_a_little() {
        let a = |ts| Alert {
            id: 1,
            ts,
            kind: "x".into(),
            severity: "".into(),
            text: "".into(),
        };
        assert!(settling(&[a(1_000)], 5_000), "newest only 4 s old");
        assert!(!settling(&[a(1_000)], 12_000));
        assert!(
            !settling(&[a(1_000), a(60_000)], 62_000),
            "the oldest waited a minute"
        );
        assert!(!settling(&[], 0));
    }
}
