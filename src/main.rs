// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Multiwave Splitter - split multichannel WAV files into mono/stereo files.
//! Copyright (C) 2026 axelsharper. Licensed under GPL-3.0-or-later.

use eframe::egui::{self, Color32, RichText};
use multiwave_splitter::split::{self, default_base, plan_outputs, OutputSpec};
use multiwave_splitter::wav::{self, WavInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

const APP_NAME: &str = "Multiwave Splitter";
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        // Matches the Linux .desktop file name so panels show the right icon.
        .with_app_id("multiwave-splitter")
        .with_inner_size([560.0, 640.0])
        .with_min_inner_size([460.0, 420.0])
        .with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |_cc| {
            let mut app = App::default();
            if let Some(p) = initial {
                app.open(p);
            }
            Ok(Box::new(app))
        }),
    )
}

struct Loaded {
    path: PathBuf,
    info: WavInfo,
    selected: Vec<bool>,
    /// Stereo Link flag per channel: links the channel with the one below.
    link: Vec<bool>,
}

enum JobResult {
    Done { count: usize, folder: PathBuf },
    Cancelled,
    Failed(String),
}

struct Job {
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    total: u64,
    started: Instant,
    rx: mpsc::Receiver<JobResult>,
}

enum Status {
    None,
    Info(String),
    Success(String, PathBuf),
    Error(String),
}

struct App {
    loaded: Option<Loaded>,
    base_override: Option<PathBuf>,
    job: Option<Job>,
    status: Status,
    /// File names awaiting an "overwrite?" confirmation.
    confirm_overwrite: Option<Vec<String>>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            loaded: None,
            base_override: None,
            job: None,
            status: Status::None,
            confirm_overwrite: None,
        }
    }
}

fn format_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn reveal_in_file_manager(folder: &Path) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(folder).spawn();
}

impl App {
    fn open(&mut self, path: PathBuf) {
        if self.job.is_some() {
            return;
        }
        match wav::read_info(&path) {
            Ok(info) => {
                let ch = info.format.channels as usize;
                self.loaded = Some(Loaded {
                    path,
                    selected: vec![true; ch],
                    link: split::default_links(ch as u16),
                    info,
                });
                self.base_override = None;
                self.status = Status::None;
            }
            Err(e) => {
                self.status = Status::Error(format!("Could not open {}: {e:#}", file_name(&path)));
            }
        }
    }

    fn base(&self) -> Option<PathBuf> {
        let l = self.loaded.as_ref()?;
        Some(
            self.base_override
                .clone()
                .unwrap_or_else(|| default_base(&l.path)),
        )
    }

    fn outputs(&self) -> Vec<OutputSpec> {
        match (&self.loaded, self.base()) {
            (Some(l), Some(base)) => {
                plan_outputs(&base, l.info.format.channels, &l.selected, &l.link)
            }
            _ => Vec::new(),
        }
    }

    fn browse_input(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .set_title("Choose a multichannel WAV file")
            .add_filter("WAV audio", &["wav", "WAV", "rf64", "bw64"]);
        if let Some(dir) = self.loaded.as_ref().and_then(|l| l.path.parent()) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(p) = dlg.pick_file() {
            self.open(p);
        }
    }

    fn browse_output(&mut self) {
        let Some(base) = self.base() else { return };
        let mut dlg = rfd::FileDialog::new()
            .set_title("Choose output folder and base name")
            .set_file_name(file_name(&base));
        if let Some(dir) = base.parent() {
            dlg = dlg.set_directory(dir);
        }
        if let Some(p) = dlg.save_file() {
            self.base_override = Some(split::base_from_user_path(&p));
        }
    }

