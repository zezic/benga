//! Signed releases, bounded downloads and a rollback-capable directory swap.
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const FEED: &str = "https://benga.zezic.ru/downloads/latest.json";
const GITHUB_FEED: &str = "https://github.com/zezic/benga/releases/latest/download/latest.json";
const MAX_MANIFEST: u64 = 128 * 1024;
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
const WINDOWS_SETUP_FILENAME: &str = "benga-setup.exe";

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to a string succeeds");
    }
    hex
}

#[derive(Clone, Debug, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Debug, Deserialize)]
struct PlatformRelease {
    #[serde(flatten)]
    archive: Artifact,
    installer: Option<Artifact>,
}
#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    tag: String,
    platforms: BTreeMap<String, PlatformRelease>,
}
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub artifact: Artifact,
    pub installer: Option<Artifact>,
}

pub fn configured() -> bool {
    option_env!("BENGA_UPDATE_PUBLIC_KEY").is_some_and(|key| !key.is_empty())
}

fn platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(90)))
        .build()
        .into()
}

fn get(url: &str, limit: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(url.starts_with("https://"), "Updates require HTTPS");
    Ok(agent()
        .get(url)
        // Feed outages should quickly fall through to the other source. Archives
        // retain the agent's longer timeout because their downloads are much larger.
        .config()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .call()?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()?)
}

fn validate_artifact(artifact: &Artifact) -> anyhow::Result<()> {
    ensure!(
        artifact.size > 0 && artifact.size <= MAX_ARCHIVE,
        "Invalid update size"
    );
    ensure!(
        artifact.sha256.len() == 64 && artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid update checksum"
    );
    Ok(())
}

fn verify_manifest(
    bytes: &[u8],
    signature: &str,
    public_key: &str,
    current: &str,
    target: &str,
) -> anyhow::Result<Option<Release>> {
    let key: [u8; 32] = STANDARD
        .decode(public_key.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid update public key"))?;
    let signature = Signature::from_slice(&STANDARD.decode(signature.trim())?)?;
    VerifyingKey::from_bytes(&key)?
        .verify_strict(bytes, &signature)
        .context("Update signature is invalid")?;
    let manifest: Manifest = serde_json::from_slice(bytes)?;
    let version = semver::Version::parse(&manifest.version)?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "Updates require a stable release version"
    );
    ensure!(
        manifest.tag == format!("v{version}"),
        "Update tag and version disagree"
    );
    if version <= semver::Version::parse(current)? {
        return Ok(None);
    }
    let Some(package) = manifest.platforms.get(target) else {
        return Ok(None);
    };
    validate_artifact(&package.archive)?;
    let base = format!(
        "https://github.com/zezic/benga/releases/download/{}/Benga-v{}-{}",
        manifest.tag, version, target
    );
    ensure!(
        package.archive.url.starts_with(&format!("{base}.")),
        "Unexpected release URL"
    );
    if let Some(installer) = &package.installer {
        validate_artifact(installer)?;
        ensure!(
            target == "windows-x86_64" && installer.url == format!("{base}-setup.exe"),
            "Unexpected Windows Setup URL"
        );
    }
    Ok(Some(Release {
        version: manifest.version,
        artifact: package.archive.clone(),
        installer: package.installer.clone(),
    }))
}

fn check_source(
    feed: &str,
    key: &str,
    current: &str,
    target: &str,
    fetch: &mut impl FnMut(&str, u64) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Option<Release>> {
    let bytes = fetch(feed, MAX_MANIFEST)?;
    let signature = String::from_utf8(fetch(&format!("{feed}.sig"), 1024)?)?;
    match verify_manifest(&bytes, &signature, key, current, target) {
        Ok(result) => Ok(result),
        // A source may switch releases between the manifest and signature requests.
        Err(_) => {
            let bytes = fetch(feed, MAX_MANIFEST)?;
            let signature = String::from_utf8(fetch(&format!("{feed}.sig"), 1024)?)?;
            verify_manifest(&bytes, &signature, key, current, target)
        }
    }
}

fn check_with(
    key: &str,
    current: &str,
    target: &str,
    mut fetch: impl FnMut(&str, u64) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Option<Release>> {
    match check_source(FEED, key, current, target, &mut fetch) {
        Ok(Some(release)) => Ok(Some(release)),
        // A mirror can be reachable but stale while its polling service is down.
        Ok(None) => match check_source(GITHUB_FEED, key, current, target, &mut fetch) {
            Ok(result) => Ok(result),
            Err(error) => {
                log::debug!("GitHub update check failed after a valid mirror check: {error:#}");
                Ok(None)
            }
        },
        Err(mirror_error) => check_source(GITHUB_FEED, key, current, target, &mut fetch)
            .with_context(|| {
                format!(
                    "Mirror update check failed ({mirror_error:#}); GitHub fallback also failed"
                )
            }),
    }
}

pub fn check() -> anyhow::Result<Option<Release>> {
    let key = option_env!("BENGA_UPDATE_PUBLIC_KEY")
        .filter(|s| !s.is_empty())
        .context("This build has no update verification key")?;
    check_with(key, env!("CARGO_PKG_VERSION"), &platform(), get)
}

/// Returns only layouts entirely owned by the app; package-manager installs use their manager.
fn install_root(executable: &Path) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let root = executable
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .context("Run the installed Benga.app to update")?;
        ensure!(
            root.file_name().is_some_and(|n| n == "Benga.app"),
            "Run the installed Benga.app to update"
        );
        return Ok(root.to_owned());
    }
    portable_install_root(executable)
}

