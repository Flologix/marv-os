//! The kernel's event log on disk (runtime-kernel phase 3, decision D10). New in marv-rs.
//!
//! One append-only JSONL file a day, `<config dir>/events/<yyyy-mm-dd>.jsonl` (UTC), for every Marv of the user's that
//! logs (the desktop, the TUI, `marv mesh allow`/`revoke`; never `marv -p`). Each line is one
//! [`marv_kernel::LoggedEvent`]: `v`, `at_ms` and the event, which never holds a message's words
//! ([`marv_kernel::KernelEvent`]). `marv kernel replay <file>` rebuilds the registry, rules and states from it.
//!
//! - **Where:** the user's own config folder, given absolute (`~/.marv`, or an absolute `MARV_CONFIG_DIR`): never a
//!   project's, and a relative config folder (which would land inside the project) is refused, as for the rules.
//! - **Private:** the folder is 0700 and the files 0600, made so on every open; a folder or file that is a symlink, or
//!   not a folder or a file, is never written through.
//! - **Never breaks a run:** like the trajectory writer, a writer thread does the I/O; appending only queues, into a
//!   bounded queue ([`EVENT_QUEUE`]): when it's full the event is dropped, never waited for. The first failure (or
//!   drop) is reported once ([`EventWriter::on_error`]); after a failure the rest are dropped quietly. A crash can
//!   leave a half-written last line, so a file is started on a fresh line before a process's first event.

use marv_fs::mode::CreateMode;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use marv_kernel::{EventLog, KernelEvent, LoggedEvent};

use crate::trajectory::OnError;

/// How many events may wait for the writer; more are dropped (and the drop reported once).
pub const EVENT_QUEUE: usize = 4096;

/// `<config dir>/events`.
pub fn events_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("events")
}

/// The day's file for an event at `at_ms`: `<config dir>/events/<yyyy-mm-dd>.jsonl` (UTC).
pub fn events_path(config_dir: &Path, at_ms: u64) -> PathBuf {
    events_dir(config_dir).join(format!("{}.jsonl", crate::time::iso_date(at_ms)))
}

enum Job {
    Line(PathBuf, String),
    Flush(std::sync::mpsc::Sender<()>),
}

/// The writer. See the module doc.
pub struct EventWriter {
    config_dir: PathBuf,
    tx: mpsc::SyncSender<Job>,
    on_error: Arc<Mutex<Option<OnError>>>,
    /// Set once the first problem was reported (a failure, or the queue full): later ones go quietly.
    told: Arc<AtomicBool>,
}

impl EventWriter {
    /// A writer into `config_dir`'s `events` folder. `config_dir` must be absolute: the user's own config folder.
    pub fn new(config_dir: &Path) -> Result<Arc<Self>, String> {
        Self::with_queue(config_dir, EVENT_QUEUE)
    }

    /// [`Self::new`] with another queue size (tests).
    pub fn with_queue(config_dir: &Path, queue: usize) -> Result<Arc<Self>, String> {
        if !config_dir.is_absolute() {
            return Err(format!(
                "the config folder {} isn't an absolute path: the event log is kept only in the user's own config folder",
                config_dir.display()
            ));
        }
        let (tx, rx) = mpsc::sync_channel::<Job>(queue);
        let on_error: Arc<Mutex<Option<OnError>>> = Arc::default();
        let told = Arc::new(AtomicBool::new(false));
        let (dir, report, told_writer) = (events_dir(config_dir), on_error.clone(), told.clone());
        std::thread::Builder::new()
            .name("marv-events".into())
            .spawn(move || writer(dir, rx, report, told_writer))
            .map_err(|e| format!("can't start the event log's writer: {e}"))?;
        Ok(Arc::new(Self { config_dir: config_dir.to_path_buf(), tx, on_error, told }))
    }

    /// Where the first problem is reported (once).
    pub fn on_error(&self, report: OnError) {
        *self.on_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
    }

    /// The file an event at `at_ms` goes to.
    pub fn path_at(&self, at_ms: u64) -> PathBuf {
        events_path(&self.config_dir, at_ms)
    }

    /// Resolves once every event appended so far is written (or dropped).
    pub async fn flush(&self) {
        let (done, wait) = std::sync::mpsc::channel();
        let tx = self.tx.clone();
        // The queue may be full: hand the flush over off the runtime's threads.
        let _ = tokio::task::spawn_blocking(move || {
            if tx.send(Job::Flush(done)).is_ok() {
                let _ = wait.recv();
            }
        })
        .await;
    }

    fn tell(&self, text: String) {
        tell_once(&self.on_error, &self.told, text);
    }
}

