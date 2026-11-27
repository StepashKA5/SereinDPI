//! Off-thread owner of the bundled DPI bypass.
//!
//! The UI only flips request flags; everything that touches the filesystem,
//! `schtasks` or the network happens here, on a blocking worker. That includes
//! the launch probe: shelling out to `tasklist`/`sc`/`schtasks` while the first
//! frame is being drawn is what made the launch screen appear late.
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use zapret::{Catalog, Paths, Service, ServiceState, render};

/// Scheduled task registered by `setup-zapret.ps1`; runs `run.vbs` elevated
/// without a UAC prompt.
const TASK_NAME: &str = "SereinZapret";
/// Companion task: `schtasks /end` kills PowerShell but not its `winws` child,
/// so stopping the bypass goes through this second hidden task.
const STOP_TASK: &str = "SereinZapretStop";
/// `winws` needs a moment to load WinDivert. Polling every 250 ms keeps the
/// launch short instead of the old 500 ms × 10 wait before the first probe.
const WINWS_START_STEP: Duration = Duration::from_millis(250);
const WINWS_START_TRIES: u32 = 12;
/// The elevated installer registers the task within a second or two, but a cold
/// start with an antivirus scan can take much longer; only a dismissed UAC
/// prompt wastes the whole budget.
const INSTALL_STEP: Duration = Duration::from_millis(500);
const INSTALL_TRIES: u32 = 20;
/// How long a strategy switch waits for the old `winws` to die. The stop task
/// is asynchronous, and starting the next `winws` too early lets its delayed
/// killer take the new process down.
const STOP_STEP: Duration = Duration::from_millis(150);
const STOP_TRIES: u32 = 10;

#[derive(Default)]
pub struct DpiBypass {
	job: Option<Job>,
	prepared: bool,
	/// Elevated one-shot installer (`serein.exe --zapret-install-task`) still running.
	elevating: Option<Child>,
	/// The elevated setup already failed once, so this run does not prompt again.
	install_failed: bool,
	/// The helper task was just registered: start the bypass without another frame.
	relaunch: bool,
	/// The launch worker found no helper task and asked for the elevated setup.
	install_requested: bool,
}

struct Job {
	receiver: Receiver<Outcome>,
	progress: Arc<AtomicU64>,
	total: u64,
}

enum Outcome {
	/// `winws` or an installed zapret service is already filtering traffic.
	AlreadyUp,
	/// The helper task is missing and only the elevated installer can add it.
	NeedsInstall,
	/// The elevated installer finished; `true` when the task is now registered.
	Installed(bool),
	Scanned {
		alive: Option<String>,
		results: Vec<(String, bool)>,
	},
	Connected(bool),
	Failed(String),
}

impl DpiBypass {
	/// Starts the bypass as early as possible in the first logic pass, before the
	/// saved login is polled and the gateway connection is opened.
	pub fn prepare(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
		demo: bool,
	) {
		if demo {
			view.dpi_bypass.supported = false;
			return;
		}
		if self.prepared {
			return;
		}
		self.prepared = true;
		let available = cfg!(windows) && winws_path().is_some();
		view.dpi_bypass.supported = available;
		if !available {
			return;
		}
		let saved = read_state_file();
		view.dpi_bypass.selected = saved.clone();
		view.dpi_bypass.strategy = saved;
		self.begin_launch(ctx, runtime, view);
	}

	pub fn sync(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
		demo: bool,
	) {
		if demo || !view.dpi_bypass.supported {
			return;
		}
		self.poll(view);
		self.poll_elevation(ctx, runtime, view);
		if self.job.is_some() {
			return;
		}
		if std::mem::take(&mut self.relaunch) {
			self.begin_launch(ctx, runtime, view);
			return;
		}
		if std::mem::take(&mut self.install_requested) {
			self.raise_install(ctx, view);
			return;
		}
		let scan = std::mem::take(&mut view.dpi_bypass.scan_requested);
		let toggle = std::mem::take(&mut view.dpi_bypass.toggle_requested);
		if !scan && !toggle {
			return;
		}
		if self.install_failed {
			// A retry after a declined prompt re-checks the helper task instead of
			// failing the same way again.
			self.install_failed = false;
			self.begin_launch(ctx, runtime, view);
			return;
		}
		if scan {
			self.begin_scan(ctx, runtime, view);
		} else {
			self.begin_toggle(ctx, runtime, view);
		}
	}

