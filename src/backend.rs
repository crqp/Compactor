use std::io;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError, bounded};

use crate::background::BackgroundHandle;
use crate::compact::size_on_disk;
use crate::compression::BackgroundCompactor;
use crate::folder::{FileKind, FolderInfo, FolderScan, FolderSummary};
use crate::gui::{GuiRequest, GuiWrapper};
use crate::persistence::{config, pathdb};

pub struct Backend {
    gui: GuiWrapper,
    msg: Receiver<GuiRequest>,
    info: Option<FolderInfo>,
}

fn format_size(size: u64, decimal: bool) -> String {
    humansize::format_size(
        size,
        if decimal {
            humansize::DECIMAL
        } else {
            humansize::BINARY
        },
    )
}

fn failed_text(summary: &FolderSummary) -> String {
    if summary.failed.count > 0 {
        format!(", {} failed", summary.failed.count)
    } else {
        String::new()
    }
}

#[allow(clippy::cast_precision_loss)]
fn progress(done: i32, total: usize) -> f32 {
    done as f32 / total as f32
}

impl Backend {
    pub fn new(gui: GuiWrapper, msg: Receiver<GuiRequest>) -> Self {
        Self {
            gui,
            msg,
            info: None,
        }
    }

    pub fn run(&mut self) {
        loop {
            match self.msg.recv() {
                Ok(GuiRequest::FolderChosen(path)) => {
                    self.gui.folder(&path);
                    self.scan_loop(path);
                }
                Ok(GuiRequest::Analyse) if self.info.is_some() => {
                    let path = self.info.take().unwrap().path;
                    self.gui.folder(&path);
                    self.scan_loop(path);
                }
                Ok(GuiRequest::Compress) if self.info.is_some() => {
                    self.compress_loop();
                }
                Ok(GuiRequest::Decompress) if self.info.is_some() => {
                    self.uncompress_loop();
                }
                Ok(msg) => {
                    eprintln!("Backend: Ignored message: {msg:?}");
                }
                Err(_) => {
                    eprintln!("Backend: exit run loop");
                    break;
                }
            }
        }
    }

    fn scan_loop(&mut self, path: PathBuf) {
        let excludes = match config().read().unwrap().current().globset() {
            Ok(excludes) => excludes,
            Err(err) => {
                self.gui
                    .status(format!("Invalid exclusion pattern: {err}"), None);
                self.gui.stopped();
                return;
            }
        };

        if excludes.is_match(&path) {
            self.gui.status(
                format!("{} is excluded by the current patterns", path.display()),
                None,
            );
            self.gui.stopped();
            return;
        }

        let scanner = FolderScan::new(path, excludes);
        let task = BackgroundHandle::spawn(scanner);
        let start = Instant::now();

        self.gui.status("Scanning", None);
        loop {
            let msg = self.msg.recv_timeout(Duration::from_millis(25));

            match msg {
                Ok(GuiRequest::Pause) => {
                    task.pause();
                    self.gui.status("Paused", Some(0.5));
                    self.gui.paused();
                }
                Ok(GuiRequest::Resume) => {
                    task.resume();
                    self.gui.status("Scanning", None);
                    self.gui.resumed();
                }
                Ok(GuiRequest::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    task.cancel();
                }
                Ok(msg) => {
                    eprintln!("Ignored message: {msg:?}");
                }
                Err(RecvTimeoutError::Timeout) => (),
            }

            match task.wait_timeout(Duration::from_millis(25)) {
                Some(Ok(info)) => {
                    self.gui
                        .status(format!("Scanned in {:.2?}", start.elapsed()), Some(1.0));
                    self.gui.summary(info.summary());
                    self.gui.scanned();
                    self.info = Some(info);
                    break;
                }
                Some(Err(info)) => {
                    self.gui.status(
                        format!("Scan stopped after {:.2?}", start.elapsed()),
                        Some(0.5),
                    );
                    self.gui.summary(info.summary());
                    self.gui.stopped();
                    self.info = Some(info);
                    break;
                }
                None => {
                    if let Some(status) = task.status() {
                        self.gui
                            .status(format!("Scanning: {}", status.0.display()), None);
                        self.gui.summary(status.1);
                    }
                }
            }
        }
    }

