// SPDX-License-Identifier: AGPL-3.0-only

// Geocoder lookup rows (schema v69): the version-2 key binds endpoint,
// language and coordinates, and each lookup keeps its own record. Synthetic
// endpoints and places only.

fn lookup_binding(endpoint: &str, language: &str) -> AddressLookupBinding {
    AddressLookupBinding::nominatim(&url::Url::parse(endpoint).expect("endpoint"), language)
        .expect("binding")
}

fn lookup_point(latitude: f64, longitude: f64) -> crate::location::Wgs84Point {
    crate::location::Wgs84Point::new(latitude, longitude).expect("point")
}

/// A lookup of `point` under `binding` that reached OSM way 42 and was told
/// `display_name`; every other field derives from `display_name` so that two
/// records for the same identity differ everywhere.
fn lookup_record(
    binding: &AddressLookupBinding,
    point: crate::location::Wgs84Point,
    display_name: &str,
    looked_up_at_ms: i64,
) -> AddressCacheRecord {
    AddressCacheRecord {
        osm_type: "way".to_owned(),
        osm_id: 42,
        display_name: display_name.to_owned(),
        name: Some(format!("{display_name} name")),
        latitude: Some(point.latitude + 0.0001),
        longitude: Some(point.longitude - 0.0001),
        house_number: Some("7".to_owned()),
        road: Some(format!("{display_name} road")),
        neighbourhood: Some(format!("{display_name} quarter")),
        city: Some(format!("{display_name} city")),
        county: Some(format!("{display_name} county")),
        postcode: Some("T1 2AB".to_owned()),
        state: Some(format!("{display_name} state")),
        state_district: Some(format!("{display_name} district")),
        country: Some("Testland".to_owned()),
        raw_json: Some(format!(
            r#"{{"osm_type":"way","osm_id":42,"display_name":"{display_name}"}}"#
        )),
        binding: binding.clone(),
        licence: Some(format!("Licence of {display_name}")),
        lookup_latitude: point.latitude,
        lookup_longitude: point.longitude,
        looked_up_at_ms,
    }
}

