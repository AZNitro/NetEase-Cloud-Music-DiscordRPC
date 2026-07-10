//! The poll loop for NetEase Cloud Music. Clears Discord presence when the
//! player window disappears, and retries failed reader attaches with backoff.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::diag;
use crate::loop_logic;
use crate::platform::window::find_by_class;
use crate::players::netease::NetEase;
use crate::players::MusicPlayer;
use crate::rpc::Rpc;

const NETEASE_APP_ID: &str = "481562643958595594";
const NETEASE_CLASS: &str = "OrpheusBrowserHost";
const POLL: Duration = Duration::from_millis(233);

/// Commands from the tray / UI thread.
pub enum Command {
    Exit,
}

/// Cached reader state for the current NetEase PID. Failed attaches are retried
/// after backoff so a startup race (window up before the DLL is loaded) can recover.
enum Cached {
    Reader(Box<dyn MusicPlayer>),
    Failed { next_retry: Instant },
}

/// Run until `running` is false or an [`Command::Exit`] arrives.
pub fn run_loop(running: Arc<AtomicBool>, commands: Receiver<Command>) -> anyhow::Result<()> {
    diag!("MusicRpc poll loop starting (NetEase only)");

    let mut rpc = Rpc::new(NETEASE_APP_ID, "NetEase CloudMusic")?;
    rpc.ensure_connected();

    let mut cached: Option<(u32, Cached)> = None;
    let mut had_presence = false;
    let mut last_detect: Option<(u32, String)> = None;
    // Log track changes only: (identity, paused).
    let mut last_track_key: Option<(String, bool)> = None;

    while running.load(Ordering::SeqCst) {
        if commands.try_recv().is_ok() {
            running.store(false, Ordering::SeqCst);
            if had_presence {
                rpc.clear();
            }
            diag!("[loop] exit requested");
            return Ok(());
        }

        let target = find_by_class(NETEASE_CLASS).map(|(title, pid)| (pid, title));
        let detected = target.is_some();

        if loop_logic::should_clear_presence(had_presence, detected) {
            diag!("[loop] clearing presence (NetEase window gone)");
            rpc.clear();
            had_presence = false;
            last_track_key = None;
        }

        if last_detect != target {
            match &target {
                Some((pid, title)) => {
                    diag!("[loop] detected NetEase window pid={pid} title={title:?}")
                }
                None => diag!("[loop] no NetEase window found (waiting)"),
            }
            last_detect = target.clone();
        }

        let Some((pid, _title)) = target else {
            cached = None;
            sleep(POLL);
            continue;
        };

        let now = Instant::now();
        let rebuild = match &cached {
            Some((p, Cached::Reader(_))) if *p == pid => false,
            Some((p, Cached::Failed { next_retry })) if *p == pid => now >= *next_retry,
            _ => true,
        };

        if rebuild {
            match NetEase::new(pid) {
                Ok(r) => cached = Some((pid, Cached::Reader(Box::new(r)))),
                Err(e) => {
                    diag!("[loop] failed to init NetEase (pid {pid}): {e:#}; retrying soon");
                    cached = Some((
                        pid,
                        Cached::Failed {
                            next_retry: now
                                + Duration::from_millis(loop_logic::ATTACH_RETRY_MS as u64),
                        },
                    ));
                }
            }
        }

        let reader = match &cached {
            Some((_, Cached::Reader(r))) => r,
            _ => {
                sleep(POLL);
                continue;
            }
        };

        match reader.player_info() {
            None => {
                if had_presence {
                    diag!("[loop] no track info; clearing presence");
                    rpc.clear();
                    had_presence = false;
                }
                last_track_key = None;
            }
            Some(info) => {
                let key = (info.identity.clone(), info.paused);
                if last_track_key.as_ref() != Some(&key) {
                    diag!(
                        "[loop] track id={} paused={} title={:?}",
                        info.identity,
                        info.paused,
                        info.title
                    );
                    last_track_key = Some(key);
                }

                if info.paused {
                    rpc.clear();
                } else {
                    rpc.update(&info);
                }
                had_presence = true;
            }
        }

        sleep(POLL);
    }

    if had_presence {
        rpc.clear();
    }
    Ok(())
}

/// Headless entry (no tray): run until the process is killed.
pub fn run() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::mem::forget(tx);
    run_loop(Arc::new(AtomicBool::new(true)), rx)
}
