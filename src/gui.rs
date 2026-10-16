use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui::{self, Color32, RichText};

use crate::backend::Backend;
use crate::compact::Compression;
use crate::config::Config;
use crate::folder::FolderSummary;
use crate::persistence::{self, config};

// messages received from the GUI
#[derive(Debug)]
pub enum GuiRequest {
    FolderChosen(PathBuf),
    Compress,
    Decompress,
    Pause,
    Resume,
    Analyse,
    Stop,
}

// messages to send to the GUI
#[derive(Debug)]
pub enum GuiResponse {
    Folder(PathBuf),
    Status { status: String, pct: Option<f32> },
    FolderSummary(FolderSummary),
    Paused,
    Resumed,
    Scanned,
    Stopped,
    Compacting,
}

pub struct GuiWrapper {
    tx: Sender<GuiResponse>,
}

impl GuiWrapper {
    pub fn new(tx: Sender<GuiResponse>) -> Self {
        Self { tx }
    }

    fn send(&self, msg: GuiResponse) {
        let _ = self.tx.send(msg);
    }

    pub fn folder<P: AsRef<std::path::Path>>(&self, path: P) {
        self.send(GuiResponse::Folder(path.as_ref().to_path_buf()));
    }

    pub fn summary(&self, info: FolderSummary) {
        self.send(GuiResponse::FolderSummary(info));
    }

    pub fn status<S: AsRef<str>>(&self, msg: S, pct: Option<f32>) {
        self.send(GuiResponse::Status {
            status: msg.as_ref().to_owned(),
            pct,
        });
    }

    pub fn paused(&self) {
        self.send(GuiResponse::Paused);
    }

    pub fn resumed(&self) {
        self.send(GuiResponse::Resumed);
    }

    pub fn scanned(&self) {
        self.send(GuiResponse::Scanned);
    }

    pub fn stopped(&self) {
        self.send(GuiResponse::Stopped);
    }

    pub fn compacting(&self) {
        self.send(GuiResponse::Compacting);
    }
}

