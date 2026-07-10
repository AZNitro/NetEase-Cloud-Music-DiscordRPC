//! The poll loop. Port of `Program.UpdateThread`, with the fixes noted in
//! `RUST_PORT_PLAN.md` plus: it remembers failed readers so a broken player
//! doesn't re-init (and re-log) every tick, and it logs the detected window
//! title so we can see what metadata the window itself exposes.

use std::thread::sleep;
use std::time::Duration;

use crate::diag;
use crate::platform::window::find_by_class;
use crate::players::{netease::NetEase, tencent::Tencent, MusicPlayer};
use crate::rpc::Rpc;

const NETEASE_APP_ID: &str = "481562643958595594";
const TENCENT_APP_ID: &str = "903485504899665990";

const NETEASE_CLASS: &str = "OrpheusBrowserHost";
const TENCENT_CLASS: &str = "QQMusic_Daemon_Wnd";

const POLL: Duration = Duration::from_millis(233);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    NetEase,
    Tencent,
}

/// Cached per-(kind,pid) reader state. `Failed` is remembered so we don't retry
/// (and spam the log) every tick for a player we already couldn't attach to.
enum Cached {
    Reader(Box<dyn MusicPlayer>),
    Failed,
}

pub fn run() -> anyhow::Result<()> {
    crate::logging::init();
    diag!("MusicRpc starting (console diagnostic build)");

    let mut netease_rpc = Rpc::new(NETEASE_APP_ID)?;
    let mut tencent_rpc = Rpc::new(TENCENT_APP_ID)?;
    netease_rpc.ensure_connected();
    tencent_rpc.ensure_connected();

    let mut cached: Option<(Kind, u32, Cached)> = None;
    let mut last_rpc: Option<Kind> = None;
    let mut last_detect: Option<(Kind, u32, String)> = None;

    loop {
        let target = detect();

        // Log detection changes only (kind / pid / title), so the log stays readable.
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

        // (Re)build the reader only when the (kind, pid) is new.
        let fresh = !matches!(&cached, Some((k, p, _)) if *k == kind && *p == pid);
        if fresh {
            match build_reader(kind, pid) {
                Ok(r) => cached = Some((kind, pid, Cached::Reader(r))),
                Err(e) => {
                    diag!("[loop] failed to init {kind:?} (pid {pid}): {e:#}");
                    cached = Some((kind, pid, Cached::Failed));
                }
            }
        }

        let reader = match &cached {
            Some((_, _, Cached::Reader(r))) => r,
            // Failed (or somehow empty): stay quiet until the window/pid changes.
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
            }
            Some(info) => {
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
}

fn detect() -> Option<(Kind, u32, String)> {
    if let Some((title, pid)) = find_by_class(NETEASE_CLASS) {
        Some((Kind::NetEase, pid, title))
    } else if let Some((title, pid)) = find_by_class(TENCENT_CLASS) {
        Some((Kind::Tencent, pid, title))
    } else {
        None
    }
}

fn build_reader(kind: Kind, pid: u32) -> anyhow::Result<Box<dyn MusicPlayer>> {
    match kind {
        Kind::NetEase => Ok(Box::new(NetEase::new(pid)?)),
        Kind::Tencent => Ok(Box::new(Tencent::new(pid)?)),
    }
}

fn rpc_for<'a>(kind: Kind, netease: &'a mut Rpc, tencent: &'a mut Rpc) -> &'a mut Rpc {
    match kind {
        Kind::NetEase => netease,
        Kind::Tencent => tencent,
    }
}