    fn start_split(&mut self, ctx: &egui::Context, overwrite_ok: bool) {
        let Some(l) = &self.loaded else { return };
        let outputs = self.outputs();
        if outputs.is_empty() {
            self.status = Status::Error("Select at least one channel.".into());
            return;
        }

        let input = l.path.clone();
        if outputs.iter().any(|o| same_file(&o.path, &input)) {
            self.status = Status::Error(
                "An output file would overwrite the input file. Choose another name.".into(),
            );
            return;
        }

        if !overwrite_ok {
            let existing: Vec<String> = outputs
                .iter()
                .filter(|o| o.path.exists())
                .map(|o| file_name(&o.path))
                .collect();
            if !existing.is_empty() {
                self.confirm_overwrite = Some(existing);
                return;
            }
        }

        let progress = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let folder = outputs[0]
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        {
            let progress = progress.clone();
            let cancel = cancel.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let res = split::split(&input, &outputs, &progress, &cancel);
                let msg = match res {
                    Ok(()) => JobResult::Done {
                        count: outputs.len(),
                        folder,
                    },
                    Err(e) if e.downcast_ref::<split::Cancelled>().is_some() => {
                        JobResult::Cancelled
                    }
                    Err(e) => JobResult::Failed(format!("{e:#}")),
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            });
        }
        self.job = Some(Job {
            progress,
            cancel,
            total: l.info.frames(),
            started: Instant::now(),
            rx,
        });
        self.status = Status::None;
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        if let Ok(res) = job.rx.try_recv() {
            self.status = match res {
                JobResult::Done { count, folder } => Status::Success(
                    format!("Done! Created {count} file(s) in {}", folder.display()),
                    folder,
                ),
                JobResult::Cancelled => {
                    Status::Info("Split cancelled. No files were changed.".into())
                }
                JobResult::Failed(e) => Status::Error(e),
            };
            self.job = None;
        }
    }

    fn channel_list(&mut self, ui: &mut egui::Ui) {
        const ROW_H: f32 = 30.0;
        const GUTTER: f32 = 22.0;
        const CH_W: f32 = 92.0;
        const LINK_W: f32 = 132.0;

        let busy = self.job.is_some();
        let Some(l) = &mut self.loaded else { return };
        let n = l.info.format.channels;

        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if ui.button("Select all").clicked() {
                    l.selected.iter_mut().for_each(|s| *s = true);
                }
                if ui.button("Select none").clicked() {
                    l.selected.iter_mut().for_each(|s| *s = false);
                }
                ui.separator();
                if ui.button("All stereo").clicked() {
                    l.link = split::default_links(n);
                }
                if ui.button("All mono").clicked() {
                    l.link.iter_mut().for_each(|s| *s = false);
                }
            });
        });
        ui.add_space(6.0);

        // Resolve the current state once per frame; edits apply on the next frame.
        let groups = split::link_groups(n, &l.link);
        let linkable: Vec<bool> = (0..n).map(|c| split::can_link(n, &l.link, c)).collect();
        let mut group_of = vec![0usize; n as usize];
        let mut row_text: Vec<Option<String>> = vec![None; n as usize];
        for (gi, g) in groups.iter().enumerate() {
            group_of[g.first as usize] = gi;
            if let Some(b) = g.second {
                group_of[b as usize] = gi;
            }
            for out in split::group_outputs(*g, &l.selected) {
                row_text[out[0] as usize] = Some(split::channel_label(&out));
                if out.len() == 2 {
                    row_text[out[1] as usize] = Some(String::new());
                }
            }
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    let cell = |row: egui::Rect, x: f32, w: f32| {
                        egui::Rect::from_min_size(
                            egui::pos2(row.left() + x, row.top()),
                            egui::vec2(w, row.height()),
                        )
                    };
                    let left_aligned = egui::Layout::left_to_right(egui::Align::Center);

                    // Header
                    let (hdr, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 18.0),
                        egui::Sense::hover(),
                    );
                    for (x, w, text) in [
                        (GUTTER, CH_W, "Channel"),
                        (GUTTER + CH_W + LINK_W, 200.0, "Output file"),
                    ] {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(cell(hdr, x, w))
                                .layout(left_aligned),
                            |ui| {
                                ui.label(RichText::new(text).weak().small());
                            },
                        );
                    }

                    let mut rows = Vec::with_capacity(n as usize);
                    for ch in 0..n as usize {
                        let (row, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), ROW_H),
                            egui::Sense::hover(),
                        );
                        if group_of[ch].is_multiple_of(2) {
                            ui.painter().rect_filled(
                                row.shrink2(egui::vec2(0.0, 1.0)),
                                4.0,
                                ui.visuals().faint_bg_color,
                            );
                        }
                        rows.push(row);

                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(cell(row, GUTTER, CH_W))
                                .layout(left_aligned),
                            |ui| {
                                ui.checkbox(&mut l.selected[ch], format!("CH {}", ch + 1));
                            },
                        );
                        if linkable[ch] {
                            ui.scope_builder(
                                egui::UiBuilder::new()
                                    .max_rect(cell(row, GUTTER + CH_W, LINK_W))
                                    .layout(left_aligned),
                                |ui| {
                                    ui.checkbox(&mut l.link[ch], "Stereo Link").on_hover_text(
                                        format!(
                                            "Make CH {} and CH {} one stereo file",
                                            ch + 1,
                                            ch + 2
                                        ),
                                    );
                                },
                            );
                        }
                        let out_rect = cell(
                            row,
                            GUTTER + CH_W + LINK_W,
                            (row.width() - GUTTER - CH_W - LINK_W).max(40.0),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(out_rect)
                                .layout(left_aligned),
                            |ui| {
                                match &row_text[ch] {
                                    Some(t) => ui.label(RichText::new(t).monospace()),
                                    None => ui.label(RichText::new("skipped").weak().italics()),
                                };
                            },
                        );
                    }

                    // Stereo brackets with the "oo" link badge. All sizes are in
                    // points, so they scale with the UI on every display density;
                    // line positions snap to pixel centres to stay crisp at 1x.
                    let accent = Color32::from_rgb(0x2f, 0x6f, 0xeb);
                    let painter = ui.painter();
                    for (gi, g) in groups.iter().enumerate() {
                        let Some(b) = g.second else { continue };
                        let (r1, r2) = (rows[g.first as usize], rows[b as usize]);
                        let active = l.selected[g.first as usize] && l.selected[b as usize];
                        let color = if active {
                            accent
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        let x = painter.round_to_pixel_center(r1.left() + 9.0);
                        let tip = r1.left() + GUTTER - 4.0;
                        let y1 = painter.round_to_pixel_center(r1.center().y);
                        let y2 = painter.round_to_pixel_center(r2.center().y);
                        painter.line(
                            vec![
                                egui::pos2(tip, y1),
                                egui::pos2(x, y1),
                                egui::pos2(x, y2),
                                egui::pos2(tip, y2),
                            ],
                            egui::Stroke::new(1.5, color),
                        );
                        // Badge: gap in the line behind two small linked rings.
                        let mid = egui::pos2(x, (y1 + y2) / 2.0);
                        let gap = egui::Rect::from_center_size(mid, egui::vec2(12.0, 9.0));
                        painter.rect_filled(gap, 2.0, ui.visuals().panel_fill);
                        if gi.is_multiple_of(2) {
                            painter.rect_filled(gap, 2.0, ui.visuals().faint_bg_color);
                        }
                        let ring = egui::Stroke::new(1.2, color);
                        painter.circle_stroke(mid - egui::vec2(2.2, 0.0), 2.8, ring);
                        painter.circle_stroke(mid + egui::vec2(2.2, 0.0), 2.8, ring);
                    }
                });
            });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let outputs = self.outputs();

        if let Some(base) = self.base() {
            let dir = base
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save as:").strong());
                ui.label(RichText::new(format!("{} CH ….wav", file_name(&base))).monospace());
            });
            ui.add(egui::Label::new(RichText::new(format!("in {dir}")).weak()).truncate())
                .on_hover_text(&dir);
            ui.label(
                RichText::new(format!("{} file(s) will be created.", outputs.len()))
                    .weak()
                    .small(),
            );
            ui.add_space(6.0);
        }

        if let Some(job) = &self.job {
            let done = job.progress.load(Ordering::Relaxed);
            let frac = if job.total > 0 {
                done as f32 / job.total as f32
            } else {
                0.0
            };
            let elapsed = job.started.elapsed().as_secs_f64();
            let eta = if frac > 0.02 {
                format!(
                    " · {} left",
                    format_duration(elapsed / frac as f64 - elapsed)
                )
            } else {
                String::new()
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .show_percentage()
                    .text(format!("{:.0}%{eta}", frac * 100.0)),
            );
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            let busy = self.job.is_some();
            let has_file = self.loaded.is_some();
            if ui
                .add_enabled(has_file && !busy, egui::Button::new("Browse…").min_size([90.0, 30.0].into()))
                .on_hover_text("Choose output folder and base file name.\nDefault: same folder and name as the input.")
                .clicked()
            {
                self.browse_output();
            }
            if self.base_override.is_some()
                && ui.add_enabled(!busy, egui::Button::new("Reset").min_size([60.0, 30.0].into())).clicked()
            {
                self.base_override = None;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if busy {
                    if ui.add(egui::Button::new("Cancel").min_size([110.0, 30.0].into())).clicked() {
                        if let Some(job) = &self.job {
                            job.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                } else {
                    let split_btn = egui::Button::new(RichText::new("Split").strong().size(16.0).color(Color32::WHITE))
                        .fill(Color32::from_rgb(0x2f, 0x6f, 0xeb))
                        .min_size([110.0, 30.0].into());
                    if ui.add_enabled(has_file && !outputs.is_empty(), split_btn).clicked() {
                        let ctx = ui.ctx().clone();
                        self.start_split(&ctx, false);
                    }
                }
            });
        });

        let mut reveal = None;
        match &self.status {
            Status::None => {}
            Status::Info(s) => {
                ui.add_space(6.0);
                ui.label(s);
            }
            Status::Error(s) => {
                ui.add_space(6.0);
                ui.label(RichText::new(s).color(ui.visuals().error_fg_color));
            }
            Status::Success(s, folder) => {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(s).color(Color32::from_rgb(0x2e, 0xa0, 0x43)));
                    if ui.link("Open folder").clicked() {
                        reveal = Some(folder.clone());
                    }
                });
            }
        }
        if let Some(f) = reveal {
            reveal_in_file_manager(&f);
        }
        ui.add_space(8.0);
    }
}