	fn poll(&mut self, view: &mut ui::MessagingUi) {
		let Some(job) = &self.job else { return };
		view.dpi_bypass.busy = true;
		view.dpi_bypass.progress = (job.total > 0).then(|| {
			(job.progress.load(Ordering::Relaxed) as f32 / job.total as f32).clamp(0.0, 1.0)
		});
		// The worker may report progress and a final outcome on the same channel.
		let mut outcomes = Vec::new();
		let finished = loop {
			match job.receiver.try_recv() {
				Ok(outcome) => outcomes.push(outcome),
				Err(TryRecvError::Empty) => break false,
				Err(TryRecvError::Disconnected) => break true,
			}
		};
		for outcome in outcomes {
			self.apply(outcome, view);
		}
		if finished {
			self.job = None;
			view.dpi_bypass.busy = false;
			view.dpi_bypass.progress = None;
		}
	}

	fn apply(&mut self, outcome: Outcome, view: &mut ui::MessagingUi) {
		match outcome {
			Outcome::AlreadyUp => {
				view.dpi_bypass.connected = true;
				view.dpi_bypass.status = "Обход уже запущен.".to_owned();
			}
			Outcome::NeedsInstall => {
				if self.install_failed {
					view.dpi_bypass.status =
						"Обход не запущен: задача не зарегистрирована. Запусти установщик от администратора."
							.to_owned();
				} else {
					view.dpi_bypass.status =
						"Подтверди запрос администратора — регистрирую обход…".to_owned();
					self.install_requested = true;
				}
			}
			Outcome::Installed(true) => {
				view.dpi_bypass.status = "Запускаю обход DPI…".to_owned();
				self.relaunch = true;
			}
			Outcome::Installed(false) => {
				self.install_failed = true;
				view.dpi_bypass.status =
					"Не удалось зарегистрировать задачу обхода. Запусти установщик от администратора."
						.to_owned();
			}
			Outcome::Scanned {
				alive: Some(id),
				results,
			} => {
				view.dpi_bypass.connected = true;
				view.dpi_bypass.results = results;
				view.dpi_bypass.strategy = Some(id.clone());
				if view.dpi_bypass.selected.is_none() {
					view.dpi_bypass.selected = Some(id.clone());
				}
				view.dpi_bypass.status = format!("Найден рабочий обход: {id}.");
			}
			Outcome::Scanned {
				alive: None,
				results,
			} => {
				view.dpi_bypass.results = results;
				view.dpi_bypass.status =
					"Ни одна стратегия не прошла проверку. Попробуйте позже.".to_owned();
			}
			Outcome::Connected(connected) => {
				view.dpi_bypass.connected = connected;
				view.dpi_bypass.status = if connected {
					"Обход подключён.".to_owned()
				} else {
					"Обход отключён.".to_owned()
				};
			}
			Outcome::Failed(error) => view.dpi_bypass.status = error,
		}
	}

	fn begin_launch(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
	) {
		let catalog = match Catalog::embedded() {
			Ok(catalog) => catalog,
			Err(error) => {
				view.dpi_bypass.status = format!("Каталог стратегий повреждён: {error}");
				return;
			}
		};
		let total = catalog.strategies.len() as u64;
		let progress = Arc::new(AtomicU64::new(0));
		let (send, receive) = mpsc::sync_channel(4);
		let worker_progress = Arc::clone(&progress);
		let ctx = ctx.clone();
		view.dpi_bypass.busy = true;
		view.dpi_bypass.progress = Some(0.0);
		view.dpi_bypass.results.clear();
		view.dpi_bypass.status = "Проверяю и запускаю обход DPI…".to_owned();
		runtime.spawn_blocking(move || {
			let outcome = launch(&catalog, &worker_progress);
			let _ = send.send(outcome);
			ctx.request_repaint();
		});
		self.job = Some(Job {
			receiver: receive,
			progress,
			total,
		});
	}

