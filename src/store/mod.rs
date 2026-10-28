//! Microsoft Store MSIX fetcher.
//!
//! Three strategies:
//! - [`Fetcher::Mirror`] — the official MSIX re-hosted by codex-app-mirror
//! - [`Fetcher::Direct`] — DisplayCatalog + FE3 SOAP, no local tool deps
//! - [`Fetcher::Winget`] — shells out to `winget.exe`
//!
//! Prefer Mirror: it is the only one that works where the Store endpoints
//! are blocked (mainland China). The other two remain as fallbacks.

mod direct;
pub mod local_file;
mod mirror;
mod winget;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PRODUCT_ID_CODEX: &str = "9PLM9XGG6VKS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fetcher {
    #[default]
    Mirror,
    Direct,
    Winget,
    /// Manual escape hatch — user provides a path to a pre-downloaded MSIX.
    /// NOT part of the auto-fallback chain; callers must invoke
    /// [`local_file::from_file`] directly with the path.
    LocalFile,
}

impl Fetcher {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mirror" => Some(Fetcher::Mirror),
            "direct" => Some(Fetcher::Direct),
            "winget" => Some(Fetcher::Winget),
            "local" | "local_file" | "localfile" | "file" => Some(Fetcher::LocalFile),
            _ => None,
        }
    }

    /// Network fetchers to try, `self` first, for auto-fallback. LocalFile
    /// isn't part of this chain — it requires explicit user intent + a file
    /// path.
    pub fn fallback_chain(self) -> Vec<Self> {
        const NETWORK: [Fetcher; 3] = [Fetcher::Mirror, Fetcher::Direct, Fetcher::Winget];
        let mut chain = vec![self];
        chain.extend(NETWORK.into_iter().filter(|f| *f != self));
        chain.retain(|f| *f != Fetcher::LocalFile);
        chain
    }
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// Absolute path to the downloaded `.msix` / `.msixbundle` on disk.
    pub msix_path: PathBuf,
    /// e.g. `OpenAI.Codex_26.422.2437.0_x64__2p2nqsd0c76g0`
    pub moniker: String,
    /// MSIX package version, e.g. `26.422.2437.0`.
    pub version: String,
}

/// Download the latest MSIX via the specified fetcher. Progress callback
/// fires periodically as bytes arrive; `total_opt` may be `None` when the
/// fetcher can't determine the full size (e.g. winget).
pub fn download_latest(
    fetcher: Fetcher,
    product_id: &str,
    dest_dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadResult> {
    match fetcher {
        Fetcher::Mirror => mirror::download_latest(dest_dir, progress),
        Fetcher::Direct => direct::download_latest(product_id, dest_dir, progress),
        Fetcher::Winget => winget::download_latest(product_id, dest_dir, progress),
        Fetcher::LocalFile => Err(anyhow::anyhow!(
            "LocalFile fetcher requires an explicit file path — call \
             store::local_file::from_file directly, not download_latest()"
        )),
    }
}

/// Try `preferred`, and if it fails the other strategies in turn. Returns which
/// fetcher actually succeeded. Callers *may* persist that choice to
/// `updater.json`, but beware: a single transient failure (offline, proxy
/// blip) followed by the network recovering on the fallback attempt would
/// permanently demote the user's chosen fetcher. The installer currently
/// does not persist for that reason — see `installer.rs`. A stronger
/// signal (N consecutive failures) would be needed before flipping.
pub fn download_latest_with_fallback(
    preferred: Fetcher,
    product_id: &str,
    dest_dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(DownloadResult, Fetcher)> {
    let mut errors = Vec::new();
    for fetcher in preferred.fallback_chain() {
        match download_latest(fetcher, product_id, dest_dir, progress) {
            Ok(r) => return Ok((r, fetcher)),
            Err(e) => errors.push(format!("  {fetcher:?}: {e:#}")),
        }
    }
    Err(anyhow::anyhow!(
        "all fetchers failed\n{}",
        errors.join("\n")
    ))
}

/// Resolve the latest available version string without downloading the MSIX.
/// Used by the update checker. Mirror and Direct only — winget has no
/// cheap version-query path that doesn't also trigger a download under
/// the current Entra-auth gate. Direct-configured installs still fall back
/// to the mirror, so they keep getting updates where the Store is blocked.
pub fn resolve_latest_version(fetcher: Fetcher, product_id: &str) -> Result<String> {
    let (first, second) = if fetcher == Fetcher::Direct {
        (Fetcher::Direct, Fetcher::Mirror)
    } else {
        (Fetcher::Mirror, Fetcher::Direct)
    };
    let resolve = |f: Fetcher| match f {
        Fetcher::Direct => direct::resolve_latest_version(product_id),
        _ => mirror::resolve_latest_version(),
    };
    resolve(first).or_else(|first_err| {
        resolve(second).map_err(|second_err| {
            anyhow::anyhow!(
                "{first:?}: {first_err:#}\n  {second:?}: {second_err:#}"
            )
        })
    })
}

/// Debug helper: dump the raw (html-decoded) SyncUpdates SOAP response.
/// Direct-only — winget has no equivalent.
pub fn debug_dump_sync_xml(product_id: &str) -> Result<String> {
    direct::debug_dump_sync_xml(product_id)
}
