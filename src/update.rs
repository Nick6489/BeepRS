use freshen::inno::InnoPolicy;
use freshen::{
    Cancellation, Error, Event, HttpTransport, InstallLayout, InstallPolicy, InstallState,
    PackageKind, ReleaseSource, Transport, TrustStore, Updater,
};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use url::Url;

pub const PRODUCT: &str = "beeprs";
pub const CHANNEL: &str = "stable";
/// Distribution target of this build. Pack the release with this exact string.
pub const TARGET: &str = env!("BEEPRS_TARGET");
pub const EXECUTABLE: &str = "beeprs.exe";

/// These paths must remain in every package. Freshen may also install new signed
/// files strictly under `sounds/`. Saves and `update-source.json` are absent
/// on purpose.
pub const OWNED_FILES: &[&str] = &[
    EXECUTABLE,
    "sounds/beep.opus",
    "sounds/bed.opus",
    "sounds/intro.opus",
    "sounds/die1.opus",
    "sounds/die2.opus",
    "sounds/die3.opus",
    "sounds/die4.opus",
    "sounds/die5.opus",
    "sounds/die6.opus",
    "sounds/die7.opus",
    "sounds/die8.opus",
    "sounds/die9.opus",
    "sounds/die10.opus",
    "sounds/die11.opus",
    "sounds/die12.opus",
    "sounds/die13.opus",
    "sounds/die14.opus",
];

/// Host-owned directory. Later releases can add sound files here without a
/// second migration. They still cannot remove an owned path or touch saves.
pub const EXPANDABLE_DIRECTORIES: &[&str] = &["sounds"];

/// Dev publisher key. The matching private key is `keys/publisher.key`, which is
/// gitignored. Sign releases with that file; do not put it in a package.
pub const PUBLISHER_KEY: [u8; 32] = [
    133, 237, 179, 216, 204, 92, 75, 26, 192, 200, 238, 178, 213, 76, 41, 67, 7, 182, 64, 213, 51,
    251, 238, 183, 184, 64, 83, 120, 224, 89, 242, 232,
];

const MANIFEST_NAME: &str = "freshen-manifest.json";
const SIGNATURE_NAME: &str = "freshen-manifest.json.sig";
/// Written by the Inno installer. The portable zip does not ship this file.
pub const DISTRIBUTION_MARKER: &str = "beeprs-distribution";
const INNO_MARKER: &str = "inno";
/// Keep in sync with AppId in installer/BeepRS.iss, including the braces.
/// The script doubles only the opening brace. This value is the expanded AppId.
pub const APP_ID: &str = "{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}";
const INNO_SETUP: &str = "setup.exe";

/// How this running copy was deployed. Taken from a local marker, never from a
/// download or from another copy's uninstall registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    Portable,
    Inno,
}

impl CopyKind {
    pub fn package_kind(self) -> PackageKind {
        match self {
            Self::Portable => PackageKind::Files,
            Self::Inno => PackageKind::Inno,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Inno => "installed",
        }
    }
}

pub fn detect_copy_kind(root: &Path) -> io::Result<CopyKind> {
    let path = root.join(DISTRIBUTION_MARKER);
    if !path.is_file() {
        return Ok(CopyKind::Portable);
    }
    let text = fs::read_to_string(&path)?;
    match text.trim() {
        INNO_MARKER => Ok(CopyKind::Inno),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown distribution in {DISTRIBUTION_MARKER}: {other}"),
        )),
    }
}

pub fn product_data_dir() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
    Ok(PathBuf::from(local).join("BeepRS"))
}

pub fn servicing_root() -> io::Result<PathBuf> {
    Ok(product_data_dir()?.join("servicing"))
}

pub fn saves_dir(root: &Path, kind: CopyKind) -> io::Result<PathBuf> {
    match kind {
        CopyKind::Portable => Ok(root.join("saves")),
        CopyKind::Inno => Ok(product_data_dir()?.join("saves")),
    }
}

