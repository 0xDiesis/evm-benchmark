//! Download and manage bench-targets from the GitHub repository.
//!
//! When the binary is distributed as a standalone release, users need the
//! bench-targets directory (Docker compose files, chain configs, shell scripts)
//! to run benchmarks against specific chains. This module downloads the latest
//! bench-targets from GitHub and extracts them to the local filesystem.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// GitHub repository for downloading bench-targets.
const GITHUB_REPO: &str = "0xDiesis/evm-benchmark";

/// Default branch to download from.
const DEFAULT_BRANCH: &str = "main";

/// Subdirectory within the repo archive to extract.
const TARGETS_PREFIX: &str = "bench-targets";

/// Return the default bench-targets directory (next to the binary or in cwd).
pub fn default_targets_dir() -> PathBuf {
    // Prefer a bench-targets/ directory next to the binary.
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .map(|p| p.join(TARGETS_PREFIX))
            .filter(|c| c.exists());
        if let Some(path) = candidate {
            return path;
        }
    }
    // Fall back to cwd/bench-targets/.
    PathBuf::from(TARGETS_PREFIX)
}

/// Check whether bench-targets are present at the given path.
pub fn targets_exist(dir: &Path) -> bool {
    dir.join("scripts").is_dir() && dir.join("chains").is_dir()
}

/// Download and extract the latest bench-targets from GitHub.
///
/// Downloads a tarball of the repository, extracts only the `bench-targets/`
/// subtree, and writes it to `dest_dir`.
pub async fn download_targets(dest_dir: &Path, branch: Option<&str>) -> Result<()> {
    let branch = branch.unwrap_or(DEFAULT_BRANCH);
    let archive_url = format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        GITHUB_REPO, branch
    );

    eprintln!("Downloading bench-targets from {GITHUB_REPO}@{branch}...");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("failed to create HTTP client")?;

    let response = client
        .get(&archive_url)
        .send()
        .await
        .context("failed to download archive")?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub returned {} for {}", response.status(), archive_url);
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read archive body")?;

    eprintln!("Extracting bench-targets to {}...", dest_dir.display());

    // Extract in a blocking task since flate2/tar are synchronous.
    let dest = dest_dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_targets(&bytes, &dest))
        .await
        .context("extract task panicked")??;

    eprintln!("Done. Bench-targets installed at {}", dest_dir.display());
    Ok(())
}

/// Extract `bench-targets/` from a gzipped tar archive.
fn extract_targets(archive_bytes: &[u8], dest_dir: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(decoder);

    // The GitHub tarball has a top-level directory like `evm-benchmark-main/`.
    // We need to strip that prefix and only extract `bench-targets/`.
    if dest_dir.exists() {
        std::fs::remove_dir_all(dest_dir)
            .context("failed to remove existing bench-targets directory")?;
    }
    std::fs::create_dir_all(dest_dir).context("failed to create destination directory")?;

    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        let path = entry.path().context("failed to read entry path")?;
        let path = path.to_path_buf();

        // Find the bench-targets/ component in the path.
        // GitHub archives look like: evm-benchmark-main/bench-targets/scripts/...
        let components: Vec<_> = path.components().collect();
        let bt_idx = components.iter().position(
            |c| matches!(c, std::path::Component::Normal(s) if s.to_str() == Some(TARGETS_PREFIX)),
        );

        let Some(idx) = bt_idx else { continue };

        // Build relative path from bench-targets/ onward.
        let rel: PathBuf = components[idx + 1..].iter().collect();
        if rel.as_os_str().is_empty() {
            continue;
        }

        let out_path = dest_dir.join(&rel);

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out_path)
                .with_context(|| format!("failed to create dir {}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry
                .unpack(&out_path)
                .with_context(|| format!("failed to unpack {}", out_path.display()))?;

            // Preserve executable permissions for shell scripts.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(mode) = entry.header().mode()
                    && mode & 0o111 != 0
                {
                    std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_targets_dir_returns_path() {
        let dir = default_targets_dir();
        // Should return a path ending in "bench-targets"
        assert!(
            dir.ends_with(TARGETS_PREFIX),
            "expected path ending in bench-targets, got: {}",
            dir.display()
        );
    }

    #[test]
    fn test_targets_exist_false_for_missing() {
        assert!(!targets_exist(Path::new("/nonexistent/bench-targets")));
    }
}