fn tell_once(on_error: &Mutex<Option<OnError>>, told: &AtomicBool, text: String) {
    if !told.swap(true, Ordering::SeqCst)
        && let Some(report) = on_error.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
    {
        report(text);
    }
}

impl EventLog for EventWriter {
    fn append(&self, at_ms: u64, event: KernelEvent) {
        let line = format!("{}\n", LoggedEvent::new(at_ms, event).line());
        match self.tx.try_send(Job::Line(self.path_at(at_ms), line)) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.tell("The kernel's event log can't keep up: some events weren't logged.".into());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
    }
}

fn writer(dir: PathBuf, rx: mpsc::Receiver<Job>, on_error: Arc<Mutex<Option<OnError>>>, told: Arc<AtomicBool>) {
    let mut open: Option<(PathBuf, std::fs::File)> = None;
    let mut failed = false;
    for job in rx {
        match job {
            Job::Flush(done) => {
                let _ = done.send(());
            }
            Job::Line(..) if failed => {}
            Job::Line(path, line) => {
                let result = (|| -> std::io::Result<()> {
                    if open.as_ref().is_none_or(|(p, _)| *p != path) {
                        open = Some((path.clone(), open_log(&dir, &path)?));
                    }
                    let (_, file) = open.as_mut().expect("opened");
                    // One write per line: lines from several Marvs appending to the same day's file don't interleave.
                    file.write_all(line.as_bytes())
                })();
                if let Err(err) = result {
                    failed = true;
                    open = None;
                    tell_once(
                        &on_error,
                        &told,
                        format!("Couldn't write the kernel's event log ({err}); events won't be logged."),
                    );
                }
            }
        }
    }
}

fn refuse(what: &Path) -> std::io::Error {
    std::io::Error::other(format!("{} is a symlink or not what it should be: not writing the event log there", what.display()))
}

/// The events folder (0700, made if missing, never a symlink), then the day's file for append (0600, never a
/// symlink), starting on a fresh line if a crash left a half-written one.
fn open_log(dir: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    use std::io::{Read, Seek, SeekFrom};
    if path.parent() != Some(dir) {
        return Err(refuse(path));
    }
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => return Err(refuse(dir)),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
            if !std::fs::symlink_metadata(dir)?.is_dir() {
                return Err(refuse(dir));
            }
        }
        Err(e) => return Err(e),
    }
    marv_fs::mode::set_mode(dir, 0o700)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(refuse(path)),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new().read(true).append(true).create(true).create_mode(0o600).open(path)?;
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(refuse(path));
    }
    marv_fs::mode::set_mode(path, 0o600)?;
    let len = file.metadata()?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            file.write_all(b"\n")?;
        }
    }
    Ok(file)
}

/// Logs a rule the person just made (the desktop's Mesh panel, `marv mesh allow`), as stored in `snapshot` (what the
/// store's `add` returned).
pub fn log_created(log: &dyn EventLog, id: marv_kernel::PolicyId, snapshot: &marv_kernel::PolicySnapshot) {
    let rule = snapshot.rules.iter().find(|r| r.id == id).cloned();
    log.append(crate::time::now_ms(), KernelEvent::Policy { id, change: marv_kernel::PolicyChange::Created, rule });
}

/// Logs a rule the person just revoked.
pub fn log_revoked(log: &dyn EventLog, id: marv_kernel::PolicyId) {
    log.append(crate::time::now_ms(), KernelEvent::Policy { id, change: marv_kernel::PolicyChange::Revoked, rule: None });
}

/// Logs the rules in `snapshot` found expired by `now_ms` and not noticed before.
pub fn notice_expiries(
    seen: &Mutex<marv_kernel::Expiries>,
    snapshot: &marv_kernel::PolicySnapshot,
    now_ms: u64,
    log: &dyn EventLog,
) {
    let expired = seen.lock().unwrap_or_else(|e| e.into_inner()).newly_expired(snapshot, now_ms);
    for id in expired {
        log.append(now_ms, KernelEvent::Policy { id, change: marv_kernel::PolicyChange::Expired, rule: None });
    }
}

/// Logs a grant the person just made or changed (the desktop), as stored in `snapshot`, and the grants it replaced.
pub fn log_grant_made(
    log: &dyn EventLog,
    id: marv_kernel::GrantId,
    snapshot: &marv_kernel::GrantSnapshot,
    replaced: &[marv_kernel::GrantId],
) {
    let now = crate::time::now_ms();
    for old in replaced {
        log.append(now, KernelEvent::GrantRevoked { id: *old });
    }
    if let Some(grant) = snapshot.grants.iter().find(|g| g.id == id).cloned() {
        log.append(now, KernelEvent::GrantMade { grant });
    }
}

