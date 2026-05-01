use proc_macro::TokenStream;

#[proc_macro]
pub fn include_manifest(_input: TokenStream) -> TokenStream {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set for include_manifest!()");
    let manifest_path = std::path::Path::new(&manifest_dir).join("Cargo.toml");
    let raw = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
    let parsed: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", manifest_path.display()));

    let package = parsed
        .get("package")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| panic!("{} is missing [package]", manifest_path.display()));
    let package_version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{} is missing package.version", manifest_path.display()));
    let vulcan = package
        .get("metadata")
        .and_then(|v| v.get("vulcan"))
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| {
            panic!(
                "{} is missing [package.metadata.vulcan]",
                manifest_path.display()
            )
        });
    let id = vulcan
        .get("id")
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "{} is missing package.metadata.vulcan.id",
                manifest_path.display()
            )
        });
    let version = vulcan
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or(package_version);
    let daemon_entry = vulcan.get("daemon_entry").and_then(toml::Value::as_str);

    let id_lit = rust_string_literal(id);
    let version_lit = rust_string_literal(version);
    let daemon_entry_tokens = match daemon_entry {
        Some(entry) => format!("Some({}.to_string())", rust_string_literal(entry)),
        None => "None".to_string(),
    };

    format!(
        "::vulcan::extensions::api::ExtensionManifest {{ id: {id_lit}.to_string(), version: {version_lit}.to_string(), daemon_entry: {daemon_entry_tokens} }}"
    )
    .parse()
    .expect("include_manifest! generated invalid tokens")
}

fn rust_string_literal(value: &str) -> String {
    format!("{value:?}")
}
