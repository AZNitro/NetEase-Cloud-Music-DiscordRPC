//! The poll loop. Port of `Program.UpdateThread`, with the fixes from analysis:
//! clear presence when the player window disappears, clear the other Discord
//! client when switching apps, and retry failed reader attaches with backoff.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::diag;
use crate::loop_logic::{self, PlayerKind};
use crate::platform::window::find_by_class;
use crate::players::{netease::NetEase, tencent::Tencent, MusicPlayer};
use crate::rpc::Rpc;

const NETEASE_APP_ID: &str = "481562643958595594";
const TENCENT_APP_ID: &str = "903485504899665990";

const NETEASE_CLASS: &str = "OrpheusBrowserHost";
const TENCENT_CLASS: &str = "QQMusic_Daemon_Wnd";

const POLL: Duration = Duration::from_millis(233);

/// Commands from the tray / UI thread.
pub enum Command {
    Exit,
}

/// Cached per-(kind,pid) reader state. Failed attaches are retried after backoff
/// so a startup race (window up before the DLL is loaded) can recover.
enum Cached {
    Reader(Box<dyn MusicPlayer>),
    Failed { next_retry: Instant },
}

/// Run until `running` is false or an [`Command::Exit`] arrives.
pub fn run_loop(running: Arc<AtomicBool>, commands: Receiver<Command>) -> anyhow::Result<()> {
    diag!("MusicRpc poll loop starting");

    let mut netease_rpc = Rpc::new(NETEASE_APP_ID, "NetEase CloudMusic")?;
    let mut tencent_rpc = Rpc::new(TENCENT_APP_ID, "QQ Music")?;
    netease_rpc.ensure_connected();
    tencent_rpc.ensure_connected();

    let mut cached: Option<(PlayerKind, u32, Cached)> = None;
    let mut last_rpc: Option<PlayerKind> = None;
    let mut last_detect: Option<(PlayerKind, u32, String)> = None;
    // Log track changes only: (kind, identity, paused).
    let mut last_track_key: Option<(PlayerKind, String, bool)> = None;

    while running.load(Ordering::SeqCst) {
        // Exit is the only command today; drain one and shut down.
        if commands.try_recv().is_ok() {
            running.store(false, Ordering::SeqCst);
            clear_last(&mut last_rpc, &mut netease_rpc, &mut tencent_rpc);
            diag!("[loop] exit requested");
            return Ok(());
        }

        let target = detect();
        let detected_kind = target.as_ref().map(|(k, _, _)| *k);

        // Clear stale presence when the player disappears or we switch apps.
        if let Some(prev) = loop_logic::presence_to_clear(last_rpc, detected_kind) {
            diag!("[loop] clearing {prev:?} presence (detect={detected_kind:?})");
            rpc_for(prev, &mut netease_rpc, &mut tencent_rpc).clear();
            if last_rpc == Some(prev) {
                last_rpc = None;
            }
            last_track_key = None;
        }

        // Log detection changes only (kind / pid / title).
        if last_detect != target {
            match &target {
                Some((k, pid, title)) => {
                    diag!("[loop] detected {k:?} window pid={pid} title={title:?}")
                }
                None => diag!("[loop] no player window found (waiting)"),
            }
            last_detect = target.clone();
        }

        let Some((kind, pid, _title)) = target else {
            cached = None;
            sleep(POLL);
            continue;
        };

        let now = Instant::now();
        let rebuild = match &cached {
            Some((k, p, Cached::Reader(_))) if *k == kind && *p == pid => false,
            Some((k, p, Cached::Failed { next_retry })) if *k == kind && *p == pid => {
                now >= *next_retry
            }
            _ => true,
        };

        if rebuild {
            match build_reader(kind, pid) {
                Ok(r) => cached = Some((kind, pid, Cached::Reader(r))),
                Err(e) => {
                    diag!("[loop] failed to init {kind:?} (pid {pid}): {e:#}; retrying soon");
                    cached = Some((
                        kind,
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
            Some((_, _, Cached::Reader(r))) => r,
            _ => {
                sleep(POLL);
                continue;
            }
        };

        match reader.player_info() {
            None => {
                if let Some(k) = last_rpc.take() {
                    diag!("[loop] no track info; clearing {k:?} presence");
                    rpc_for(k, &mut netease_rpc, &mut tencent_rpc).clear();
                }
                last_track_key = None;
            }
            Some(info) => {
                let key = (kind, info.identity.clone(), info.paused);
                if last_track_key.as_ref() != Some(&key) {
                    diag!(
                        "[loop] track {:?} id={} paused={} title={:?}",
                        kind,
                        info.identity,
                        info.paused,
                        info.title
                    );
                    last_track_key = Some(key);
                }

                let rpc = rpc_for(kind, &mut netease_rpc, &mut tencent_rpc);
                if info.paused {
                    rpc.clear();
                } else {
                    rpc.update(&info);
                }
                last_rpc = Some(kind);
            }
        }

        sleep(POLL);
    }

    clear_last(&mut last_rpc, &mut netease_rpc, &mut tencent_rpc);
    Ok(())
}

fn clear_last(last_rpc: &mut Option<PlayerKind>, netease: &mut Rpc, tencent: &mut Rpc) {
    if let Some(k) = last_rpc.take() {
        rpc_for(k, netease, tencent).clear();
    }
}

fn detect() -> Option<(PlayerKind, u32, String)> {
    if let Some((title, pid)) = find_by_class(NETEASE_CLASS) {
        Some((PlayerKind::NetEase, pid, title))
    } else if let Some((title, pid)) = find_by_class(TENCENT_CLASS) {
        Some((PlayerKind::Tencent, pid, title))
    } else {
        None
    }
}

fn build_reader(kind: PlayerKind, pid: u32) -> anyhow::Result<Box<dyn MusicPlayer>> {
    match kind {
        PlayerKind::NetEase => Ok(Box::new(NetEase::new(pid)?)),
        PlayerKind::Tencent => Ok(Box::new(Tencent::new(pid)?)),
    }
}

fn rpc_for<'a>(kind: PlayerKind, netease: &'a mut Rpc, tencent: &'a mut Rpc) -> &'a mut Rpc {
    match kind {
        PlayerKind::NetEase => netease,
        PlayerKind::Tencent => tencent,
    }
}

/// Headless entry (no tray): run until the process is killed.
pub fn run() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::mem::forget(tx);
    run_loop(Arc::new(AtomicBool::new(true)), rx)
}
