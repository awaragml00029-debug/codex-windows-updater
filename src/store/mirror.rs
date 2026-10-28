//! Mirror fetcher: the official MSIX re-hosted byte-for-byte by
//! Wangnov/codex-app-mirror (MIT), for networks where the Store endpoints
//! are unreachable — e.g. mainland China, where the DisplayCatalog TLS
//! handshake gets cut ("tls handshake eof").
//!
//! Flow:
//! 1. SHA256SUMS -> `<sha256>  OpenAI.Codex_<ver>_x64__<id>.Msix`
//! 2. Download the MSIX from the first source that works, hashing while it
//!    streams; a hash mismatch counts as that source failing.
//!
//! Trust doesn't rest on the mirror: the hash pins the file the checksum list
//! names, and the installer still runs `signature::verify_msix` (OpenAI's
//! signature) before extracting anything.

use super::DownloadResult;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// The mirror's own CDN: routes mainland China to an S3 copy, everyone else
/// to Cloudflare R2. Only ever serves the latest build.
const CDN_LATEST: &str = "https://codexapp.agentsmirror.com/latest";
const GITHUB_LATEST: &str = "https://github.com/Wangnov/codex-app-mirror/releases/latest/download";
/// Public GitHub download proxies, tried after GitHub itself.
const GITHUB_PROXIES: &[&str] = &[
    "https://gh-proxy.com/",
    "https://gh.llkk.cc/",
    "https://ghfast.top/",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct LatestX64 {
    /// Lowercase hex.
    sha256: String,
    /// e.g. `OpenAI.Codex_26.1023.6123.0_x64__2p2nqsd0c76g0`
    moniker: String,
    /// e.g. `OpenAI.Codex_26.1023.6123.0_x64__2p2nqsd0c76g0.Msix`
    file_name: String,
}

pub(super) fn download_latest(
    dest_dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadResult> {
    let client = http_client()?;
    let latest = fetch_latest(&client)?;
    let version = moniker_version(&latest.moniker)?;
    std::fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join(format!("{}.msix", latest.moniker));

    let mut errors = Vec::new();
    for url in msix_urls(&latest.file_name) {
        match download_verified(&client, &url, &dest, &latest.sha256, progress) {
            Ok(()) => {
                return Ok(DownloadResult {
                    msix_path: dest,
                    moniker: latest.moniker,
                    version,
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                errors.push(format!("{url}: {e:#}"));
            }
        }
    }
    bail!("every mirror source failed\n    {}", errors.join("\n    "))
}

/// Latest MSIX version per the mirror's checksum list — no MSIX download.
pub(super) fn resolve_latest_version() -> Result<String> {
    let latest = fetch_latest(&http_client()?)?;
    moniker_version(&latest.moniker)
}

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("codex-windows-updater/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn checksum_urls() -> Vec<String> {
    // The CDN only has the combined list (macOS entries too); the parser
    // picks the x64 MSIX line out of either.
    let mut urls = vec![format!("{CDN_LATEST}/checksums")];
    urls.extend(github_urls("SHA256SUMS-windows.txt"));
    urls
}

fn msix_urls(file_name: &str) -> Vec<String> {
    let mut urls = vec![format!("{CDN_LATEST}/win-x64")];
    urls.extend(github_urls(file_name));
    urls
}

fn github_urls(file_name: &str) -> Vec<String> {
    let direct = format!("{GITHUB_LATEST}/{file_name}");
    let mut urls: Vec<String> = GITHUB_PROXIES.iter().map(|p| format!("{p}{direct}")).collect();
    urls.insert(0, direct);
    urls
}

fn fetch_latest(client: &Client) -> Result<LatestX64> {
    let mut errors = Vec::new();
    for url in checksum_urls() {
        let text = match client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
        {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{url}: {e}"));
                continue;
            }
        };
        match parse_x64(&text) {
            Some(latest) => return Ok(latest),
            None => errors.push(format!("{url}: no x64 MSIX line in checksum list")),
        }
    }
    bail!(
        "couldn't read the mirror's checksum list\n    {}",
        errors.join("\n    ")
    )
}

/// Find `<64 hex>  OpenAI.Codex_<ver>_x64__<id>.Msix` in a `sha256sum` list.
fn parse_x64(text: &str) -> Option<LatestX64> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        // sha256sum marks binary mode with a leading '*'
        let file_name = parts.next()?.trim_start_matches('*');
        if parts.next().is_some()
            || hash.len() != 64
            || !hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return None;
        }
        let dot = file_name.rfind('.')?;
        let (moniker, ext) = file_name.split_at(dot);
        if !ext.eq_ignore_ascii_case(".msix")
            || !moniker.starts_with("OpenAI.Codex_")
            || moniker.split('_').nth(2) != Some("x64")
        {
            return None;
        }
        Some(LatestX64 {
            sha256: hash.to_ascii_lowercase(),
            moniker: moniker.to_string(),
            file_name: file_name.to_string(),
        })
    })
}

