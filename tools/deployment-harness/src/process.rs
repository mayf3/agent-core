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
) -> Result<StartedProcess> {
    let port = reserve_port()?;
    let listen = format!("127.0.0.1:{port}");
    let endpoint = format!("http://{listen}");
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
        ) {
            break;
        }
        if Instant::now() >= deadline {
            stop(pid);
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
    })
}

pub fn probe(address: &str, path: &str, timeout: Duration) -> bool {
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
    let mut response = [0u8; 64];
    stream
        .read(&mut response)
        .ok()
        .is_some_and(|read| response[..read].starts_with(b"HTTP/1.1 200"))
}

pub fn stop(pid: u32) {
    if pid == 0 || pid > i32::MAX as u32 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let running = unsafe { libc::kill(pid as i32, 0) == 0 };
        if !running {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

fn reserve_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn append_log(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

unsafe fn lower_soft_limit(resource: libc::c_int, maximum: u64) -> std::io::Result<()> {
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
