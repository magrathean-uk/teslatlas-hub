// SPDX-License-Identifier: AGPL-3.0-only

// C7 part C configuration test (spec section 7.5, C7 T64): the defaults of the
// geocoder section. Pure functions and parsed text only; nothing is sent.

use super::{valid_config, *};

/// The public default endpoint of C7 K-DEFAULT-ENDPOINT.
const PUBLIC_ENDPOINT: &str = "https://nominatim.openstreetmap.org/";

#[test]
fn c7_t64_defaults() {
    // C7 R56 and C7 R57: enabled, and no endpoint, which means the public host.
    let defaults = GeocoderConfig::default();
    assert!(defaults.enabled);
    assert!(defaults.endpoint.is_none());
    assert_eq!(
        defaults.endpoint_url(false).expect("the public default"),
        Url::parse(PUBLIC_ENDPOINT).expect("URL")
    );
    // Test mode never falls back to the public host.
    assert!(matches!(
        defaults.endpoint_url(true),
        Err(ConfigError::InvalidGeocoderEndpoint)
    ));
    // A configured endpoint replaces the default and gets its trailing slash.
    let configured = GeocoderConfig {
        endpoint: Some("https://geo.example.test/nominatim".into()),
        ..GeocoderConfig::default()
    };
    assert_eq!(
        configured.endpoint_url(false).expect("configured").as_str(),
        "https://geo.example.test/nominatim/"
    );

    // The same defaults reach a loaded file. The base text is the one the kept
    // configuration tests parse (`valid_config`).
    let base = valid_config(Path::new("/var/lib/teslatlas"));
    let (parsed, _) = HubConfig::from_exact_bytes(base.as_bytes()).expect("the base parses");
    assert!(parsed.geocoder.enabled);
    assert!(parsed.geocoder.endpoint.is_none());
    let explicit = format!("{base}[geocoder]\nenabled = true\n");
    let (parsed, _) =
        HubConfig::from_exact_bytes(explicit.as_bytes()).expect("enabled alone parses");
    assert!(parsed.geocoder.enabled);
    for endpoint in [
        "https://user:pw@geo.example.test/",
        "https://geo.example.test/?key=abc",
        "https://geo.example.test/#frag",
        "http://geo.example.test/",
    ] {
        let text = format!("{base}[geocoder]\nendpoint = \"{endpoint}\"\n");
        let refused = HubConfig::from_exact_bytes(text.as_bytes());
        assert!(
            matches!(refused, Err(ConfigError::InvalidGeocoderEndpoint)),
            "{endpoint}: {refused:?}"
        );
    }
}