    // Ph'nglui mglw'nafh Cthulhu R'lyeh wgah'nagl fhtagn.
    #[allow(clippy::too_many_lines)]
    fn compress_loop(&mut self) {
        let (send_file, send_file_rx) = bounded::<PathBuf>(1);
        let (recv_result_tx, recv_result) = bounded::<(PathBuf, io::Result<bool>)>(1);

        let compression = Some(config().read().unwrap().current().compression);
        let compactor = BackgroundCompactor::new(compression, send_file_rx, recv_result_tx);
        let task = BackgroundHandle::spawn(compactor);
        let start = Instant::now();

        let mut folder = self.info.take().expect("fileinfo");
        let total = folder.len(FileKind::Compressible);
        let mut done = 0;

        let mut last_update = Instant::now();
        let mut last_write = Instant::now();
        let mut paused = false;
        let mut stopped = false;

        let old_size = folder.physical_size;
        let compressible_size = folder.summary().compressible.physical_size;

        let incompressible = pathdb();
        let mut incompressible = incompressible.write().unwrap();
        let _ = incompressible.load();

        self.gui.compacting();

        self.gui.status("Compacting", Some(0.0));
        loop {
            while paused && !stopped {
                self.gui.status("Paused", Some(progress(done, total)));

                self.gui.summary(folder.summary());

                match self.msg.recv() {
                    Ok(GuiRequest::Pause) => {
                        paused = true;
                    }
                    Ok(GuiRequest::Resume) => {
                        self.gui.status("Compacting", Some(progress(done, total)));
                        self.gui.resumed();
                        paused = false;
                        last_update = Instant::now();
                    }
                    Ok(GuiRequest::Stop) | Err(_) => {
                        stopped = true;
                        break;
                    }
                    Ok(_) => (),
                }
            }

            if stopped {
                break;
            }

            if last_write.elapsed() > Duration::from_secs(60) {
                let _ = incompressible.save();
                last_write = Instant::now();
            }

            let mut displayed = false;

            if let Some(mut fi) = folder.pop(FileKind::Compressible) {
                send_file
                    .send(folder.path.join(&fi.path))
                    .expect("send_file");

                if !displayed && last_update.elapsed() > Duration::from_millis(50) {
                    self.gui.status(
                        format!("Compacting: {}", fi.path.display()),
                        Some(progress(done, total)),
                    );
                    last_update = Instant::now();
                    displayed = true;
                }

                loop {
                    match self.msg.try_recv() {
                        Ok(GuiRequest::Pause) if !paused && !stopped => {
                            self.gui.status(
                                format!("Pausing after {}", fi.path.display()),
                                Some(progress(done, total)),
                            );
                            self.gui.paused();
                            paused = true;
                        }
                        Ok(GuiRequest::Resume) => {
                            self.gui.resumed();
                            paused = false;
                        }
                        Ok(GuiRequest::Stop) if !stopped => {
                            self.gui.status(
                                format!("Stopping after {}", fi.path.display()),
                                Some(progress(done, total)),
                            );
                            stopped = true;
                        }
                        Ok(_) | Err(TryRecvError::Empty) => (),
                        Err(TryRecvError::Disconnected) => {
                            stopped = true;
                        }
                    }

                    let received = recv_result.recv_timeout(Duration::from_millis(25));

                    if let Err(RecvTimeoutError::Disconnected) = received {
                        folder.push(FileKind::Skipped, fi);
                        stopped = true;
                        break;
                    }

                    if let Ok((path, result)) = received {
                        done += 1;
                        match result {
                            Ok(true) => {
                                fi.physical_size = size_on_disk(&path).unwrap_or(fi.physical_size);

                                // Irritatingly Windows can return success when it fails.
                                if fi.physical_size == fi.logical_size {
                                    incompressible.insert(path);
                                    folder.push(FileKind::Skipped, fi);
                                } else {
                                    folder.push(FileKind::Compressed, fi);
                                }
                            }
                            Ok(false) => {
                                incompressible.insert(path);
                                folder.push(FileKind::Skipped, fi);
                            }
                            Err(err) => {
                                self.gui.status(
                                    format!("Error: {}, {}", err, fi.path.display()),
                                    Some(progress(done, total)),
                                );
                                folder.push(FileKind::Failed, fi);
                            }
                        }

                        if last_update.elapsed() > Duration::from_millis(50) {
                            self.gui.summary(folder.summary());
                        }

                        break;
                    }

                    if !displayed && last_update.elapsed() > Duration::from_millis(50) {
                        self.gui.status(
                            format!("Compacting: {}", fi.path.display()),
                            Some(progress(done, total)),
                        );

                        displayed = true;
                    }
                }
            } else {
                break;
            }
        }

        drop(send_file);
        task.wait();

        let _ = incompressible.save();

        let new_size = folder.physical_size;
        let decimal = config().read().unwrap().current().decimal;

        let msg = format!(
            "Compacted {} in {} files, saving {} in {:.2?}{}",
            format_size(compressible_size, decimal),
            done,
            format_size(old_size - new_size, decimal),
            start.elapsed(),
            failed_text(&folder.summary())
        );

        self.gui.status(msg, Some(progress(done, total)));
        self.gui.summary(folder.summary());
        self.gui.scanned();

        self.info = Some(folder);
    }