	/// Watches the elevated installer without spawning anything per frame: when
	/// the helper process exits, the registration is verified on the worker.
	fn poll_elevation(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
	) {
		let status = match self.elevating.as_mut() {
			Some(child) => child.try_wait(),
			None => return,
		};
		match status {
			Ok(None) => ctx.request_repaint_after(Duration::from_millis(400)),
			Ok(Some(_)) => {
				self.elevating = None;
				self.begin_install_check(ctx, runtime, view);
			}
			Err(_) => {
				self.elevating = None;
				self.install_failed = true;
				view.dpi_bypass.busy = false;
				view.dpi_bypass.status = "Установщик обхода не запустился.".to_owned();
			}
		}
	}

	fn begin_install_check(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
	) {
		let (send, receive) = mpsc::sync_channel(1);
		let ctx = ctx.clone();
		view.dpi_bypass.busy = true;
		view.dpi_bypass.progress = None;
		view.dpi_bypass.status = "Проверяю регистрацию задачи обхода…".to_owned();
		runtime.spawn_blocking(move || {
			let mut registered = false;
			for _ in 0..INSTALL_TRIES {
				// The runner is written before the task, so both must be there.
				if task_registered() && state_dir().join("run.vbs").exists() {
					registered = true;
					break;
				}
				std::thread::sleep(INSTALL_STEP);
			}
			let _ = send.send(Outcome::Installed(registered));
			ctx.request_repaint();
		});
		self.job = Some(Job {
			receiver: receive,
			progress: Arc::new(AtomicU64::new(0)),
			total: 0,
		});
	}

	fn begin_scan(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
	) {
		let catalog = match Catalog::embedded() {
			Ok(catalog) => catalog,
			Err(error) => {
				view.dpi_bypass.status = format!("Каталог стратегий повреждён: {error}");
				return;
			}
		};
		let total = catalog.strategies.len() as u64;
		let progress = Arc::new(AtomicU64::new(0));
		let (send, receive) = mpsc::sync_channel(4);
		let worker_progress = Arc::clone(&progress);
		let ctx = ctx.clone();
		let test_all = !view.dpi_bypass.auto;
		view.dpi_bypass.busy = true;
		view.dpi_bypass.progress = Some(0.0);
		view.dpi_bypass.results.clear();
		view.dpi_bypass.status = "Перебираю стратегии…".to_owned();
		runtime.spawn_blocking(move || {
			let outcome = scan(&catalog, &worker_progress, test_all, false);
			let _ = send.send(outcome);
			ctx.request_repaint();
		});
		self.job = Some(Job {
			receiver: receive,
			progress,
			total,
		});
	}

	fn begin_toggle(
		&mut self,
		ctx: &eframe::egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
	) {
		let connect = !view.dpi_bypass.connected;
		let strategy = view
			.dpi_bypass
			.selected
			.clone()
			.or_else(|| view.dpi_bypass.strategy.clone())
			.unwrap_or_else(|| "general".to_owned());
		let (send, receive) = mpsc::sync_channel(1);
		let ctx = ctx.clone();
		view.dpi_bypass.busy = true;
		view.dpi_bypass.status = if connect {
			"Подключаю обход…".to_owned()
		} else {
			"Отключаю обход…".to_owned()
		};
		runtime.spawn_blocking(move || {
			let outcome = toggle(connect, &strategy);
			let _ = send.send(outcome);
			ctx.request_repaint();
		});
		self.job = Some(Job {
			receiver: receive,
			progress: Arc::new(AtomicU64::new(0)),
			total: 0,
		});
	}

