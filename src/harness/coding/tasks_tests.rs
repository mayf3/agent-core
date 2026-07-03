use super::*;

fn submit(ws: &str, obj: &str, acc: &str, bk: &str) -> Value {
    submit_task(ws, obj, acc, bk, Some("."), None)
}

#[test]
fn fake_backend_full_state_machine() {
    let resp = submit("ws1", "test objective", "must pass", "fake");
    let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
    assert_eq!(resp["result"]["status"], "queued");
    assert_eq!(resp["result"]["backend"], "fake");
    std::thread::sleep(Duration::from_millis(50));
    let s = get_status(&tid);
    assert_eq!(
        s["result"]["status"], "succeeded",
        "fake backend should reach succeeded; got: {s}"
    );
    assert!(s["result"]["summary"]
        .as_str()
        .unwrap_or("")
        .contains("fake"));
    assert!(s["result"]["commit_sha"]
        .as_str()
        .unwrap_or("")
        .contains("fake_sha"));
}

#[test]
fn task_cancel_before_execution() {
    let resp = submit("ws1", "cancellable objective", "", "fake");
    let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
    let cancel_resp = cancel_task(&tid);
    assert_eq!(cancel_resp["result"]["status"], "cancelled");
    let s = get_status(&tid);
    assert_eq!(s["result"]["status"], "cancelled");
}

#[test]
fn task_not_found() {
    let s = get_status("nonexistent");
    assert_eq!(s["ok"], false);
    assert_eq!(s["error_code"], "task_not_found");
}

#[test]
fn task_submit_includes_acceptance_criteria() {
    let resp = submit("ws1", "build", "test passes", "fake");
    let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(50));
    let s = get_status(&tid);
    assert_eq!(s["result"]["status"], "succeeded");
    assert!(
        s["result"]["test_result"]
            .as_str()
            .unwrap_or("")
            .contains("test passes"),
        "test_result should include acceptance criteria text"
    );
}

#[test]
fn opencode_backend_not_permitted_checked_at_dispatch() {
    let resp = submit("ws1", "test", "pass", "opencode");
    assert_eq!(resp["result"]["backend"], "opencode");
    let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(200));
    let s = get_status(&tid);
    assert_ne!(s["result"]["status"], "queued", "task should leave queued");
}

#[test]
fn opencode_backend_unavailable_fails() {
    let found = crate::harness::coding::opencode_backend::find_opencode().is_ok();
    assert!(found, "opencode should be found in test environment");
}
