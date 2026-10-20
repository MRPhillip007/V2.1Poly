//! Starting the bot (spec §4.2): the hot thread, the cold runtime and the two queues between them. The
//! cold runtime runs on the calling thread; both end when `stop` is set.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rtrb::RingBuffer;

use crate::cold::run_cold;
use crate::hot::{pin, spawn_hot};
use crate::shared::clock::ExchangeClock;
use crate::shared::config::{Config, Mode};
use crate::shared::msgs::{ColdMsg, HotMsg};
use crate::shared::secrets::Secrets;

/// The queue from the hot thread: a full one drops messages, counted.
const HOT_QUEUE: usize = 65_536;
/// The queue to the hot thread.
const COLD_QUEUE: usize = 1_024;

/// Runs the bot until `stop` is set.
pub fn run(cfg: Config, secrets: Secrets, stop: Arc<AtomicBool>) -> Result<()> {
    if cfg.mode == Mode::Live {
        bail!("боевой режим будет в вехе 3, сейчас — только mode = \"demo\"");
    }
    let strategy = cfg.strategy()?;
    crate::shared::tls::init();
    let clock = Arc::new(ExchangeClock::new());
    let (to_cold, from_hot) = RingBuffer::<HotMsg>::new(HOT_QUEUE);
    let (to_hot, from_cold) = RingBuffer::<ColdMsg>::new(COLD_QUEUE);
    let hot = spawn_hot(&cfg, strategy.v21.clone(), to_cold, from_cold, clock.clone(), stop.clone());
    if cfg.cores.pin {
        pin(cfg.cores.cold);
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("the cold runtime")
        .and_then(|rt| rt.block_on(run_cold(cfg, strategy, secrets, from_hot, to_hot, clock, stop.clone())));
    // whatever ended the cold side ends the hot thread too
    stop.store(true, Ordering::Relaxed);
    let hot = hot.join();
    result?;
    if hot.is_err() {
        bail!("the hot thread panicked");
    }
    Ok(())
}
