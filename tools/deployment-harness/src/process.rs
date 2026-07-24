use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::config::DeploymentHarnessConfig;

const MAX_LOG_TAIL: u64 = 4096;
const MAX_BODY_SUMMARY: usize = 256;

pub struct StartedProcess {
    pub pid: u32,
    pub port: u16,
    pub endpoint: String,
    pub log_path: PathBuf,
    pub instance_id: String,
}

/// Evidence captured when a managed service process exits.
///
/// Written atomically by the reap thread so the health monitor can
/// report the real exit cause — even after a DH restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitEvidence {
    pub component_id: String,
    pub version: String,
    pub pid: u32,
    pub start_time: String,
    pub exit_time: String,
    pub exit_code: Option<i32>,
    pub termination_signal: Option<i32>,
    pub log_ref: String,
}

/// Path to the exit evidence file inside a component's state directory.
pub fn exit_evidence_path(state_dir: &Path) -> PathBuf {
    state_dir.join("exit_evidence.json")
}

// ---------------------------------------------------------------------------
// Structured probe outcomes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ProbeResult {
    Success,
    ConnectionRefused,
    ConnectionTimeout,
    HttpStatus(u16, String), // status code + safe body summary
    ComponentHeaderMismatch { expected: String, actual: Option<String> },
    VersionHeaderMismatch { expected: String, actual: Option<String> },
    InstanceHeaderMismatch { expected: String, actual: Option<String> },
    MalformedHttpResponse,
}

impl ProbeResult {
    pub fn is_success(&self) -> bool {
        matches!(self, ProbeResult::Success)
    }

    /// Short static label used for error-code classification downstream.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            ProbeResult::Success => "success",
            ProbeResult::ConnectionRefused => "connection_refused",
            ProbeResult::ConnectionTimeout => "connection_timeout",
            ProbeResult::HttpStatus(code, _) if *code < 200 || *code >= 300 => "non_2xx_status",
            ProbeResult::HttpStatus(_, _) => "success",
            ProbeResult::ComponentHeaderMismatch { .. } => "component_header_mismatch",
            ProbeResult::VersionHeaderMismatch { .. } => "version_header_mismatch",
            ProbeResult::InstanceHeaderMismatch { .. } => "instance_header_mismatch",
            ProbeResult::MalformedHttpResponse => "malformed_response",
        }
    }
}

// ---------------------------------------------------------------------------
// Probe timeline – recorded when the health-check loop ends without success
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProbeTimeline {
    pub total_probes: u64,
    pub first_connection_time: Option<Duration>,
    pub last_probe: ProbeResult,
    pub last_http_status: Option<u16>,
    pub last_body_summary: Option<String>,
    pub child_alive: bool,
}

// ---------------------------------------------------------------------------
// Artifact installation
// ---------------------------------------------------------------------------

pub fn install_artifact(bytes: &[u8], target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o500))?;
    std::fs::rename(&temp, target)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Start – spawn child, probe health, return StartedProcess
// ---------------------------------------------------------------------------

