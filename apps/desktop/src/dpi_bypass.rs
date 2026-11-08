//! Off-thread owner of the bundled DPI bypass.
//!
//! The UI only flips request flags; everything that touches the filesystem,
//! `schtasks` or the network happens here, on a blocking worker.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use zapret::{Catalog, Paths, Service, ServiceState, render};

/// Scheduled task registered by `setup-zapret.ps1`; runs `run.ps1` elevated
/// without a UAC prompt.
const TASK_NAME: &str = "SereinZapret";
/// Companion task: `schtasks /end` kills PowerShell but not its `winws` child,
/// so stopping the bypass goes through this second hidden task.
const STOP_TASK: &str = "SereinZapretStop";

#[derive(Default)]
pub struct DpiBypass {
	job: Option<Job>,
	initialized: bool,
	elevating: bool,
}

struct Job {
	receiver: Receiver<Result<Outcome, String>>,
	progress: Arc<AtomicU64>,
	total: u64,
}

enum Outcome {
	Scanned {
		alive: Option<String>,
		results: Vec<(String, bool)>,
	},
	Connected(bool),
}

impl DpiBypass {
	pub fn sync(
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
		let available = cfg!(windows) && winws_path().is_some();
		view.dpi_bypass.supported = available;
		if !available {
			return;
		}
		if !self.initialized {
			self.initialized = true;
			view.dpi_bypass.strategy = read_state_file();
			view.dpi_bypass.selected = view.dpi_bypass.strategy.clone();
			// A bypass may already be up as our task, as an installed zapret
			// service, or as a hand-started winws: do not fight it.
			view.dpi_bypass.connected = winws_running() || service_running();
			if view.dpi_bypass.connected {
				view.dpi_bypass.status = "Обход уже запущен.".to_owned();
			} else if !task_registered() || !state_dir().join("run.vbs").exists() {
				// No manual scripts for end users: raise the UAC prompt ourselves
				// and let the elevated process register the helper task.
				view.dpi_bypass.status =
					"Подтверди запрос администратора — регистрирую обход…".to_owned();
				if !self.elevating {
					self.elevating = true;
					elevate_install();
				}
			} else {
				// Connect automatically on launch instead of waiting for a click.
				view.dpi_bypass.status = "Ищу рабочий обход…".to_owned();
				view.dpi_bypass.scan_requested = true;
			}
		}

		self.poll(view);

		if self.elevating && task_registered() {
			self.elevating = false;
			view.dpi_bypass.status = "Ищу рабочий обход…".to_owned();
			view.dpi_bypass.scan_requested = true;
		}

		if self.job.is_some() {
			return;
		}
		let scan = std::mem::take(&mut view.dpi_bypass.scan_requested);
		let toggle = std::mem::take(&mut view.dpi_bypass.toggle_requested);
		if !scan && !toggle {
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
		let result = match job.receiver.try_recv() {
			Ok(result) => result,
			Err(TryRecvError::Empty) => return,
			Err(TryRecvError::Disconnected) => {
				Err("Фоновый процесс обхода остановился.".to_owned())
			}
		};
		self.job = None;
		view.dpi_bypass.busy = false;
		view.dpi_bypass.progress = None;
		match result {
			Ok(Outcome::Scanned {
				alive: Some(id),
				results,
			}) => {
				view.dpi_bypass.connected = true;
				view.dpi_bypass.results = results;
				view.dpi_bypass.strategy = Some(id.clone());
				if view.dpi_bypass.selected.is_none() {
					view.dpi_bypass.selected = Some(id.clone());
				}
				view.dpi_bypass.status = format!("Найден рабочий обход: {id}.");
			}
			Ok(Outcome::Scanned {
				alive: None,
				results,
			}) => {
				view.dpi_bypass.results = results;
				view.dpi_bypass.status =
					"Ни одна стратегия не прошла проверку. Попробуйте позже.".to_owned();
			}
			Ok(Outcome::Connected(connected)) => {
				view.dpi_bypass.connected = connected;
				view.dpi_bypass.status = if connected {
					"Обход подключён.".to_owned()
				} else {
					"Обход отключён.".to_owned()
				};
			}
			Err(error) => view.dpi_bypass.status = error,
		}
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
		let (send, receive) = mpsc::sync_channel(1);
		let worker_progress = Arc::clone(&progress);
		let ctx = ctx.clone();
		let test_all = !view.dpi_bypass.auto;
		view.dpi_bypass.busy = true;
		view.dpi_bypass.progress = Some(0.0);
		view.dpi_bypass.results.clear();
		view.dpi_bypass.status = "Перебираю стратегии…".to_owned();
		runtime.spawn_blocking(move || {
			let result = scan(&catalog, &worker_progress, test_all)
				.map_err(|error| format!("Обход: {error}"));
			let _ = send.send(result);
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
			let result = toggle(connect, &strategy).map_err(|error| format!("Обход: {error}"));
			let _ = send.send(result);
			ctx.request_repaint();
		});
		self.job = Some(Job {
			receiver: receive,
			progress: Arc::new(AtomicU64::new(0)),
			total: 0,
		});
	}
}

/// Walks the catalog, applying each strategy in turn until Discord answers.
/// In manual mode every strategy is tested so the page can list all results.
fn scan(catalog: &Catalog, progress: &AtomicU64, test_all: bool) -> Result<Outcome, String> {
	let paths = paths().ok_or_else(|| "не найдена папка zapret рядом с клиентом".to_owned())?;
	let mut results: Vec<(String, bool)> = Vec::with_capacity(catalog.strategies.len());
	let mut alive = None;
	// The strategy that worked last time goes first, so a healthy setup costs
	// one probe instead of a full sweep.
	let mut order: Vec<&zapret::Strategy> = catalog.iter().collect();
	if let Some(saved) = read_state_file() {
		order.sort_by_key(|strategy| strategy.id != saved);
	}
	for (index, strategy) in order.into_iter().enumerate() {
		progress.store(index as u64, Ordering::Relaxed);
		let args = render(strategy, &paths, None);
		let mut works = false;
		if write_runner(&args).is_ok() {
			let _ = end_task();
			if run_task().is_ok() {
				let mut started = false;
				for _ in 0..10 {
					if winws_running() {
						started = true;
						break;
					}
					std::thread::sleep(Duration::from_millis(500));
				}
				if index == 0 && !started {
					let _ = end_task();
					return Err(
						"winws не запустился. Лог: %LOCALAPPDATA%\\serein-zapret\\winws.log"
							.to_owned(),
					);
				}
				// Same idea as zapret's own targets.txt: a strategy counts as
				// working only when the Discord endpoints actually answer.
				let (ok, total) = zapret::probe::probe_discord(Duration::from_secs(4));
				works = ok * 2 >= total;
			}
		}
		results.push((strategy.id.clone(), works));
		if works && alive.is_none() {
			alive = Some(strategy.id.clone());
			write_state_file(&strategy.id)?;
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
	Ok(Outcome::Scanned { alive, results })
}

fn toggle(connect: bool, strategy: &str) -> Result<Outcome, String> {
	if !connect {
		end_task()?;
		return Ok(Outcome::Connected(false));
	}
	let catalog = Catalog::embedded().map_err(|error| error.to_string())?;
	let chosen = catalog
		.strategy(strategy)
		.or_else(|| catalog.strategy("general"))
		.ok_or_else(|| "стратегия не найдена".to_owned())?;
	let paths = paths().ok_or_else(|| "не найдена папка zapret рядом с клиентом".to_owned())?;
	let args = render(chosen, &paths, None);
	write_runner(&args)?;
	let _ = end_task();
	run_task()?;
	std::thread::sleep(Duration::from_secs(2));
	if !winws_running() {
		return Err(
			"winws не запустился: проверь, зарегистрирована ли задача SereinZapret".to_owned(),
		);
	}
	write_state_file(&chosen.id)?;
	Ok(Outcome::Connected(true))
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
	let quote = |value: &str| format!("\"{}\"", value.replace('"', "\"\""));
	let line = args
		.iter()
		.map(|arg| {
			if arg.contains(' ') {
				quote(arg)
			} else {
				arg.clone()
			}
		})
		.collect::<Vec<_>>()
		.join(" ");
	// `wscript.exe` is a GUI process: it never allocates a console, so no
	// PowerShell/cmd window flashes when the task starts.
	let script = format!(
		"Dim sh\r\nSet sh = CreateObject(\"WScript.Shell\")\r\nsh.CurrentDirectory = {bin}\r\nsh.Run {cmd}, 0, False\r\n",
		bin = quote(&paths.bin.display().to_string()),
		cmd = quote(&format!("{} {line}", paths.bin.join("winws.exe").display())),
	);
	write_utf16(&dir.join("run.vbs"), &script)?;
	write_stop_script(&dir)
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

/// Spawns the elevated one-shot installer; the user only sees the UAC prompt.
fn elevate_install() {
	let Ok(exe) = std::env::current_exe() else {
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
	let _ = command
		.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &inner])
		.spawn();
}

/// Elevated entry point (`serein.exe --zapret-install-task`): writes the runner
/// and registers the hidden SYSTEM task, so later launches never prompt.
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

	// `schtasks /sd` parses dates in the system locale (`12/31/2099` fails on a
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
