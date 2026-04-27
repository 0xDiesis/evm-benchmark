//! Download and manage bench-targets from the GitHub repository.
//!
//! When the binary is distributed as a standalone release, users need the
//! bench-targets directory (Docker compose files, chain configs, shell scripts)
//! to run benchmarks against specific chains. This module downloads the latest
//! bench-targets from GitHub and extracts them to the local filesystem.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::sync::{LazyLock, Mutex};

/// GitHub repository for downloading bench-targets.
const GITHUB_REPO: &str = "0xDiesis/evm-benchmark";

/// Default branch to download from.
const DEFAULT_BRANCH: &str = "main";

/// Subdirectory within the repo archive to extract.
const TARGETS_PREFIX: &str = "bench-targets";

#[cfg(test)]
static TEST_ARCHIVE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[cfg(test)]
static TEST_ARCHIVE_BASE_URL: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Return the default bench-targets directory (next to the binary or in cwd).
pub fn default_targets_dir() -> PathBuf {
    default_targets_dir_from(std::env::current_exe())
}

fn default_targets_dir_from(current_exe: std::io::Result<PathBuf>) -> PathBuf {
    // Prefer a bench-targets/ directory next to the binary.
    if let Ok(exe) = current_exe {
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
    let archive_url = archive_url_for_branch(branch);

    download_targets_from_url(dest_dir, &archive_url).await
}

fn archive_url_for_branch(branch: &str) -> String {
    #[cfg(test)]
    if let Some(base_url) = TEST_ARCHIVE_BASE_URL.lock().unwrap().clone() {
        return format!("{}/{}.tar.gz", base_url.trim_end_matches('/'), branch);
    }

    format!(
        "https://github.com/{}/archive/refs/heads/{}.tar.gz",
        GITHUB_REPO, branch
    )
}

async fn download_targets_from_url(dest_dir: &Path, archive_url: &str) -> Result<()> {
    eprintln!("Downloading bench-targets from {archive_url}...");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("failed to create HTTP client")?;

    let response = client
        .get(archive_url)
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
            std::fs::create_dir_all(out_path.parent().unwrap())?;
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
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tar::{Builder, Header};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "evm-benchmark-{prefix}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn append_dir(builder: &mut Builder<GzEncoder<Vec<u8>>>, path: &str, mode: u32) {
        let mut header = Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(mode);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, path, std::io::empty())
            .unwrap();
    }

    fn append_file(builder: &mut Builder<GzEncoder<Vec<u8>>>, path: &str, mode: u32, body: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_mode(mode);
        header.set_size(body.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, path, body).unwrap();
    }

    fn build_archive() -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        append_dir(&mut builder, "evm-benchmark-main/bench-targets", 0o755);
        append_dir(
            &mut builder,
            "evm-benchmark-main/bench-targets/scripts",
            0o755,
        );
        append_dir(
            &mut builder,
            "evm-benchmark-main/bench-targets/chains",
            0o755,
        );
        append_file(
            &mut builder,
            "evm-benchmark-main/bench-targets/scripts/run.sh",
            0o755,
            b"#!/bin/sh\necho ok\n",
        );
        append_file(
            &mut builder,
            "evm-benchmark-main/bench-targets/chains/devnet.yml",
            0o644,
            b"name: devnet\n",
        );
        append_file(
            &mut builder,
            "evm-benchmark-main/bench-targets/root.txt",
            0o644,
            b"root\n",
        );
        append_file(
            &mut builder,
            "evm-benchmark-main/README.md",
            0o644,
            b"ignored\n",
        );
        builder.finish().unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn build_archive_without_directory_entries() -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        append_file(
            &mut builder,
            "evm-benchmark-main/bench-targets/scripts/run.sh",
            0o755,
            b"#!/bin/sh\necho ok\n",
        );
        builder.finish().unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn test_default_targets_dir_returns_path() {
        let dir = default_targets_dir();
        let display = dir.display().to_string();
        // Should return a path ending in "bench-targets"
        assert!(
            dir.ends_with(TARGETS_PREFIX),
            "expected path ending in bench-targets, got: {}",
            display
        );
    }

    #[test]
    fn test_default_targets_dir_prefers_existing_directory_next_to_binary() {
        let dir = unique_temp_dir("default-targets-existing");
        let exe_path = dir.join("bin").join("evm-benchmark");
        let targets_dir = dir.join("bin").join(TARGETS_PREFIX);
        std::fs::create_dir_all(&targets_dir).unwrap();

        let resolved = default_targets_dir_from(Ok(exe_path));

        assert_eq!(resolved, targets_dir);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_default_targets_dir_falls_back_when_adjacent_directory_missing() {
        let dir = unique_temp_dir("default-targets-missing");
        let exe_path = dir.join("bin").join("evm-benchmark");

        let resolved = default_targets_dir_from(Ok(exe_path));

        assert_eq!(resolved, PathBuf::from(TARGETS_PREFIX));
    }

    #[test]
    fn test_default_targets_dir_falls_back_when_current_exe_fails() {
        let resolved = default_targets_dir_from(Err(std::io::Error::other("boom")));

        assert_eq!(resolved, PathBuf::from(TARGETS_PREFIX));
    }

    #[test]
    fn test_archive_url_for_branch_falls_back_to_github_when_override_missing() {
        let _guard = TEST_ARCHIVE_LOCK.lock().unwrap();
        *TEST_ARCHIVE_BASE_URL.lock().unwrap() = None;

        let url = archive_url_for_branch("feature-branch");

        assert_eq!(
            url,
            "https://github.com/0xDiesis/evm-benchmark/archive/refs/heads/feature-branch.tar.gz"
        );
    }

    #[test]
    fn test_targets_exist_false_for_missing() {
        assert!(!targets_exist(Path::new("/nonexistent/bench-targets")));
    }

    #[test]
    fn test_targets_exist_true_when_scripts_and_chains_present() {
        let dir = unique_temp_dir("targets-exist");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::create_dir_all(dir.join("chains")).unwrap();

        assert!(targets_exist(&dir));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_extract_targets_replaces_existing_content_and_keeps_targets_only() {
        let dir = unique_temp_dir("extract-success");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale.txt"), b"old").unwrap();

        extract_targets(&build_archive(), &dir).unwrap();

        assert!(dir.join("scripts/run.sh").is_file());
        assert!(dir.join("chains/devnet.yml").is_file());
        assert!(dir.join("root.txt").is_file());
        assert!(!dir.join("README.md").exists());
        assert!(!dir.join("stale.txt").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(dir.join("scripts/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_extract_targets_creates_missing_parent_directories_for_files() {
        let dir = unique_temp_dir("extract-parent-create");

        extract_targets(&build_archive_without_directory_entries(), &dir).unwrap();

        assert!(dir.join("scripts/run.sh").is_file());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_extract_targets_rejects_invalid_archive_bytes() {
        let dir = unique_temp_dir("extract-invalid");
        let err = extract_targets(b"not-a-tarball", &dir).unwrap_err();
        let message = err.to_string();
        let read_tar = message.contains("failed to read tar");
        let invalid_gzip = message.contains("invalid gzip header");

        assert!(read_tar || invalid_gzip, "unexpected error: {err:#}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_extract_targets_errors_when_destination_is_a_file() {
        let dir = unique_temp_dir("extract-file-dest");
        std::fs::write(&dir, b"not-a-directory").unwrap();

        let err = extract_targets(&build_archive(), &dir).unwrap_err();

        assert!(
            err.to_string()
                .contains("failed to remove existing bench-targets directory"),
            "unexpected error: {err:#}"
        );
        let _ = std::fs::remove_file(dir);
    }

    #[tokio::test]
    async fn test_download_targets_from_url_extracts_bench_targets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/archive.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(build_archive()))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("download-success");
        download_targets_from_url(&dir, &format!("{}/archive.tar.gz", server.uri()))
            .await
            .unwrap();

        assert!(dir.join("scripts/run.sh").is_file());
        assert!(dir.join("chains/devnet.yml").is_file());
        assert!(targets_exist(&dir));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn test_download_targets_uses_default_branch_and_public_wrapper() {
        let _guard = TEST_ARCHIVE_LOCK.lock().unwrap();
        let server = MockServer::start().await;
        *TEST_ARCHIVE_BASE_URL.lock().unwrap() = Some(server.uri());
        Mock::given(method("GET"))
            .and(path("/main.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(build_archive()))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("download-default-branch");
        download_targets(&dir, None).await.unwrap();

        assert!(dir.join("scripts/run.sh").is_file());

        *TEST_ARCHIVE_BASE_URL.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn test_download_targets_uses_explicit_branch() {
        let _guard = TEST_ARCHIVE_LOCK.lock().unwrap();
        let server = MockServer::start().await;
        *TEST_ARCHIVE_BASE_URL.lock().unwrap() = Some(server.uri());
        Mock::given(method("GET"))
            .and(path("/feature.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(build_archive()))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("download-explicit-branch");
        download_targets(&dir, Some("feature")).await.unwrap();

        assert!(dir.join("chains/devnet.yml").is_file());

        *TEST_ARCHIVE_BASE_URL.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn test_download_targets_from_url_surfaces_http_status_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing.tar.gz"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("download-404");
        let err = download_targets_from_url(&dir, &format!("{}/missing.tar.gz", server.uri()))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("GitHub returned 404"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn test_download_targets_from_url_surfaces_extract_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/broken.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes("broken"))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("download-invalid");
        let err = download_targets_from_url(&dir, &format!("{}/broken.tar.gz", server.uri()))
            .await
            .unwrap_err();
        let message = err.to_string();
        let read_tar = message.contains("failed to read tar");
        let eof = message.contains("unexpected end of file");

        assert!(read_tar || eof, "unexpected error: {err:#}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
