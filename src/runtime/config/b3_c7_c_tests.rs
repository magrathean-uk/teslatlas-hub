// SPDX-License-Identifier: AGPL-3.0-only

// C7 part C configuration test (spec section 7.5, C7 T64): the defaults of the
// geocoder section. Pure functions and parsed text only; nothing is sent.

use super::{valid_config, *};

/// The public OpenStreetMap host, which the owner may choose but which is never a default.
const PUBLIC_ENDPOINT: &str = "https://nominatim.openstreetmap.org/";

#[test]
fn c7_t64_defaults() {
    // DEC-246: off, and no endpoint, so no provider in any mode.
    let defaults = GeocoderConfig::default();
    assert!(!defaults.enabled);
    assert!(defaults.endpoint.is_none());
    for test_mode in [false, true] {
        assert!(matches!(
            defaults.endpoint_url(test_mode),
            Err(ConfigError::InvalidGeocoderEndpoint)
        ));
    }
    // A configured endpoint is used and gets its trailing slash.
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
    let base = valid_config(Path::new("/var/lib/tesatlas"));
    let (parsed, _) = HubConfig::from_exact_bytes(base.as_bytes()).expect("the base parses");
    assert!(!parsed.geocoder.enabled);
    assert!(parsed.geocoder.endpoint.is_none());
    // Turning lookup on without naming a provider is refused when the file loads.
    let explicit = format!("{base}[geocoder]\nenabled = true\n");
    assert!(matches!(
        HubConfig::from_exact_bytes(explicit.as_bytes()),
        Err(ConfigError::InvalidGeocoderEndpoint)
    ));
    // What the Mac app's onboarding writes when the owner says yes.
    let chosen = format!("{base}[geocoder]\nenabled = true\nendpoint = \"{PUBLIC_ENDPOINT}\"\n");
    let (parsed, _) =
        HubConfig::from_exact_bytes(chosen.as_bytes()).expect("the owner's choice parses");
    assert!(parsed.geocoder.enabled);
    assert_eq!(
        parsed.geocoder.endpoint_url(false).expect("chosen"),
        Url::parse(PUBLIC_ENDPOINT).expect("URL")
    );
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
