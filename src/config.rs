use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::compact::Compression;

#[derive(Debug, Default)]
pub struct ConfigFile {
    backing: Option<PathBuf>,
    config: Config,
    load_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub decimal: bool,
    pub compression: Compression,
    pub excludes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            decimal: false,
            compression: Compression::default(),
            excludes: vec![
                "*:\\Windows*",
                "*:\\System Volume Information*",
                "*:\\$*",
                "*.7z",
                "*.aac",
                "*.avi",
                "*.ba",
                "*.{bik,bk2,bnk,pc_binkvid}",
                "*.br",
                "*.bz2",
                "*.cab",
                "*.dl_",
                "*.docx",
                "*.flac",
                "*.flv",
                "*.gif",
                "*.gz",
                "*.jpeg",
                "*.jpg",
                "*.log",
                "*.lz4",
                "*.lzma",
                "*.lzx",
                "*.m[24]v",
                "*.m4a",
                "*.mkv",
                "*.mp[234]",
                "*.mpeg",
                "*.mpg",
                "*.ogg",
                "*.onepkg",
                "*.png",
                "*.pptx",
                "*.rar",
                "*.upk",
                "*.vob",
                "*.vs[st]x",
                "*.wem",
                "*.webm",
                "*.wm[afv]",
                "*.xap",
                "*.xnb",
                "*.xlsx",
                "*.xz",
                "*.zst",
                "*.zstd",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

impl ConfigFile {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let mut new = Self {
            backing: Some(path.as_ref().to_owned()),
            ..Self::default()
        };

        match std::fs::read(path) {
            Ok(data) => match serde_json::from_slice::<Config>(&data) {
                Ok(config) => new.config = config,
                Err(err) => {
                    new.load_error = Some(format!("Could not read saved settings: {err}"));
                }
            },
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => {
                new.load_error = Some(format!("Could not read saved settings: {err}"));
            }
        }

        new
    }

    pub fn save(&self) -> io::Result<()> {
        match &self.backing {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }

                let data = serde_json::to_string_pretty(&self.config).expect("Serialize");
                std::fs::write(path, &data)
            }
            None => Ok(()),
        }
    }

    pub fn current(&self) -> Config {
        self.config.clone()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn replace(&mut self, config: Config) {
        self.config = config;
    }
}

impl Config {
    pub fn globset(&self) -> Result<GlobSet, String> {
        let mut globs = GlobSetBuilder::new();
        for glob in &self.excludes {
            let glob = GlobBuilder::new(glob)
                .case_insensitive(true)
                .build()
                .map_err(|error| error.to_string())?;
            globs.add(glob);
        }
        globs.build().map_err(|error| error.to_string())
    }
}

#[test]
fn test_config() {
    let config = Config::default();

    assert!(config.globset().is_ok());
    let gs = config.globset().unwrap();

    assert!(gs.is_match("C:\\foo\\bar\\hmm.rar"));
    assert!(gs.is_match("C:\\Windows\\System32\\floop\\bla.txt"));
    assert!(gs.is_match("c:\\WINDOWS\\System32\\floop\\bla.txt"));
    assert!(gs.is_match("C:\\x\\PHOTO.JPG"));
    assert!(gs.is_match("C:\\x.lz4"));
}