fn portable_install_root(executable: &Path) -> anyhow::Result<PathBuf> {
    let root = executable.parent().context("App directory is missing")?;
    ensure!(
        root.join(".benga-portable").is_file(),
        "Use the package manager or installer to update this installation"
    );
    Ok(root.to_owned())
}

#[cfg(windows)]
fn registered_windows_setup(target: &Path) -> bool {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
    };

    // The uninstall key is derived from the AppId in packaging/windows.iss.
    let uninstall_key: Vec<u16> =
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{29AA6F93-C24A-4A0C-A631-1645D7460F6B}_is1"
            .encode_utf16()
            .chain([0])
            .collect();
    let value: Vec<u16> = "InstallLocation\0".encode_utf16().collect();
    let mut location = vec![0u16; 32768];
    let mut bytes = (location.len() * size_of::<u16>()) as u32;
    // The registry call writes at most `bytes` bytes to `location`.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            uninstall_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            location.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if result != ERROR_SUCCESS {
        return false;
    }
    let location = &location[..bytes as usize / size_of::<u16>()];
    let Some(location) = location.strip_suffix(&[0]) else {
        return false;
    };
    let path = PathBuf::from(OsString::from_wide(location));
    path.is_absolute() && path.canonicalize().is_ok_and(|path| path == target)
}

#[cfg(not(windows))]
fn registered_windows_setup(_: &Path) -> bool {
    false
}

fn executable_in(root: &Path) -> PathBuf {
    root.join(if cfg!(target_os = "macos") {
        "Contents/MacOS/benga"
    } else if cfg!(windows) {
        "benga.exe"
    } else {
        "benga"
    })
}

pub struct Prepared {
    // Kept alive until the helper takes ownership of the staged files.
    directory: tempfile::TempDir,
    target: PathBuf,
    payload: PreparedPayload,
}

enum PreparedPayload {
    Archive(PathBuf),
    Setup { sha256: String },
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir))
}

fn extract(bytes: &[u8], destination: &Path, zipped: bool) -> anyhow::Result<()> {
    let mut total = 0u64;
    if zipped {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let path = entry.enclosed_name().context("Unsafe ZIP path")?;
            ensure!(safe_relative(&path), "Unsafe ZIP path");
            ensure!(
                entry.unix_mode().map_or(true, |m| m & 0o170000 != 0o120000),
                "Symlinks are not allowed in updates"
            );
            total = total
                .checked_add(entry.size())
                .context("Archive size overflow")?;
            ensure!(total <= MAX_UNPACKED, "Extracted update is too large");
            let output = destination.join(path);
            if entry.is_dir() {
                fs::create_dir_all(output)?;
            } else {
                fs::create_dir_all(output.parent().unwrap())?;
                let mut file = fs::File::create(&output)?;
                std::io::copy(&mut entry, &mut file)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &output,
                        fs::Permissions::from_mode(entry.unix_mode().unwrap_or(0o644) & 0o777),
                    )?;
                }
            }
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            ensure!(safe_relative(&path), "Unsafe tar path");
            ensure!(
                entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
                "Links and special files are not allowed in updates"
            );
            total = total
                .checked_add(entry.size())
                .context("Archive size overflow")?;
            ensure!(total <= MAX_UNPACKED, "Extracted update is too large");
            ensure!(entry.unpack_in(destination)?, "Unsafe tar entry");
        }
    }
    Ok(())
}

fn read_update(
    reader: impl Read,
    size: u64,
    mut progress: impl FnMut(u8),
) -> anyhow::Result<Vec<u8>> {
    let mut reader = reader.take(size + 1);
    let mut bytes = Vec::new();
    let mut buffer = [0; 64 * 1024];
    let mut last_percent = 0;
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        // Report completion only after EOF and the size check.
        let percent = (100 * bytes.len() as u64 / size).min(99) as u8;
        if percent > last_percent {
            progress(percent);
            last_percent = percent;
        }
    }
    ensure!(
        bytes.len() as u64 == size,
        "Downloaded update has the wrong size"
    );
    progress(100);
    Ok(bytes)
}