    // Oh no, not again.
    #[allow(clippy::too_many_lines)]
    fn uncompress_loop(&mut self) {
        let (send_file, send_file_rx) = bounded::<PathBuf>(1);
        let (recv_result_tx, recv_result) = bounded::<(PathBuf, io::Result<bool>)>(1);

        let compactor = BackgroundCompactor::new(None, send_file_rx, recv_result_tx);
        let task = BackgroundHandle::spawn(compactor);
        let start = Instant::now();

        let mut folder = self.info.take().expect("fileinfo");
        let total = folder.len(FileKind::Compressed);
        let mut done = 0;

        let mut last_update = Instant::now();
        let mut paused = false;
        let mut stopped = false;

        let old_size = folder.physical_size;

        self.gui.compacting();

        self.gui.status("Expanding", Some(0.0));
        loop {
            while paused && !stopped {
                self.gui.status("Paused", Some(progress(done, total)));

                self.gui.summary(folder.summary());

                match self.msg.recv() {
                    Ok(GuiRequest::Pause) => {
                        paused = true;
                    }
                    Ok(GuiRequest::Resume) => {
                        self.gui.status("Expanding", Some(progress(done, total)));
                        self.gui.resumed();
                        paused = false;
                        last_update = Instant::now();
                    }
                    Ok(GuiRequest::Stop) | Err(_) => {
                        stopped = true;
                        break;
                    }
                    Ok(_) => (),
                }
            }

            if stopped {
                break;
            }

            if last_update.elapsed() > Duration::from_millis(50) {
                self.gui.status("Expanding", Some(progress(done, total)));
                last_update = Instant::now();

                self.gui.summary(folder.summary());
            }

            if let Some(mut fi) = folder.pop(FileKind::Compressed) {
                send_file
                    .send(folder.path.join(&fi.path))
                    .expect("send_file");

                let mut waiting = false;
                loop {
                    match self.msg.try_recv() {
                        Ok(GuiRequest::Pause) if !paused && !stopped => {
                            self.gui.status(
                                format!("Pausing after {}", fi.path.display()),
                                Some(progress(done, total)),
                            );
                            self.gui.paused();
                            paused = true;
                        }
                        Ok(GuiRequest::Resume) => {
                            self.gui.resumed();
                            paused = false;
                        }
                        Ok(GuiRequest::Stop) if !stopped => {
                            self.gui.status(
                                format!("Stopping after {}", fi.path.display()),
                                Some(progress(done, total)),
                            );
                            stopped = true;
                        }
                        Ok(_) | Err(TryRecvError::Empty) => (),
                        Err(TryRecvError::Disconnected) => {
                            stopped = true;
                        }
                    }

                    let received = recv_result.recv_timeout(Duration::from_millis(25));

                    if let Err(RecvTimeoutError::Disconnected) = received {
                        folder.push(FileKind::Skipped, fi);
                        stopped = true;
                        break;
                    }

                    if let Ok((_path, result)) = received {
                        done += 1;
                        match result {
                            Ok(_) => {
                                fi.physical_size = fi.logical_size;
                                folder.push(FileKind::Compressible, fi);
                            }
                            Err(err) => {
                                self.gui.status(
                                    format!("Error: {}, {}", err, fi.path.display()),
                                    Some(progress(done, total)),
                                );
                                folder.push(FileKind::Failed, fi);
                            }
                        }

                        break;
                    }

                    if !waiting && last_update.elapsed() > Duration::from_millis(50) {
                        self.gui.status(
                            format!("Expanding: {}", fi.path.display()),
                            Some(progress(done, total)),
                        );

                        last_update = Instant::now();
                        waiting = true;
                    }
                }
            } else {
                break;
            }
        }

        drop(send_file);
        task.wait();

        let new_size = folder.physical_size;

        let msg = format!(
            "Expanded {} files wasting {} in {:.2?}{}",
            done,
            format_size(
                new_size - old_size,
                config().read().unwrap().current().decimal
            ),
            start.elapsed(),
            failed_text(&folder.summary())
        );

        self.gui.status(msg, Some(progress(done, total)));
        self.gui.summary(folder.summary());
        self.gui.scanned();

        self.info = Some(folder);
    }
}
