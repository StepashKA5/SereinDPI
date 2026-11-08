//! Turns catalog placeholders into concrete `winws` arguments.

use crate::catalog::Strategy;
use std::path::{Path, PathBuf};

/// Directories the catalog refers to through `{bin}` and `{lists}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
	pub bin: PathBuf,
	pub lists: PathBuf,
}

/// Optional game-filter port lists; the catalog keeps Flowseal's placeholders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameFilter {
	pub tcp: String,
	pub udp: String,
}

impl Paths {
	pub fn new(bin: impl Into<PathBuf>, lists: impl Into<PathBuf>) -> Self {
		Self {
			bin: bin.into(),
			lists: lists.into(),
		}
	}

	/// Standard layout: `<root>/bin` plus `<root>/lists`.
	pub fn from_root(root: &Path) -> Self {
		Self::new(root.join("bin"), root.join("lists"))
	}
}

/// Renders one strategy into ready-to-spawn arguments.
///
/// Unknown placeholders are left untouched so a broken catalog fails loudly at
/// `winws` instead of silently dropping a filter.
pub fn render(strategy: &Strategy, paths: &Paths, game: Option<&GameFilter>) -> Vec<String> {
	let bin = prefix(&paths.bin);
	let lists = prefix(&paths.lists);
	let tcp = game
		.map(|filter| filter.tcp.as_str())
		.filter(|value| !value.trim().is_empty());
	let udp = game
		.map(|filter| filter.udp.as_str())
		.filter(|value| !value.trim().is_empty());

	strategy
		.args
		.iter()
		.map(|arg| {
			let mut text = arg.replace("{bin}", &bin).replace("{lists}", &lists);
			text = expand(&text, "{gamefilter_tcp}", tcp);
			text = expand(&text, "{gamefilter_udp}", udp);
			text
		})
		.filter(|arg| !arg.is_empty())
		// `--filter-tcp=` with an empty game-filter list would make winws fail to
		// start, so an emptied option is dropped entirely.
		.filter(|arg| !arg.ends_with('='))
		.collect()
}

/// A directory rendered as an argument prefix, always ending with a separator.
fn prefix(dir: &Path) -> String {
	let mut text = dir.to_string_lossy().into_owned();
	if !text.ends_with(['\\', '/']) {
		text.push(std::path::MAIN_SEPARATOR);
	}
	text
}

fn expand(text: &str, placeholder: &str, value: Option<&str>) -> String {
	if !text.contains(placeholder) {
		return text.to_owned();
	}
	match value {
		Some(value) => text.replace(placeholder, value.trim()),
		// `--wf-tcp=80,443,{gamefilter_tcp}` loses the separator with the value.
		None => text
			.replace(&format!(",{placeholder}"), "")
			.replace(placeholder, ""),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::catalog::Catalog;

	fn general() -> Strategy {
		Catalog::embedded()
			.expect("catalog")
			.strategy("general")
			.expect("general strategy")
			.clone()
	}

	#[test]
	fn paths_get_a_trailing_separator() {
		let paths = Paths::from_root(Path::new(r"C:\Program Files\Serein\zapret"));
		let args = render(&general(), &paths, None);
		let hostlist = args
			.iter()
			.find(|arg| arg.starts_with("--hostlist="))
			.expect("hostlist argument");
		let value = hostlist.trim_start_matches("--hostlist=");
		assert!(
			value.ends_with(&format!(
				"lists{0}list-general.txt",
				std::path::MAIN_SEPARATOR
			)),
			"unexpected hostlist path: {value}"
		);
		assert!(!value.contains("\\\\"), "separator duplicated: {value}");
	}

	#[test]
	fn missing_game_filter_drops_the_separator_only() {
		let args = render(&general(), &Paths::from_root(Path::new("zapret")), None);
		let wf_tcp = args
			.iter()
			.find(|arg| arg.starts_with("--wf-tcp="))
			.expect("wf-tcp argument");
		assert_eq!(wf_tcp, "--wf-tcp=80,443,2053,2083,2087,2096,8443");
		assert!(
			!args.iter().any(|arg| arg.contains("{gamefilter")),
			"placeholder leaked"
		);
	}

	#[test]
	fn game_filter_values_are_inserted() {
		let game = GameFilter {
			tcp: "1024-65535".to_owned(),
			udp: "1024-65535".to_owned(),
		};
		let args = render(
			&general(),
			&Paths::from_root(Path::new("zapret")),
			Some(&game),
		);
		assert!(
			args.iter()
				.any(|arg| arg == "--wf-tcp=80,443,2053,2083,2087,2096,8443,1024-65535"),
			"game filter TCP ports missing: {args:?}"
		);
	}

	#[test]
	fn blank_game_filter_is_treated_as_absent() {
		let game = GameFilter {
			tcp: "   ".to_owned(),
			udp: String::new(),
		};
		let args = render(
			&general(),
			&Paths::from_root(Path::new("zapret")),
			Some(&game),
		);
		assert!(
			args.iter()
				.any(|arg| arg == "--wf-tcp=80,443,2053,2083,2087,2096,8443")
		);
	}
}