fn lookup_row_count(store: &HubStore) -> i64 {
    store
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM address_lookup_cache", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn shared_address_rows(store: &HubStore) -> Vec<(String, i64, String)> {
    store
        .open()
        .unwrap()
        .prepare("SELECT osm_type, osm_id, display_name FROM address_cache ORDER BY osm_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn address_lookup_key_is_a_pinned_v2_digest_of_endpoint_language_and_rounded_coordinates() {
    // Known answers computed outside this crate (Python hashlib over the
    // eight-byte big-endian length prefix and bytes of each component).
    let plain = lookup_binding("https://geocoder.example.net/", "en");
    assert_eq!(plain.endpoint, "https://geocoder.example.net:443/");
    let key = address_lookup_key(&plain, lookup_point(51.0, -0.1));
    assert_eq!(key, "v2:kku91oPzdPaUIgnOzoOJNzOQ4UdwYkfQ0NWrFHVPAko");
    assert_eq!(key.len(), 46, "inside the column's 64-byte CHECK");
    let deep = lookup_binding("https://geocoder.example.net/nominatim/", "de-CH");
    assert_eq!(
        address_lookup_key(&deep, lookup_point(-33.865143, 151.2099)),
        "v2:Uvy0uQbhAQBas6WQhufCkFYWbPebUCcBC8M4GNHquhE"
    );

    // Coordinates round at six places, as the version-1 key did.
    assert_eq!(
        address_lookup_key(&plain, lookup_point(51.000_000_4, -0.100_000_4)),
        key
    );
    assert_ne!(
        address_lookup_key(&plain, lookup_point(51.000_001, -0.1)),
        key
    );
    assert_ne!(
        address_lookup_key(&plain, lookup_point(51.0, -0.100_001)),
        key
    );
    // Latitude and longitude are not interchangeable.
    assert_ne!(
        address_lookup_key(&plain, lookup_point(0.5, 0.25)),
        address_lookup_key(&plain, lookup_point(0.25, 0.5))
    );

    // The components are length-prefixed: moving a byte from one to the next
    // is a different key, not the same concatenation.
    let split_a = AddressLookupBinding {
        endpoint: "ab".to_owned(),
        language: "c".to_owned(),
        ..plain.clone()
    };
    let split_b = AddressLookupBinding {
        endpoint: "a".to_owned(),
        language: "bc".to_owned(),
        ..plain.clone()
    };
    assert_ne!(
        address_lookup_key(&split_a, lookup_point(1.0, 2.0)),
        address_lookup_key(&split_b, lookup_point(1.0, 2.0))
    );
}

#[test]
fn the_binding_normalises_the_endpoint_and_changes_with_each_component() {
    let base = lookup_binding("https://geocoder.example.net/nominatim/", "en");
    assert_eq!(base.provider, "nominatim");
    assert_eq!(base.endpoint, "https://geocoder.example.net:443/nominatim/");
    assert_eq!(base.endpoint_host, "geocoder.example.net");
    assert_eq!(base.language, "en");

    // Spelling does not matter: host case and the default port are one binding.
    for same in [
        "https://GEOCODER.example.NET/nominatim/",
        "https://geocoder.example.net:443/nominatim/",
    ] {
        assert_eq!(lookup_binding(same, "en"), base, "{same}");
    }
    assert_eq!(
        lookup_binding("http://127.0.0.1:8080/", "en").endpoint,
        "http://127.0.0.1:8080/"
    );
    assert_eq!(
        lookup_binding("http://[::1]/", "en").endpoint,
        "http://[::1]:80/"
    );

    // Each component alone makes a different binding and so a different key.
    let point = lookup_point(10.0, 20.0);
    let base_key = address_lookup_key(&base, point);
    for (what, other) in [
        (
            "host",
            lookup_binding("https://other.example.net/nominatim/", "en"),
        ),
        (
            "port",
            lookup_binding("https://geocoder.example.net:8443/nominatim/", "en"),
        ),
        (
            "path",
            lookup_binding("https://geocoder.example.net/other/", "en"),
        ),
        (
            "language",
            lookup_binding("https://geocoder.example.net/nominatim/", "de"),
        ),
        (
            "language case",
            lookup_binding("https://geocoder.example.net/nominatim/", "EN"),
        ),
    ] {
        assert_ne!(other, base, "{what}");
        assert_ne!(address_lookup_key(&other, point), base_key, "{what}");
    }
    assert_ne!(
        address_lookup_key(&lookup_binding("http://h.example.net:8443/", "en"), point),
        address_lookup_key(&lookup_binding("https://h.example.net:8443/", "en"), point),
        "scheme"
    );

    // What cannot name an endpoint is refused.
    for refused in [
        "ftp://geocoder.example.net/",
        "https://user:secret@geocoder.example.net/",
        "https://geocoder.example.net/?key=1",
        "https://geocoder.example.net/#top",
    ] {
        assert!(
            matches!(
                AddressLookupBinding::nominatim(&url::Url::parse(refused).unwrap(), "en"),
                Err(StoreError::InvalidAddressCache)
            ),
            "{refused}"
        );
    }
    let endpoint = url::Url::parse("https://geocoder.example.net/").unwrap();
    let long_language = "x".repeat(65);
    for language in ["", "e\nn", long_language.as_str()] {
        assert!(
            matches!(
                AddressLookupBinding::nominatim(&endpoint, language),
                Err(StoreError::InvalidAddressCache)
            ),
            "{language:?}"
        );
    }
    assert!(AddressLookupBinding::nominatim(&endpoint, &"x".repeat(64)).is_ok());
}

#[test]
fn two_lookups_reaching_one_osm_identity_keep_their_own_records() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let binding = lookup_binding("https://geocoder.example.net/", "en");
    let near = lookup_point(51.5, -0.1);
    let far = lookup_point(-33.9, 151.2);

    let first = lookup_record(&binding, near, "Alpha", 10);
    let second = lookup_record(&binding, far, "Beta", 20);
    store.put_address_cache(&first).unwrap();
    store.put_address_cache(&second).unwrap();

    // Each lookup reads back exactly its own record, every field of it.
    assert_eq!(store.cached_address(&binding, near).unwrap(), Some(first));
    assert_eq!(store.cached_address(&binding, far).unwrap(), Some(second));
    assert_eq!(lookup_row_count(&store), 2);

    // The shared identity row is the first response's and was not rewritten.
    assert_eq!(
        shared_address_rows(&store),
        vec![("way".to_owned(), 42, "Alpha".to_owned())]
    );

    // Re-recording a lookup updates that lookup alone.
    let refreshed = lookup_record(&binding, near, "Alpha renamed", 30);
    store.put_address_cache(&refreshed).unwrap();
    assert_eq!(
        store.cached_address(&binding, near).unwrap(),
        Some(refreshed)
    );
    assert_eq!(
        store.cached_address(&binding, far).unwrap().map(|record| (
            record.display_name,
            record.name,
            record.looked_up_at_ms
        )),
        Some(("Beta".to_owned(), Some("Beta name".to_owned()), 20))
    );
    assert_eq!(lookup_row_count(&store), 2);
    assert_eq!(
        shared_address_rows(&store),
        vec![("way".to_owned(), 42, "Alpha".to_owned())]
    );
}

