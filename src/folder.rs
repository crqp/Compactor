use std::collections::VecDeque;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::GlobSet;
use walkdir::WalkDir;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
    FILE_ATTRIBUTE_TEMPORARY,
};

use crate::background::{Background, ControlToken};
use crate::compact::{self, size_on_disk};
use crate::persistence::pathdb;

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub logical_size: u64,
    pub physical_size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GroupInfo {
    pub files: VecDeque<FileInfo>,
    pub logical_size: u64,
    pub physical_size: u64,
}

#[derive(Debug, Clone)]
pub struct FolderInfo {
    pub path: PathBuf,
    pub logical_size: u64,
    pub physical_size: u64,
    pub compressible: GroupInfo,
    pub compressed: GroupInfo,
    pub skipped: GroupInfo,
    pub failed: GroupInfo,
}

#[derive(Debug, Clone, Default)]
pub struct FolderSummary {
    pub logical_size: u64,
    pub physical_size: u64,
    pub compressible: GroupSummary,
    pub compressed: GroupSummary,
    pub skipped: GroupSummary,
    pub failed: GroupSummary,
}

#[derive(Debug, Clone, Default)]
pub struct GroupSummary {
    pub count: usize,
    pub logical_size: u64,
    pub physical_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum FileKind {
    Compressed,
    Compressible,
    Skipped,
    Failed,
}

impl FolderInfo {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            logical_size: 0,
            physical_size: 0,
            compressible: GroupInfo::default(),
            compressed: GroupInfo::default(),
            skipped: GroupInfo::default(),
            failed: GroupInfo::default(),
        }
    }

    pub fn summary(&self) -> FolderSummary {
        FolderSummary {
            logical_size: self.logical_size,
            physical_size: self.physical_size,
            compressible: self.compressible.summary(),
            compressed: self.compressed.summary(),
            skipped: self.skipped.summary(),
            failed: self.failed.summary(),
        }
    }

    pub fn len(&self, kind: FileKind) -> usize {
        match kind {
            FileKind::Compressible => self.compressible.files.len(),
            FileKind::Compressed => self.compressed.files.len(),
            FileKind::Skipped => self.skipped.files.len(),
            FileKind::Failed => self.failed.files.len(),
        }
    }

    pub fn pop(&mut self, kind: FileKind) -> Option<FileInfo> {
        let ret = match kind {
            FileKind::Compressible => self.compressible.pop(),
            FileKind::Compressed => self.compressed.pop(),
            FileKind::Skipped => self.skipped.pop(),
            FileKind::Failed => self.failed.pop(),
        };

        if let Some(fi) = ret {
            self.logical_size -= fi.logical_size;
            self.physical_size -= fi.physical_size;

            Some(fi)
        } else {
            None
        }
    }

    pub fn push(&mut self, kind: FileKind, fi: FileInfo) {
        self.logical_size += fi.logical_size;
        self.physical_size += fi.physical_size;

        match kind {
            FileKind::Compressible => self.compressible.push(fi),
            FileKind::Compressed => self.compressed.push(fi),
            FileKind::Skipped => self.skipped.push(fi),
            FileKind::Failed => self.failed.push(fi),
        }
    }
}

impl GroupInfo {
    pub fn summary(&self) -> GroupSummary {
        GroupSummary {
            count: self.files.len(),
            logical_size: self.logical_size,
            physical_size: self.physical_size,
        }
    }

    fn pop(&mut self) -> Option<FileInfo> {
        let ret = self.files.pop_front();

        if let Some(fi) = ret {
            self.logical_size -= fi.logical_size;
            self.physical_size -= fi.physical_size;

            Some(fi)
        } else {
            None
        }
    }

    fn push(&mut self, fi: FileInfo) {
        self.logical_size += fi.logical_size;
        self.physical_size += fi.physical_size;
        self.files.push_back(fi);
    }
}

#[derive(Debug)]
pub struct FolderScan {
    path: PathBuf,
    excludes: GlobSet,
}

impl FolderScan {
    pub fn new<P: AsRef<Path>>(path: P, excludes: GlobSet) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            excludes,
        }
    }
}

impl Background for FolderScan {
    type Output = Result<FolderInfo, FolderInfo>;
    type Status = (PathBuf, FolderSummary);

    fn run(self, control: &ControlToken<Self::Status>) -> Self::Output {
        let FolderScan { path, excludes } = self;
        let mut ds = FolderInfo::new(&path);
        let incompressible = pathdb();
        let mut incompressible = incompressible.write().unwrap();
        let _ = incompressible.load();

        let mut last_status = Instant::now();

        // 1. Handle excludes separately for directories to allow pruning, while
        //    still recording accurate sizes for files.
        // 2. Ignore errors - consider recording them somewhere in future.
        // 3. Only process files.
        // 4. Grab metadata - should be infallible on Windows, it comes with the
        //    DirEntry.
        // 5. GetCompressedFileSizeW() or skip.
        let walker = WalkDir::new(&path)
            .into_iter()
            .filter_entry(|entry| entry.file_type().is_file() || !excludes.is_match(entry.path()))
            .filter_map(|result| result.map_err(|error| eprintln!("Error: {error:?}")).ok())
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.metadata().map(|metadata| (entry, metadata)).ok())
            .filter_map(|(entry, metadata)| {
                size_on_disk(entry.path())
                    .map(|size| (entry, metadata, size))
                    .ok()
            });

        for (entry, metadata, physical) in walker {
            let shortname = entry
                .path()
                .strip_prefix(&path)
                .unwrap_or_else(|_e| entry.path())
                .to_path_buf();

            let fi = FileInfo {
                path: shortname,
                logical_size: metadata.len().max(physical),
                physical_size: physical,
            };

            if control.is_cancelled_with_pause() {
                return Err(ds);
            }

            if last_status.elapsed() >= Duration::from_millis(50) {
                last_status = Instant::now();
                control.set_status((fi.path.clone(), ds.summary()));
            }

            if fi.physical_size < fi.logical_size {
                // Only externally backed files can be decompressed again, while
                // legacy NTFS compression and sparse files cannot.
                if compact::detect_compression(entry.path())
                    .is_ok_and(|compression| compression.is_some())
                {
                    ds.push(FileKind::Compressed, fi);
                } else {
                    ds.push(FileKind::Skipped, fi);
                }
            } else if fi.logical_size <= 4096
                || metadata.file_attributes()
                    & (FILE_ATTRIBUTE_READONLY
                        | FILE_ATTRIBUTE_SYSTEM
                        | FILE_ATTRIBUTE_TEMPORARY
                        | FILE_ATTRIBUTE_COMPRESSED)
                    != 0
                || incompressible.contains(entry.path())
                || excludes.is_match(entry.path())
            {
                ds.push(FileKind::Skipped, fi);
            } else {
                ds.push(FileKind::Compressible, fi);
            }
        }

        Ok(ds)
    }
}

#[test]
fn it_walks() {
    use crate::background::BackgroundHandle;
    use crate::config::Config;

    let gs = Config::default().globset().unwrap();
    let scanner = FolderScan::new("C:\\Games", gs);

    let task = BackgroundHandle::spawn(scanner);

    let deadline = Instant::now() + Duration::from_millis(2000);

    loop {
        let ret = task.wait_timeout(Duration::from_millis(100));

        if ret.is_some() {
            println!("Scanned: {ret:?}");
            break;
        }

        println!("Status: {:?}", task.status());

        if Instant::now() > deadline {
            task.cancel();
        }
    }
}
