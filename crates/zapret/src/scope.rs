//! WinDivert filter strings that bound which packets `winws` may touch.
//!
//! `--wf-raw` replaces the catalog's `--wf-tcp` / `--wf-udp` filters, so a
//! strategy rendered with a scope no longer intercepts every port on the
//! machine.

/// Ports the shipped strategies already declare for Discord traffic.
const DISCORD_TCP: [u16; 7] = [80, 443, 2053, 2083, 2087, 2096, 8443];
const DISCORD_UDP_FIXED: [u16; 1] = [443];
const DISCORD_UDP_RANGES: [(u16, u16); 2] = [(19294, 19344), (50000, 50100)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
	/// Discord endpoints only: the client's whole traffic surface, and nothing
	/// else on the machine.
	Discord,
	/// Sockets bound to one local port range. WinDivert cannot filter by
	/// process, so this only narrows traffic when the client pins the source
	/// ports its connectors use.
	AppPorts { low: u16, high: u16 },
}

impl Scope {
	/// WinDivert expression for `--wf-raw`.
	pub fn wf_raw(self) -> String {
		match self {
			Self::Discord => {
				let tcp = DISCORD_TCP
					.iter()
					.map(|port| format!("tcp.DstPort == {port}"))
					.collect::<Vec<_>>()
					.join(" or ");
				let mut udp = DISCORD_UDP_FIXED
					.iter()
					.map(|port| format!("udp.DstPort == {port}"))
					.collect::<Vec<_>>();
				for (low, high) in DISCORD_UDP_RANGES {
					udp.push(format!("(udp.DstPort >= {low} and udp.DstPort <= {high})"));
				}
				format!(
					"outbound and ((tcp and ({tcp})) or (udp and ({})))",
					udp.join(" or ")
				)
			}
			Self::AppPorts { low, high } => format!(
				"outbound and ((tcp and tcp.SrcPort >= {low} and tcp.SrcPort <= {high}) or \
				 (udp and udp.SrcPort >= {low} and udp.SrcPort <= {high}))"
			),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn discord_scope_covers_every_strategy_port() {
		let filter = Scope::Discord.wf_raw();
		assert!(filter.starts_with("outbound and"), "{filter}");
		for port in DISCORD_TCP {
			assert!(
				filter.contains(&format!("tcp.DstPort == {port}")),
				"missing {port}"
			);
		}
		assert!(
			filter.contains("udp.DstPort >= 50000"),
			"missing voice range"
		);
		assert!(
			!filter.contains("tcp.SrcPort"),
			"discord scope must not pin ports"
		);
	}

	#[test]
	fn app_scope_pins_source_ports_only() {
		let filter = Scope::AppPorts {
			low: 39_000,
			high: 39_100,
		}
		.wf_raw();
		assert!(filter.contains("tcp.SrcPort >= 39000"));
		assert!(filter.contains("udp.SrcPort <= 39100"));
		assert!(
			!filter.contains("DstPort"),
			"app scope must ignore destinations"
		);
	}
}