fn state_root(root: &Path, kind: CopyKind) -> io::Result<PathBuf> {
    match kind {
        CopyKind::Portable => Ok(root.to_path_buf()),
        CopyKind::Inno => servicing_root(),
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum UpdateSource {
    Directory { path: PathBuf },
    Manifest { manifest: Url, signature: Url },
}

pub struct StartupReport {
    pub lines: Vec<String>,
    pub needs_attention: bool,
}

pub struct ReleaseOffer {
    pub version: String,
    pub notes: String,
    candidate: freshen::Candidate,
    loaded: LoadedSource,
}

pub fn acknowledge_installation(root: &Path, kind: CopyKind) -> freshen::Result<StartupReport> {
    let mut report = StartupReport {
        lines: Vec::new(),
        needs_attention: false,
    };
    let state = state_root(root, kind)?;
    if kind == CopyKind::Inno {
        fs::create_dir_all(&state)?;
    }
    match freshen::confirm_startup(&state) {
        Ok(true) => report
            .lines
            .push("Startup confirmed for this update.".into()),
        Ok(false) => {}
        Err(error) => {
            report.needs_attention = true;
            report
                .lines
                .push(format!("Could not confirm startup: {error}"));
        }
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        let status = freshen::installation_status(&state)?;
        let still_waiting = status
            .as_ref()
            .is_some_and(|status| status.state == InstallState::AwaitingConfirmation)
            && Instant::now() < deadline;
        if still_waiting {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        break status;
    };
    let Some(status) = status else {
        return Ok(report);
    };
    report.lines.push(format!(
        "Update record: {} {} — {}",
        status.version,
        state_label(&status.state),
        status.detail
    ));
    match status.state {
        InstallState::Committed
        | InstallState::RolledBack
        | InstallState::Prepared
        | InstallState::RebootRequired => {
            if let Some(line) = discard_when_free(&state, deadline)? {
                report.lines.push(line);
            }
        }
        InstallState::Applying
        | InstallState::AwaitingConfirmation
        | InstallState::RollingBack
        | InstallState::Failed => {
            report.needs_attention = true;
            report.lines.push(format!(
                "Quit BeepRS, then recover with: freshen-release recover {}",
                state.display()
            ));
        }
    }
    Ok(report)
}

fn discard_when_free(root: &Path, deadline: Instant) -> freshen::Result<Option<String>> {
    loop {
        match freshen::discard_finished_installation(root) {
            Ok(()) => return Ok(Some("Previous update files were cleared.".into())),
            Err(Error::Busy) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(Error::Busy) => {
                return Ok(Some(
                    "The updater is still finishing. It will be cleared on a later launch.".into(),
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn state_label(state: &InstallState) -> &'static str {
    match state {
        InstallState::Prepared => "prepared",
        InstallState::Applying => "applying",
        InstallState::AwaitingConfirmation => "awaiting confirmation",
        InstallState::Committed => "committed",
        InstallState::RollingBack => "rolling back",
        InstallState::RolledBack => "rolled back",
        InstallState::Failed => "failed",
        InstallState::RebootRequired => "reboot required",
    }
}

pub fn current_version() -> Result<semver::Version, semver::Error> {
    env!("CARGO_PKG_VERSION").parse()
}

pub fn identity_line(kind: CopyKind) -> String {
    format!(
        "This is {PRODUCT} {} for {TARGET}, channel {CHANNEL}, {}.",
        env!("CARGO_PKG_VERSION"),
        kind.label()
    )
}

pub fn missing_source_message(root: &Path) -> String {
    let path = root.join("update-source.json");
    format!(
        "No update source is configured.\r\n\
         Create {shown}.\r\n\
         That file is kept when the program and sounds are replaced.\r\n\
         \r\n\
         A folder of release files (freshen-manifest.json, freshen-manifest.json.sig, and the zip):\r\n\
         {{\"type\":\"directory\",\"path\":\"C:\\\\path\\\\to\\\\release\"}}\r\n\
         \r\n\
         Or HTTPS URLs:\r\n\
         {{\"type\":\"manifest\",\"manifest\":\"https://example/freshen-manifest.json\",\"signature\":\"https://example/freshen-manifest.json.sig\"}}",
        shown = path.display()
    )
}

pub fn find_release(
    loaded: &LoadedSource,
    kind: CopyKind,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(String),
) -> Result<Option<ReleaseOffer>, Box<dyn std::error::Error>> {
    let version = current_version()?;
    let mut reporter = Progress {
        download_marks: u64::MAX,
        progress,
    };
    let Some(candidate) = find_candidate(loaded, version, kind, cancel, &mut reporter)? else {
        return Ok(None);
    };
    let release = candidate.release();
    Ok(Some(ReleaseOffer {
        version: release.version.to_string(),
        notes: release.notes.clone(),
        candidate,
        loaded: loaded.clone(),
    }))
}

pub fn download_release(
    offer: ReleaseOffer,
    kind: CopyKind,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(String),
) -> Result<freshen::PreparedUpdate, Box<dyn std::error::Error>> {
    let version = current_version()?;
    let mut reporter = Progress {
        download_marks: u64::MAX,
        progress,
    };
    prepare_candidate(
        &offer.loaded,
        version,
        kind,
        offer.candidate,
        cancel,
        &mut reporter,
    )
}

pub fn install_release(
    root: &Path,
    kind: CopyKind,
    prepared: freshen::PreparedUpdate,
) -> Result<freshen::Handoff, Box<dyn std::error::Error>> {
    let policy = match kind {
        CopyKind::Portable => InstallPolicy::portable_expanding(
            root.to_path_buf(),
            EXECUTABLE.to_string(),
            OWNED_FILES.iter().map(|path| (*path).to_string()).collect(),
            EXPANDABLE_DIRECTORIES
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
        ),
        CopyKind::Inno => {
            let servicing = servicing_root()?;
            fs::create_dir_all(&servicing)?;
            InstallPolicy {
                root: servicing,
                layout: InstallLayout::Inno(InnoPolicy {
                    installer: INNO_SETUP.into(),
                    app_id: APP_ID.into(),
                    directory: root.to_path_buf(),
                    executable: EXECUTABLE.to_string(),
                    machine_wide: false,
                    enhanced: true,
                    timeout_secs: 1800,
                    relaunch: true,
                }),
                exit_timeout_secs: 120,
                confirmation_timeout_secs: 120,
            }
        }
    };
    Ok(prepared.arm_current_exe(policy, Vec::new())?)
}

#[derive(Clone)]
pub enum LoadedSource {
    GitHub,
    Directory(PathBuf),
    Manifest(Url, Url),
}

const GITHUB_OWNER: &str = "Nick6489";
const GITHUB_REPOSITORY: &str = "BeepRS";

pub fn load_source(root: &Path) -> Result<LoadedSource, Box<dyn std::error::Error>> {
    let path = root.join("update-source.json");
    if !path.is_file() {
        return Ok(LoadedSource::GitHub);
    }
    read_source(root)
}

fn read_source(root: &Path) -> Result<LoadedSource, Box<dyn std::error::Error>> {
    let path = root.join("update-source.json");
    let bytes = fs::read(&path)?;
    if bytes.len() > 64 * 1024 {
        return Err("update-source.json is larger than 64 KiB".into());
    }
    let source: UpdateSource = serde_json::from_slice(&bytes)?;
    Ok(match source {
        UpdateSource::Directory { path } => {
            let directory = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            if !directory.is_dir() {
                return Err(
                    format!("Update directory does not exist: {}", directory.display()).into(),
                );
            }
            LoadedSource::Directory(directory)
        }
        UpdateSource::Manifest {
            manifest,
            signature,
        } => LoadedSource::Manifest(manifest, signature),
    })
}

fn find_candidate(
    loaded: &LoadedSource,
    version: semver::Version,
    kind: CopyKind,
    cancel: &Cancellation,
    progress: &mut Progress<'_>,
) -> Result<Option<freshen::Candidate>, Box<dyn std::error::Error>> {
    let package = kind.package_kind();
    match loaded {
        LoadedSource::GitHub => {
            let updater = updater(version, HttpTransport::new(Duration::from_secs(120))?)?;
            let source = ReleaseSource::GitHub {
                owner: GITHUB_OWNER.into(),
                repository: GITHUB_REPOSITORY.into(),
            };
            Ok(
                updater.check_for_kind(&source, package, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
        LoadedSource::Directory(directory) => {
            let updater = updater(
                version,
                DirectoryTransport {
                    root: directory.clone(),
                },
            )?;
            let source = local_manifest_source()?;
            Ok(
                updater.check_for_kind(&source, package, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
        LoadedSource::Manifest(manifest, signature) => {
            let updater = updater(version, HttpTransport::new(Duration::from_secs(120))?)?;
            let source = ReleaseSource::Manifest {
                document: manifest.clone(),
                signature: signature.clone(),
            };
            Ok(
                updater.check_for_kind(&source, package, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
    }
}

fn prepare_parent(kind: CopyKind) -> io::Result<PathBuf> {
    match kind {
        CopyKind::Portable => Ok(std::env::temp_dir()),
        CopyKind::Inno => {
            let root = servicing_root()?;
            fs::create_dir_all(&root)?;
            Ok(root)
        }
    }
}

fn prepare_candidate(
    loaded: &LoadedSource,
    version: semver::Version,
    kind: CopyKind,
    candidate: freshen::Candidate,
    cancel: &Cancellation,
    progress: &mut Progress<'_>,
) -> Result<freshen::PreparedUpdate, Box<dyn std::error::Error>> {
    let temporary = prepare_parent(kind)?;
    match loaded {
        LoadedSource::GitHub => {
            let updater = updater(version, HttpTransport::new(Duration::from_secs(120))?)?;
            Ok(
                updater.prepare(candidate, &temporary, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
        LoadedSource::Directory(directory) => {
            let updater = updater(
                version,
                DirectoryTransport {
                    root: directory.clone(),
                },
            )?;
            Ok(
                updater.prepare(candidate, &temporary, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
        LoadedSource::Manifest(_, _) => {
            let updater = updater(version, HttpTransport::new(Duration::from_secs(120))?)?;
            Ok(
                updater.prepare(candidate, &temporary, cancel, &mut |event| {
                    progress.report(event)
                })?,
            )
        }
    }
}

fn local_manifest_source() -> Result<ReleaseSource, url::ParseError> {
    Ok(ReleaseSource::Manifest {
        document: Url::parse(&format!("https://updates.invalid/{MANIFEST_NAME}"))?,
        signature: Url::parse(&format!("https://updates.invalid/{SIGNATURE_NAME}"))?,
    })
}

fn updater<T: Transport>(version: semver::Version, transport: T) -> freshen::Result<Updater<T>> {
    Ok(Updater {
        product: PRODUCT.into(),
        channel: CHANNEL.into(),
        current_version: version,
        target: TARGET.into(),
        trust: TrustStore::new(vec![PUBLISHER_KEY])?,
        transport,
        max_download: 64 * 1024 * 1024,
        max_unpacked: 128 * 1024 * 1024,
    })
}

struct Progress<'a> {
    download_marks: u64,
    progress: &'a mut dyn FnMut(String),
}

impl Progress<'_> {
    fn report(&mut self, event: Event) {
        let line = match event {
            Event::Checking => "Checking for an update.".to_string(),
            Event::Verifying => "Verifying the package.".to_string(),
            Event::Extracting { path } => format!("Extracting {path}."),
            Event::Ready => "The update is ready to install.".to_string(),
            Event::Downloading { received, total } => {
                let mark = match total {
                    Some(total) if total > 0 => received * 10 / total,
                    _ => received / (256 * 1024),
                };
                if mark != self.download_marks || received == total.unwrap_or(0) {
                    self.download_marks = mark;
                    match total {
                        Some(total) => format!("Downloaded {received} of {total} bytes."),
                        None => format!("Downloaded {received} bytes."),
                    }
                } else {
                    return;
                }
            }
            _ => return,
        };
        (self.progress)(line);
    }
}

/// Reads release files from one directory. The URL's file name selects the file,
/// so a local test can use the https names Freshen stores in the manifest.
struct DirectoryTransport {
    root: PathBuf,
}

impl Transport for DirectoryTransport {
    fn download(
        &self,
        url: &Url,
        output: &mut dyn Write,
        limit: u64,
        cancel: &Cancellation,
        events: &mut dyn FnMut(Event),
    ) -> freshen::Result<u64> {
        cancel.check()?;
        let name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| Error::Invalid("download name".into()))?;
        if name.contains(['/', '\\']) {
            return Err(Error::Invalid("download name".into()));
        }
        let path = self.root.join(name);
        let mut file = File::open(&path).map_err(|error| {
            std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
        let length = file.metadata()?.len();
        if length > limit {
            return Err(Error::SizeLimit);
        }
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            cancel.check()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            received = received.checked_add(count as u64).ok_or(Error::SizeLimit)?;
            if received > limit {
                return Err(Error::SizeLimit);
            }
            output.write_all(&buffer[..count])?;
            events(Event::Downloading {
                received,
                total: Some(length),
            });
        }
        Ok(received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_key_is_accepted() {
        TrustStore::new(vec![PUBLISHER_KEY]).unwrap();
    }

    #[test]
    fn local_private_key_matches_embedded_public_key() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("keys/publisher.key");
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text.trim())
            .unwrap();
        let seed: [u8; 32] = bytes.try_into().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(key.verifying_key().to_bytes(), PUBLISHER_KEY);
    }

    #[test]
    fn owned_files_are_the_program_and_sounds() {
        assert_eq!(
            OWNED_FILES,
            [
                "beeprs.exe",
                "sounds/beep.opus",
                "sounds/bed.opus",
                "sounds/intro.opus",
                "sounds/die1.opus",
                "sounds/die2.opus",
                "sounds/die3.opus",
                "sounds/die4.opus",
                "sounds/die5.opus",
                "sounds/die6.opus",
                "sounds/die7.opus",
                "sounds/die8.opus",
                "sounds/die9.opus",
                "sounds/die10.opus",
                "sounds/die11.opus",
                "sounds/die12.opus",
                "sounds/die13.opus",
                "sounds/die14.opus",
            ]
        );
        assert!(OWNED_FILES.iter().all(|path| !path.contains("saves")));
        assert!(OWNED_FILES.iter().all(|path| *path != "update-source.json"));
        assert!(
            !OWNED_FILES.contains(&DISTRIBUTION_MARKER),
            "the installer marker is not a portable owned file"
        );
        assert_eq!(EXPANDABLE_DIRECTORIES, ["sounds"]);
    }

    #[test]
    fn missing_marker_is_portable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_copy_kind(dir.path()).unwrap(), CopyKind::Portable);
    }

    #[test]
    fn inno_marker_selects_the_installer_kind() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DISTRIBUTION_MARKER), "inno\n").unwrap();
        assert_eq!(detect_copy_kind(dir.path()).unwrap(), CopyKind::Inno);
        assert_eq!(CopyKind::Inno.package_kind(), PackageKind::Inno);
        assert_eq!(CopyKind::Portable.package_kind(), PackageKind::Files);
    }

    #[test]
    fn unknown_marker_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DISTRIBUTION_MARKER), "mystery").unwrap();
        let error = detect_copy_kind(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn directory_transport_reads_the_url_file_name_and_enforces_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("beeprs.zip"), b"package-bytes").unwrap();
        let transport = DirectoryTransport {
            root: dir.path().to_path_buf(),
        };
        let url = Url::parse("https://updates.invalid/beeprs.zip").unwrap();
        let cancel = Cancellation::default();
        let mut output = Vec::new();
        let received = transport
            .download(&url, &mut output, 64, &cancel, &mut |_| {})
            .unwrap();
        assert_eq!(received, output.len() as u64);
        assert_eq!(output, b"package-bytes");

        let mut output = Vec::new();
        assert!(matches!(
            transport.download(&url, &mut output, 4, &cancel, &mut |_| {}),
            Err(Error::SizeLimit)
        ));

        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            transport.download(&url, &mut Vec::new(), 64, &cancel, &mut |_| {}),
            Err(Error::Cancelled)
        ));
    }
}