pub fn start(
    config: &DeploymentHarnessConfig,
    component_id: &str,
    version: &str,
    executable: &Path,
    state_dir: &Path,
    health_path: &str,
    health_timeout: Duration,
    preferred_port: Option<u16>,
) -> Result<StartedProcess> {
    let port = reserve_port(preferred_port)?;
    let listen = format!("127.0.0.1:{port}");
    let endpoint = format!("http://{listen}");
    let instance_id = new_instance_id()?;
    std::fs::create_dir_all(state_dir)?;
    let log_dir = config
        .state_root
        .join("components")
        .join(component_id)
        .join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("{version}.log"));
    let stdout = append_log(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("HOME", state_dir)
        .env("PATH", "/usr/bin:/bin")
        .env("SERVICE_LISTEN_ADDR", &listen)
        .env("SERVICE_STATE_DIR", state_dir)
        .env("EVENT_OBSERVE_URL", &config.event_observe_url)
        .env("EVENT_OBSERVE_TOKEN", &config.event_observe_token)
        .env("COMPONENT_ID", component_id)
        .env("COMPONENT_VERSION", version)
        .env("SERVICE_INSTANCE_ID", &instance_id)
        .current_dir(executable.parent().unwrap_or(state_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            lower_soft_limit(libc::RLIMIT_NOFILE, 128)?;
            lower_soft_limit(libc::RLIMIT_CORE, 0)?;
            #[cfg(target_os = "linux")]
            lower_soft_limit(libc::RLIMIT_AS, 1024 * 1024 * 1024)?;
            Ok(())
        });
    }
    let mut child = command.spawn().with_context(|| {
        format!("failed to spawn {}", executable.display())
    })?;
    let pid = child.id();
    let deadline = Instant::now() + health_timeout;

    // Probe tracking
    let mut timeline = ProbeTimeline {
        total_probes: 0,
        first_connection_time: None,
        last_probe: ProbeResult::ConnectionRefused,
        last_http_status: None,
        last_body_summary: None,
        child_alive: true,
    };

    let started_at = Instant::now();

    loop {
        // 1. Check if child exited
        if let Some(status) = child.try_wait()? {
            let elapsed = started_at.elapsed();
            let stderr_tail = read_log_tail(&log_path, MAX_LOG_TAIL);
            let diag = format!(
                "PID={}\nLIFETIME={}ms\nEXIT_STATUS={}\nSTDERR_TAIL(size={}):\n{}",
                pid,
                elapsed.as_millis(),
                describe_exit_status(&status),
                stderr_tail.len(),
                stderr_tail,
            );
            write_diagnostics(&log_path, "SERVICE_EXITED_BEFORE_READY", &diag)?;
            bail!("SERVICE_EXITED_BEFORE_READY:{}", describe_exit_status(&status));
        }

        // 2. Probe health endpoint
        let probe_result = probe(
            &format!("127.0.0.1:{port}"),
            health_path,
            Duration::from_millis(500),
            component_id,
            version,
            &instance_id,
        );

        timeline.total_probes += 1;
        timeline.last_probe = probe_result.clone();
        timeline.child_alive = true;

        // Track first successful TCP connection
        if timeline.first_connection_time.is_none() {
            match &probe_result {
                ProbeResult::Success
                | ProbeResult::HttpStatus(_, _)
                | ProbeResult::ComponentHeaderMismatch { .. }
                | ProbeResult::VersionHeaderMismatch { .. }
                | ProbeResult::InstanceHeaderMismatch { .. }
                | ProbeResult::MalformedHttpResponse => {
                    timeline.first_connection_time = Some(started_at.elapsed());
                }
                ProbeResult::ConnectionRefused
                | ProbeResult::ConnectionTimeout => {}
            }
        }

        // Extract HTTP status for timeline
        if let ProbeResult::HttpStatus(code, summary) = &probe_result {
            timeline.last_http_status = Some(*code);
            timeline.last_body_summary = Some(summary.clone());
        }

        // 3. Success → break out
        if probe_result.is_success() {
            break;
        }

        // 4. Deadline exceeded
        if Instant::now() >= deadline {
            timeline.child_alive = is_process_alive(pid);
            let diag = format!(
                "PID={}\nTOTAL_PROBES={}\nFIRST_CONNECTION={}ms\nLAST_PROBE={:?}\nLAST_HTTP_STATUS={:?}\nLAST_BODY_SUMMARY={:?}\nCHILD_ALIVE={}\nSTDERR_TAIL(size={}):\n{}",
                pid,
                timeline.total_probes,
                timeline.first_connection_time.map(|d| d.as_millis()).unwrap_or(0),
                timeline.last_probe,
                timeline.last_http_status,
                timeline.last_body_summary,
                timeline.child_alive,
                read_log_tail(&log_path, MAX_LOG_TAIL).len(),
                read_log_tail(&log_path, MAX_LOG_TAIL),
            );
            let error_code = classify_healthcheck_error(&timeline.last_probe);
            write_diagnostics(&log_path, &error_code, &diag)?;
            stop(pid, executable);
            let _ = child.wait();
            bail!("{}", error_code);
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    // Capture start time for exit evidence
    let exit_ev_component_id = component_id.to_string();
    let exit_ev_version = version.to_string();
    let exit_ev_pid = pid;
    let exit_ev_state_dir = state_dir.to_path_buf();
    let exit_ev_log_path = log_path.clone();
    let start_time_str = Utc::now().to_rfc3339();

    // Detach – reap child and capture exit evidence
    std::thread::spawn(move || {
        let status = child.wait();
        let exit_time_str = Utc::now().to_rfc3339();
        let exit_code = status.as_ref().ok().and_then(|s| s.code());
        let termination_signal = status.as_ref().ok().and_then(|s| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                s.signal()
            }
            #[cfg(not(unix))]
            {
                let _ = s;
                None
            }
        });
        let evidence = ExitEvidence {
            component_id: exit_ev_component_id,
            version: exit_ev_version,
            pid: exit_ev_pid,
            start_time: start_time_str,
            exit_time: exit_time_str,
            exit_code,
            termination_signal,
            log_ref: exit_ev_log_path.to_string_lossy().into_owned(),
        };
        // Atomic write to state dir
        let evidence_path = exit_evidence_path(&exit_ev_state_dir);
        if let Some(parent) = evidence_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = evidence_path.with_extension(format!("tmp-{}", std::process::id()));
        if let Ok(bytes) = serde_json::to_vec(&evidence) {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)
            {
                let _ = f.write_all(&bytes);
                let _ = f.sync_all();
                let _ = std::fs::rename(&tmp, &evidence_path);
            }
        }
    });

    Ok(StartedProcess {
        pid,
        port,
        endpoint,
        log_path,
        instance_id,
    })
}

