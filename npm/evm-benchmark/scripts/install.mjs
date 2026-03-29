#!/usr/bin/env node

/**
 * Downloads the correct pre-built evm-benchmark (evm-benchmark) binary for
 * the current platform from GitHub Releases and places it in bin/.
 */

import { readFileSync, chmodSync, mkdirSync, existsSync } from "fs";
import { execSync } from "child_process";
import { join, dirname } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = JSON.parse(
  readFileSync(join(__dirname, "..", "package.json"), "utf8")
);
const VERSION = pkg.version;
const REPO = "0xDiesis/evm-benchmark";
const BINARY_NAME = "evm-benchmark";

const PLATFORM_MAP = {
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
};

const key = `${process.platform}-${process.arch}`;
const target = PLATFORM_MAP[key];

if (!target) {
  console.error(`evm-benchmark: unsupported platform ${key}`);
  console.error(
    "Build from source: git clone https://github.com/0xDiesis/evm-benchmark && cd evm-benchmark && make build"
  );
  process.exit(1);
}

const binDir = join(__dirname, "..", "bin");
const binPath = join(binDir, "evm-benchmark");

// Skip if already downloaded
if (existsSync(binPath)) {
  process.exit(0);
}

mkdirSync(binDir, { recursive: true });

const archiveUrl = `https://github.com/${REPO}/releases/download/v${VERSION}/${BINARY_NAME}-v${VERSION}-${target}.tar.gz`;

console.log(`evm-benchmark: downloading ${target} binary...`);

try {
  // Download and extract, then rename the binary
  const tmpDir = join(binDir, "_tmp");
  mkdirSync(tmpDir, { recursive: true });
  execSync(`curl -fsSL "${archiveUrl}" | tar -xz -C "${tmpDir}"`, {
    stdio: "inherit",
  });

  // The tarball contains evm-benchmark in a subdirectory
  execSync(`find "${tmpDir}" -name "${BINARY_NAME}" -exec mv {} "${binPath}" \\;`, {
    stdio: "inherit",
  });
  execSync(`rm -rf "${tmpDir}"`);
  chmodSync(binPath, 0o755);

  console.log("evm-benchmark: installed successfully");
  console.log(
    "Run `evm-benchmark --setup` to download chain targets (Docker configs, scripts)"
  );
} catch {
  console.error(`evm-benchmark: failed to download binary from ${archiveUrl}`);
  console.error(
    "Build from source: git clone https://github.com/0xDiesis/evm-benchmark && cd evm-benchmark && make build"
  );
  process.exit(1);
}
