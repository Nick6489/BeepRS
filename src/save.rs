use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// One saved game. The score file lives under `saves/` and is not part of the
/// Freshen owned-file list, so an update must leave it in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    pub name: String,
    pub aliens_destroyed: u64,
    path: PathBuf,
}

#[derive(Debug)]
pub struct Library {
    dir: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    Characters,
    Reserved,
    Duplicate,
}

#[derive(Debug)]
pub enum CreateError {
    Name(NameError),
    Io(io::Error),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "Could not save the game: {error}"),
        }
    }
}

impl std::error::Error for CreateError {}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "Enter a name.",
            Self::TooLong => "Use 40 characters or fewer.",
            Self::Characters => "Use letters, numbers, spaces, and hyphens.",
            Self::Reserved => "That name is reserved on Windows.",
            Self::Duplicate => "A game with that name is already saved.",
        })
    }
}

impl std::error::Error for NameError {}

#[derive(Debug, Serialize, Deserialize)]
struct SaveData {
    name: String,
    aliens_destroyed: u64,
}

impl Library {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn list(&self) -> io::Result<Vec<Game>> {
        let mut games = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            let data: SaveData = serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {error}", path.display()),
                )
            })?;
            games.push(Game {
                name: data.name,
                aliens_destroyed: data.aliens_destroyed,
                path,
            });
        }
        games.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(games)
    }

    pub fn create(&self, name: &str) -> Result<Game, CreateError> {
        let display = name.trim();
        let slug = slug(display).map_err(CreateError::Name)?;
        let path = self.dir.join(format!("{slug}.json"));
        if path.exists() {
            return Err(CreateError::Name(NameError::Duplicate));
        }
        let game = Game {
            name: display.to_string(),
            aliens_destroyed: 0,
            path,
        };
        self.write(&game).map_err(CreateError::Io)?;
        Ok(game)
    }

    /// Persist `destroyed` and return only after the new score is on disk.
    pub fn store(&self, game: &Game, destroyed: u64) -> io::Result<()> {
        let stored = Game {
            aliens_destroyed: destroyed,
            ..game.clone()
        };
        self.write(&stored)
    }

    fn write(&self, game: &Game) -> io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(&SaveData {
            name: game.name.clone(),
            aliens_destroyed: game.aliens_destroyed,
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        bytes.push(b'\n');
        write_atomic(&game.path, &bytes)
    }
}

pub fn slug(name: &str) -> Result<String, NameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(NameError::Empty);
    }
    if trimmed.chars().count() > 40 {
        return Err(NameError::TooLong);
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
    {
        return Err(NameError::Characters);
    }
    let mut slug = String::new();
    let mut hyphen = false;
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            hyphen = false;
        } else if !hyphen && !slug.is_empty() {
            slug.push('-');
            hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(NameError::Empty);
    }
    if reserved_windows_name(&slug) {
        return Err(NameError::Reserved);
    }
    Ok(slug)
}

/// `CON.json` still addresses the console device, so the file stem is checked.
fn reserved_windows_name(slug: &str) -> bool {
    let stem = slug.to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(".tmp");
    let temporary = PathBuf::from(temporary_name);
    {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            if path.exists() {
                fs::remove_file(path)?;
                fs::rename(&temporary, path)
            } else {
                let _ = fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_safe_file_stems() {
        assert_eq!(slug("First Contact").unwrap(), "first-contact");
        assert_eq!(slug("  Night--Run  ").unwrap(), "night-run");
        assert_eq!(slug(""), Err(NameError::Empty));
        assert_eq!(slug("   "), Err(NameError::Empty));
        assert_eq!(slug("alien/one"), Err(NameError::Characters));
        assert_eq!(slug(&"a".repeat(41)), Err(NameError::TooLong));
        assert_eq!(slug("con"), Err(NameError::Reserved));
        assert_eq!(slug("COM1"), Err(NameError::Reserved));
        assert_eq!(slug("aux"), Err(NameError::Reserved));
    }

    #[test]
    fn each_game_keeps_its_own_score() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let mut alpha = library.create("Alpha").unwrap();
        let beta = library.create("Beta").unwrap();
        assert!(matches!(
            library.create("alpha"),
            Err(CreateError::Name(NameError::Duplicate))
        ));

        library.store(&alpha, 2).unwrap();
        alpha.aliens_destroyed = 2;
        library.store(&alpha, 3).unwrap();
        library.store(&beta, 1).unwrap();

        let games = library.list().unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].name, "Alpha");
        assert_eq!(games[0].aliens_destroyed, 3);
        assert_eq!(games[1].name, "Beta");
        assert_eq!(games[1].aliens_destroyed, 1);
        assert!(dir.path().join("alpha.json").is_file());
        assert!(dir.path().join("beta.json").is_file());
    }
}