fn moniker_version(moniker: &str) -> Result<String> {
    moniker
        .split('_')
        .nth(1)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("couldn't parse version from moniker: {moniker}"))
}

fn download_verified(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let mut resp = client.get(url).send()?.error_for_status()?;
    let total = resp.content_length();
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    progress(0, total);
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        written += n as u64;
        progress(written, total);
    }
    file.flush()?;
    let actual = hex(&hasher.finalize());
    if actual != expected_sha256 {
        bail!("SHA-256 mismatch: expected {expected_sha256}, got {actual}");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const X64: &str = "9890e869a0d40fc00f396ce48ba05dc45ccb8bb36d151dcd1a2ff23d270f2e4d";

    #[test]
    fn picks_x64_msix_out_of_the_combined_list() {
        let text = format!(
            "ed6d5585145732222c6756ee8b07e047b92747433b4826299afb9297871680c6  Codex-mac-arm64.dmg\n\
             {X64}  OpenAI.Codex_26.1023.6123.0_x64__2p2nqsd0c76g0.Msix\n\
             60ada29d9c36d1ba26c4a4901e8c3e8a931ad3c83fe639c7532697e13f67c062  OpenAI.Codex_26.1023.6123.0_arm64__2p2nqsd0c76g0.Msix\n"
        );
        let latest = parse_x64(&text).unwrap();
        assert_eq!(latest.sha256, X64);
        assert_eq!(latest.moniker, "OpenAI.Codex_26.1023.6123.0_x64__2p2nqsd0c76g0");
        assert_eq!(latest.file_name, "OpenAI.Codex_26.1023.6123.0_x64__2p2nqsd0c76g0.Msix");
        assert_eq!(moniker_version(&latest.moniker).unwrap(), "26.1023.6123.0");
    }

    #[test]
    fn accepts_binary_marker_and_uppercase_hash() {
        let text = format!(
            "{}  *OpenAI.Codex_1.2.3.0_x64__abc.msix",
            X64.to_ascii_uppercase()
        );
        let latest = parse_x64(&text).unwrap();
        assert_eq!(latest.sha256, X64);
        assert_eq!(latest.file_name, "OpenAI.Codex_1.2.3.0_x64__abc.msix");
    }

    #[test]
    fn rejects_lists_without_an_x64_msix() {
        assert_eq!(parse_x64(""), None);
        assert_eq!(
            parse_x64(&format!("{X64}  OpenAI.Codex_1.2.3.0_arm64__abc.Msix")),
            None
        );
        assert_eq!(parse_x64("deadbeef  OpenAI.Codex_1.2.3.0_x64__abc.Msix"), None);
        assert_eq!(parse_x64(&format!("{X64}  Codex-mac-x64.dmg")), None);
    }

    #[test]
    fn github_sources_go_direct_first_then_proxies() {
        let urls = msix_urls("a.Msix");
        assert_eq!(urls[0], "https://codexapp.agentsmirror.com/latest/win-x64");
        assert_eq!(
            urls[1],
            "https://github.com/Wangnov/codex-app-mirror/releases/latest/download/a.Msix"
        );
        assert_eq!(
            urls[2],
            "https://gh-proxy.com/https://github.com/Wangnov/codex-app-mirror/releases/latest/download/a.Msix"
        );
        assert_eq!(urls.len(), 2 + GITHUB_PROXIES.len());
    }
}
