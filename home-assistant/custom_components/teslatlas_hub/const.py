"""Constants for the Teslatlas Hub integration."""

from __future__ import annotations

from typing import Final

from homeassistant.const import Platform

DOMAIN: Final = "teslatlas_hub"
PLATFORMS: Final = (Platform.SENSOR,)

CONF_ACCESS_TOKEN: Final = "access_token"
CONF_HUB_ID: Final = "hub_id"
CONF_PAIRING_SECRET: Final = "pairing_secret"
CONF_PORT: Final = "port"
CONF_USE_TLS: Final = "use_tls"

CONFIG_ENTRY_MINOR_VERSION: Final = 1
CONFIG_ENTRY_VERSION: Final = 1
DEFAULT_PORT: Final = 443
ZEROCONF_TYPE: Final = "_teslatlas-hub._tcp.local."
