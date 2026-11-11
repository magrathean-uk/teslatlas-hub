// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

#[test]
fn import_source_tables_load() {
    let text = r#"data_dir = "/fictional/hub"
bind = "127.0.0.1:21447"
[import_source]
source_url = "postgresql://reader@source.example/sourcedb"
source_key = "home-source"
[import_source.current]
source_url = "postgresql://reader@source.example/sourcedb"
source_key = "home-source"
vehicle_id = "20000000-0000-4000-8000-000000000001"
car_id = 42
password_file = "/fictional/hub/source-password"
"#;
    let (config, _) = HubConfig::from_exact_bytes(text.as_bytes()).unwrap();
    assert_eq!(
        config.import_source.source_key.as_deref(),
        Some("home-source")
    );
    assert_eq!(
        config.import_source.current.unwrap().source_key,
        "home-source"
    );
}

#[test]
fn mt4_4_unknown_top_level_table_shows_the_import_source_hint() {
    let text = "data_dir=\"/fictional/hub\"\nbind=\"127.0.0.1:21447\"\n[unlisted_import_shape]\nenabled=true\n";
    let error = HubConfig::from_exact_bytes(text.as_bytes())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Import settings are in [import_source]; see docs/guides/configuration.md."),
        "{error}"
    );
}

const EARLIER_TABLE: &str = crate::runtime::legacy_names::DEPRECATED_CONFIG_TABLE_IMPORT_SOURCE;

fn earlier_table_text(table: &str) -> String {
    format!(
        r#"data_dir = "/fictional/hub"
bind = "127.0.0.1:21447"
[{table}]
source_url = "postgresql://reader@source.example/sourcedb"
source_key = "home-source"
page_size = 500
[{table}.current]
source_url = "postgresql://reader@source.example/sourcedb"
source_key = "home-source"
vehicle_id = "20000000-0000-4000-8000-000000000001"
car_id = 42
password_file = "/fictional/hub/source-password"
"#
    )
}

#[test]
fn deprecated_alias_import_table_loads_as_the_current_table() {
    let (earlier, earlier_digest) =
        HubConfig::from_exact_bytes(earlier_table_text(EARLIER_TABLE).as_bytes()).unwrap();
    let current_text = earlier_table_text("import_source");
    let (current, current_digest) = HubConfig::from_exact_bytes(current_text.as_bytes()).unwrap();
    assert_eq!(
        format!("{:?}", earlier.import_source),
        format!("{:?}", current.import_source)
    );
    assert_eq!(earlier.import_source.page_size, 500);
    assert_eq!(
        earlier.import_source.current.as_ref().unwrap().car_id,
        42
    );
    // The digest identifies the bytes as written, not the rewritten text.
    assert_ne!(earlier_digest, current_digest);
}

#[test]
fn deprecated_alias_import_table_accepts_dotted_keys() {
    let text = format!(
        "data_dir = \"/fictional/hub\"\nbind = \"127.0.0.1:21447\"\n{EARLIER_TABLE}.source_key = \"home-source\"\n"
    );
    let (config, _) = HubConfig::from_exact_bytes(text.as_bytes()).unwrap();
    assert_eq!(
        config.import_source.source_key.as_deref(),
        Some("home-source")
    );
}

#[test]
fn deprecated_alias_import_table_with_the_current_table_is_refused() {
    let text = format!(
        "{}[import_source]\nsource_key = \"other\"\n",
        format!(
            "data_dir = \"/fictional/hub\"\nbind = \"127.0.0.1:21447\"\n[{EARLIER_TABLE}]\nsource_key = \"home-source\"\n"
        )
    );
    let error = HubConfig::from_exact_bytes(text.as_bytes())
        .unwrap_err()
        .to_string();
    assert!(error.contains("keep only [import_source]"), "{error}");
}

#[test]
fn deprecated_alias_import_table_still_refuses_unknown_keys_and_tables() {
    let text = earlier_table_text(EARLIER_TABLE).replace("page_size = 500", "unlisted_key = 1");
    assert!(HubConfig::from_exact_bytes(text.as_bytes()).is_err());
    let text = format!("{}[unlisted_import_shape]\nenabled = true\n", earlier_table_text(EARLIER_TABLE));
    let error = HubConfig::from_exact_bytes(text.as_bytes())
        .unwrap_err()
        .to_string();
    assert!(error.contains("Import settings are in [import_source]"), "{error}");
}

#[test]
fn deprecated_alias_import_table_warning_names_the_replacement() {
    let warning = deprecated_table_warning();
    assert!(warning.contains(&format!("[{EARLIER_TABLE}]")));
    assert!(warning.contains("use [import_source]"));
    assert!(warning.contains("a later version"));
    assert_eq!(warning.lines().count(), 1);
    let mut table: toml::Table = earlier_table_text("import_source").parse().unwrap();
    assert!(!rename_deprecated_tables(&mut table).unwrap());
    let mut table: toml::Table = earlier_table_text(EARLIER_TABLE).parse().unwrap();
    assert!(rename_deprecated_tables(&mut table).unwrap());
    assert!(table.contains_key("import_source") && !table.contains_key(EARLIER_TABLE));
}