pub fn format_size(size: u64, decimal: bool) -> String {
    humansize::format_size(
        size,
        if decimal {
            humansize::DECIMAL
        } else {
            humansize::BINARY
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Compact,
    Settings,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Idle,
    Running,
    Paused,
}

struct CompactorApp {
    req: Sender<GuiRequest>,
    resp: Receiver<GuiResponse>,
    page: Page,
    folder: Option<PathBuf>,
    summary: FolderSummary,
    status: String,
    pct: Option<f32>,
    job_state: JobState,
    partial: bool,
    decimal: bool,
    compression: Compression,
    excludes: String,
    excludes_stale: bool,
    settings_error: Option<String>,
    version: String,
    build_date: &'static str,
}

impl CompactorApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        req: Sender<GuiRequest>,
        resp: Receiver<GuiResponse>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();

        if let Err(err) = ctrlc::set_handler(move || {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }) {
            eprintln!("Unable to install Ctrl-C handler: {err}");
        }

        let (current, load_error) = {
            let config = config().read().unwrap();
            (config.current(), config.load_error().map(str::to_owned))
        };

        Self {
            req,
            resp,
            page: Page::Compact,
            folder: None,
            summary: FolderSummary::default(),
            status: String::new(),
            pct: None,
            job_state: JobState::Idle,
            partial: false,
            decimal: current.decimal,
            compression: current.compression,
            excludes: current.excludes.join("\n"),
            excludes_stale: false,
            settings_error: load_error,
            version: format!("{}-{}", env!("CARGO_PKG_VERSION"), short_sha()),
            build_date: option_env!("VERGEN_BUILD_DATE").unwrap_or("unknown"),
        }
    }

    fn request(&mut self, req: GuiRequest) {
        if self.job_state == JobState::Idle {
            self.job_state = JobState::Running;
        }
        let _ = self.req.send(req);
    }

    fn handle(&mut self, msg: GuiResponse) {
        match msg {
            GuiResponse::Folder(path) => {
                self.folder = Some(path);
                self.summary = FolderSummary::default();
                self.pct = None;
                self.job_state = JobState::Running;
                self.partial = false;
                self.excludes_stale = false;
            }
            GuiResponse::Status { status, pct } => {
                self.status = status;
                self.pct = pct;
            }
            GuiResponse::FolderSummary(info) => self.summary = info,
            GuiResponse::Paused => self.job_state = JobState::Paused,
            GuiResponse::Resumed => {
                if self.job_state == JobState::Paused {
                    self.job_state = JobState::Running;
                }
            }
            GuiResponse::Compacting => self.job_state = JobState::Running,
            GuiResponse::Scanned => {
                self.job_state = JobState::Idle;
                self.partial = false;
            }
            GuiResponse::Stopped => {
                self.job_state = JobState::Idle;
                self.partial = true;
            }
        }
    }

    fn choose_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Select a directory");

        if let Some(dir) = std::env::var_os("ProgramFiles") {
            dialog = dialog.set_directory(dir);
        }

        if let Some(path) = dialog.pick_folder() {
            self.request(GuiRequest::FolderChosen(path));
        }
    }

    fn save_config(&mut self) {
        let new = Config {
            decimal: self.decimal,
            compression: self.compression,
            excludes: self.excludes.lines().map(str::to_owned).collect(),
        };

        match new.globset() {
            Ok(_) => {
                let mut current = config().write().unwrap();
                let changed = current.current().excludes != new.excludes;
                current.replace(new);
                match current.save() {
                    Ok(()) => {
                        self.settings_error = None;
                        self.excludes_stale |= changed;
                    }
                    Err(err) => self.settings_error = Some(format!("Error saving settings: {err}")),
                }
            }
            Err(err) => self.settings_error = Some(err),
        }
    }

    fn reset_config(&mut self) {
        let new = Config::default();
        self.decimal = new.decimal;
        self.compression = new.compression;
        self.excludes = new.excludes.join("\n");
        self.excludes_stale = true;

        let mut current = config().write().unwrap();
        current.replace(new);
        match current.save() {
            Ok(()) => self.settings_error = None,
            Err(err) => self.settings_error = Some(format!("Error saving settings: {err}")),
        }
    }

    fn size(&self, size: u64) -> String {
        format_size(size, self.decimal)
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Compactor");
            ui.separator();
            ui.selectable_value(&mut self.page, Page::Compact, "Compact");
            ui.selectable_value(&mut self.page, Page::Settings, "Settings");
            ui.selectable_value(&mut self.page, Page::About, "About");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} / {}", self.version, self.build_date))
                        .weak()
                        .small(),
                );
            });
        });
    }

    fn compact_page(&mut self, ui: &mut egui::Ui) {
        let label = match &self.folder {
            Some(path) => path.display().to_string(),
            None => "Choose a folder…".to_owned(),
        };

        ui.vertical_centered(|ui| {
            let width = ui.available_width();
            let text_width = width - 2.0 * ui.spacing().button_padding.x;
            let display_label = truncate_from_start(
                ui,
                &label,
                text_width,
                &egui::TextStyle::Button.resolve(ui.style()),
            );
            let truncated = display_label != label;
            let button = egui::Button::new(display_label)
                .truncate()
                .min_size(egui::vec2(width, 32.0));

            let response = ui.add_enabled(self.job_state == JobState::Idle, button);
            let clicked = response.clicked();
            if truncated {
                response.on_hover_text(&label);
            }

            if clicked {
                self.choose_folder();
            }
        });

        if self.folder.is_none() {
            return;
        }

        if !self.status.is_empty() {
            ui.add_space(8.0);
            let display_status = truncate_status(ui, &self.status, ui.available_width());
            let truncated = display_status != self.status;
            let response = ui.add(egui::Label::new(display_status).truncate());
            if truncated {
                response.on_hover_text(&self.status);
            }
        }

        if self.partial {
            ui.colored_label(
                Color32::YELLOW,
                "Partial analysis. Analyse again for complete results.",
            );
        }

        if let Some(pct) = self.pct {
            ui.add(egui::ProgressBar::new(pct).show_percentage());
        } else if self.job_state != JobState::Idle {
            ui.add(egui::ProgressBar::new(0.0).animate(true));
        }

        ui.add_space(4.0);

        if self.excludes_stale {
            ui.colored_label(
                Color32::YELLOW,
                "Exclusions changed. Analyse again before compressing.",
            );
        }

        ui.horizontal(|ui| {
            if self.job_state == JobState::Idle {
                if ui.button("Analyse").clicked() {
                    self.request(GuiRequest::Analyse);
                }

                if self.summary.compressible.count > 0
                    && ui
                        .add_enabled(!self.excludes_stale, egui::Button::new("Compress"))
                        .clicked()
                {
                    self.request(GuiRequest::Compress);
                }

                if self.summary.compressed.count > 0
                    && ui
                        .add_enabled(!self.excludes_stale, egui::Button::new("Decompress"))
                        .clicked()
                {
                    self.request(GuiRequest::Decompress);
                }
            } else {
                if self.job_state == JobState::Paused {
                    if ui.button("Resume").clicked() {
                        self.request(GuiRequest::Resume);
                    }
                } else if ui.button("Pause").clicked() {
                    self.request(GuiRequest::Pause);
                }

                if ui.button("Stop").clicked() {
                    self.request(GuiRequest::Stop);
                }
            }
        });

        ui.separator();
        self.analysis(ui);
    }

    fn analysis(&self, ui: &mut egui::Ui) {
        let summary = &self.summary;
        let saved = summary
            .compressed
            .logical_size
            .saturating_sub(summary.compressed.physical_size);
        let ratio_hundredths = if summary.logical_size > 0 {
            let numerator = u128::from(summary.physical_size) * 100;
            let denominator = u128::from(summary.logical_size);
            let quotient = numerator / denominator;
            let remainder = numerator % denominator;
            let round_up = remainder > denominator - remainder
                || (remainder == denominator - remainder && quotient % 2 == 1);
            if round_up { quotient + 1 } else { quotient }
        } else {
            100
        };

        egui::Grid::new("summary")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                summary_row(
                    ui,
                    Color32::WHITE,
                    format!(
                        "{} of {} saved ({}.{:02}x, {} on-disk)",
                        self.size(saved),
                        self.size(summary.logical_size),
                        ratio_hundredths / 100,
                        ratio_hundredths % 100,
                        self.size(summary.physical_size)
                    ),
                );
                summary_row(
                    ui,
                    Color32::GREEN,
                    format!(
                        "{} in {} compressed",
                        self.size(summary.compressed.physical_size),
                        summary.compressed.count
                    ),
                );
                summary_row(
                    ui,
                    Color32::from_rgb(0x00, 0x79, 0xd6),
                    format!(
                        "{} in {} compressible",
                        self.size(summary.compressible.physical_size),
                        summary.compressible.count
                    ),
                );
                summary_row(
                    ui,
                    Color32::ORANGE,
                    format!(
                        "{} in {} skipped",
                        self.size(summary.skipped.physical_size),
                        summary.skipped.count
                    ),
                );
                summary_row(
                    ui,
                    Color32::LIGHT_RED,
                    format!(
                        "{} in {} failed",
                        self.size(summary.failed.physical_size),
                        summary.failed.count
                    ),
                );
            });

        self.breakdown(ui);
    }

    fn breakdown(&self, ui: &mut egui::Ui) {
        let total = self.summary.logical_size;

        if total == 0 {
            return;
        }

        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
        let painter = ui.painter();

        let segments = [
            (self.summary.compressed.physical_size, Color32::GREEN),
            (
                self.summary.compressible.physical_size,
                Color32::from_rgb(0x00, 0x79, 0xd6),
            ),
            (self.summary.skipped.physical_size, Color32::ORANGE),
            (self.summary.failed.physical_size, Color32::LIGHT_RED),
        ];

        let mut x = rect.left();
        for (size, color) in segments {
            let scaled_ratio =
                u128::from(size.min(total)) * u128::from(u16::MAX) / u128::from(total);
            let scaled_ratio = u16::try_from(scaled_ratio).unwrap_or(u16::MAX);
            let width = rect.width() * f32::from(scaled_ratio) / f32::from(u16::MAX);
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(width, rect.height()),
                ),
                egui::CornerRadius::ZERO,
                color,
            );
            x += width;
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.label("File patterns to exclude from compression");

        ui.add_enabled_ui(self.job_state == JobState::Idle, |ui| {
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.excludes)
                            .desired_width(f32::INFINITY)
                            .desired_rows(12)
                            .font(egui::TextStyle::Monospace),
                    );
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Compression");
                egui::ComboBox::from_id_salt("compression")
                    .selected_text(self.compression.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.compression,
                            Compression::Xpress4k,
                            "XPRESS4K (fast, low compression)",
                        );
                        ui.selectable_value(
                            &mut self.compression,
                            Compression::Xpress8k,
                            "XPRESS8K (default)",
                        );
                        ui.selectable_value(
                            &mut self.compression,
                            Compression::Xpress16k,
                            "XPRESS16K",
                        );
                        ui.selectable_value(
                            &mut self.compression,
                            Compression::Lzx,
                            "LZX (slow, high compression)",
                        );
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Units");
                ui.selectable_value(&mut self.decimal, false, "Binary (MiB)");
                ui.selectable_value(&mut self.decimal, true, "Decimal (MB)");
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.save_config();
                }

                if ui.button("Reset").clicked() {
                    self.reset_config();
                }
            });
        });

        if self.job_state != JobState::Idle {
            ui.colored_label(
                Color32::YELLOW,
                "Settings cannot be changed while a job is running.",
            );
        }

        if let Some(err) = &self.settings_error {
            ui.colored_label(Color32::LIGHT_RED, err);
        }
    }

    fn about_page(ui: &mut egui::Ui) {
        ui.heading("FWUCKY EDITION OF REGRET");
        ui.label(RichText::new("Backups are for wimps.").strong());
        ui.add_space(8.0);

        ui.label("A graphical interface to Windows' transparent filesystem compression.");
        ui.label(
            "For use with files and programs that rarely change. Any file modifications will \
             undo the compression for that file, so re-running this tool periodically is a \
             good idea.",
        );

        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Written in");
            link(ui, "Rust", "https://www.rust-lang.org/");
            ui.label("by");
            link(ui, "Cirqup", "https://github.com/crqp");
            ui.label("at");
            link(ui, "crqp/Compactor", "https://github.com/crqp/Compactor");
        });

        ui.horizontal(|ui| {
            ui.label("Originally by");
            link(ui, "Thomas Hurst", "https://hur.st/");
            ui.label("at");
            link(
                ui,
                "Freaky/Compactor",
                "https://github.com/Freaky/Compactor",
            );
        });

        ui.horizontal(|ui| {
            ui.label("GUI provided by");
            link(ui, "egui", "https://egui.rs/");
        });
    }
}

