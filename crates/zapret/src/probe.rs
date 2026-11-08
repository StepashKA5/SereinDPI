//! Connectivity probe used to score `winws` strategies.
//!
//! A blocked Discord endpoint usually fails during the TLS handshake, which a
//! plain TCP connect cannot see. The probe therefore performs one real HTTPS
//! request through the system `curl` (present on Windows 10+) instead of adding
//! another TLS stack.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
	/// Endpoint answered with a 2xx status.
	Reachable { status: u16, millis: u128 },
	/// Endpoint answered, but not in the way we expect.
	Unexpected { status: u16 },
	/// Timeout, reset or DNS failure: the shape of a blocked connection.
	Failed(String),
}

impl Probe {
	pub fn is_reachable(&self) -> bool {
		matches!(self, Self::Reachable { .. })
	}

	/// The endpoint answered at all, whatever the status code.
	pub fn is_answer(&self) -> bool {
		matches!(self, Self::Reachable { .. } | Self::Unexpected { .. })
	}
}

/// Endpoint that must answer before the client can log in.
pub const DISCORD_GATEWAY: &str = "https://discord.com/api/v10/gateway";

/// Endpoints a working bypass must reach; mirrors zapret's own `targets.txt`.
pub const DISCORD_TARGETS: [&str; 3] = [
	"https://discord.com/api/v10/gateway",
	"https://gateway.discord.gg",
	"https://cdn.discordapp.com",
];

/// Counts how many Discord endpoints answer (`ok`, `total`).
pub fn probe_discord(timeout: Duration) -> (usize, usize) {
	// Any HTTP answer means TLS and the request went through; the CDN
	// legitimately replies 403 to a bare GET, so requiring 2xx would reject
	// every working strategy.
	let ok = DISCORD_TARGETS
		.iter()
		.filter(|url| probe(url, timeout).is_answer())
		.count();
	(ok, DISCORD_TARGETS.len())
}

/// Runs one HTTPS GET and reports the status code.
pub fn probe(url: &str, timeout: Duration) -> Probe {
	let started = Instant::now();
	let binary = if cfg!(windows) { "curl.exe" } else { "curl" };
	let seconds = timeout.as_secs().max(1).to_string();
	let mut command = std::process::Command::new(binary);
	// Never flash a console window on the user's desktop.
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		command.creation_flags(CREATE_NO_WINDOW);
	}
	let output = command
		.args([
			"--silent",
			"--output",
			null_device(),
			"--write-out",
			"%{http_code}",
		])
		.args(["--max-time", &seconds, "--url", url])
		.output();

	let elapsed = started.elapsed().as_millis();
	match output {
		Ok(output) => {
			let status: u16 = String::from_utf8_lossy(&output.stdout)
				.trim()
				.parse()
				.unwrap_or(0);
			if !output.status.success() || status == 0 {
				return Probe::Failed(format!("curl exit {}", output.status.code().unwrap_or(-1)));
			}
			if (200..300).contains(&status) {
				Probe::Reachable {
					status,
					millis: elapsed,
				}
			} else {
				Probe::Unexpected { status }
			}
		}
		Err(error) => Probe::Failed(error.to_string()),
	}
}

fn null_device() -> &'static str {
	if cfg!(windows) { "NUL" } else { "/dev/null" }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unreachable_host_fails_instead_of_hanging() {
		// 127.0.0.1:1 refuses instantly; the probe must report a failure, not a status.
		let result = probe("http://127.0.0.1:1/", Duration::from_secs(2));
		assert!(!result.is_reachable(), "unexpected success: {result:?}");
	}
}
