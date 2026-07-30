use std::path::Path;

use routelane::domain::{HealthState, Profile};
use routelane::health::HealthReport;
use routelane::ipc::{
    read_frame, write_frame, DaemonEnvelope, DaemonRequest, DaemonResponse, DaemonResult,
    DaemonStatus, IpcError,
};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::process::Command;

async fn run_cli(runtime: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_routelanectl"))
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .await
        .unwrap()
}

async fn serve_once(
    runtime: &Path,
    expected: impl FnOnce(&DaemonRequest) + Send + 'static,
    response: Result<DaemonResult, IpcError>,
) {
    let socket_dir = runtime.join("routelane");
    std::fs::create_dir(&socket_dir).unwrap();
    let listener = UnixListener::bind(socket_dir.join("routelane.sock")).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
        expected(&request.request);
        let response = match response {
            Ok(result) => DaemonResponse::success(request.request_id, result),
            Err(error) => DaemonResponse::failure(request.request_id, error),
        };
        write_frame(&mut stream, &response).await.unwrap();
    });
}

fn inactive_status() -> DaemonStatus {
    DaemonStatus {
        generation: 7,
        health: HealthReport::inactive(),
        desired_profile_id: None,
        applied_profile_id: None,
        last_error: None,
    }
}

fn applied_status() -> DaemonStatus {
    DaemonStatus {
        generation: 7,
        health: HealthReport::from_parts(HealthState::Healthy, Default::default(), vec![]),
        desired_profile_id: Some("work".into()),
        applied_profile_id: Some("work".into()),
        last_error: None,
    }
}

fn profile_json() -> &'static str {
    r#"{
      "id":"work",
      "name":"Work",
      "enabled":true,
      "lanes":[{
        "id":"vpn",
        "name":"VPN",
        "interface":"wg0",
        "fallback_lane":null,
        "failure_policy":"Inactive"
      }],
      "rules":[]
    }"#
}

#[tokio::test]
async fn status_json_is_machine_readable_and_keeps_stderr_empty() {
    let runtime = TempDir::new().unwrap();
    serve_once(
        runtime.path(),
        |request| assert_eq!(request, &DaemonRequest::GetStatus),
        Ok(DaemonResult::Status(inactive_status())),
    )
    .await;

    let output = run_cli(runtime.path(), &["status", "--json"]).await;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["generation"], 7);
    assert_eq!(value["health"]["state"], "Inactive");
}

#[tokio::test]
async fn status_text_is_bounded_and_human_readable() {
    let runtime = TempDir::new().unwrap();
    serve_once(
        runtime.path(),
        |_| {},
        Ok(DaemonResult::Status(applied_status())),
    )
    .await;

    let output = run_cli(runtime.path(), &["status"]).await;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "state=Healthy generation=7 applied_profile=work\n"
    );
}

#[tokio::test]
async fn apply_profile_reads_json_and_uses_daemon_protocol() {
    let runtime = TempDir::new().unwrap();
    let config = runtime.path().join("profile.json");
    std::fs::write(&config, profile_json()).unwrap();
    serve_once(
        runtime.path(),
        |request| match request {
            DaemonRequest::ApplyProfile(profile) => {
                assert_eq!(profile.id, "work".into());
                assert_eq!(profile.lanes[0].interface.as_str(), "wg0");
            }
            other => panic!("unexpected request: {other:?}"),
        },
        Ok(DaemonResult::Applied(routelane::ipc::ApplyProfileResult {
            generation: 8,
            plan_hash: serde_json::from_str(&format!("[{}]", ["1"; 32].join(","))).unwrap(),
        })),
    )
    .await;

    let output = run_cli(runtime.path(), &["apply-profile", config.to_str().unwrap()]).await;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "applied generation=8\n"
    );
}

#[tokio::test]
async fn deactivate_uses_daemon_protocol() {
    let runtime = TempDir::new().unwrap();
    serve_once(
        runtime.path(),
        |request| assert_eq!(request, &DaemonRequest::Deactivate),
        Ok(DaemonResult::Deactivated),
    )
    .await;

    let output = run_cli(runtime.path(), &["deactivate"]).await;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "deactivated\n");
}

#[tokio::test]
async fn usage_config_and_daemon_failures_have_stable_exit_codes() {
    let runtime = TempDir::new().unwrap();
    let usage = run_cli(runtime.path(), &["unknown"]).await;
    assert_eq!(usage.status.code(), Some(2));
    assert!(usage.stdout.is_empty());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("Usage:"));

    let config = runtime.path().join("broken.json");
    std::fs::write(&config, "{broken").unwrap();
    let invalid = run_cli(runtime.path(), &["apply-profile", config.to_str().unwrap()]).await;
    assert_eq!(invalid.status.code(), Some(3));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("invalid profile JSON"));

    let disconnected = run_cli(runtime.path(), &["status"]).await;
    assert_eq!(disconnected.status.code(), Some(4));
    assert!(disconnected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&disconnected.stderr).contains("IPC_CONNECT_FAILED"));
    assert!(disconnected.stderr.len() < 1024);
}

#[tokio::test]
async fn an_unresponsive_daemon_times_out_with_a_bounded_error() {
    let runtime = TempDir::new().unwrap();
    let socket_dir = runtime.path().join("routelane");
    std::fs::create_dir(&socket_dir).unwrap();
    let listener = UnixListener::bind(socket_dir.join("routelane.sock")).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: DaemonEnvelope = read_frame(&mut stream).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    });

    let output = run_cli(runtime.path(), &["status"]).await;
    server.abort();

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("IPC_TIMEOUT"));
    assert!(output.stderr.len() < 1024);
}

#[test]
fn fixtures_encode_the_public_health_and_profile_shapes() {
    assert_eq!(inactive_status().health.state, HealthState::Inactive);
    let profile: Profile = serde_json::from_str(profile_json()).unwrap();
    assert_eq!(profile.id, "work".into());
}
