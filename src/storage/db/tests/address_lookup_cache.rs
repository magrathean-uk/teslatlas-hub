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

// G2-4: provider-unbound rows never satisfy a current lookup.
#[test]
fn version_one_lookup_rows_remain_unmatched_on_the_floor_store() {
    let test = "version_one_lookup_rows_remain_unmatched_on_the_floor_store";
    let temporary = g2_4_m1::copied_store(include_bytes!(
        "../../../../fixtures/g2-4/m1/store-f.sqlite.zst"
    ));
    let store = HubStore::open_existing(temporary.path()).unwrap();
    let connection = store.open().unwrap();
    connection.execute("INSERT INTO address_cache(osm_type, osm_id, display_name) VALUES ('way', 42, 'Version one place')", []).unwrap();
    connection.execute("INSERT INTO address_lookup_cache(lookup_key,latitude,longitude,osm_type,osm_id,looked_up_at_ms) VALUES ('51.500000:-0.100000',51.5,-0.1,'way',42,17)", []).unwrap();
    let legacy_rows = || {
        let mut statement = connection
            .prepare(
                "SELECT * FROM address_lookup_cache WHERE provider IS NULL ORDER BY lookup_key",
            )
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = legacy_rows();
    let shared_rows = || {
        let mut statement = connection
            .prepare("SELECT * FROM address_cache ORDER BY osm_type, osm_id")
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let shared_before = shared_rows();
    let count_before = lookup_row_count(&store);
    let binding = lookup_binding("https://geocoder.example.net/", "en");
    let point = lookup_point(51.5, -0.1);
    assert!(
        matches!(store.cached_address(&binding, point), Ok(None)),
        "assertion failed: {test}: a version one provider-NULL row never matches a current lookup"
    );
    let current_key = address_lookup_key(&binding, point);
    connection.execute("INSERT INTO address_lookup_cache(lookup_key,latitude,longitude,osm_type,osm_id,looked_up_at_ms,provider,endpoint,endpoint_host,language,display_name) VALUES (?1,51.5,-0.1,'way',42,19,NULL,?2,?3,?4,'Unbound lookup')", params![current_key,binding.endpoint,binding.endpoint_host,binding.language]).unwrap();
    assert!(
        matches!(store.cached_address(&binding, point), Ok(None)),
        "assertion failed: {test}: a provider-NULL row under the real current key is excluded before decoding"
    );
    let removed = connection
        .execute(
            "DELETE FROM address_lookup_cache WHERE lookup_key=?1 AND provider IS NULL",
            [&current_key],
        )
        .unwrap();
    assert!(
        removed == 1 && legacy_rows() == before,
        "assertion failed: {test}: removing the current-key probe preserves every original row"
    );
    let fresh = lookup_record(&binding, point, "Fresh lookup", 23);
    store
        .put_address_cache(&fresh)
        .labelled_ok(test, "a current lookup can coexist with version one rows");
    assert!(
        store.cached_address(&binding, point).unwrap() == Some(fresh),
        "assertion failed: {test}: the fresh provider-bound lookup is returned"
    );
    assert!(
        legacy_rows() == before,
        "assertion failed: {test}: every provider-NULL row and value remains unchanged"
    );
    assert!(
        shared_rows() == shared_before,
        "assertion failed: {test}: existing shared identity rows remain unchanged"
    );
    assert!(
        lookup_row_count(&store) == count_before + 1,
        "assertion failed: {test}: the fresh lookup adds one distinct row"
    );
}
