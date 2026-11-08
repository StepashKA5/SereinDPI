//! Parsed `winws` strategy catalog shipped with the app.

/// Catalog embedded into the binary; kept in sync by `tools/gen-strategies.ps1`.
pub const EMBEDDED_CATALOG: &str = include_str!("../data/strategies.json");

/// One named `winws` argument list with `{bin}` / `{lists}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strategy {
	pub id: String,
	pub name: String,
	pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
	pub version: u32,
	pub source: String,
	pub strategies: Vec<Strategy>,
}

#[derive(Debug)]
pub enum CatalogError {
	Json(serde_json::Error),
	Structure(&'static str),
	Empty,
	DuplicateId(String),
}

impl std::fmt::Display for CatalogError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Json(error) => write!(f, "invalid strategy catalog: {error}"),
			Self::Structure(field) => write!(f, "strategy catalog is missing {field}"),
			Self::Empty => f.write_str("strategy catalog contains no strategies"),
			Self::DuplicateId(id) => write!(f, "duplicate strategy id: {id}"),
		}
	}
}

impl std::error::Error for CatalogError {}

impl Catalog {
	/// Catalog compiled into the binary.
	pub fn embedded() -> Result<Self, CatalogError> {
		Self::parse(EMBEDDED_CATALOG)
	}

	/// Parses and validates a catalog document.
	pub fn parse(text: &str) -> Result<Self, CatalogError> {
		let value: serde_json::Value = serde_json::from_str(text).map_err(CatalogError::Json)?;
		let version = value
			.get("version")
			.and_then(serde_json::Value::as_u64)
			.unwrap_or(0) as u32;
		let source = value
			.get("source")
			.and_then(serde_json::Value::as_str)
			.unwrap_or_default()
			.to_owned();
		let entries = value
			.get("strategies")
			.and_then(serde_json::Value::as_array)
			.ok_or(CatalogError::Structure("strategies"))?;

		let mut strategies: Vec<Strategy> = Vec::with_capacity(entries.len());
		for entry in entries {
			let id = string_field(entry, "id")?;
			if strategies.iter().any(|seen| seen.id == id) {
				return Err(CatalogError::DuplicateId(id));
			}
			let args = entry
				.get("args")
				.and_then(serde_json::Value::as_array)
				.ok_or(CatalogError::Structure("args"))?
				.iter()
				.map(|arg| arg.as_str().map(str::to_owned))
				.collect::<Option<Vec<_>>>()
				.ok_or(CatalogError::Structure("args"))?;
			if args.is_empty() {
				return Err(CatalogError::Structure("args"));
			}
			strategies.push(Strategy {
				id,
				name: string_field(entry, "name")?,
				args,
			});
		}

		if strategies.is_empty() {
			return Err(CatalogError::Empty);
		}
		Ok(Self {
			version,
			source,
			strategies,
		})
	}

	pub fn strategy(&self, id: &str) -> Option<&Strategy> {
		self.strategies.iter().find(|strategy| strategy.id == id)
	}

	pub fn iter(&self) -> impl Iterator<Item = &Strategy> {
		self.strategies.iter()
	}
}

fn string_field(value: &serde_json::Value, field: &'static str) -> Result<String, CatalogError> {
	value
		.get(field)
		.and_then(serde_json::Value::as_str)
		.map(str::to_owned)
		.ok_or(CatalogError::Structure(field))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn embedded_catalog_is_valid() {
		let catalog = Catalog::embedded().expect("embedded catalog parses");
		assert!(
			catalog.strategies.len() >= 10,
			"catalog is suspiciously small"
		);
		for strategy in catalog.iter() {
			assert!(
				strategy.args.iter().any(|arg| arg.starts_with("--wf-")),
				"{} has no WinDivert filter",
				strategy.id
			);
		}
	}

	#[test]
	fn duplicate_ids_are_rejected() {
		let text = r#"{"version":1,"strategies":[
			{"id":"a","name":"a","args":["--wf-tcp=443"]},
			{"id":"a","name":"b","args":["--wf-tcp=443"]}
		]}"#;
		assert!(matches!(Catalog::parse(text), Err(CatalogError::DuplicateId(id)) if id == "a"));
	}

	#[test]
	fn empty_catalog_is_rejected() {
		assert!(matches!(
			Catalog::parse(r#"{"version":1,"strategies":[]}"#),
			Err(CatalogError::Empty)
		));
	}

	#[test]
	fn broken_entries_name_the_missing_field() {
		assert!(matches!(
			Catalog::parse(r#"{"version":1,"strategies":[{"id":"a","args":["--wf-tcp=443"]}]}"#),
			Err(CatalogError::Structure("name"))
		));
	}
}
