//! Verified artifact loading and descriptor-backed materialization.

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use sha2::{Digest, Sha256};
use std::fs::File;
#[cfg(not(target_os = "linux"))]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
#[cfg(not(target_os = "linux"))]
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A held executable inode whose bytes and platform protection were verified.
pub struct ResolvedArtifact {
    file: File,
    digest: String,
    device: u64,
    inode: u64,
    len: u64,
    materialized_path: Option<PathBuf>,
}

impl ResolvedArtifact {
    /// Re-verify seals/inode/bytes immediately before descriptor-path exec.
    pub fn verified_execution_path(&self) -> Result<PathBuf, ArtifactError> {
        let metadata = self.file.metadata().map_err(store_error)?;
        if metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.len() != self.len
            || metadata.nlink() != expected_link_count()
            || !metadata.file_type().is_file()
        {
            return Err(ArtifactError::MaterializationChanged);
        }
        verify_platform_protection(&self.file)?;
        if digest_file(&self.file)? != self.digest {
            return Err(ArtifactError::DigestMismatch);
        }
        Ok(execution_path(
            &self.file,
            self.materialized_path.as_deref(),
        ))
    }

    /// Non-Linux private fallback path, removed on drop. Linux memfd has no path.
    pub fn materialized_path(&self) -> Option<&Path> {
        self.materialized_path.as_deref()
    }
}

impl Drop for ResolvedArtifact {
    fn drop(&mut self) {
        if let Some(path) = &self.materialized_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Rehash CAS bytes and create a non-replaceable executable object. Linux uses
/// a sealed memfd; other Unix systems use a private random file that remains in
/// a mode-0700 runtime directory until its read-only descriptor is dropped.
pub fn resolve_artifact(
    artifact_root: &Path,
    digest_str: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let digest = Sha256Digest::parse(digest_str).map_err(|_| ArtifactError::InvalidDigest)?;
    let bytes = ContentStore::new(artifact_root.to_path_buf())
        .load(&digest)
        .map_err(classify_store_error)?;
    if Sha256Digest::compute(&bytes).as_str() != digest_str {
        return Err(ArtifactError::DigestMismatch);
    }
    materialize_verified(artifact_root, &bytes, digest_str)
}

#[cfg(target_os = "linux")]
fn materialize_verified(
    _artifact_root: &Path,
    bytes: &[u8],
    digest: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let name = std::ffi::CString::new("agent-core-calculator").map_err(store_error)?;
    let raw_fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        ) as libc::c_int
    };
    if raw_fd < 0 {
        return Err(store_error(std::io::Error::last_os_error()));
    }
    let mut file = unsafe { File::from_raw_fd(raw_fd) };
    file.write_all(bytes).map_err(store_error)?;
    file.sync_all().map_err(store_error)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(store_error)?;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, required_seals()) } != 0 {
        return Err(store_error(std::io::Error::last_os_error()));
    }
    let metadata = file.metadata().map_err(store_error)?;
    let artifact = ResolvedArtifact {
        file,
        digest: digest.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        materialized_path: None,
    };
    verify_platform_protection(&artifact.file)?;
    if digest_file(&artifact.file)? != digest {
        return Err(ArtifactError::DigestMismatch);
    }
    Ok(artifact)
}

#[cfg(target_os = "linux")]
fn required_seals() -> libc::c_int {
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
}

#[cfg(target_os = "linux")]
fn verify_platform_protection(file: &File) -> Result<(), ArtifactError> {
    let actual = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if actual < 0 || actual & required_seals() != required_seals() {
        return Err(ArtifactError::MaterializationChanged);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn materialize_verified(
    artifact_root: &Path,
    bytes: &[u8],
    digest: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let runtime = private_runtime_dir(artifact_root)?;
    let path = create_random_path(&runtime)?;
    let result = materialize_private_file(&path, bytes, digest);
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn materialize_private_file(
    path: &Path,
    bytes: &[u8],
    digest: &str,
) -> Result<ResolvedArtifact, ArtifactError> {
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o000)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(store_error)?;
    writer.write_all(bytes).map_err(store_error)?;
    writer.sync_all().map_err(store_error)?;
    writer
        .set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(store_error)?;
    drop(writer);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(store_error)?;
    let metadata = file.metadata().map_err(store_error)?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 || digest_file(&file)? != digest {
        return Err(ArtifactError::DigestMismatch);
    }
    Ok(ResolvedArtifact {
        file,
        digest: digest.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        materialized_path: Some(path.to_path_buf()),
    })
}

#[cfg(not(target_os = "linux"))]
fn verify_platform_protection(_file: &File) -> Result<(), ArtifactError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn expected_link_count() -> u64 {
    0
}

#[cfg(not(target_os = "linux"))]
fn expected_link_count() -> u64 {
    1
}

#[cfg(not(target_os = "linux"))]
fn private_runtime_dir(root: &Path) -> Result<PathBuf, ArtifactError> {
    let state = root.join(".capability-host");
    let runtime = state.join("runtime");
    for directory in [&state, &runtime] {
        std::fs::create_dir_all(directory).map_err(store_error)?;
        let metadata = std::fs::symlink_metadata(directory).map_err(store_error)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::UnsafeMaterializationRoot);
        }
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(store_error)?;
    }
    Ok(runtime)
}

#[cfg(not(target_os = "linux"))]
fn create_random_path(runtime: &Path) -> Result<PathBuf, ArtifactError> {
    for _ in 0..16 {
        let mut random = [0u8; 24];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut random))
            .map_err(store_error)?;
        let path = runtime.join(format!("artifact-{}", hex::encode(random)));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(ArtifactError::StoreError(
        "could not allocate artifact materialization".into(),
    ))
}

fn digest_file(file: &File) -> Result<String, ArtifactError> {
    let mut reader = file.try_clone().map_err(store_error)?;
    reader.seek(SeekFrom::Start(0)).map_err(store_error)?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).map_err(store_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

#[cfg(target_os = "linux")]
fn execution_path(file: &File, _fallback: Option<&Path>) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(not(target_os = "linux"))]
fn execution_path(_file: &File, fallback: Option<&Path>) -> PathBuf {
    fallback
        .unwrap_or_else(|| Path::new("/nonexistent"))
        .to_path_buf()
}

fn classify_store_error(error: impl std::fmt::Display) -> ArtifactError {
    let message = error.to_string();
    if message.contains("not found") || message.contains("No such") {
        ArtifactError::NotFound
    } else if message.contains("mismatch") {
        ArtifactError::DigestMismatch
    } else {
        ArtifactError::StoreError(message)
    }
}

fn store_error(error: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::StoreError(error.to_string())
}

#[derive(Debug)]
pub enum ArtifactError {
    InvalidDigest,
    NotFound,
    DigestMismatch,
    UnsafeMaterializationRoot,
    MaterializationChanged,
    StoreError(String),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDigest => write!(f, "artifact digest format is invalid"),
            Self::NotFound => write!(f, "artifact not found in store"),
            Self::DigestMismatch => write!(f, "artifact content digest mismatch"),
            Self::UnsafeMaterializationRoot => write!(f, "artifact runtime root is unsafe"),
            Self::MaterializationChanged => write!(f, "artifact materialization changed"),
            Self::StoreError(message) => write!(f, "artifact store error: {message}"),
        }
    }
}