#[test]
fn a_changed_endpoint_component_or_language_misses_and_leaves_the_other_lookup_alone() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let base = lookup_binding("https://geocoder.example.net/nominatim/", "en");
    let point = lookup_point(48.8566, 2.3522);
    let original = lookup_record(&base, point, "Original", 1);
    store.put_address_cache(&original).unwrap();

    let variants = [
        (
            "host",
            lookup_binding("https://other.example.net/nominatim/", "en"),
        ),
        (
            "port",
            lookup_binding("https://geocoder.example.net:8443/nominatim/", "en"),
        ),
        (
            "path",
            lookup_binding("https://geocoder.example.net/other/", "en"),
        ),
        (
            "scheme",
            lookup_binding("http://geocoder.example.net:443/nominatim/", "en"),
        ),
        (
            "language",
            lookup_binding("https://geocoder.example.net/nominatim/", "de"),
        ),
    ];
    for (index, (what, variant)) in variants.iter().enumerate() {
        assert_eq!(
            store.cached_address(variant, point).unwrap(),
            None,
            "{what} must miss"
        );
        // Storing the variant's answer must not touch the original's row.
        let name = format!("Variant {what}");
        let record = lookup_record(variant, point, &name, 100 + index as i64);
        store.put_address_cache(&record).unwrap();
        assert_eq!(
            store.cached_address(variant, point).unwrap(),
            Some(record),
            "{what}"
        );
        assert_eq!(
            store.cached_address(&base, point).unwrap(),
            Some(original.clone()),
            "{what} must not rewrite the original lookup"
        );
    }
    assert_eq!(lookup_row_count(&store), 1 + variants.len() as i64);

    // The stored columns say what each row was asked under.
    let connection = store.open().unwrap();
    let mut statement = connection
        .prepare(
            "SELECT provider, endpoint, endpoint_host, language, display_name
             FROM address_lookup_cache ORDER BY looked_up_at_ms",
        )
        .unwrap();
    let rows: Vec<(String, String, String, String, String)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected = |binding: &AddressLookupBinding, name: &str| {
        (
            "nominatim".to_owned(),
            binding.endpoint.clone(),
            binding.endpoint_host.clone(),
            binding.language.clone(),
            name.to_owned(),
        )
    };
    let mut want = vec![expected(&base, "Original")];
    want.extend(
        variants
            .iter()
            .map(|(what, variant)| expected(variant, &format!("Variant {what}"))),
    );
    assert_eq!(rows, want);
    assert_eq!(
        rows[4].1, "http://geocoder.example.net:443/nominatim/",
        "scheme variant keeps its own explicit port"
    );
}

#[test]
fn lookups_survive_a_restart_with_their_own_records() {
    let temporary = crate::private_tempdir().unwrap();
    let binding = lookup_binding("https://geocoder.example.net/", "en");
    let near = lookup_point(51.5, -0.1);
    let far = lookup_point(-33.9, 151.2);
    let first = lookup_record(&binding, near, "Alpha", 10);
    let second = lookup_record(&binding, far, "Beta", 20);
    {
        let store = HubStore::initialize(temporary.path()).unwrap();
        store.put_address_cache(&first).unwrap();
        store.put_address_cache(&second).unwrap();
    }
    let reopened = HubStore::initialize(temporary.path()).unwrap();
    assert_eq!(
        reopened.cached_address(&binding, near).unwrap(),
        Some(first)
    );
    assert_eq!(
        reopened.cached_address(&binding, far).unwrap(),
        Some(second)
    );
    assert_eq!(
        reopened
            .cached_address(&lookup_binding("https://geocoder.example.net/", "de"), near)
            .unwrap(),
        None
    );
}