impl eframe::App for CompactorApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut disconnected = false;

        loop {
            match self.resp.try_recv() {
                Ok(msg) => self.handle(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if self.job_state != JobState::Idle {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if disconnected {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| self.header(ui));

        egui::CentralPanel::default().show(ui, |ui| match self.page {
            Page::Compact => self.compact_page(ui),
            Page::Settings => self.settings_page(ui),
            Page::About => Self::about_page(ui),
        });
    }
}

fn truncate_status(ui: &egui::Ui, status: &str, max_width: f32) -> String {
    let font_id = egui::TextStyle::Body.resolve(ui.style());

    if let Some((prefix, path, suffix)) = status_path_parts(status) {
        let prefix_width = text_width(ui, prefix, &font_id);
        let suffix_width = text_width(ui, suffix, &font_id);
        let path_width = max_width - prefix_width - suffix_width;
        if path_width > 0.0 {
            let path = truncate_from_start(ui, path, path_width, &font_id);
            let display = format!("{prefix}{path}{suffix}");
            if text_width(ui, &display, &font_id) <= max_width {
                return display;
            }
        }
    }

    truncate_from_start(ui, status, max_width, &font_id)
}

fn status_path_parts(status: &str) -> Option<(&str, &str, &str)> {
    const EXCLUDED_SUFFIX: &str = " is excluded by the current patterns";

    for prefix in [
        "Scanning: ",
        "Compacting: ",
        "Expanding: ",
        "Pausing after ",
        "Stopping after ",
    ] {
        if let Some(path) = status.strip_prefix(prefix) {
            return Some((prefix, path, ""));
        }
    }

    if let Some(path) = status.strip_suffix(EXCLUDED_SUFFIX) {
        return Some(("", path, EXCLUDED_SUFFIX));
    }

    if status.starts_with("Error: ") {
        let delimiter = status.rfind(", ")?;
        return Some((&status[..delimiter + 2], &status[delimiter + 2..], ""));
    }

    None
}

fn truncate_from_start(
    ui: &egui::Ui,
    text: &str,
    max_width: f32,
    font_id: &egui::FontId,
) -> String {
    const ELLIPSIS: &str = "…";

    if text_width(ui, text, font_id) <= max_width {
        return text.to_owned();
    }

    if text_width(ui, ELLIPSIS, font_id) > max_width {
        return String::new();
    }

    let boundaries: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    let mut low = 0;
    let mut high = boundaries.len();

    while low < high {
        let mid = low + (high - low) / 2;
        let candidate = format!("{ELLIPSIS}{}", &text[boundaries[mid]..]);
        if text_width(ui, &candidate, font_id) <= max_width {
            high = mid;
        } else {
            low = mid + 1;
        }
    }

    let start = boundaries.get(low).copied().unwrap_or(text.len());
    format!("{ELLIPSIS}{}", &text[start..])
}

fn text_width(ui: &egui::Ui, text: &str, font_id: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id.clone(), Color32::PLACEHOLDER)
        .size()
        .x
}

fn summary_row(ui: &mut egui::Ui, color: Color32, text: String) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, color);
    ui.label(text);
    ui.end_row();
}

