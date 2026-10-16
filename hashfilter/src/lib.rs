use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::hash::Hash;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::path::PathBuf;

use siphasher::sip128::{Hasher128, SipHasher};

#[derive(Debug, Default)]
pub struct HashFilter {
    path: Option<PathBuf>,
    last_offset: u64,
    filter: HashSet<u128>,
    pending: Vec<u128>,
}

impl HashFilter {
    pub fn open<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: Some(path.as_ref().to_owned()),
            ..Self::default()
        }
    }

    pub fn set_backing<P: AsRef<Path>>(&mut self, path: P) {
        self.path = Some(path.as_ref().to_owned());
        self.last_offset = 0;
    }

    /// # Errors
    ///
    /// Returns an I/O error if opening fails for a reason other than `NotFound`, or if
    /// locking, inspecting, seeking, or reading the file fails for a reason other than
    /// `UnexpectedEof`.
    pub fn load(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(ref error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        file.lock_shared()?;

        if self.last_offset > file.metadata()?.len() {
            self.last_offset = 0;
        }

        if self.last_offset > 0 {
            file.seek(SeekFrom::Start(self.last_offset))?;
        }

        let mut file = BufReader::new(file);
        let mut buf = [0; 16];
        loop {
            match file.read_exact(&mut buf) {
                Ok(()) => self.filter.insert(u128::from_le_bytes(buf)),
                Err(ref error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(error),
            };

            self.last_offset += 16;
        }
    }

    /// # Errors
    ///
    /// Returns an I/O error if creating the parent directory, opening or locking the file,
    /// reading its metadata, resizing or seeking it, writing hashes, or syncing it fails.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        if self.pending.is_empty() {
            return Ok(());
        }

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;

        let end = file.metadata()?.len();
        if end % 16 != 0 {
            file.set_len(end - (end % 16))?;
        }
        file.seek(SeekFrom::End(0))?;

        let mut file = BufWriter::new(file);

        while let Some(key) = self.pending.pop() {
            file.write_all(&key.to_le_bytes())?;
        }

        if end == self.last_offset {
            self.last_offset = file.seek(SeekFrom::End(0))?;
        }

        file.into_inner()?.sync_all()?;

        Ok(())
    }

    pub fn insert<P: AsRef<Path>>(&mut self, path: P) -> bool {
        let key = Self::key_for(path.as_ref());

        if self.filter.insert(key) {
            self.pending.push(key);
            return true;
        }

        false
    }

    pub fn contains<P: AsRef<Path>>(&self, path: P) -> bool {
        self.filter.contains(&Self::key_for(path.as_ref()))
    }

    fn key_for(path: &Path) -> u128 {
        let mut hash = SipHasher::new();
        path.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .hash(&mut hash);
        let digest = hash.finish128();
        (u128::from(digest.h1) << 64) | u128::from(digest.h2)
    }
}

#[test]
fn it_ignores_path_case() {
    let mut hf = HashFilter::default();

    assert!(hf.insert(PathBuf::from("C:\\Games\\foo.dll")));
    assert!(hf.contains("c:\\games\\FOO.DLL"));
    assert!(!hf.insert("C:\\GAMES\\foo.dll"));
}

#[test]
fn it_recovers_from_replacement() {
    let dir = tempfile::Builder::new()
        .prefix("hashfilter-test")
        .tempdir()
        .unwrap();
    let db = dir.path().join("test.dat");
    let mut hf = HashFilter::open(&db);

    hf.insert(PathBuf::from("/a"));
    hf.insert(PathBuf::from("/b"));
    hf.save().unwrap();
    hf.load().unwrap();

    let replacement = dir.path().join("replacement.dat");
    let mut other = HashFilter::open(&replacement);
    other.insert(PathBuf::from("/c"));
    other.save().unwrap();
    std::fs::copy(&replacement, &db).unwrap();

    hf.load().unwrap();
    assert!(hf.contains("/c"));
}

#[test]
fn it_seems_to_work() {
    let dir = tempfile::Builder::new()
        .prefix("hashfilter-test")
        .tempdir()
        .unwrap();
    let db = dir.path().join("test.dat");
    let mut hf = HashFilter::open(&db);
    let _ = hf.load();

    let paths = vec![
        PathBuf::from("/path/to/some/file0"),
        PathBuf::from("/path/to/some/file1"),
        PathBuf::from("/path/to/some/file2"),
        PathBuf::from("/path/to/some/file3"),
        PathBuf::from("/path/to/some/file4"),
        PathBuf::from("/path/to/some/file5"),
        PathBuf::from("/path/to/some/file6"),
        PathBuf::from("/path/to/some/file7"),
        PathBuf::from("/path/to/some/file8"),
        PathBuf::from("/path/to/some/file9"),
    ];

    for path in &paths {
        hf.insert(path);
    }

    hf.save().unwrap();

    let mut hf2 = HashFilter::open(&db);
    hf2.load().unwrap();
    hf2.insert(PathBuf::from("/path/to/some/file10"));
    hf2.save().unwrap();

    for path in &paths {
        assert!(hf2.contains(path));
    }

    hf.load().unwrap();
    assert!(hf.contains(PathBuf::from("/path/to/some/file10")));
}