#[test]
fn an_invalid_lookup_record_stores_nothing() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let binding = lookup_binding("https://geocoder.example.net/", "en");
    let point = lookup_point(1.0, 2.0);
    let good = lookup_record(&binding, point, "Good", 1);

    let mut hand_built = good.clone();
    hand_built.binding.endpoint = "https://geocoder.example.net/".to_owned();
    let mut wrong_host = good.clone();
    wrong_host.binding.endpoint_host = "other.example.net".to_owned();
    let mut wrong_provider = good.clone();
    wrong_provider.binding.provider = "google".to_owned();
    let mut empty_language = good.clone();
    empty_language.binding.language = String::new();
    let mut empty_licence = good.clone();
    empty_licence.licence = Some(String::new());
    let mut long_licence = good.clone();
    long_licence.licence = Some("l".repeat(513));
    let mut control_licence = good.clone();
    control_licence.licence = Some("ODbL\n1.0".to_owned());
    for (what, record) in [
        ("endpoint not in normal form", hand_built),
        ("endpoint host disagrees", wrong_host),
        ("provider is not nominatim", wrong_provider),
        ("empty language", empty_language),
        ("empty licence", empty_licence),
        ("licence over 512 bytes", long_licence),
        ("control character in licence", control_licence),
    ] {
        assert!(
            matches!(
                store.put_address_cache(&record),
                Err(StoreError::InvalidAddressCache)
            ),
            "{what}"
        );
    }
    assert_eq!(lookup_row_count(&store), 0);
    assert!(shared_address_rows(&store).is_empty());

    let mut longest = good.clone();
    longest.licence = Some("l".repeat(512));
    store.put_address_cache(&longest).unwrap();
    assert_eq!(
        store
            .cached_address(&binding, point)
            .unwrap()
            .and_then(|record| record.licence)
            .map(|licence| licence.len()),
        Some(512)
    );
}

/// The v68 shape of `address_lookup_cache`: the six original columns only.
const LEGACY_LOOKUP_ROWS_SQL: &str = "
    SELECT lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms
    FROM address_lookup_cache ORDER BY lookup_key";

type LegacyLookupRow = (String, f64, f64, String, i64, i64);

/// osm type, osm id, display name, raw JSON of a shared `address_cache` row.
type SharedAddressRow = (String, i64, Option<String>, Option<String>);

fn legacy_lookup_rows(connection: &Connection) -> Vec<LegacyLookupRow> {
    connection
        .prepare(LEGACY_LOOKUP_ROWS_SQL)
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_v68_store_migrates_and_its_old_lookup_rows_stay_readable_but_are_never_matched() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    // A v68 catalogue: the lookup table has its six original columns, and the
    // rows written by the previous release use the version-1 key.
    connection.execute_batch(REWIND_V69_SQL).unwrap();
    connection
        .execute_batch(
            "INSERT INTO address_cache(osm_type, osm_id, display_name, name, latitude, longitude,
                                       road, city, country, raw_json)
                 VALUES ('way', 42, 'Old Name, Old City', 'Old Name', 51.5001, -0.1001,
                         'Old Road', 'Old City', 'Testland', '{\"old\":true}'),
                        ('node', 7, 'Corner Shop', NULL, NULL, NULL, NULL, NULL, NULL, NULL);
             INSERT INTO address_lookup_cache(
                 lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms)
                 VALUES ('51.500000:-0.100000', 51.5, -0.1, 'way', 42, 111),
                        ('51.500100:-0.100100', 51.5001, -0.1001, 'way', 42, 222),
                        ('10.000000:20.000000', 10.0, 20.0, 'node', 7, 333);
             PRAGMA user_version = 68;",
        )
        .unwrap();
    let before_rows = legacy_lookup_rows(&connection);
    assert_eq!(before_rows.len(), 3);
    let before_shared: Vec<SharedAddressRow> = connection
        .prepare(
            "SELECT osm_type, osm_id, display_name, raw_json FROM address_cache ORDER BY osm_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(connection);
    drop(store);

    let migrated = HubStore::initialize(temporary.path()).unwrap();
    let connection = migrated.open().unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    connection
        .execute_batch("PRAGMA foreign_key_check;")
        .unwrap();

    // The version-1 rows are present and unchanged, with NULL in every new
    // column (the provider-unknown shape), and still read through the shared
    // identity they point at.
    assert_eq!(legacy_lookup_rows(&connection), before_rows);
    let unknown_columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM address_lookup_cache
             WHERE provider IS NULL AND endpoint IS NULL AND endpoint_host IS NULL
               AND language IS NULL AND licence IS NULL AND display_name IS NULL
               AND name IS NULL AND result_latitude IS NULL AND result_longitude IS NULL
               AND house_number IS NULL AND road IS NULL AND neighbourhood IS NULL
               AND city IS NULL AND county IS NULL AND postcode IS NULL
               AND state IS NULL AND state_district IS NULL AND country IS NULL
               AND raw_json IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown_columns, 3);
    let readable: Vec<(String, String)> = connection
        .prepare(
            "SELECT l.lookup_key, a.display_name FROM address_lookup_cache l
             JOIN address_cache a ON a.osm_type = l.osm_type AND a.osm_id = l.osm_id
             ORDER BY l.lookup_key",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        readable,
        vec![
            ("10.000000:20.000000".to_owned(), "Corner Shop".to_owned()),
            (
                "51.500000:-0.100000".to_owned(),
                "Old Name, Old City".to_owned()
            ),
            (
                "51.500100:-0.100100".to_owned(),
                "Old Name, Old City".to_owned()
            ),
        ]
    );

    // No new lookup is answered from them, whatever the binding.
    for binding in [
        lookup_binding("https://geocoder.example.net/", "en"),
        lookup_binding("http://127.0.0.1:8080/", "de"),
    ] {
        for (latitude, longitude) in [(51.5, -0.1), (51.5001, -0.1001), (10.0, 20.0)] {
            assert_eq!(
                migrated
                    .cached_address(&binding, lookup_point(latitude, longitude))
                    .unwrap(),
                None,
                "{latitude},{longitude}"
            );
        }
    }

    // A fresh lookup at a legacy row's coordinates reaching the same identity
    // is its own row; neither the old rows nor the shared identity change.
    let binding = lookup_binding("https://geocoder.example.net/", "en");
    let fresh = lookup_record(
        &binding,
        lookup_point(51.5, -0.1),
        "New Name, New City",
        999,
    );
    migrated.put_address_cache(&fresh).unwrap();
    assert_eq!(
        migrated
            .cached_address(&binding, lookup_point(51.5, -0.1))
            .unwrap(),
        Some(fresh)
    );
    assert_eq!(lookup_row_count(&migrated), 4);
    let connection = migrated.open().unwrap();
    let v1_after: Vec<LegacyLookupRow> = legacy_lookup_rows(&connection)
        .into_iter()
        .filter(|row| !row.0.starts_with("v2:"))
        .collect();
    assert_eq!(v1_after, before_rows);
    let after_shared: Vec<SharedAddressRow> = connection
        .prepare(
            "SELECT osm_type, osm_id, display_name, raw_json FROM address_cache ORDER BY osm_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after_shared, before_shared);
}