pub fn prepare(release: &Release, progress: impl FnMut(u8)) -> anyhow::Result<Prepared> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let setup_root = executable
        .parent()
        .filter(|path| registered_windows_setup(path));
    let (target, artifact) = match setup_root {
        Some(root) => (
            root.to_owned(),
            release
                .installer
                .as_ref()
                .context("Download the latest Windows Setup installer to update this copy")?,
        ),
        None => (install_root(&executable)?, &release.artifact),
    };
    ensure!(
        !receipt_path(&target)?.exists(),
        "A previous update still needs cleanup; reopen Benga before updating again"
    );
    let parent = target.parent().context("Installation has no parent")?;
    let directory = tempfile::Builder::new()
        .prefix(".benga-update-")
        .tempdir_in(parent)
        .context("Installation is read-only; move Benga to a writable folder to update")?;
    let mut response = agent().get(&artifact.url).call()?;
    let bytes = read_update(response.body_mut().as_reader(), artifact.size, progress)?;
    let sha256 = artifact.sha256.to_ascii_lowercase();
    ensure!(
        sha256_hex(&bytes) == sha256,
        "Update checksum does not match"
    );
    if setup_root.is_some() {
        fs::write(directory.path().join(WINDOWS_SETUP_FILENAME), bytes)?;
        return Ok(Prepared {
            directory,
            target,
            payload: PreparedPayload::Setup { sha256 },
        });
    }
    extract(&bytes, directory.path(), !cfg!(target_os = "linux"))?;
    let staged = if cfg!(target_os = "macos") {
        directory.path().join("Benga.app")
    } else {
        directory.path().join("Benga")
    };
    ensure!(
        executable_in(&staged).is_file(),
        "Update is missing the executable"
    );
    ensure!(
        install_root(&executable_in(&staged))? == staged,
        "Invalid update layout"
    );
    Ok(Prepared {
        directory,
        target,
        payload: PreparedPayload::Archive(staged),
    })
}

impl Prepared {
    pub fn is_setup(&self) -> bool {
        matches!(self.payload, PreparedPayload::Setup { .. })
    }