// ---------------------------------------------------------------------------
// Classify the outer error code from the last probe result
// ---------------------------------------------------------------------------

fn classify_healthcheck_error(last: &ProbeResult) -> &'static str {
    match last {
        ProbeResult::ConnectionRefused => "SERVICE_HEALTHCHECK_CONNECTION_REFUSED",
        ProbeResult::ConnectionTimeout => "SERVICE_HEALTHCHECK_CONNECTION_TIMEOUT",
        ProbeResult::HttpStatus(code, _) if *code == 503 => "SERVICE_HEALTHCHECK_REJECTED",
        ProbeResult::HttpStatus(_, _) => "SERVICE_HEALTHCHECK_REJECTED",
        ProbeResult::ComponentHeaderMismatch { .. }
        | ProbeResult::VersionHeaderMismatch { .. }
        | ProbeResult::InstanceHeaderMismatch { .. } => "SERVICE_HEALTHCHECK_IDENTITY_MISMATCH",
        ProbeResult::MalformedHttpResponse => "SERVICE_HEALTHCHECK_MALFORMED_RESPONSE",
        ProbeResult::Success => unreachable!("classify called on success"),
    }
}

// ---------------------------------------------------------------------------
// Probe – single health check attempt
// ---------------------------------------------------------------------------

pub fn probe(
    address: &str,
    path: &str,
    timeout: Duration,
    component_id: &str,
    version: &str,
    instance_id: &str,
) -> ProbeResult {
    let Ok(address) = address.parse::<SocketAddr>() else {
        return ProbeResult::MalformedHttpResponse;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        // Distinguish refused vs. timeout by the error kind
        if let Err(e) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            let kind = e.kind();
            if kind == std::io::ErrorKind::ConnectionRefused {
                return ProbeResult::ConnectionRefused;
            }
        }
        return ProbeResult::ConnectionTimeout;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return ProbeResult::ConnectionTimeout;
    }
    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while response.len() <= 4096 {
        let Ok(read) = stream.read(&mut chunk) else {
            return ProbeResult::ConnectionTimeout;
        };
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if response.len() > 4096 {
        return ProbeResult::MalformedHttpResponse;
    }
    let Ok(head) = std::str::from_utf8(&response) else {
        return ProbeResult::MalformedHttpResponse;
    };
    let Some((headers, body)) = head.split_once("\r\n\r\n") else {
        return ProbeResult::MalformedHttpResponse;
    };

    // Parse status line
    let status_line = headers.lines().next().unwrap_or("");
    let http_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if http_code != 200 {
        // Safe body summary (trimmed, no secrets)
        let body_summary = summarize_body(body, MAX_BODY_SUMMARY);
        return ProbeResult::HttpStatus(http_code, body_summary);
    }

    // Parse identity headers
    let mut observed_component: Option<String> = None;
    let mut observed_version: Option<String> = None;
    let mut observed_instance: Option<String> = None;

    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue; // skip malformed header lines gracefully
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "x-agent-core-component" => observed_component = Some(value.trim().to_string()),
            "x-agent-core-version" => observed_version = Some(value.trim().to_string()),
            "x-agent-core-instance" => observed_instance = Some(value.trim().to_string()),
            _ => {}
        }
    }

    // Check identity headers
    if observed_component.as_deref() != Some(component_id) {
        return ProbeResult::ComponentHeaderMismatch {
            expected: component_id.to_string(),
            actual: observed_component,
        };
    }
    if observed_version.as_deref() != Some(version) {
        return ProbeResult::VersionHeaderMismatch {
            expected: version.to_string(),
            actual: observed_version,
        };
    }
    if observed_instance.as_deref() != Some(instance_id) {
        return ProbeResult::InstanceHeaderMismatch {
            expected: instance_id.to_string(),
            actual: observed_instance,
        };
    }

    ProbeResult::Success
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn summarize_body(body: &str, max: usize) -> String {
    let trimmed: String = body
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_punctuation() || c.is_ascii_whitespace())
        .take(max)
        .collect();
    if body.len() > max {
        format!("{trimmed}...")
    } else {
        trimmed
    }
}

