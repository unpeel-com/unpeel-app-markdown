//! The entire Unpeel integration, self-contained. An Unpeel App is a plain
//! terminal program; when hosted, Unpeel exports `UNPEEL_SESSION_ID` and
//! friends, and the App talks back over two tiny surfaces:
//!
//! - **Activity**: POST the canonical hook events (`UserPromptSubmit` busy,
//!   `Stop` idle, `PermissionRequest` attention) to `/hook/<session_id>` on
//!   every registered port, mirrored into the durable
//!   `last-hook-event.json` seed so the latch survives frontend restarts.
//! - **Status text**: the `status.json` marker in the session dir — atomic
//!   whole-file overwrite, debounced, announced on the state bus.
//! - **Live context**: the `app-context.json` marker — what the App is
//!   showing (open file, cursor line, selection), surfaced by Unpeel as
//!   this pane's `app_context` on MCP pane-context queries. Read on
//!   demand, so written without a state-bus ping.
//!
//! Outside Unpeel every call is a silent no-op. No SDK required; this file
//! is the whole contract and is freely copyable into any App.

use std::io::Write as _;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct Host {
    pub session_id: String,
    pub session_dir: PathBuf,
    pub app_port: Option<u16>,
    pub port_registry: PathBuf,
}

impl Host {
    pub fn detect() -> Option<Host> {
        let session_id = std::env::var("UNPEEL_SESSION_ID").ok()?;
        if session_id.trim().is_empty() {
            return None;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let unpeel_home = std::env::var_os("UNPEEL_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".unpeel")))?;
        let session_dir = std::env::var_os("UNPEEL_SESSION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| unpeel_home.join("app-sessions").join(&session_id));
        let port_registry = std::env::var_os("UNPEEL_APP_PORT_REGISTRY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| unpeel_home.join("app-ports"));
        Some(Host {
            session_id,
            session_dir,
            app_port: std::env::var("UNPEEL_APP_PORT")
                .ok()
                .and_then(|port| port.parse().ok()),
            port_registry,
        })
    }

    /// Every port to notify: the launching instance first, then the shared
    /// registry (multiple Unpeel instances can run at once), deduplicated.
    fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.app_port.into_iter().collect();
        if let Ok(raw) = std::fs::read_to_string(&self.port_registry) {
            for line in raw.lines() {
                if let Ok(port) = line.trim().parse::<u16>()
                    && !ports.contains(&port)
                {
                    ports.push(port);
                }
            }
        }
        ports
    }
}

/// Sidebar presence: activity plus one short status line. Construct once and
/// keep it for the process lifetime; drop or `flush` before exit so a
/// trailing debounced status lands.
pub struct StatusReporter {
    host: Option<Host>,
    last_written: Option<(Instant, String)>,
    pending: Option<String>,
    context_last: Option<(Instant, String)>,
    context_pending: Option<String>,
}

impl StatusReporter {
    pub fn detect() -> Self {
        StatusReporter {
            host: Host::detect(),
            last_written: None,
            pending: None,
            context_last: None,
            context_pending: None,
        }
    }

    /// The session is working — sidebar spinner. Usage scans are quick and
    /// local, so this app never claims Busy; kept because it is part of the
    /// three-event contract any App can use.
    #[allow(dead_code)]
    pub fn busy(&self) {
        self.post_hook_event("UserPromptSubmit");
    }

    /// The session settled — spinner clears, unread integrates.
    pub fn idle(&self) {
        self.post_hook_event("Stop");
    }

    /// The session needs the user — attention accent, and Unpeel's ordinary
    /// needs-input notification path (desktop banner, phone push). An editor
    /// is user-paced, so unused here; part of the three-event contract.
    #[allow(dead_code)]
    pub fn attention(&self) {
        self.post_hook_event("PermissionRequest");
    }

    /// Short, single-line sidebar status ("Codex 3% · Claude $2.10").
    /// Rapid-fire and identical writes coalesce; the latest text wins.
    pub fn set_status(&mut self, text: &str) {
        if self.host.is_none() {
            return;
        }
        let text = text.trim().replace(['\n', '\r'], " ");
        if let Some((_, last)) = &self.last_written
            && *last == text
            && self.pending.is_none()
        {
            return;
        }
        if let Some((at, _)) = &self.last_written
            && at.elapsed() < DEBOUNCE
        {
            self.pending = Some(text);
            return;
        }
        self.write_status(&text);
    }

    pub fn flush(&mut self) {
        if let Some(text) = self.pending.take() {
            self.write_status(&text);
        }
        if let Some(entry) = self.context_pending.take() {
            self.write_context(&entry);
        }
    }

    /// Publish this App's live context (open file, cursor line, selection) as
    /// the `app-context.json` marker in the session dir. Unpeel reads it
    /// fresh on pane-context queries and surfaces it verbatim as this pane's
    /// `app_context` on `sessions current` / `apps context` neighbor entries
    /// — read-on-demand data, so unlike status there is no state-changed
    /// ping. Safe to call per keystroke: identical payloads write nothing
    /// and rapid-fire changes coalesce like status.
    pub fn set_context(&mut self, app_id: &str, context: &serde_json::Value) {
        if self.host.is_none() {
            return;
        }
        let entry = format!(r#""app":{},"context":{context}"#, json_string(app_id));
        if let Some((_, last)) = &self.context_last
            && *last == entry
            && self.context_pending.is_none()
        {
            return;
        }
        if let Some((at, _)) = &self.context_last
            && at.elapsed() < DEBOUNCE
        {
            self.context_pending = Some(entry);
            return;
        }
        self.write_context(&entry);
    }

    fn write_context(&mut self, entry: &str) {
        let Some(host) = &self.host else { return };
        // Never create the session dir — the session may already be gone.
        if !host.session_dir.is_dir() {
            return;
        }
        let body = format!(r#"{{{entry},"updated_at":{}}}"#, now_ms());
        let tmp = host.session_dir.join(".app-context.json.tmp");
        let _ = std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, host.session_dir.join("app-context.json")));
        self.context_pending = None;
        self.context_last = Some((Instant::now(), entry.to_string()));
    }

    /// Report what this App is currently showing ("hero.md", a picked note,
    /// a project folder). Unpeel folds it into the session's title the way
    /// agent auto-titles work: it keeps following later calls (picker →
    /// another document retitles), and a user's manual rename permanently
    /// wins. Not debounced — call it on real navigation, not per keystroke.
    pub fn set_title(&self, text: &str) {
        let Some(host) = &self.host else { return };
        if !host.session_dir.is_dir() {
            return;
        }
        let text = text.trim().replace(['\n', '\r'], " ");
        if text.is_empty() {
            return;
        }
        let body = format!(
            r#"{{"text":{},"updated_at":{}}}"#,
            json_string(&text),
            now_ms()
        );
        let tmp = host.session_dir.join(".app-title.json.tmp");
        let ok = std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, host.session_dir.join("app-title.json")))
            .is_ok();
        if ok {
            post_json(host, "/state-changed", r#"{"change":"session-markers"}"#);
        }
    }

    fn write_status(&mut self, text: &str) {
        let Some(host) = &self.host else { return };
        // Never create the session dir — the session may already be gone.
        if !host.session_dir.is_dir() {
            return;
        }
        let body = format!(
            r#"{{"text":{},"updated_at":{}}}"#,
            json_string(text),
            now_ms()
        );
        let tmp = host.session_dir.join(".status.json.tmp");
        let ok = std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, host.session_dir.join("status.json")))
            .is_ok();
        if ok {
            // The ping is an optimisation; frontends still poll.
            post_json(host, "/state-changed", r#"{"change":"session-markers"}"#);
        }
        self.pending = None;
        self.last_written = Some((Instant::now(), text.to_string()));
    }

    fn post_hook_event(&self, event: &str) {
        let Some(host) = &self.host else { return };
        let body = format!(r#"{{"hook_event_name":"{event}"}}"#);
        // Durable seed first, so the latch survives even when no instance is
        // listening right now — the same file the provider hook scripts keep.
        if host.session_dir.is_dir() {
            let tmp = host
                .session_dir
                .join(format!(".last-hook-event.json.{}", std::process::id()));
            if std::fs::write(&tmp, &body).is_ok() {
                let _ = std::fs::rename(&tmp, host.session_dir.join("last-hook-event.json"));
            }
        }
        post_json(host, &format!("/hook/{}", host.session_id), &body);
    }
}

impl Drop for StatusReporter {
    fn drop(&mut self) {
        self.flush();
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// Fire-and-forget local POST to every registered instance. Failures are
/// ignored: a port whose owner has gone is normal.
fn post_json(host: &Host, path: &str, body: &str) {
    for port in host.ports() {
        let address = format!("127.0.0.1:{port}");
        let Ok(target) = address.parse() else {
            continue;
        };
        let Ok(mut stream) = TcpStream::connect_timeout(&target, IO_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter_for(dir: &std::path::Path) -> StatusReporter {
        StatusReporter {
            host: Some(Host {
                session_id: "test-session".into(),
                session_dir: dir.to_path_buf(),
                app_port: None,
                port_registry: dir.join("no-ports"),
            }),
            last_written: None,
            pending: None,
            context_last: None,
            context_pending: None,
        }
    }

    #[test]
    fn set_context_writes_and_coalesces_the_marker() {
        let temp = tempfile::tempdir().unwrap();
        let mut reporter = reporter_for(temp.path());
        let marker = temp.path().join("app-context.json");

        let context = serde_json::json!({
            "file": "/notes/todo.md",
            "cursor_line": 12,
            "selection_lines": [4, 9],
            "dirty": false,
        });
        reporter.set_context("unpeel.app.markdown", &context);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(written["app"], "unpeel.app.markdown");
        assert_eq!(written["context"], context);
        assert!(written["updated_at"].as_u64().is_some());

        // An identical payload writes nothing; a changed one lands within
        // the debounce via pending + flush.
        std::fs::remove_file(&marker).unwrap();
        reporter.set_context("unpeel.app.markdown", &context);
        assert!(!marker.exists(), "identical context must not rewrite");
        let moved = serde_json::json!({ "file": "/notes/todo.md", "cursor_line": 13 });
        reporter.set_context("unpeel.app.markdown", &moved);
        reporter.flush();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(written["context"]["cursor_line"], 13);
    }
}
