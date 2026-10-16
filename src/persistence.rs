use std::sync::{LazyLock, RwLock};

use directories::ProjectDirs;
use hashfilter::HashFilter;

use crate::config::ConfigFile;

static PATHDB: LazyLock<RwLock<HashFilter>> = LazyLock::new(|| RwLock::new(HashFilter::default()));
static CONFIG: LazyLock<RwLock<ConfigFile>> = LazyLock::new(|| RwLock::new(ConfigFile::default()));

pub fn init() {
    if let Some(dirs) = ProjectDirs::from("", "Freaky", "Compactor") {
        pathdb()
            .write()
            .unwrap()
            .set_backing(dirs.cache_dir().join("incompressible.dat"));
        *config().write().unwrap() = ConfigFile::new(dirs.config_dir().join("config.json"));
    }
}

pub fn config() -> &'static RwLock<ConfigFile> {
    &CONFIG
}

pub fn pathdb() -> &'static RwLock<HashFilter> {
    &PATHDB
}