fn link(ui: &mut egui::Ui, label: &str, url: &str) {
    ui.hyperlink_to(label, url);
}

fn load_icon() -> egui::IconData {
    let image = image::load_from_memory_with_format(
        include_bytes!("../compact.ico"),
        image::ImageFormat::Ico,
    )
    .expect("compact.ico")
    .into_rgba8();
    let (width, height) = image.dimensions();

    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn short_sha() -> &'static str {
    match option_env!("VERGEN_GIT_SHA") {
        Some(sha) => sha.get(..8).unwrap_or(sha),
        None => "unknown",
    }
}

pub fn run() {
    persistence::init();

    let (req_tx, req_rx) = crossbeam_channel::bounded::<GuiRequest>(128);
    let (resp_tx, resp_rx) = crossbeam_channel::bounded::<GuiResponse>(128);

    let backend = std::thread::spawn(move || {
        Backend::new(GuiWrapper::new(resp_tx), req_rx).run();
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Compactor")
            .with_icon(load_icon())
            .with_inner_size([680.0, 420.0])
            .with_min_inner_size([560.0, 400.0])
            .with_resizable(true),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Compactor",
        options,
        Box::new(move |cc| Ok(Box::new(CompactorApp::new(cc, req_tx, resp_rx)))),
    );

    if let Err(err) = result {
        eprintln!("GUI error: {err}");
    }

    if backend.join().is_err() {
        eprintln!("Backend thread panicked");
    }
}