	/// Spawns the elevated one-shot installer; the user only sees the UAC prompt.
	fn raise_install(&mut self, ctx: &eframe::egui::Context, view: &mut ui::MessagingUi) {
		let Ok(exe) = std::env::current_exe() else {
			self.install_failed = true;
			view.dpi_bypass.status = "Не удалось перезапустить установщик обхода.".to_owned();
			return;
		};
		let inner = format!(
			"Start-Process -FilePath '{}' -ArgumentList '--zapret-install-task' -Verb RunAs -WindowStyle Hidden",
			exe.display()
		);
		let mut command = std::process::Command::new("powershell.exe");
		#[cfg(windows)]
		{
			use std::os::windows::process::CommandExt;
			command.creation_flags(0x0800_0000);
		}
		command.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &inner]);
		match command.spawn() {
			Ok(child) => {
				self.elevating = Some(child);
				// Keep the page's actions disabled until the registration is checked.
				view.dpi_bypass.busy = true;
				view.dpi_bypass.status =
					"Подтверди запрос администратора — регистрирую обход…".to_owned();
				ctx.request_repaint_after(Duration::from_millis(400));
			}
			Err(_) => {
				self.install_failed = true;
				view.dpi_bypass.status = "Не удалось запустить установщик обхода.".to_owned();
			}
		}
	}
}

