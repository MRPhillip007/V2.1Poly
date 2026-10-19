//! The secrets file, kept apart from the config so that the config can be shared and logged. The
//! port of `pmbot/src/secrets.rs`; milestone 2 reads only the Telegram values.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use zeroize::{Zeroize, Zeroizing};

/// The keys milestone 3 reads; they may already be in the file and are not kept here.
const LATER_KEYS: [&str; 6] = ["signer_key", "deposit_wallet", "api_key", "api_secret", "api_passphrase", "relayer_key"];

/// What the bot must keep private. `Debug` shows `***` in place of every value, and the values are
/// wiped from memory when the struct is dropped. An empty value in the file is no value.
pub struct Secrets {
    /// Token of the Telegram bot.
    pub telegram_token: Option<String>,
    /// The one Telegram chat the bot writes to.
    pub telegram_chat_id: Option<String>,
}

impl Secrets {
    /// Reads the secrets file at `path`. On Unix the file must not be accessible to group or
    /// others (`chmod 600`).
    ///
    /// Errors name the file, keys and line numbers; they never quote its content, because they get
    /// logged.
    pub fn load(path: &Path) -> Result<Secrets> {
        let mut file = File::open(path).with_context(|| format!("cannot open secrets file {}", path.display()))?;
        // the mode is read from the open file, so it cannot change between the check and the read
        if let Some(mode) = permission_bits(&file).with_context(|| format!("cannot stat secrets file {}", path.display()))? {
            check_mode(path, mode)?;
        }
        let mut text = Zeroizing::new(String::new());
        file.read_to_string(&mut text).with_context(|| format!("cannot read secrets file {}", path.display()))?;
        text.parse::<Secrets>().with_context(|| format!("invalid secrets file {}", path.display()))
    }
}

impl FromStr for Secrets {
    type Err = anyhow::Error;

    /// Parses secrets text. This is not `derive(Deserialize)` on purpose: the errors of serde and
    /// toml quote the offending line or value, and here that line holds a secret.
    fn from_str(text: &str) -> Result<Secrets> {
        let mut table: toml::Table = text
            .parse()
            .map_err(|err: toml::de::Error| anyhow!("not valid TOML at line {}: {}", line_of(&err, text), err.message()))?;
        let mut take = |key: &str, numbers: bool| -> Result<Option<String>> {
            match table.remove(key) {
                Some(toml::Value::String(value)) if value.is_empty() => Ok(None),
                Some(toml::Value::String(value)) => Ok(Some(value)),
                Some(toml::Value::Integer(n)) if numbers => Ok(Some(n.to_string())),
                Some(_) => bail!("`{key}` must be a quoted string"),
                None => Ok(None),
            }
        };
        let secrets = Secrets { telegram_token: take("telegram_token", false)?, telegram_chat_id: take("telegram_chat_id", true)? };
        for key in LATER_KEYS {
            if let Some(toml::Value::String(mut value)) = table.remove(key) {
                value.zeroize();
            }
        }
        if let Some(key) = table.keys().next() {
            bail!("unknown key `{key}`");
        }
        Ok(secrets)
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        for value in [&mut self.telegram_token, &mut self.telegram_chat_id].into_iter().flatten() {
            value.zeroize();
        }
    }
}

/// Stands in for a value in `Debug` output.
struct Redacted<'a>(&'a Option<String>);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() { "***" } else { "none" })
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("telegram_token", &Redacted(&self.telegram_token))
            .field("telegram_chat_id", &Redacted(&self.telegram_chat_id))
            .finish()
    }
}

/// The 1-based line a parse error points at, so that a message can name the line without quoting
/// it.
fn line_of(err: &toml::de::Error, text: &str) -> usize {
    let at = err.span().map_or(0, |span| span.start.min(text.len()));
    1 + text.as_bytes()[..at].iter().filter(|&&byte| byte == b'\n').count()
}

/// The permission bits of an open file, on platforms that have them.
#[cfg(unix)]
fn permission_bits(file: &File) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Some(file.metadata()?.permissions().mode()))
}

#[cfg(not(unix))]
fn permission_bits(_file: &File) -> Result<Option<u32>> {
    Ok(None)
}

/// Group and others must have no access at all, as ssh demands of a private key.
fn check_mode(path: &Path, mode: u32) -> Result<()> {
    if mode & 0o077 != 0 {
        bail!(
            "secrets file {path} has mode {mode:04o}: group and others must have no access (run `chmod 600 {path}`)",
            path = path.display(),
            mode = mode & 0o777,
        );
    }
    Ok(())
}