    /// Spawn a separate copy so Windows never needs to move its running executable.
    pub fn launch(self, restart: bool) -> anyhow::Result<()> {
        let helper = self.directory.path().join(if cfg!(windows) {
            "update-helper.exe"
        } else {
            "update-helper"
        });
        fs::copy(std::env::current_exe()?, &helper)?;
        let mut command = Command::new(helper);
        command.current_dir(self.directory.path());
        match &self.payload {
            PreparedPayload::Archive(staged) => {
                command.arg("--apply-update").arg(&self.target).arg(staged);
            }
            PreparedPayload::Setup { sha256 } => {
                command.arg("--apply-setup").arg(&self.target).arg(sha256);
            }
        }
        let mut child = command
            .arg(std::process::id().to_string())
            .arg(if restart { "restart" } else { "quit" })
            .spawn()
            .context("Could not start the update helper")?;
        if self.is_setup() {
            let receipt = CleanupReceipt {
                backup: None,
                staging: self.directory.path().to_owned(),
                helper_pid: child.id(),
            };
            if let Err(error) = reserve_update(&self.target, &receipt) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        let _ = self.directory.keep();
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CleanupReceipt {
    #[serde(skip_serializing_if = "Option::is_none")]
    backup: Option<PathBuf>,
    staging: PathBuf,
    helper_pid: u32,
}

fn receipt_path(target: &Path) -> anyhow::Result<PathBuf> {
    let parent = target.parent().context("Missing install parent")?;
    let hash = sha256_hex(target.as_os_str().as_encoded_bytes());
    // Keep mutable updater state outside macOS's signed application bundle.
    Ok(parent.join(format!(".benga-update-state-{}.json", &hash[..16])))
}

fn reserve_update(target: &Path, receipt: &CleanupReceipt) -> anyhow::Result<()> {
    let path = receipt_path(target)?;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing install parent")?)?;
    serde_json::to_writer(&mut file, receipt)?;
    file.persist_noclobber(path)
        .context("Another update is pending cleanup")?;
    Ok(())
}

fn read_receipt(path: &Path) -> anyhow::Result<Option<CleanupReceipt>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(bytes.len() <= 16 * 1024, "Invalid update cleanup receipt");
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn validate_cleanup_path(path: &Path, parent: &Path, prefix: &str) -> anyhow::Result<()> {
    ensure!(
        path.parent() == Some(parent),
        "Update cleanup must stay beside this installation"
    );
    ensure!(
        path.file_name().is_some_and(|name| {
            name.to_str().is_some_and(|name| {
                name.strip_prefix(prefix).is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_alphanumeric())
                })
            })
        }),
        "Invalid update cleanup directory"
    );
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Update cleanup cannot follow links"
            );
            ensure!(
                path.canonicalize()?.parent() == Some(parent),
                "Update cleanup escaped install parent"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn cleanup(target: &Path) -> anyhow::Result<bool> {
    let receipt_file = receipt_path(target)?;
    let Some(receipt) = read_receipt(&receipt_file)? else {
        return Ok(true);
    };
    let parent = target.parent().context("Missing install parent")?;
    if let Some(backup) = &receipt.backup {
        validate_cleanup_path(backup, parent, ".benga-backup-")?;
    }
    validate_cleanup_path(&receipt.staging, parent, ".benga-update-")?;
    ensure!(receipt.helper_pid > 1, "Invalid update helper PID");
    if process_alive(receipt.helper_pid)? {
        return Ok(false);
    }
    for path in receipt
        .backup
        .iter()
        .chain(std::iter::once(&receipt.staging))
    {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::remove_file(receipt_file)?;
    Ok(true)
}

/// Call on a background thread after graphics startup. Retain the previous app
/// until startup succeeds and its launching helper has exited.
pub fn cleanup_after_start() {
    let target = std::env::current_exe()
        .context("Find the running app")
        .and_then(|path| {
            let executable = path.canonicalize()?;
            if let Some(parent) = executable
                .parent()
                .filter(|path| registered_windows_setup(path))
            {
                return Ok(parent.to_owned());
            }
            install_root(&executable)
        });
    let target = match target {
        Ok(target) => target,
        Err(error) => {
            log::debug!("Update cleanup: {error:#}");
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match cleanup(&target) {
            Ok(true) => return,
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(false) => {
                log::debug!("Update helper is still running; retry cleanup at the next startup");
                return;
            }
            Err(error) => {
                log::debug!("Update cleanup: {error:#}");
                return;
            }
        }
    }
}

fn replace_installation(
    target: &Path,
    staged: &Path,
    backup: &Path,
    after_swap: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    ensure!(!backup.exists(), "A previous update backup needs attention");
    fs::rename(target, backup).context("Could not move the previous installation")?;
    if let Err(error) = fs::rename(staged, target) {
        fs::rename(backup, target).context("Update failed and rollback needs manual recovery")?;
        bail!("Could not install update; previous version restored: {error}");
    }
    if let Err(error) = after_swap() {
        fs::rename(target, staged).context("Could not move the failed update out of the way")?;
        fs::rename(backup, target).context("Update failed and rollback needs manual recovery")?;
        bail!("Could not finish update; previous version restored: {error}");
    }
    Ok(())
}

/// Called before graphics, settings and resource loading in the helper process.
pub fn run_helper() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    let setup = cfg!(windows) && args.get(1).is_some_and(|s| s == "--apply-setup");
    if !setup && args.get(1).is_none_or(|s| s != "--apply-update") {
        return Ok(false);
    }
    ensure!(args.len() == 6, "Invalid update helper arguments");
    let target = PathBuf::from(&args[2]).canonicalize()?;
    let pid: u32 = args[4].to_str().context("Invalid PID")?.parse()?;
    ensure!(pid > 1 && pid != std::process::id(), "Invalid parent PID");
    let restart = match args[5].to_str() {
        Some("restart") => true,
        Some("quit") => false,
        _ => bail!("Invalid update restart mode"),
    };
    #[cfg(windows)]
    if setup {
        let sha256 = args[3].to_str().context("Invalid Windows Setup checksum")?;
        run_windows_setup(&target, sha256, pid, restart)?;
        return Ok(true);
    }
    let staged = PathBuf::from(&args[3]).canonicalize()?;
    let staging = staged.parent().context("Missing staging directory")?;
    let parent = target.parent().context("Missing install parent")?;
    ensure!(
        staging.parent() == Some(parent)
            && staging
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".benga-update-")),
        "Invalid staging directory"
    );
    ensure!(
        install_root(&executable_in(&target))? == target
            && install_root(&executable_in(&staged))? == staged
            && executable_in(&staged).is_file(),
        "Invalid update layout"
    );
    wait_for_exit(pid)?;
    let backup = parent.join(format!(".benga-backup-{pid}"));
    let receipt = CleanupReceipt {
        backup: Some(backup.clone()),
        staging: staging.to_owned(),
        helper_pid: std::process::id(),
    };
    // Reserving the receipt before any rename also excludes concurrent helpers.
    reserve_update(&target, &receipt)?;
    replace_installation(&target, &staged, &backup, || {
        if restart {
            if cfg!(target_os = "macos") {
                let status = Command::new("/usr/bin/open")
                    .arg("-n")
                    .arg(&target)
                    .current_dir(parent)
                    .status()?;
                ensure!(status.success(), "macOS could not open the updated app");
            } else {
                Command::new(executable_in(&target))
                    .current_dir(parent)
                    .spawn()
                    .context("Could not restart the updated app")?;
            }
        }
        Ok(())
    })?;
    Ok(true)
}

