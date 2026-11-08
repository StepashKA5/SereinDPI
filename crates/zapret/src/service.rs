//! Thin wrapper around the `serein-zapret` Windows service.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
	/// `sc` does not know the service (not installed, or no permission to ask).
	Missing,
	Stopped,
	StartPending,
	StopPending,
	Running,
	Paused,
	Unknown(String),
}

impl ServiceState {
	pub fn is_running(&self) -> bool {
		matches!(self, Self::Running)
	}

	pub fn is_busy(&self) -> bool {
		matches!(self, Self::StartPending | Self::StopPending)
	}
}

#[derive(Debug, Clone)]
pub struct Service {
	name: String,
}

impl Service {
	pub fn new(name: impl Into<String>) -> Self {
		Self { name: name.into() }
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	#[cfg(windows)]
	pub fn state(&self) -> ServiceState {
		match sc(&["query", &self.name]) {
			Ok(output) if output.status.success() => {
				parse_state(&String::from_utf8_lossy(&output.stdout))
			}
			_ => ServiceState::Missing,
		}
	}

	#[cfg(windows)]
	pub fn start(&self) -> Result<(), String> {
		sc(&["start", &self.name])
			.map_err(|error| error.to_string())
			.and_then(|output| {
				if output.status.success() {
					Ok(())
				} else {
					Err(first_line(&output.stderr))
				}
			})
	}

	#[cfg(windows)]
	pub fn stop(&self) -> Result<(), String> {
		sc(&["stop", &self.name])
			.map_err(|error| error.to_string())
			.and_then(|output| {
				// Stopping an already stopped service is not an error for callers.
				if output.status.success() || first_line(&output.stderr).contains("1062") {
					Ok(())
				} else {
					Err(first_line(&output.stderr))
				}
			})
	}

	#[cfg(not(windows))]
	pub fn state(&self) -> ServiceState {
		ServiceState::Missing
	}

	#[cfg(not(windows))]
	pub fn start(&self) -> Result<(), String> {
		Err("the DPI bypass helper is Windows-only".to_owned())
	}

	#[cfg(not(windows))]
	pub fn stop(&self) -> Result<(), String> {
		Err("the DPI bypass helper is Windows-only".to_owned())
	}
}

#[cfg(windows)]
fn sc(args: &[&str]) -> std::io::Result<std::process::Output> {
	use std::os::windows::process::CommandExt;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	std::process::Command::new("sc.exe")
		.args(args)
		.creation_flags(CREATE_NO_WINDOW)
		.output()
}

#[cfg(windows)]
fn first_line(bytes: &[u8]) -> String {
	String::from_utf8_lossy(bytes)
		.lines()
		.next()
		.unwrap_or_default()
		.trim()
		.to_owned()
}

/// Parses `sc query` output, e.g. `        STATE              : 4  RUNNING`.
fn parse_state(output: &str) -> ServiceState {
	let Some(line) = output
		.lines()
		.find(|line| line.trim_start().starts_with("STATE"))
	else {
		return ServiceState::Unknown(String::new());
	};
	let Some((_, value)) = line.split_once(':') else {
		return ServiceState::Unknown(line.trim().to_owned());
	};
	// Skip the numeric state code, keep the symbolic name.
	let token = value
		.split_whitespace()
		.find(|token| token.chars().any(|character| character.is_alphabetic()))
		.unwrap_or_default();
	match token {
		"STOPPED" => ServiceState::Stopped,
		"START_PENDING" => ServiceState::StartPending,
		"STOP_PENDING" => ServiceState::StopPending,
		"RUNNING" => ServiceState::Running,
		"PAUSED" => ServiceState::Paused,
		other => ServiceState::Unknown(other.to_owned()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_sc_query_output() {
		let output = "\r\nSERVICE_NAME: serein-zapret\r\n        TYPE               : 110  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n";
		assert_eq!(parse_state(output), ServiceState::Running);
	}

	#[test]
	fn parses_pending_states() {
		assert_eq!(
			parse_state("        STATE              : 2  START_PENDING"),
			ServiceState::StartPending
		);
		assert_eq!(
			parse_state("        STATE              : 3  STOP_PENDING"),
			ServiceState::StopPending
		);
	}

	#[test]
	fn unknown_output_does_not_panic() {
		assert_eq!(parse_state(""), ServiceState::Unknown(String::new()));
		assert!(matches!(
			parse_state("nothing useful"),
			ServiceState::Unknown(_)
		));
	}
}