/// Logs a grant the person just revoked.
pub fn log_grant_revoked(log: &dyn EventLog, id: marv_kernel::GrantId) {
    log.append(crate::time::now_ms(), KernelEvent::GrantRevoked { id });
}

/// Logs the grants in `snapshot` found expired by `now_ms` and not noticed before (`seen`: those noticed).
pub fn notice_grant_expiries(
    seen: &Mutex<std::collections::BTreeSet<marv_kernel::GrantId>>,
    snapshot: &marv_kernel::GrantSnapshot,
    now_ms: u64,
    log: &dyn EventLog,
) {
    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    for grant in snapshot.grants.iter().filter(|g| !g.live_at(now_ms)) {
        if seen.insert(grant.id) {
            log.append(now_ms, KernelEvent::GrantExpired { id: grant.id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_kernel::{AgentKey, AgentState, parse_log};
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    fn state(n: u8) -> KernelEvent {
        KernelEvent::State { agent: AgentKey::Mesh(format!("m{n}")), from: None, to: AgentState::Idle }
    }

    /// A temp folder, removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("marv-events-{}-{}", std::process::id(), marv_fs::random_hex()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const DAY: u64 = 1_760_659_200_000; // 2025-10-17T00:00:00Z

    #[cfg(unix)]
    #[tokio::test]
    async fn events_go_to_the_days_private_file_in_order() {
        let tmp = Tmp::new();
        let config = tmp.path().join(".marv");
        let log = EventWriter::new(&config).unwrap();
        log.append(DAY + 5, state(1));
        log.append(DAY + 6, state(2));
        log.append(DAY + 86_400_000, state(3));
        log.flush().await;
        let today = config.join("events/2025-10-17.jsonl");
        assert_eq!(log.path_at(DAY), today);
        let (events, skipped) = parse_log(&std::fs::read_to_string(&today).unwrap());
        assert_eq!((events.iter().map(|(t, _)| *t).collect::<Vec<_>>(), skipped), (vec![DAY + 5, DAY + 6], 0));
        assert_eq!(events[1].1, state(2));
        assert!(config.join("events/2025-10-18.jsonl").is_file(), "the next day's file");
        assert_eq!(std::fs::metadata(&today).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(config.join("events")).unwrap().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_half_written_line_is_closed_and_a_wide_folder_or_file_made_private() {
        let tmp = Tmp::new();
        let config = tmp.path().join(".marv");
        let dir = config.join("events");
        std::fs::create_dir_all(&dir).unwrap();
        marv_fs::mode::set_mode(&dir, 0o755).unwrap();
        let file = dir.join("2025-10-17.jsonl");
        std::fs::write(&file, "{\"v\":1,\"at").unwrap();
        marv_fs::mode::set_mode(&file, 0o644).unwrap();
        let log = EventWriter::new(&config).unwrap();
        log.append(DAY, state(1));
        log.flush().await;
        let (events, skipped) = parse_log(&std::fs::read_to_string(&file).unwrap());
        assert_eq!((events.len(), skipped), (1, 1));
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failure_is_told_once_and_never_breaks_the_caller() {
        let tmp = Tmp::new();
        let config = tmp.path().join(".marv");
        std::fs::create_dir_all(&config).unwrap();
        // The events folder is a symlink to somewhere else: refused.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, config.join("events")).unwrap();
        let told: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = EventWriter::new(&config).unwrap();
        let t = told.clone();
        log.on_error(Arc::new(move |text| t.lock().unwrap().push(text)));
        for n in 0..5 {
            log.append(DAY, state(n));
        }
        log.flush().await;
        assert_eq!(told.lock().unwrap().len(), 1, "{:?}", told.lock().unwrap());
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written through the symlink");
        assert!(EventWriter::new(Path::new(".marv")).is_err(), "a relative config folder is refused");
    }

    #[tokio::test]
    async fn a_full_queue_drops_events_and_says_so_once() {
        let tmp = Tmp::new();
        let log = EventWriter::with_queue(&tmp.path().join(".marv"), 1).unwrap();
        let told: Arc<Mutex<Vec<String>>> = Arc::default();
        let t = told.clone();
        log.on_error(Arc::new(move |text| t.lock().unwrap().push(text)));
        for n in 0..200 {
            log.append(DAY, state((n % 250) as u8));
        }
        log.flush().await;
        let told = told.lock().unwrap();
        assert!(told.len() <= 1, "{told:?}");
        let (events, _) = parse_log(&std::fs::read_to_string(log.path_at(DAY)).unwrap());
        assert!(!events.is_empty() && events.len() <= 200);
    }
}