#[cfg(windows)]
fn run_windows_setup(
    target: &Path,
    sha256: &str,
    pid: u32,
    interactive: bool,
) -> anyhow::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    wait_for_exit(pid)?;
    ensure!(
        registered_windows_setup(target),
        "Benga's Windows Setup registration changed"
    );
    let helper = std::env::current_exe()?.canonicalize()?;
    let staging = helper
        .parent()
        .context("Missing Windows Setup staging directory")?;
    let parent = target.parent().context("Missing install parent")?;
    validate_cleanup_path(staging, parent, ".benga-update-")?;
    let receipt = read_receipt(&receipt_path(target)?)?.context("Missing Windows Setup receipt")?;
    ensure!(
        receipt.backup.is_none()
            && receipt.staging == staging
            && receipt.helper_pid == std::process::id(),
        "Invalid Windows Setup receipt"
    );

    let installer = staging.join(WINDOWS_SETUP_FILENAME);
    // Deny writes and replacement between verification and process creation.
    const FILE_SHARE_READ: u32 = 1;
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&installer)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_ARCHIVE + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_ARCHIVE && sha256_hex(&bytes) == sha256,
        "Windows Setup checksum does not match"
    );
    drop(bytes);
    let mut command = Command::new(&installer);
    command
        .current_dir(staging)
        .args(["/NORESTART", "/NORESTARTAPPLICATIONS"]);
    if !interactive {
        command.args(["/SILENT", "/SP-"]);
    }
    let mut child = command.spawn().context("Could not start Windows Setup")?;
    drop(file);
    let status = child.wait()?;
    let _ = fs::remove_file(&installer);
    ensure!(status.success(), "Windows Setup exited with {status}");
    Ok(())
}

fn process_alive(pid: u32) -> anyhow::Result<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Ok(Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}")])
            .creation_flags(0x08000000)
            .status()?
            .success())
    }
    #[cfg(unix)]
    {
        Ok(Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?
            .success())
    }
}

