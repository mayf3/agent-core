use anyhow::{bail, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::DeploymentHarnessConfig;

pub struct StartedProcess {
    pub pid: u32,
    pub port: u16,
    pub endpoint: String,
    pub log_path: PathBuf,
    pub instance_id: String,
}

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
    let stdout = append_log(&log_path)?;
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
    let mut child = command.spawn()?;
    let pid = child.id();
    let deadline = Instant::now() + health_timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("SERVICE_EXITED_BEFORE_READY:{status}");
        }
        if probe(
            &format!("127.0.0.1:{port}"),
            health_path,
            Duration::from_millis(500),
            component_id,
            version,
            &instance_id,
        ) {
            break;
        }
        if Instant::now() >= deadline {
            stop(pid, executable);
            let _ = child.wait();
            bail!("SERVICE_HEALTHCHECK_FAILED");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(StartedProcess {
        pid,
        port,
        endpoint,
        log_path,
        instance_id,
    })
}

pub fn probe(
    address: &str,
    path: &str,
    timeout: Duration,
    component_id: &str,
    version: &str,
    instance_id: &str,
) -> bool {
    let Ok(address) = address.parse::<SocketAddr>() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while response.len() <= 4096 {
        let Ok(read) = stream.read(&mut chunk) else {
            return false;
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
        return false;
    }
    let Ok(head) = std::str::from_utf8(&response) else {
        return false;
    };
    let Some((headers, _)) = head.split_once("\r\n\r\n") else {
        return false;
    };
    if !headers.starts_with("HTTP/1.1 200") {
        return false;
    }
    let mut observed_component = None;
    let mut observed_version = None;
    let mut observed_instance = None;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "x-agent-core-component" => observed_component = Some(value.trim()),
            "x-agent-core-version" => observed_version = Some(value.trim()),
            "x-agent-core-instance" => observed_instance = Some(value.trim()),
            _ => {}
        }
    }
    observed_component == Some(component_id)
        && observed_version == Some(version)
        && observed_instance == Some(instance_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_probe_rejects_unbound_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        });
        assert!(!probe(
            &address.to_string(),
            "/health",
            Duration::from_secs(1),
            "component",
            "0.1.0",
            &format!("instance_{}", "a".repeat(64)),
        ));
    }
}
