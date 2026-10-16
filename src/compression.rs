use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use compresstimator::Compresstimator;
use crossbeam_channel::{Receiver, Sender};
use filetime::FileTime;
use windows_sys::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_WRITE_ATTRIBUTES};

use crate::background::{Background, ControlToken};
use crate::compact::{self, Compression};

#[derive(Debug)]
pub struct BackgroundCompactor {
    compression: Option<Compression>,
    files_in: Receiver<PathBuf>,
    files_out: Sender<(PathBuf, io::Result<bool>)>,
}

impl BackgroundCompactor {
    pub fn new(
        compression: Option<Compression>,
        files_in: Receiver<PathBuf>,
        files_out: Sender<(PathBuf, io::Result<bool>)>,
    ) -> Self {
        Self {
            compression,
            files_in,
            files_out,
        }
    }
}

fn lock_error(err: std::fs::TryLockError) -> io::Error {
    match err {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "file is locked by another process",
        ),
        std::fs::TryLockError::Error(err) => err,
    }
}

fn handle_file(path: &Path, compression: Option<Compression>) -> io::Result<bool> {
    let est = Compresstimator::with_block_size(8192);
    let meta = std::fs::metadata(path)?;
    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES | FILE_READ_DATA)
        .open(path)?;

    handle.try_lock().map_err(lock_error)?;

    let ret = match compression {
        Some(compression) => match est.compresstimate_len(&handle, meta.len()) {
            Ok(ratio) if ratio < 0.95 => compact::compress_file_handle(&handle, compression),
            Ok(_) => Ok(false),
            Err(err) => Err(err),
        },
        None => compact::uncompress_file_handle(&handle).map(|()| true),
    };

    let _ = filetime::set_file_handle_times(
        &handle,
        Some(FileTime::from_last_access_time(&meta)),
        Some(FileTime::from_last_modification_time(&meta)),
    );

    let _ = handle.unlock();

    ret
}

impl Background for BackgroundCompactor {
    type Output = ();
    type Status = ();

    fn run(self, control: &ControlToken<Self::Status>) -> Self::Output {
        for file in &self.files_in {
            if control.is_cancelled_with_pause() {
                break;
            }

            let ret = handle_file(&file, self.compression);
            if self.files_out.send((file, ret)).is_err() {
                break;
            }
        }
    }
}