impl App {
    fn overwrite_dialog(&mut self, ctx: &egui::Context) {
        let Some(existing) = &self.confirm_overwrite else {
            return;
        };
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("overwrite")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading("Overwrite files?");
            ui.add_space(4.0);
            ui.label(format!(
                "{} file(s) already exist and will be replaced:",
                existing.len()
            ));
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for name in existing {
                        ui.label(RichText::new(name).monospace());
                    }
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Overwrite").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        if modal.should_close() && choice.is_none() {
            choice = Some(false);
        }
        if let Some(ok) = choice {
            self.confirm_overwrite = None;
            if ok {
                self.start_split(ctx, true);
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job();
        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Drag & drop
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if let Some(p) = dropped.into_iter().next() {
            self.open(p);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        self.overwrite_dialog(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.job.is_none(),
                        egui::Button::new("Open WAV…").min_size([100.0, 28.0].into()),
                    )
                    .clicked()
                {
                    self.browse_input();
                }
                match &self.loaded {
                    Some(l) => {
                        ui.label(RichText::new(file_name(&l.path)).strong().size(15.0))
                            .on_hover_text(l.path.display().to_string());
                    }
                    None => {
                        ui.label(RichText::new("No file loaded").weak());
                    }
                }
            });
            if let Some(l) = &self.loaded {
                let f = &l.info.format;
                ui.label(
                    RichText::new(format!(
                        "{} channels · {} Hz · {} · {} · {}",
                        f.channels,
                        f.sample_rate,
                        f.describe_encoding(),
                        format_duration(l.info.duration_secs()),
                        format_size(l.info.file_len),
                    ))
                    .weak(),
                );
            }
            ui.add_space(8.0);
        });

        egui::Panel::bottom("bottom").show(ui, |ui| self.bottom_bar(ui));

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if self.loaded.is_some() && !hovering {
                self.channel_list(ui);
            } else {
                ui.centered_and_justified(|ui| {
                    let text = if hovering {
                        "Drop to open"
                    } else {
                        "Drop a multichannel WAV file here\nor click “Open WAV…”"
                    };
                    ui.label(RichText::new(text).size(18.0).weak());
                });
            }
        });
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use multiwave_splitter::wav::{SampleKind, WavFormat, WavWriter};

    #[test]
    fn click_through_split() {
        let dir = std::env::temp_dir().join(format!("mws-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("Gig.wav");
        let fmt = WavFormat {
            channels: 5,
            sample_rate: 48000,
            container_bits: 24,
            valid_bits: 24,
            kind: SampleKind::Int,
        };
        let mut w = WavWriter::create(&input, fmt).unwrap();
        w.write(&vec![7u8; 4800 * 15]).unwrap();
        w.finalize().unwrap();

        let path = input.clone();
        let mut harness = Harness::builder()
            .with_size([560.0, 640.0])
            .build_eframe(move |_cc| {
                let mut app = App::default();
                app.open(path);
                app
            });
        harness.run();

        // Pair CH 3-4 -> two mono files; untick CH 5.
        harness
            .get_all_by_label("Stereo Link")
            .nth(1)
            .unwrap()
            .click();
        harness.get_all_by_label("CH 5").next().unwrap().click(); // the checkbox (the preview label comes after)
        harness.run();
        harness.get_by_label("Split").click();
        harness.step();

        for _ in 0..200 {
            harness.step();
            if harness.state().job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            matches!(harness.state().status, Status::Success(..)),
            "split did not succeed"
        );
        harness.run();

        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(" CH "))
            .collect();
        names.sort();
        assert_eq!(names, ["Gig CH 1-2.wav", "Gig CH 3.wav", "Gig CH 4.wav"]);

        // Splitting again asks before overwriting.
        harness.get_by_label("Split").click();
        harness.run();
        assert_eq!(
            harness.state().confirm_overwrite.as_ref().map(Vec::len),
            Some(3)
        );
        assert!(harness.state().job.is_none());
        harness.get_by_label("Overwrite").click();
        harness.step();
        assert!(harness.state().confirm_overwrite.is_none());
        for _ in 0..200 {
            harness.step();
            if harness.state().job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(harness.state().status, Status::Success(..)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn three_channels_link_two_and_three() {
        let dir = std::env::temp_dir().join(format!("mws-ui3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("Three.wav");
        let fmt = WavFormat {
            channels: 3,
            sample_rate: 48000,
            container_bits: 16,
            valid_bits: 16,
            kind: SampleKind::Int,
        };
        let mut w = WavWriter::create(&input, fmt).unwrap();
        w.write(&vec![1u8; 480 * 6]).unwrap();
        w.finalize().unwrap();

        let path = input.clone();
        let mut harness = Harness::builder()
            .with_size([560.0, 640.0])
            .build_eframe(move |_cc| {
                let mut app = App::default();
                app.open(path);
                app
            });
        harness.run();

        // Default: only CH 1 offers Stereo Link (1-2 linked, CH 3 is last).
        assert_eq!(harness.get_all_by_label("Stereo Link").count(), 1);
        // Unlink CH 1 -> the option moves to CH 2 as well.
        harness.get_by_label("Stereo Link").click();
        harness.run();
        assert_eq!(harness.get_all_by_label("Stereo Link").count(), 2);
        // Link CH 2 -> CH 1 loses its option (CH 2 is now linked).
        harness
            .get_all_by_label("Stereo Link")
            .nth(1)
            .unwrap()
            .click();
        harness.run();
        assert_eq!(harness.get_all_by_label("Stereo Link").count(), 1);

        let names: Vec<String> = harness
            .state()
            .outputs()
            .iter()
            .map(|o| file_name(&o.path))
            .collect();
        assert_eq!(names, ["Three CH 1.wav", "Three CH 2-3.wav"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