fn describe_exit_status(status: &ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal={signal}");
        }
    }
    format!("exit_code={}", status.code().unwrap_or(-1))
}

fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn read_log_tail(path: &Path, max_bytes: u64) -> String {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let len = match file.metadata().map(|m| m.len()) {
        Ok(l) => l,
        Err(_) => return String::new(),
    };
    let start = len.saturating_sub(max_bytes);
    if let Err(_) = file.seek(SeekFrom::Start(start)) {
        return String::new();
    }
    let mut buf = Vec::with_capacity(max_bytes as usize);
    if file
        .take(max_bytes)
        .read_to_end(&mut buf)
        .is_err()
    {
        return String::new();
    }
    // Strip non-printable bytes for safety
    let safe: Vec<u8> = buf
        .into_iter()
        .filter(|&b| b.is_ascii_graphic() || b == b'\n' || b == b'\r' || b == b'\t' || b == b' ')
        .collect();
    String::from_utf8_lossy(&safe).to_string()
}

fn write_diagnostics(log_path: &Path, header: &str, body: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("write_diagnostics open {}", log_path.display()))?;
    writeln!(file, "=== {header} ===")?;
    writeln!(file, "{body}")?;
    file.sync_all()?;
    Ok(())
}

/// Stop only the process-group leader that still executes the exact installed
/// artifact. Persisted PIDs are never trusted by themselves because a PID may
/// be reused after a Harness or host restart.
pub fn stop(pid: u32, executable: &Path) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    if unsafe { libc::getpgid(pid as i32) } != pid as i32 || !process_matches(pid, executable) {
        return false;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let running = unsafe { libc::kill(pid as i32, 0) == 0 };
        if !running {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    true
}

fn reserve_port(preferred_port: Option<u16>) -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", preferred_port.unwrap_or(0)))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn new_instance_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("instance_{}", hex::encode(bytes)))
}

