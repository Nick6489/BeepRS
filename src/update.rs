use freshen::{
    Cancellation, Error, Event, HttpTransport, InstallPolicy, InstallState, ReleaseSource,
    Transport, TrustStore, Updater,
};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{Read, Write},
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

/// Must match the signed zip exactly. Saves and `update-source.json` are absent
/// on purpose: Freshen replaces this list and leaves every other file alone.
/// Adding a path later requires a policy change in the already-shipped build.
pub const OWNED_FILES: &[&str] = &[
    EXECUTABLE,
    "sounds/beep.opus",
    "sounds/bed.opus",
    "sounds/intro.opus",
    "sounds/die1.opus",
    "sounds/die2.opus",
    "sounds/die3.opus",
];

/// Dev publisher key. The matching private key is `keys/publisher.key`, which is
/// gitignored. Sign releases with that file; do not put it in a package.
pub const PUBLISHER_KEY: [u8; 32] = [
    133, 237, 179, 216, 204, 92, 75, 26, 192, 200, 238, 178, 213, 76, 41, 67, 7, 182, 64, 213, 51,
    251, 238, 183, 184, 64, 83, 120, 224, 89, 242, 232,
];

const MANIFEST_NAME: &str = "freshen-manifest.json";
const SIGNATURE_NAME: &str = "freshen-manifest.json.sig";

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

pub fn acknowledge_installation(root: &Path) -> freshen::Result<StartupReport> {
    let mut report = StartupReport {
        lines: Vec::new(),
        needs_attention: false,
    };
    match freshen::confirm_startup(root) {
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
        let status = freshen::installation_status(root)?;
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
        InstallState::Committed | InstallState::RolledBack | InstallState::Prepared => {
            if let Some(line) = discard_when_free(root, deadline)? {
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
                root.display()
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
    }
}

pub fn current_version() -> Result<semver::Version, semver::Error> {
    env!("CARGO_PKG_VERSION").parse()
}

pub fn identity_line() -> String {
    format!(
        "This is {PRODUCT} {} for {TARGET}, channel {CHANNEL}.",
        env!("CARGO_PKG_VERSION")
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

pub fn load_source(root: &Path) -> Result<LoadedSource, Box<dyn std::error::Error>> {
    read_source(root)
}

pub fn find_release(
    loaded: &LoadedSource,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(String),
) -> Result<Option<ReleaseOffer>, Box<dyn std::error::Error>> {
    let version = current_version()?;
    let mut reporter = Progress {
        download_marks: u64::MAX,
        progress,
    };
    let Some(candidate) = find_candidate(loaded, version, cancel, &mut reporter)? else {
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
        offer.candidate,
        cancel,
        &mut reporter,
    )
}

pub fn install_release(
    root: &Path,
    prepared: freshen::PreparedUpdate,
) -> Result<freshen::Handoff, Box<dyn std::error::Error>> {
    let policy = InstallPolicy::portable(
        root.to_path_buf(),
        EXECUTABLE.to_string(),
        OWNED_FILES.iter().map(|path| (*path).to_string()).collect(),
    );
    Ok(prepared.arm(policy, &std::env::current_exe()?, Vec::new())?)
}

#[derive(Clone)]
pub enum LoadedSource {
    Directory(PathBuf),
    Manifest(Url, Url),
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
    cancel: &Cancellation,
    progress: &mut Progress<'_>,
) -> Result<Option<freshen::Candidate>, Box<dyn std::error::Error>> {
    match loaded {
        LoadedSource::Directory(directory) => {
            let updater = updater(
                version,
                DirectoryTransport {
                    root: directory.clone(),
                },
            )?;
            let source = local_manifest_source()?;
            Ok(updater.check(&source, cancel, &mut |event| progress.report(event))?)
        }
        LoadedSource::Manifest(manifest, signature) => {
            let updater = updater(version, HttpTransport::new(Duration::from_secs(120))?)?;
            let source = ReleaseSource::Manifest {
                document: manifest.clone(),
                signature: signature.clone(),
            };
            Ok(updater.check(&source, cancel, &mut |event| progress.report(event))?)
        }
    }
}

fn prepare_candidate(
    loaded: &LoadedSource,
    version: semver::Version,
    candidate: freshen::Candidate,
    cancel: &Cancellation,
    progress: &mut Progress<'_>,
) -> Result<freshen::PreparedUpdate, Box<dyn std::error::Error>> {
    let temporary = std::env::temp_dir();
    match loaded {
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
            ]
        );
        assert!(OWNED_FILES.iter().all(|path| !path.contains("saves")));
        assert!(OWNED_FILES.iter().all(|path| *path != "update-source.json"));
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
