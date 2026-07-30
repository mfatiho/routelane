use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

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

fn write_private(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        event_sequence: 0,
        health: HealthReport::inactive(),
        desired_profile_id: None,
        applied_profile_id: None,
        last_error: None,
    }
}

fn applied_status() -> DaemonStatus {
    DaemonStatus {
        generation: 7,
        event_sequence: 4,
        health: HealthReport::from_parts(HealthState::Healthy, Default::default(), vec![]),
        desired_profile_id: Some("work".into()),
        applied_profile_id: Some("work".into()),
        last_error: None,
    }
}

fn profile_json() -> &'static str {
    r#"{
      "schema_version":2,
      "language":"en",
      "profiles":[{
        "id":"work",
        "name":"Work",
        "enabled":false,
        "lanes":[{
          "id":"vpn",
          "name":"VPN",
          "interface":"wg0",
          "fallback_lane":null,
          "failure_policy":"Inactive"
        }],
        "rules":[]
      }]
    }"#
}

fn multiple_profile_json() -> &'static str {
    r#"{
      "schema_version":2,
      "language":"en",
      "profiles":[
        {"id":"work","name":"Work","enabled":false,"lanes":[],"rules":[]},
        {"id":"privacy","name":"Privacy","enabled":false,"lanes":[],"rules":[]}
      ]
    }"#
}

async fn assert_local_config_error_without_daemon_contact(
    runtime: &TempDir,
    args: &[&str],
) -> std::process::Output {
    let socket_dir = runtime.path().join("routelane");
    std::fs::create_dir(&socket_dir).unwrap();
    let listener = UnixListener::bind(socket_dir.join("routelane.sock")).unwrap();

    let output = tokio::time::timeout(Duration::from_secs(2), run_cli(runtime.path(), args))
        .await
        .expect("local config rejection must not hang");

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "invalid local input must not contact the daemon"
    );
    output
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

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["generation"], 7);
    assert_eq!(value["event_sequence"], 0);
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

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "state=Healthy generation=7 applied_profile=work\n"
    );
}

#[tokio::test]
async fn apply_profile_reads_schema_v2_and_uses_daemon_protocol() {
    let runtime = TempDir::new().unwrap();
    let config = runtime.path().join("profile.json");
    write_private(&config, profile_json());
    serve_once(
        runtime.path(),
        |request| match request {
            DaemonRequest::ApplyProfile(profile) => {
                assert_eq!(profile.id, "work".into());
                assert!(profile.enabled);
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

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "applied generation=8\n"
    );
}

#[tokio::test]
async fn apply_profile_requires_a_selector_for_multiple_profiles_and_selects_by_name() {
    let runtime = TempDir::new().unwrap();
    let config = runtime.path().join("profiles.json");
    write_private(&config, multiple_profile_json());

    let missing = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", config.to_str().unwrap()],
    )
    .await;
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("--profile"),
        "stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    serve_once(
        runtime.path(),
        |request| match request {
            DaemonRequest::ApplyProfile(profile) => {
                assert_eq!(profile.id, "privacy".into());
                assert!(profile.enabled);
            }
            other => panic!("unexpected request: {other:?}"),
        },
        Ok(DaemonResult::Applied(routelane::ipc::ApplyProfileResult {
            generation: 9,
            plan_hash: serde_json::from_str(&format!("[{}]", ["2"; 32].join(","))).unwrap(),
        })),
    )
    .await;

    let selected = run_cli(
        runtime.path(),
        &[
            "apply-profile",
            config.to_str().unwrap(),
            "--profile",
            "Privacy",
        ],
    )
    .await;
    assert!(selected.status.success());
    assert_eq!(
        String::from_utf8(selected.stdout).unwrap(),
        "applied generation=9\n"
    );
}

