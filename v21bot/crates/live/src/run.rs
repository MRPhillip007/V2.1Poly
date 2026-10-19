//! Starting the bot: the hot thread and the cold runtime (milestone 2, task 16).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Result};

use crate::shared::config::Config;
use crate::shared::secrets::Secrets;

/// Runs the bot until `stop` is set.
pub fn run(cfg: Config, secrets: Secrets, stop: Arc<AtomicBool>) -> Result<()> {
    let _ = (cfg, secrets, stop);
    bail!("the demo mode is not built yet (milestone 2, task 16)")
}
