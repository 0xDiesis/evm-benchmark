use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under the workspace root")
        .to_path_buf()
}

fn temp_report_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}.json"))
}

async fn mount_preflight_mismatch(server: &MockServer) {
    Mock::given(method("POST"))
        .and(body_string_contains("\"method\":\"eth_blockNumber\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})),
        )
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"method\":\"eth_chainId\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"result":"0x1"})),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn cli_main_exits_with_preflight_error_after_valid_parse() {
    let server = MockServer::start().await;
    mount_preflight_mismatch(&server).await;

    let output = Command::new(env!("CARGO_BIN_EXE_evm-benchmark"))
        .current_dir(workspace_root())
        .arg("--rpc-endpoints")
        .arg(server.uri())
        .arg("--ws")
        .arg("ws://localhost:8546")
        .arg("--senders")
        .arg("1")
        .arg("--txs")
        .arg("1")
        .arg("--workers")
        .arg("1")
        .arg("--batch-size")
        .arg("1")
        .arg("--out")
        .arg(temp_report_path("cli-main-preflight"))
        .output()
        .expect("failed to run evm-benchmark binary");

    assert!(
        !output.status.success(),
        "expected preflight mismatch to fail, stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("configured chain_id=19803 but RPC reports chain_id=1"),
        "stderr did not contain preflight mismatch: {stderr}"
    );
}