#[test]
fn the_v69_step_adds_only_nullable_columns_with_their_checks() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    let columns: Vec<(String, String, i64)> = connection
        .prepare(
            "SELECT name, type, \"notnull\" FROM pragma_table_info('address_lookup_cache')
             ORDER BY cid",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let names: Vec<&str> = columns.iter().map(|column| column.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "lookup_key",
            "latitude",
            "longitude",
            "osm_type",
            "osm_id",
            "looked_up_at_ms",
            "provider",
            "endpoint",
            "endpoint_host",
            "language",
            "licence",
            "display_name",
            "name",
            "result_latitude",
            "result_longitude",
            "house_number",
            "road",
            "neighbourhood",
            "city",
            "county",
            "postcode",
            "state",
            "state_district",
            "country",
            "raw_json",
        ]
    );
    for column in &columns[6..] {
        assert_eq!(column.2, 0, "{} is nullable (old rows have none)", column.0);
        assert!(
            column.1 == "TEXT" || column.1 == "REAL",
            "{}: STRICT type {}",
            column.0,
            column.1
        );
    }
    let strict: i64 = connection
        .query_row(
            "SELECT strict FROM pragma_table_list('address_lookup_cache')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(strict, 1);

    // The column checks hold for rows that bypass the Rust validation: each
    // of these is refused by a CHECK, not by a mistake in the statement.
    connection
        .execute_batch(
            "INSERT INTO address_cache(osm_type, osm_id, display_name) VALUES ('way', 1, 'x')",
        )
        .unwrap();
    let insert = |column: &str, value: &str| {
        connection.execute_batch(&format!(
            "INSERT INTO address_lookup_cache(lookup_key, latitude, longitude, osm_type,
                                              osm_id, looked_up_at_ms, {column})
             VALUES ('key-{column}', 1, 1, 'way', 1, 1, {value})"
        ))
    };
    for (column, value) in [
        ("provider", "'google'"),
        ("endpoint", "''"),
        ("endpoint_host", "''"),
        ("language", "''"),
        ("licence", "''"),
        ("display_name", "''"),
        ("result_latitude", "91.0"),
        ("result_longitude", "-181.0"),
        ("raw_json", "'{not json'"),
    ] {
        let error = insert(column, value).expect_err(column).to_string();
        assert!(
            error.contains("CHECK constraint failed"),
            "{column}: {error}"
        );
    }
    for (column, value) in [
        ("provider", "'nominatim'"),
        ("licence", "'ODbL 1.0'"),
        ("raw_json", "'{}'"),
        ("result_latitude", "-90.0"),
    ] {
        insert(column, value).unwrap_or_else(|error| panic!("{column}: {error}"));
    }
    assert_eq!(lookup_row_count(&store), 4);
}
