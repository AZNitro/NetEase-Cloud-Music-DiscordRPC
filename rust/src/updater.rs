//! The poll loop. Port of `Program.UpdateThread`, with the fixes noted in
//! `RUST_PORT_PLAN.md` (correct PID in the QQ Music branch; owned handles).

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

pub fn run() -> anyhow::Result<()> {
    crate::logging::init();
    diag!("MusicRpc starting (console diagnostic build)");

    let mut netease_rpc = Rpc::new(NETEASE_APP_ID)?;
    let mut tencent_rpc = Rpc::new(TENCENT_APP_ID)?;
    netease_rpc.ensure_connected();
    tencent_rpc.ensure_connected();

    // Cached reader: (kind, pid, reader). Reused across ticks while the PID holds.
    let mut cached: Option<(Kind, u32, Box<dyn MusicPlayer>)> = None;
    // Which client currently shows a presence, so we can clear the right one.
    let mut last_rpc: Option<Kind> = None;
    // Last detection result, logged only on change to avoid per-tick spam.
    let mut last_seen: Option<Option<Kind>> = None;

    loop {
        let target = if let Some((_title, pid)) = find_by_class(NETEASE_CLASS) {
            Some((Kind::NetEase, pid))
        } else if let Some((_title, pid)) = find_by_class(TENCENT_CLASS) {
            Some((Kind::Tencent, pid))
        } else {
            None
        };

        let seen = target.map(|(k, _)| k);
        if last_seen != Some(seen) {
            match seen {
                Some(k) => diag!("[loop] detected {k:?} player window"),
                None => diag!("[loop] no player window found (waiting)"),
            }
            last_seen = Some(seen);
        }

        let Some((kind, pid)) = target else {
            // No player window: drop the cached reader and leave presence as-is
            // (matches the original behaviour).
            cached = None;
            sleep(POLL);
            continue;
        };

        let reuse = matches!(&cached, Some((k, _, p)) if *k == kind && p.validate(pid));
        if !reuse {
            diag!("[loop] creating {kind:?} reader for pid {pid}");
            let created: anyhow::Result<Box<dyn MusicPlayer>> = match kind {
                Kind::NetEase => NetEase::new(pid).map(|p| Box::new(p) as Box<dyn MusicPlayer>),
                Kind::Tencent => Tencent::new(pid).map(|p| Box::new(p) as Box<dyn MusicPlayer>),
            };
            match created {
                Ok(p) => cached = Some((kind, pid, p)),
                Err(e) => {
                    diag!("[loop] failed to init {kind:?}: {e:#}");
                    cached = None;
                    sleep(POLL);
                    continue;
                }
            }
        }

        let info = cached.as_ref().and_then(|(_, _, p)| p.player_info());

        match info {
            None => {
                if let Some(k) = last_rpc.take() {
                    diag!("[loop] no track info; clearing {k:?} presence");
                    rpc_for(k, &mut netease_rpc, &mut tencent_rpc).clear();
                }
            }
            Some(info) => {
                let rpc = rpc_for(kind, &mut netease_rpc, &mut tencent_rpc);
                if info.paused {
                    diag!("[loop] paused; clearing {kind:?} presence");
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

fn rpc_for<'a>(kind: Kind, netease: &'a mut Rpc, tencent: &'a mut Rpc) -> &'a mut Rpc {
    match kind {
        Kind::NetEase => netease,
        Kind::Tencent => tencent,
    }
}
