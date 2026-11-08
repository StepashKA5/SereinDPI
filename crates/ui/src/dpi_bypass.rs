//! Settings page for the bundled DPI bypass (`winws`).
//!
//! The page only flips request flags; the desktop worker owns the privileged
//! work and writes status, progress and per-strategy results back here.

use crate::design;

impl Default for DpiBypass {
	fn default() -> Self {
		Self {
			auto: true,
			..Self::defaults()
		}
	}
}

impl DpiBypass {
	fn defaults() -> Self {
		Self {
			status: String::new(),
			busy: false,
			progress: None,
			scan_requested: false,
			toggle_requested: false,
			connected: false,
			supported: false,
			auto: true,
			strategy: None,
			selected: None,
			results: Vec::new(),
		}
	}
}

pub struct DpiBypass {
	pub status: String,
	pub busy: bool,
	pub progress: Option<f32>,
	pub scan_requested: bool,
	pub toggle_requested: bool,
	pub connected: bool,
	pub supported: bool,
	/// Automatic mode stops at the first working strategy, manual checks them all.
	pub auto: bool,
	pub strategy: Option<String>,
	/// Strategy that manual mode should connect, or the last good one.
	pub selected: Option<String>,
	/// `(strategy id, works)` for the last sweep, in catalog order.
	pub results: Vec<(String, bool)>,
}

impl DpiBypass {
	pub fn new() -> Self {
		Self::default()
	}
}

impl crate::MessagingUi {
	pub(super) fn dpi_bypass_settings(&mut self, ui: &mut egui::Ui, demo: bool) {
		let colors = design::palette(ui);
		design::card(ui, |ui| {
			ui.spacing_mut().item_spacing.y = 2.0;
			ui.label(
				design::semibold(ui, crate::i18n::translate("dpi-bypass-title"), 17.0)
					.color(colors.text_strong),
			);
			ui.add(
				egui::Label::new(
					egui::RichText::new(&self.dpi_bypass.status)
						.size(13.0)
						.color(colors.muted),
				)
				.truncate(),
			);
			ui.add_space(12.0);
			let supported = self.dpi_bypass.supported;
			let busy = self.dpi_bypass.busy;
			ui.add_enabled_ui(supported && !busy, |ui| {
				ui.horizontal(|ui| {
					ui.spacing_mut().item_spacing.x = 6.0;
					for (auto, key) in [
						(true, "dpi-bypass-mode-auto"),
						(false, "dpi-bypass-mode-manual"),
					] {
						let kind = if self.dpi_bypass.auto == auto {
							design::ButtonKind::Primary
						} else {
							design::ButtonKind::Outline
						};
						if design::button(ui, &crate::i18n::translate(key), kind).clicked() {
							self.dpi_bypass.auto = auto;
						}
					}
				});
				ui.add_space(10.0);
				ui.horizontal(|ui| {
					ui.spacing_mut().item_spacing.x = 8.0;
					let scan_key = if self.dpi_bypass.auto {
						"dpi-bypass-scan"
					} else {
						"dpi-bypass-test-all"
					};
					if design::button(
						ui,
						&crate::i18n::translate(scan_key),
						design::ButtonKind::Outline,
					)
					.clicked()
					{
						self.dpi_bypass.scan_requested = true;
					}
					let key = if self.dpi_bypass.connected {
						"dpi-bypass-disconnect"
					} else {
						"dpi-bypass-connect"
					};
					if design::button(
						ui,
						&crate::i18n::translate(key),
						design::ButtonKind::Primary,
					)
					.clicked()
					{
						self.dpi_bypass.toggle_requested = true;
					}
					if busy {
						ui.add(egui::Spinner::new().size(16.0));
					}
				});
			});
			if let Some(progress) = self.dpi_bypass.progress {
				ui.add_space(10.0);
				ui.add(
					egui::ProgressBar::new(progress)
						.desired_height(6.0)
						.corner_radius(3)
						.fill(colors.accent),
				);
			}
			if !self.dpi_bypass.results.is_empty() {
				ui.add_space(12.0);
				let rows = self.dpi_bypass.results.clone();
				let width = ui.available_width();
				egui::ScrollArea::vertical()
					.max_height(260.0)
					.auto_shrink([false, true])
					.show(ui, |ui| {
						ui.set_width(width);
						ui.spacing_mut().item_spacing.y = 2.0;
						for (id, works) in rows {
							let chosen = self.dpi_bypass.selected.as_deref() == Some(id.as_str());
							let color = if works {
								colors.accent
							} else {
								colors.muted.gamma_multiply(0.7)
							};
							let text = format!("• {id}");
							let response = ui.add_enabled(
								works && !busy,
								egui::Button::new(
									egui::RichText::new(text).size(13.0).color(if chosen {
										colors.text_strong
									} else {
										color
									}),
								)
								.frame(chosen)
								.min_size(egui::vec2(width, 24.0)),
							);
							if response.clicked() {
								self.dpi_bypass.selected = Some(id.clone());
								self.dpi_bypass.strategy = Some(id);
							}
						}
					});
			} else if let Some(strategy) = &self.dpi_bypass.strategy {
				ui.add_space(8.0);
				ui.add(
					egui::Label::new(egui::RichText::new(strategy).size(12.0).color(colors.muted))
						.truncate(),
				);
			}
			if !self.dpi_bypass.supported {
				ui.add_space(10.0);
				design::notice(
					ui,
					design::Level::Warning,
					&crate::i18n::translate("dpi-bypass-unsupported"),
				);
			}
			ui.add_space(14.0);
			ui.horizontal_wrapped(|ui| {
				ui.spacing_mut().item_spacing.x = 4.0;
				ui.label(
					egui::RichText::new(
						"Обход: zapret (bol-van), сборка Flowseal, WinDivert — спасибо авторам.",
					)
					.size(11.0)
					.color(colors.muted.gamma_multiply(0.8)),
				);
				ui.hyperlink_to(
					egui::RichText::new("zapret").size(11.0),
					"https://github.com/bol-van/zapret",
				);
				ui.hyperlink_to(
					egui::RichText::new("Flowseal").size(11.0),
					"https://github.com/Flowseal/zapret-discord-youtube",
				);
				ui.hyperlink_to(
					egui::RichText::new("WinDivert").size(11.0),
					"https://github.com/basil00/WinDivert",
				);
			});
			if demo {
				ui.add_space(10.0);
				design::hint(ui, &crate::i18n::translate("dpi-bypass-demo"));
			}
		});
	}
}
