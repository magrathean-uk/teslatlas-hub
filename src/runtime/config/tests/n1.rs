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