/// One launch decision plus, when it can be taken, the bypass itself.
///
/// The remembered strategy goes straight to `winws`: nothing is stopped that is
/// not running and no sweep delays the first protected packet. The catalog is
/// only walked when that strategy does not pass the probe.
fn launch(catalog: &Catalog, progress: &AtomicU64) -> Outcome {
	match policy(
		winws_running(),
		service_running(),
		task_registered(),
		state_dir().join("run.vbs").exists(),
	) {
		Launch::AlreadyUp => Outcome::AlreadyUp,
		Launch::Install => Outcome::NeedsInstall,
		Launch::Start => scan(catalog, progress, false, true),
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Launch {
	AlreadyUp,
	Install,
	Start,
}

/// An already filtering bypass must never be replaced or re-installed, and a
/// missing helper task must be registered before `winws` can start at all.
fn policy(winws: bool, service: bool, task: bool, runner: bool) -> Launch {
	if winws || service {
		Launch::AlreadyUp
	} else if !task || !runner {
		Launch::Install
	} else {
		Launch::Start
	}
}

/// Walks the catalog, applying each strategy in turn until Discord answers.
/// In manual mode every strategy is tested so the page can list all results.
/// `fresh` says nothing is filtering yet, so the first attempt skips the stop
/// that would otherwise run the WMI-based stop task for no reason.
fn scan(catalog: &Catalog, progress: &AtomicU64, test_all: bool, fresh: bool) -> Outcome {
	let Some(paths) = paths() else {
		return Outcome::Failed("Обход: не найдена папка zapret рядом с клиентом".to_owned());
	};
	let mut results: Vec<(String, bool)> = Vec::with_capacity(catalog.strategies.len());
	let mut alive = None;
	// The strategy that worked last time goes first, so a healthy setup costs
	// one probe instead of a full sweep.
	let mut order: Vec<&zapret::Strategy> = catalog.iter().collect();
	if let Some(saved) = read_state_file() {
		order.sort_by_key(|strategy| strategy.id != saved);
	}
	let mut need_stop = !fresh;
	for (index, strategy) in order.into_iter().enumerate() {
		progress.store(index as u64, Ordering::Relaxed);
		let args = render(strategy, &paths, None);
		let mut works = false;
		if write_runner(&args).is_ok() {
			if need_stop {
				stop_winws();
			}
			need_stop = true;
			if run_task().is_ok() {
				let started = wait_for_winws();
				if index == 0 && !started {
					let _ = end_task();
					return Outcome::Failed(failure_detail());
				}
				// Same idea as zapret's own targets.txt: a strategy counts as
				// working only when the Discord endpoints actually answer.
				let (ok, total) = zapret::probe::probe_discord(Duration::from_secs(4));
				works = started && ok * 2 >= total;
			}
		}
		results.push((strategy.id.clone(), works));
		if works && alive.is_none() {
			alive = Some(strategy.id.clone());
			if let Err(error) = write_state_file(&strategy.id) {
				return Outcome::Failed(format!("Обход: {error}"));
			}
			if !test_all {
				break;
			}
		}
	}
	// A working strategy stays up: stopping it here is what left the app
	// without a bypass before login.
	if alive.is_none() {
		let _ = end_task();
	}
	Outcome::Scanned { alive, results }
}

fn toggle(connect: bool, strategy: &str) -> Outcome {
	if !connect {
		return match end_task() {
			Ok(()) => Outcome::Connected(false),
			Err(error) => Outcome::Failed(format!("Обход: {error}")),
		};
	}
	let catalog = match Catalog::embedded() {
		Ok(catalog) => catalog,
		Err(error) => return Outcome::Failed(format!("Обход: {error}")),
	};
	let (Some(chosen), Some(paths)) = (
		catalog
			.strategy(strategy)
			.or_else(|| catalog.strategy("general")),
		paths(),
	) else {
		return Outcome::Failed(
			"Обход: стратегия или папка zapret рядом с клиентом не найдены".to_owned(),
		);
	};
	let args = render(chosen, &paths, None);
	if let Err(error) = write_runner(&args) {
		return Outcome::Failed(format!("Обход: {error}"));
	}
	stop_winws();
	if let Err(error) = run_task() {
		return Outcome::Failed(format!("Обход: {error}"));
	}
	if !wait_for_winws() {
		return Outcome::Failed(failure_detail());
	}
	if let Err(error) = write_state_file(&chosen.id) {
		return Outcome::Failed(format!("Обход: {error}"));
	}
	Outcome::Connected(true)
}

fn wait_for_winws() -> bool {
	for _ in 0..WINWS_START_TRIES {
		if winws_running() {
			return true;
		}
		std::thread::sleep(WINWS_START_STEP);
	}
	false
}

fn install_dir() -> Option<PathBuf> {
	std::env::current_exe()
		.ok()?
		.parent()
		.map(Path::to_path_buf)
}

fn paths() -> Option<Paths> {
	let root = install_dir()?.join("zapret");
	root.join("bin")
		.join("winws.exe")
		.exists()
		.then(|| Paths::from_root(&root))
}

fn winws_path() -> Option<PathBuf> {
	let path = install_dir()?.join("zapret").join("bin").join("winws.exe");
	path.exists().then_some(path)
}

fn state_dir() -> PathBuf {
	let base = std::env::var_os("LOCALAPPDATA")
		.map(PathBuf::from)
		.unwrap_or_else(std::env::temp_dir);
	base.join("serein-zapret")
}

fn write_runner(args: &[String]) -> Result<(), String> {
	let paths = paths().ok_or_else(|| "не найдена папка zapret рядом с клиентом".to_owned())?;
	let dir = state_dir();
	std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
	ensure_lists(args, &paths.lists)?;
	let log = dir.join("winws.log");
	let script = run_script(&paths.bin, &paths.bin.join("winws.exe"), args, &log);
	write_utf16(&dir.join("run.vbs"), &script)?;
	write_stop_script(&dir)
}

/// `winws` exits when a list file it was told to load is missing, and the
/// catalog references Flowseal's empty `-user.txt` files that their launcher
/// creates rather than shipping in the zip. An absent override list means "no
/// user additions", so create it empty instead of failing the whole strategy.
fn ensure_lists(args: &[String], lists: &Path) -> Result<(), String> {
	const FLAGS: [&str; 4] = [
		"--hostlist=",
		"--hostlist-exclude=",
		"--ipset=",
		"--ipset-exclude=",
	];
	for arg in args {
		let Some(value) = FLAGS.iter().find_map(|flag| arg.strip_prefix(flag)) else {
			continue;
		};
		let path = Path::new(value);
		if path.parent() != Some(lists) || path.exists() {
			continue;
		}
		std::fs::File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
	}
	Ok(())
}

/// Arguments joined into one command line, with anything containing a space
/// quoted so `C:\Program Files` survives.
fn quote_args(args: &[String]) -> String {
	args.iter()
		.map(|arg| {
			if arg.contains(' ') {
				format!("\"{arg}\"")
			} else {
				arg.clone()
			}
		})
		.collect::<Vec<_>>()
		.join(" ")
}

/// A VBScript string literal; an embedded quote is written twice.
fn vbs_string(value: &str) -> String {
	format!("\"{}\"", value.replace('"', "\"\""))
}

/// The runner script itself, kept separate so the quoting can be tested.
///
/// `wscript.exe` is a GUI process: it never allocates a console, so nothing
/// flashes when the task starts. The command goes through `cmd.exe` because
/// `sh.Run` hands its line straight to CreateProcess - a bare `>` there would
/// be passed to `winws` as an argument and the launch log would never exist.
fn run_script(bin: &Path, winws: &Path, args: &[String], log: &Path) -> String {
	let command = format!(
		"cmd.exe /c \"\"{}\" {} > \"{}\" 2>&1\"",
		winws.display(),
		quote_args(args),
		log.display()
	);
	format!(
		"Dim sh\r\nSet sh = CreateObject(\"WScript.Shell\")\r\nsh.CurrentDirectory = {bin}\r\nsh.Run {cmd}, 0, False\r\n",
		bin = vbs_string(&bin.display().to_string()),
		cmd = vbs_string(&command),
	)
}

/// Last line `winws` wrote, so a start failure explains itself instead of
/// pointing at a log the user has to open by hand.
fn failure_detail() -> String {
	let path = state_dir().join("winws.log");
	std::fs::read_to_string(&path)
		.ok()
		.and_then(|text| {
			text.lines()
				.rev()
				.find(|line| !line.trim().is_empty())
				.map(str::to_owned)
		})
		.map(|line| format!("winws не запустился: {line}"))
		.unwrap_or_else(|| format!("winws не запустился. Лог: {}", path.display()))
}

/// Silent `winws` killer used by the stop task.
fn write_stop_script(dir: &Path) -> Result<(), String> {
	let script = "Set svc = GetObject(\"winmgmts:\\\\.\\root\\cimv2\")\r\n\
		For Each p In svc.ExecQuery(\"SELECT * FROM Win32_Process WHERE Name='winws.exe'\")\r\n\
		p.Terminate\r\n\
		Next\r\n";
	write_utf16(&dir.join("stop.vbs"), script)
}

/// WScript reads a `.vbs` without a BOM as ANSI, which would mangle Cyrillic
/// paths; UTF-16LE with a BOM is what `wscript.exe` expects for Unicode.
fn write_utf16(path: &Path, script: &str) -> Result<(), String> {
	let mut bytes = vec![0xFF, 0xFE];
	bytes.extend(script.encode_utf16().flat_map(u16::to_le_bytes));
	std::fs::write(path, bytes).map_err(|error| error.to_string())
}
fn write_state_file(strategy: &str) -> Result<(), String> {
	let dir = state_dir();
	std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
	std::fs::write(dir.join("strategy.txt"), strategy).map_err(|error| error.to_string())
}

fn read_state_file() -> Option<String> {
	let text = std::fs::read_to_string(state_dir().join("strategy.txt")).ok()?;
	let trimmed = text.trim().to_owned();
	(!trimmed.is_empty()).then_some(trimmed)
}

fn run_task() -> Result<(), String> {
	schtasks(&["/run", "/tn", TASK_NAME]).map(|_| ())
}

fn end_task() -> Result<(), String> {
	// Kill the elevated winws first (the main task cannot), then the host.
	let _ = run("schtasks", &["/run", "/tn", STOP_TASK]);
	schtasks(&["/end", "/tn", TASK_NAME]).map(|_| ())
}

/// Stops whatever is filtering and waits for `winws` to leave, so the next
/// strategy is not started into the delayed WMI killer of the stop task.
fn stop_winws() {
	let _ = end_task();
	for _ in 0..STOP_TRIES {
		if !winws_running() {
			break;
		}
		std::thread::sleep(STOP_STEP);
	}
}

/// True when `winws` is alive, whether it was started by our task, by an
/// installed zapret service, or by hand. The image name is ASCII, so the
/// localized console output does not matter.
fn winws_running() -> bool {
	run("tasklist", &["/fi", "IMAGENAME eq winws.exe", "/nh"])
		.is_some_and(|output| output.to_ascii_lowercase().contains("winws.exe"))
}

/// Zapret installed as a Windows service (`service.bat` from the Flowseal bundle).
fn service_running() -> bool {
	run("sc.exe", &["query", "zapret"]).is_some_and(|output| output.contains("RUNNING"))
}

fn task_registered() -> bool {
	schtasks(&["/query", "/tn", TASK_NAME]).is_ok()
}

/// Elevated entry point (`serein.exe --zapret-install-task`): writes the runner
/// and registers the hidden task, so later launches never prompt.
pub fn install_task() -> Result<(), String> {
	let catalog = Catalog::embedded().map_err(|error| error.to_string())?;
	let saved = read_state_file();
	let strategy = saved
		.as_deref()
		.and_then(|id| catalog.strategy(id))
		.or_else(|| catalog.strategy("general"))
		.ok_or_else(|| "стратегия не найдена".to_owned())?;
	let paths = paths().ok_or_else(|| "не найдена папка zapret рядом с клиентом".to_owned())?;
	let args = render(strategy, &paths, None);
	write_runner(&args)?;
	write_state_file(&strategy.id)?;

	// `schtasks /sd` parses dates in the system locale, so the date is left out:
	// the one-shot trigger lands at the next midnight and never matters because
	// the app starts the task on demand. `/ru` (unlike an XML principal) also
	// grants the current user the right to run it without elevation.
	let action = format!(
		"wscript.exe //B \"{}\"",
		state_dir().join("run.vbs").display()
	);
	let user = std::env::var("USERNAME").unwrap_or_else(|_| "SYSTEM".to_owned());
	// A task left by an older build may be owned by SYSTEM and not replaceable
	// in place, so drop it first (failing is fine: it may not exist).
	let _ = run("schtasks", &["/delete", "/tn", TASK_NAME, "/f"]);
	let mut command = std::process::Command::new("schtasks");
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		command.creation_flags(0x0800_0000);
	}
	let output = command
		.args([
			"/create", "/tn", TASK_NAME, "/tr", &action, "/sc", "once", "/st", "00:00", "/ru",
			&user, "/rl", "HIGHEST", "/f",
		])
		.output()
		.map_err(|error| error.to_string())?;
	if !output.status.success() {
		return Err(format!(
			"schtasks не смог создать задачу (код {})",
			output.status.code().unwrap_or(-1)
		));
	}
	// Second task that only kills winws: Task Scheduler ends the host process
	// but leaves its child running.
	let stop_action = format!(
		"wscript.exe //B \"{}\"",
		state_dir().join("stop.vbs").display()
	);
	let _ = run("schtasks", &["/delete", "/tn", STOP_TASK, "/f"]);
	let stop = std::process::Command::new("schtasks")
		.args([
			"/create",
			"/tn",
			STOP_TASK,
			"/tr",
			stop_action.as_str(),
			"/sc",
			"once",
			"/st",
			"00:00",
			"/ru",
			&user,
			"/rl",
			"HIGHEST",
			"/f",
		])
		.output()
		.map_err(|error| error.to_string())?;
	if !stop.status.success() {
		return Err("schtasks не смог создать задачу остановки".to_owned());
	}
	Ok(())
}

fn run(program: &str, args: &[&str]) -> Option<String> {
	let mut command = std::process::Command::new(program);
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		command.creation_flags(CREATE_NO_WINDOW);
	}
	command.args(args).output().ok().map(|output| {
		format!(
			"{}{}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		)
	})
}

fn schtasks(args: &[&str]) -> Result<String, String> {
	let mut command = std::process::Command::new("schtasks");
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		command.creation_flags(CREATE_NO_WINDOW);
	}
	let output = command
		.args(args)
		.output()
		.map_err(|error| error.to_string())?;
	if !output.status.success() {
		// Console output is in the OEM code page; surfacing it raw is mojibake.
		return Err(format!(
			"schtasks завершился с кодом {}",
			output.status.code().unwrap_or(-1)
		));
	}
	Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Kept for the future in-app strategy switch: the helper service exposes the
/// same start/stop surface as the scheduled task.
#[allow(dead_code)]
fn service_state() -> ServiceState {
	Service::new(zapret::SERVICE_NAME).state()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn launch_policy_keeps_a_running_bypass_and_installs_before_starting() {
		// Filtering already: never stop or re-install what is up.
		assert_eq!(policy(true, false, false, false), Launch::AlreadyUp);
		assert_eq!(policy(false, true, false, false), Launch::AlreadyUp);
		// Without the helper task (or its runner) `winws` cannot start at all.
		assert_eq!(policy(false, false, false, true), Launch::Install);
		assert_eq!(policy(false, false, true, false), Launch::Install);
		assert_eq!(policy(false, false, true, true), Launch::Start);
	}

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("serein-test-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("scratch dir");
		dir
	}

	#[test]
	fn absent_override_lists_are_created_empty() {
		// winws exits on an ipset it cannot open, and the bundle has not shipped
		// the `-user.txt` overrides since Flowseal moved them into their launcher.
		let root = scratch("lists");
		let lists = root.join("lists");
		let user = lists.join("ipset-exclude-user.txt");
		let existing = lists.join("list-exclude.txt");
		let args = vec![
			format!("--ipset-exclude={}", user.display()),
			format!("--hostlist-exclude={}", existing.display()),
			"--wf-tcp=80,443".to_owned(),
		];
		std::fs::write(&existing, "example.com\n").expect("existing list");
		let _ = std::fs::remove_file(&user);

		ensure_lists(&args, &lists).expect("lists ensured");

		assert!(user.exists(), "missing override list was not created");
		assert_eq!(
			std::fs::read_to_string(&user).expect("created list"),
			"",
			"created list must stay empty"
		);
		assert_eq!(
			std::fs::read_to_string(&existing).expect("existing list"),
			"example.com\n",
			"an existing list must not be truncated"
		);
		let _ = std::fs::remove_dir_all(root);
	}

	#[test]
	fn runner_redirects_winws_output_to_the_log() {
		// sh.Run hands the line to CreateProcess, so a bare `>` would reach
		// winws as an argument and the launch log would stay a lie.
		let script = run_script(
			Path::new(r"C:\app\zapret\bin"),
			Path::new(r"C:\app\zapret\bin\winws.exe"),
			&[
				"--wf-tcp=80,443".to_owned(),
				"--hostlist=C:\\a b\\list.txt".to_owned(),
			],
			Path::new(r"C:\state\winws.log"),
		);
		// An arg with a space keeps its quotes, and every quote is doubled for
		// the VBS string literal.
		assert!(
			script.contains(
				r#"--wf-tcp=80,443 ""--hostlist=C:\a b\list.txt"" > ""C:\state\winws.log"" 2>&1"#
			),
			"quoting or redirect lost: {script}"
		);
		assert!(
			script.ends_with("\"\", 0, False\r\n"),
			"hidden window or line ending lost: {script}"
		);
		assert!(
			script.contains("sh.Run \"cmd.exe /c "),
			"sh.Run must go through cmd.exe: {script}"
		);
	}
}