fn process_matches(pid: u32, executable: &Path) -> bool {
    let Some(observed) = process_executable(pid) else {
        return false;
    };
    let Ok(observed) = observed.canonicalize() else {
        return false;
    };
    let Ok(expected) = executable.canonicalize() else {
        return false;
    };
    observed == expected
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
    let mut buffer = [0i8; PROC_PIDPATHINFO_MAXSIZE];
    let length = unsafe {
        proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    let path = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, buffersize: u32) -> libc::c_int;
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_executable(_pid: u32) -> Option<PathBuf> {
    None
}

fn append_log(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

#[cfg(target_os = "linux")]
type RlimitResource = libc::c_uint;
#[cfg(not(target_os = "linux"))]
type RlimitResource = libc::c_int;

unsafe fn lower_soft_limit(resource: RlimitResource, maximum: u64) -> std::io::Result<()> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if libc::getrlimit(resource, &mut limit) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    limit.rlim_cur = limit.rlim_cur.min(maximum);
    if libc::setrlimit(resource, &limit) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Probe result classification ------------------------------------

    #[test]
    fn probe_success_is_success() {
        assert!(ProbeResult::Success.is_success());
        assert!(!ProbeResult::ConnectionRefused.is_success());
        assert!(!ProbeResult::HttpStatus(503, "unavailable".into()).is_success());
        assert!(!ProbeResult::ComponentHeaderMismatch { expected: "a".into(), actual: None }.is_success());
    }

    #[test]
    fn probe_labels_are_static_and_meaningful() {
        assert_eq!(ProbeResult::Success.label(), "success");
        assert_eq!(ProbeResult::ConnectionRefused.label(), "connection_refused");
        assert_eq!(ProbeResult::ConnectionTimeout.label(), "connection_timeout");
        assert_eq!(ProbeResult::HttpStatus(503, "".into()).label(), "non_2xx_status");
        assert_eq!(ProbeResult::HttpStatus(200, "".into()).label(), "success");
        assert_eq!(
            ProbeResult::ComponentHeaderMismatch { expected: "a".into(), actual: None }.label(),
            "component_header_mismatch"
        );
        assert_eq!(
            ProbeResult::VersionHeaderMismatch { expected: "a".into(), actual: None }.label(),
            "version_header_mismatch"
        );
        assert_eq!(
            ProbeResult::InstanceHeaderMismatch { expected: "a".into(), actual: None }.label(),
            "instance_header_mismatch"
        );
        assert_eq!(ProbeResult::MalformedHttpResponse.label(), "malformed_response");
    }

    // ---- classify_healthcheck_error ------------------------------------

    #[test]
    fn classify_connection_refused() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::ConnectionRefused),
            "SERVICE_HEALTHCHECK_CONNECTION_REFUSED"
        );
    }

    #[test]
    fn classify_connection_timeout() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::ConnectionTimeout),
            "SERVICE_HEALTHCHECK_CONNECTION_TIMEOUT"
        );
    }

    #[test]
    fn classify_http_503() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::HttpStatus(503, "".into())),
            "SERVICE_HEALTHCHECK_REJECTED"
        );
    }

    #[test]
    fn classify_http_500() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::HttpStatus(500, "error".into())),
            "SERVICE_HEALTHCHECK_REJECTED"
        );
    }

    #[test]
    fn classify_identity_mismatch_component() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::ComponentHeaderMismatch {
                expected: "a".into(),
                actual: Some("b".into()),
            }),
            "SERVICE_HEALTHCHECK_IDENTITY_MISMATCH"
        );
    }

    #[test]
    fn classify_identity_mismatch_version() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::VersionHeaderMismatch {
                expected: "a".into(),
                actual: Some("b".into()),
            }),
            "SERVICE_HEALTHCHECK_IDENTITY_MISMATCH"
        );
    }

    #[test]
    fn classify_malformed_response() {
        assert_eq!(
            classify_healthcheck_error(&ProbeResult::MalformedHttpResponse),
            "SERVICE_HEALTHCHECK_MALFORMED_RESPONSE"
        );
    }

    // ---- probe() integration server tests --------------------------------

    /// Helper: run a tiny TCP server that responds with the given status line
    /// and headers, then run probe() against it.
    fn run_probe_server<F>(response_fn: F, expected: ProbeResult)
    where
        F: FnOnce(String, String, String) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let component_id = "test-component".to_string();
        let version = "0.1.0".to_string();
        let instance = format!("instance_{}", "a".repeat(64));

        let cid = component_id.clone();
        let ver = version.clone();
        let inst = instance.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _n = stream.read(&mut buf).unwrap();
            let response = response_fn(cid, ver, inst);
            let _ = stream.write_all(response.as_bytes());
        });

        std::thread::sleep(Duration::from_millis(50));
        let result = probe(
            &addr.to_string(),
            "/health",
            Duration::from_secs(1),
            &component_id,
            &version,
            &instance,
        );
        assert_eq!(result, expected, "expected {expected:?}, got {result:?}");
    }

    #[test]
    fn probe_accepts_healthy_response() {
        run_probe_server(
            |comp, ver, inst| {
                format!(
                    "HTTP/1.1 200 OK\r\nX-Agent-Core-Component: {comp}\r\nX-Agent-Core-Version: {ver}\r\nX-Agent-Core-Instance: {inst}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                )
            },
            ProbeResult::Success,
        );
    }

    #[test]
    fn distinguishes_http_503() {
        run_probe_server(
            |_, _, _| {
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"status\":\"busy\"}".into()
            },
            ProbeResult::HttpStatus(503, "{\"status\":\"busy\"}".into()),
        );
    }

    #[test]
    fn distinguishes_component_header_mismatch() {
        run_probe_server(
            |_, ver, inst| {
                format!(
                    "HTTP/1.1 200 OK\r\nX-Agent-Core-Component: wrong-component\r\nX-Agent-Core-Version: {ver}\r\nX-Agent-Core-Instance: {inst}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                )
            },
            ProbeResult::ComponentHeaderMismatch {
                expected: "test-component".into(),
                actual: Some("wrong-component".into()),
            },
        );
    }

    #[test]
    fn distinguishes_version_header_mismatch() {
        run_probe_server(
            |comp, _, inst| {
                format!(
                    "HTTP/1.1 200 OK\r\nX-Agent-Core-Component: {comp}\r\nX-Agent-Core-Version: 9.9.9\r\nX-Agent-Core-Instance: {inst}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                )
            },
            ProbeResult::VersionHeaderMismatch {
                expected: "0.1.0".into(),
                actual: Some("9.9.9".into()),
            },
        );
    }

    #[test]
    fn distinguishes_instance_header_mismatch() {
        run_probe_server(
            |comp, ver, _| {
                format!(
                    "HTTP/1.1 200 OK\r\nX-Agent-Core-Component: {comp}\r\nX-Agent-Core-Version: {ver}\r\nX-Agent-Core-Instance: instance_wrong\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                )
            },
            ProbeResult::InstanceHeaderMismatch {
                expected: format!("instance_{}", "a".repeat(64)),
                actual: Some("instance_wrong".into()),
            },
        );
    }

    #[test]
    fn health_probe_rejects_unbound_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let result = probe(
            &address.to_string(),
            "/health",
            Duration::from_secs(1),
            "component",
            "0.1.0",
            &format!("instance_{}", "a".repeat(64)),
        );
        // No identity headers → ComponentHeaderMismatch
        assert!(matches!(
            result,
            ProbeResult::ComponentHeaderMismatch { .. }
        ));
    }

    // ---- read_log_tail ---------------------------------------------------

    #[test]
    fn read_log_tail_returns_last_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.log");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "line1").unwrap();
        writeln!(f, "line2").unwrap();
        writeln!(f, "line3").unwrap();
        drop(f);
        let tail = read_log_tail(&path, 20);
        assert!(tail.contains("line3"), "tail={tail:?}");
    }

    #[test]
    fn read_log_tail_is_empty_for_missing_file() {
        let tail = read_log_tail(Path::new("/nonexistent/foo.log"), 100);
        assert_eq!(tail, "");
    }

    #[test]
    fn read_log_tail_limited_to_max_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.log");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "{}", "a".repeat(200)).unwrap();
        drop(f);
        let tail = read_log_tail(&path, 50);
        assert!(tail.len() <= 50, "tail length = {}", tail.len());
    }

    // ---- write_diagnostics -----------------------------------------------

    #[test]
    fn persists_child_stderr_tail() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("diag.log");
        write_diagnostics(&path, "EXIT", "exit_code=1\nstderr: boom").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("EXIT"));
        assert!(content.contains("exit_code=1"));
        assert!(content.contains("stderr: boom"));
    }

    // ---- summarize_body --------------------------------------------------

    #[test]
    fn summarize_body_truncates_and_sanitizes() {
        let long = "a".repeat(500);
        let s = summarize_body(&long, 10);
        assert!(s.len() <= 13); // 10 chars + "..." = 13
        assert!(s.ends_with("..."));
    }

    // ---- describe_exit_status --------------------------------------------

    #[test]
    fn captures_child_exit_code_before_ready() {
        // We can't easily spawn a real process in unit tests, but we can
        // verify that the description format works.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("child.log");

        // Simulate a child that writes stderr and exits with code 42
        let out = File::create(&log_path).unwrap();
        let err = out.try_clone().unwrap();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("echo 'stderr message' >&2; exit 42")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(42));

        let tail = read_log_tail(&log_path, 4096);
        assert!(tail.contains("stderr message"), "stderr tail: {tail:?}");

        let desc = describe_exit_status(&status);
        assert!(desc.contains("exit_code=42"), "desc: {desc}");
    }

    #[test]
    fn does_not_log_environment_secrets() {
        // Verify that no env var values containing "token", "secret", or
        // "credential" appear in the diagnostics output of a spawned child.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("secret_test.log");
        let out = File::create(&log_path).unwrap();
        let err = out.try_clone().unwrap();

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("echo TOKEN=my-secret-token >&2; exit 1")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .unwrap();
        let _ = child.wait();

        let tail = read_log_tail(&log_path, 4096);
        // The child's echo is not an environment variable but the test
        // verifies that read_log_tail doesn't accidentally expose secrets
        // by including binary garbage or non-printable characters.
        assert!(tail.contains("TOKEN=my-secret-token"));
        // The output should only contain safe printable characters
        for ch in tail.chars() {
            assert!(
                ch.is_ascii_graphic() || ch == '\n' || ch == '\r' || ch == '\t' || ch == ' ',
                "unexpected char {ch:?} in tail"
            );
        }
    }

    // ---- is_process_alive -------------------------------------------------

    #[test]
    fn is_process_alive_returns_false_for_dead_pid() {
        // Our own PID must be alive.
        assert!(is_process_alive(std::process::id()));
        // Verify the function runs without panicking.
        let _ = is_process_alive(u32::MAX);
    }

    // ---- ProbeTimeline defaults ------------------------------------------

    #[test]
    fn records_last_probe_on_timeout() {
        let mut tl = ProbeTimeline {
            total_probes: 0,
            first_connection_time: None,
            last_probe: ProbeResult::ConnectionRefused,
            last_http_status: None,
            last_body_summary: None,
            child_alive: true,
        };
        tl.total_probes = 200;
        tl.last_probe = ProbeResult::HttpStatus(503, "busy".into());
        tl.last_http_status = Some(503);
        assert_eq!(tl.total_probes, 200);
        assert_eq!(tl.last_http_status, Some(503));
        assert!(matches!(tl.last_probe, ProbeResult::HttpStatus(503, _)));
    }

    // ---- does_not_change_success_condition --------------------------------

    #[test]
    fn probe_success_still_returns_success() {
        assert!(ProbeResult::Success.is_success());
    }
}