fn wait_for_exit(pid: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let status = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("Wait-Process -Id {pid} -Timeout 30 -ErrorAction SilentlyContinue; if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}; exit 0")])
            .creation_flags(0x08000000)
            .status()?;
        ensure!(status.success(), "Previous Benga process did not exit");
        Ok(())
    }
    #[cfg(unix)]
    {
        for _ in 0..150 {
            let alive = process_alive(pid)?;
            if !alive {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        bail!("Previous Benga process did not exit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;

    #[test]
    fn update_progress_completes_only_after_the_signed_size_matches() -> anyhow::Result<()> {
        let archive = vec![0xab; 256 * 1024];
        let size = archive.len() as u64;
        let mut updates = Vec::new();
        let bytes = read_update(archive.as_slice(), size, |percent| updates.push(percent))?;
        assert_eq!(bytes, archive);
        assert_eq!(updates, [25, 50, 75, 99, 100]);

        updates.clear();
        assert!(read_update(&archive[..archive.len() - 1], size, |percent| {
            updates.push(percent);
        })
        .is_err());
        assert!(!updates.contains(&100));

        updates.clear();
        let mut oversized = std::io::repeat(0).take(size + 5);
        assert!(read_update(&mut oversized, size, |percent| updates.push(percent)).is_err());
        assert!(!updates.contains(&100));
        assert_eq!(oversized.limit(), 4);
        Ok(())
    }

    #[test]
    fn cleanup_receipt_path_matches_existing_installations() -> anyhow::Result<()> {
        assert_eq!(
            receipt_path(Path::new("portable/Benga"))?,
            Path::new("portable/.benga-update-state-a391dafe42c64c05.json")
        );
        Ok(())
    }

    fn signed_fixture(version: &str) -> (Vec<u8>, Vec<u8>, String) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let bytes = format!(
            "{{\"version\":\"{version}\",\"tag\":\"v{version}\",\"platforms\":{{\"macos-aarch64\":{{\"url\":\"https://github.com/zezic/benga/releases/download/v{version}/Benga-v{version}-macos-aarch64.zip\",\"sha256\":\"{}\",\"size\":42}}}}}}\n",
            "a".repeat(64)
        )
        .into_bytes();
        let signature = STANDARD
            .encode(signing.sign(&bytes).to_bytes())
            .into_bytes();
        let key = STANDARD.encode(signing.verifying_key().to_bytes());
        (bytes, signature, key)
    }

    #[test]
    fn github_fallback_recovers_from_unavailable_or_untrusted_mirrors() -> anyhow::Result<()> {
        let (bytes, signature, key) = signed_fixture("1.2.0");
        for mirror_body in [None, Some(b"<html>Maintenance</html>".to_vec()), {
            let mut tampered = bytes.clone();
            tampered.push(b' ');
            Some(tampered)
        }] {
            let mut requests = Vec::new();
            let release = check_with(&key, "1.0.0", "macos-aarch64", |url, limit| {
                requests.push(url.to_owned());
                assert_eq!(
                    limit,
                    if url.ends_with(".sig") {
                        1024
                    } else {
                        MAX_MANIFEST
                    }
                );
                match url {
                    FEED => mirror_body.clone().context("Mirror is offline"),
                    GITHUB_FEED => Ok(bytes.clone()),
                    _ if url == format!("{FEED}.sig") || url == format!("{GITHUB_FEED}.sig") => {
                        Ok(signature.clone())
                    }
                    _ => bail!("Unexpected URL: {url}"),
                }
            })?
            .context("Expected the signed GitHub release")?;
            assert_eq!(release.version, "1.2.0");
            assert!(requests.iter().any(|url| url == GITHUB_FEED));
        }
        Ok(())
    }

    #[test]
    fn valid_mirror_updates_avoid_github_and_retry_publication_races() -> anyhow::Result<()> {
        let (bytes, signature, key) = signed_fixture("1.2.0");
        let (older, _, _) = signed_fixture("1.1.0");
        for race in [false, true] {
            let mut manifests = 0;
            let release = check_with(&key, "1.0.0", "macos-aarch64", |url, _| {
                if url == FEED {
                    manifests += 1;
                    Ok(if race && manifests == 1 {
                        older.clone()
                    } else {
                        bytes.clone()
                    })
                } else if url == format!("{FEED}.sig") {
                    Ok(signature.clone())
                } else {
                    bail!("A valid mirror must not need GitHub")
                }
            })?
            .context("Expected a mirror release")?;
            assert_eq!(release.version, "1.2.0");
            assert_eq!(manifests, if race { 2 } else { 1 });
        }
        Ok(())
    }

    #[test]
    fn stale_mirror_cannot_hide_a_new_github_release() -> anyhow::Result<()> {
        let (mirror, mirror_signature, key) = signed_fixture("1.0.0");
        let (github, github_signature, _) = signed_fixture("1.2.0");
        let release = check_with(&key, "1.0.0", "macos-aarch64", |url, _| match url {
            FEED => Ok(mirror.clone()),
            GITHUB_FEED => Ok(github.clone()),
            _ if url == format!("{FEED}.sig") => Ok(mirror_signature.clone()),
            _ if url == format!("{GITHUB_FEED}.sig") => Ok(github_signature.clone()),
            _ => bail!("Unexpected URL"),
        })?
        .context("Expected a newer GitHub release")?;
        assert_eq!(release.version, "1.2.0");
        // A verified no-update answer remains usable when GitHub is unavailable.
        assert!(check_with(&key, "1.0.0", "macos-aarch64", |url, _| {
            match url {
                FEED => Ok(mirror.clone()),
                _ if url == format!("{FEED}.sig") => Ok(mirror_signature.clone()),
                _ => bail!("GitHub is offline"),
            }
        })?
        .is_none());
        Ok(())
    }

    #[test]
    fn github_fallback_cannot_bypass_signatures_or_stable_version_checks() -> anyhow::Result<()> {
        for (version, tampered, accepted) in [
            ("1.2.0", true, false),
            ("1.2.0-beta.1", false, false),
            ("1.2.0+build", false, false),
            ("0.9.0", false, true),
        ] {
            let (mut bytes, signature, key) = signed_fixture(version);
            if tampered {
                bytes.push(b' ');
            }
            let result = check_with(&key, "1.0.0", "macos-aarch64", |url, _| match url {
                FEED => bail!("Mirror is offline"),
                GITHUB_FEED => Ok(bytes.clone()),
                _ if url == format!("{GITHUB_FEED}.sig") => Ok(signature.clone()),
                _ => bail!("Unexpected URL"),
            });
            if accepted {
                assert!(result?.is_none(), "Downgrades must never produce an update");
            } else {
                assert!(
                    result.is_err(),
                    "The GitHub feed must also be authenticated and stable"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn embedded_models_need_only_the_portable_marker_for_self_updates() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("benga");
        fs::write(&executable, b"standalone binary with embedded models")?;
        assert!(portable_install_root(&executable).is_err());
        fs::write(directory.path().join(".benga-portable"), b"")?;
        assert_eq!(portable_install_root(&executable)?, directory.path());
        assert!(!directory.path().join("assets/models").exists());
        Ok(())
    }

    #[test]
    fn signature_tampering_and_downgrades_are_rejected() -> anyhow::Result<()> {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let key = STANDARD.encode(signing.verifying_key().to_bytes());
        let data = br#"{"version":"1.2.0","tag":"v1.2.0","platforms":{"macos-aarch64":{"url":"https://github.com/zezic/benga/releases/download/v1.2.0/Benga-v1.2.0-macos-aarch64.zip","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":42}}}"#;
        let sig = STANDARD.encode(signing.sign(data).to_bytes());
        assert!(verify_manifest(data, &sig, &key, "1.0.0", "macos-aarch64")?.is_some());
        assert!(verify_manifest(data, &sig, &key, "1.0.0", "macos-x86_64")?.is_none());
        assert!(verify_manifest(data, &sig, &key, "1.2.0", "macos-aarch64")?.is_none());
        assert!(verify_manifest(data, &sig, &key, "2.0.0", "macos-aarch64")?.is_none());
        let mut tampered = data.to_vec();
        tampered[12] = b'9';
        assert!(verify_manifest(&tampered, &sig, &key, "1.0.0", "macos-aarch64").is_err());
        assert!(verify_manifest(
            data,
            &sig,
            &STANDARD.encode([9; 32]),
            "1.0.0",
            "macos-aarch64"
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn windows_setup_requires_a_signed_asset_for_the_same_release() -> anyhow::Result<()> {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let key = STANDARD.encode(signing.verifying_key().to_bytes());
        let target = "windows-x86_64";
        let base =
            "https://github.com/zezic/benga/releases/download/v1.2.0/Benga-v1.2.0-windows-x86_64";
        let mut manifest = serde_json::json!({
            "version": "1.2.0",
            "tag": "v1.2.0",
            "platforms": {
                target: {"url": format!("{base}.zip"), "sha256": "a".repeat(64), "size": 42}
            }
        });
        let verify = |manifest: &serde_json::Value| {
            let payload = serde_json::to_vec(manifest)?;
            let signature = STANDARD.encode(signing.sign(&payload).to_bytes());
            verify_manifest(&payload, &signature, &key, "1.0.0", target)?
                .context("Missing Windows release")
        };
        assert!(verify(&manifest)?.installer.is_none());
        let url = format!("{base}-setup.exe");
        manifest["platforms"][target]["installer"] =
            serde_json::json!({"url": url, "sha256": "b".repeat(64), "size": 123});
        let release = verify(&manifest)?;
        assert_eq!(release.artifact.url, format!("{base}.zip"));
        let installer = release.installer.context("Missing installer")?;
        assert_eq!(installer.url, url);
        assert_eq!(installer.sha256, "b".repeat(64));
        assert_eq!(installer.size, 123);

        manifest["platforms"][target]["installer"]["url"] =
            serde_json::json!(format!("{base}-setup.exe?other"));
        assert!(verify(&manifest).is_err());
        manifest["platforms"][target]["installer"]["url"] = serde_json::json!(url);
        manifest["platforms"][target]["installer"]["size"] = serde_json::json!(MAX_ARCHIVE + 1);
        assert!(verify(&manifest).is_err());
        Ok(())
    }

    #[test]
    fn archive_links_and_traversal_are_rejected() -> anyhow::Result<()> {
        assert!(!safe_relative(Path::new("../bad")));
        assert!(!safe_relative(Path::new("/bad")));
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        tar.append_link(&mut header, "Benga/evil", "/tmp/evil")?;
        let bytes = tar.into_inner()?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&bytes)?;
        let dir = tempfile::tempdir()?;
        assert!(extract(&encoder.finish()?, dir.path(), false).is_err());
        Ok(())
    }

    #[test]
    fn zip_roundtrip_preserves_binary_and_rejects_traversal() -> anyhow::Result<()> {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive.start_file(
            "Benga/benga",
            zip::write::SimpleFileOptions::default().unix_permissions(0o755),
        )?;
        archive.write_all(b"binary payload")?;
        let bytes = archive.finish()?.into_inner();
        let directory = tempfile::tempdir()?;
        extract(&bytes, directory.path(), true)?;
        assert_eq!(
            fs::read(directory.path().join("Benga/benga"))?,
            b"binary payload"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(directory.path().join("Benga/benga"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }

        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive.start_file("../outside", zip::write::SimpleFileOptions::default())?;
        archive.write_all(b"must not escape")?;
        let bytes = archive.finish()?.into_inner();
        assert!(extract(&bytes, directory.path(), true).is_err());
        Ok(())
    }

    #[test]
    fn tar_parent_traversal_cannot_escape_staging() -> anyhow::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        // Construct an adversarial header directly; tar's writer rejects this path.
        let name = b"../outside";
        header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        header.set_cksum();
        let mut tar = header.as_bytes().to_vec();
        tar.push(b'x');
        tar.resize(512 * 4, 0);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar)?;
        let parent = tempfile::tempdir()?;
        let staging = parent.path().join("staging");
        fs::create_dir(&staging)?;
        assert!(extract(&encoder.finish()?, &staging, false).is_err());
        assert!(!parent.path().join("outside").exists());
        Ok(())
    }

    fn installation_fixture(parent: &Path) -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
        let target = parent.join("Benga");
        let staged = parent.join("staged");
        let backup = parent.join(".benga-backup-1234");
        fs::create_dir(&target)?;
        fs::create_dir(&staged)?;
        fs::write(target.join("version"), b"old")?;
        fs::write(staged.join("version"), b"new")?;
        Ok((target, staged, backup))
    }

    #[test]
    fn directory_swap_rolls_back_failed_move_and_restart() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let (target, staged, backup) = installation_fixture(directory.path())?;
        let missing = directory.path().join("missing");
        assert!(replace_installation(&target, &missing, &backup, || Ok(())).is_err());
        assert_eq!(fs::read(target.join("version"))?, b"old");
        assert!(!backup.exists());

        assert!(
            replace_installation(&target, &staged, &backup, || bail!("restart failed")).is_err()
        );
        assert_eq!(fs::read(target.join("version"))?, b"old");
        assert_eq!(fs::read(staged.join("version"))?, b"new");
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn successful_swap_keeps_backup_until_confirmed_startup() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let (target, staged, backup) = installation_fixture(directory.path())?;
        replace_installation(&target, &staged, &backup, || Ok(()))?;
        assert_eq!(fs::read(target.join("version"))?, b"new");
        assert_eq!(fs::read(backup.join("version"))?, b"old");
        assert!(!staged.exists());
        Ok(())
    }

    #[test]
    fn cleanup_rejects_paths_outside_sibling_update_directories() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let parent = directory.path().canonicalize()?;
        assert!(
            validate_cleanup_path(&parent.join("Documents"), &parent, ".benga-backup-").is_err()
        );
        assert!(validate_cleanup_path(
            &parent.join("../.benga-backup-123"),
            &parent,
            ".benga-backup-"
        )
        .is_err());
        assert!(validate_cleanup_path(
            &parent.join(".benga-backup-123"),
            &parent,
            ".benga-backup-"
        )
        .is_ok());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir()?;
            let link = parent.join(".benga-backup-456");
            std::os::unix::fs::symlink(outside.path(), &link)?;
            assert!(validate_cleanup_path(&link, &parent, ".benga-backup-").is_err());
        }
        Ok(())
    }

    #[test]
    fn cleanup_waits_for_helper_and_removes_only_recorded_directories() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let parent = directory.path().canonicalize()?;
        let (target, _, backup) = installation_fixture(&parent)?;
        fs::create_dir(&backup)?;
        let staging = parent.join(".benga-update-abc123");
        let unrelated = parent.join(".benga-update-unrelated");
        fs::create_dir(&staging)?;
        fs::create_dir(&unrelated)?;
        let receipt_file = receipt_path(&target)?;
        let mut receipt = CleanupReceipt {
            backup: Some(backup.clone()),
            staging: staging.clone(),
            helper_pid: std::process::id(),
        };
        fs::write(&receipt_file, serde_json::to_vec(&receipt)?)?;
        assert!(!cleanup(&target)?);
        assert!(backup.exists() && staging.exists());

        #[cfg(unix)]
        let mut exited = Command::new("/usr/bin/true").spawn()?;
        #[cfg(windows)]
        let mut exited = Command::new("cmd.exe").args(["/C", "exit", "0"]).spawn()?;
        receipt.helper_pid = exited.id();
        exited.wait()?;
        fs::write(&receipt_file, serde_json::to_vec(&receipt)?)?;
        assert!(cleanup(&target)?);
        assert!(!backup.exists() && !staging.exists() && !receipt_file.exists());
        assert!(target.exists() && unrelated.exists());

        fs::create_dir(&staging)?;
        receipt.backup = None;
        reserve_update(&target, &receipt)?;
        assert!(reserve_update(&target, &receipt).is_err());
        assert!(cleanup(&target)?);
        assert!(!staging.exists() && !receipt_file.exists());
        assert!(target.exists() && unrelated.exists());
        Ok(())
    }
}