#[tokio::test]
async fn selector_ambiguity_missing_match_and_semantic_errors_stay_local() {
    let runtime = TempDir::new().unwrap();
    let ambiguous = runtime.path().join("ambiguous.json");
    write_private(
        &ambiguous,
        r#"{"schema_version":2,"language":"en","profiles":[
          {"id":"a","name":"Same","enabled":false,"lanes":[],"rules":[]},
          {"id":"b","name":"Same","enabled":false,"lanes":[],"rules":[]}
        ]}"#,
    );
    let output = assert_local_config_error_without_daemon_contact(
        &runtime,
        &[
            "apply-profile",
            ambiguous.to_str().unwrap(),
            "--profile",
            "Same",
        ],
    )
    .await;
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("duplicate profile name"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let id_name_collision = runtime.path().join("id-name-collision.json");
    write_private(
        &id_name_collision,
        r#"{"schema_version":2,"language":"en","profiles":[
          {"id":"a","name":"Alpha","enabled":false,"lanes":[],"rules":[]},
          {"id":"b","name":"a","enabled":false,"lanes":[],"rules":[]}
        ]}"#,
    );
    let output = assert_local_config_error_without_daemon_contact(
        &runtime,
        &[
            "apply-profile",
            id_name_collision.to_str().unwrap(),
            "--profile",
            "a",
        ],
    )
    .await;
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ambiguous"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let output = assert_local_config_error_without_daemon_contact(
        &runtime,
        &[
            "apply-profile",
            id_name_collision.to_str().unwrap(),
            "--profile",
            "missing",
        ],
    )
    .await;
    assert!(String::from_utf8_lossy(&output.stderr).contains("not found"));

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let invalid = runtime.path().join("invalid-semantic.json");
    write_private(
        &invalid,
        r#"{"schema_version":2,"language":"en","profiles":[{
          "id":"bad","name":"Bad","enabled":false,
          "lanes":[
            {"id":"dup","name":"A","interface":"wg0","fallback_lane":null,"failure_policy":"Inactive"},
            {"id":"dup","name":"B","interface":"wg1","fallback_lane":null,"failure_policy":"Inactive"}
          ],"rules":[]
        }]}"#,
    );
    let output = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", invalid.to_str().unwrap()],
    )
    .await;
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid profile"));
}

#[tokio::test]
async fn symlink_fifo_and_oversize_config_inputs_are_rejected_without_contact_or_hang() {
    let runtime = TempDir::new().unwrap();
    let target = runtime.path().join("target.json");
    write_private(&target, profile_json());
    let link = runtime.path().join("profile-link.json");
    symlink(&target, &link).unwrap();
    let linked = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", link.to_str().unwrap()],
    )
    .await;
    assert!(String::from_utf8_lossy(&linked.stderr).contains("regular file"));

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let unsafe_mode = runtime.path().join("unsafe-mode.json");
    write_private(&unsafe_mode, profile_json());
    std::fs::set_permissions(&unsafe_mode, std::fs::Permissions::from_mode(0o666)).unwrap();
    let unsafe_result = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", unsafe_mode.to_str().unwrap()],
    )
    .await;
    assert!(String::from_utf8_lossy(&unsafe_result.stderr).contains("permissions"));

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let fifo = runtime.path().join("profile.fifo");
    let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    let fifo_result = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", fifo.to_str().unwrap()],
    )
    .await;
    assert!(String::from_utf8_lossy(&fifo_result.stderr).contains("regular file"));

    std::fs::remove_dir_all(runtime.path().join("routelane")).unwrap();
    let oversize = runtime.path().join("oversize.json");
    write_private(&oversize, vec![b' '; 1_048_577]);
    let large = assert_local_config_error_without_daemon_contact(
        &runtime,
        &["apply-profile", oversize.to_str().unwrap()],
    )
    .await;
    assert!(
        String::from_utf8_lossy(&large.stderr).contains("maximum"),
        "stderr: {}",
        String::from_utf8_lossy(&large.stderr)
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
    write_private(&config, "{broken");
    let invalid = run_cli(runtime.path(), &["apply-profile", config.to_str().unwrap()]).await;
    assert_eq!(invalid.status.code(), Some(3));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("configuration is not valid JSON"),
        "stderr: {}",
        String::from_utf8_lossy(&invalid.stderr)
    );

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
    let document: serde_json::Value = serde_json::from_str(profile_json()).unwrap();
    let profile: Profile = serde_json::from_value(document["profiles"][0].clone()).unwrap();
    assert_eq!(profile.id, "work".into());
}
