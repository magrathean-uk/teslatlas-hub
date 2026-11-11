"""Constants for the Teslatlas Hub integration."""

from __future__ import annotations

from typing import Final

from homeassistant.const import Platform

DOMAIN: Final = "teslatlas_hub"
PLATFORMS: Final = (Platform.SENSOR,)

CONF_ACCESS_TOKEN: Final = "access_token"
CONF_DEVICE_ID: Final = "device_id"
CONF_DEVICE_NAME: Final = "device_name"
CONF_HUB_ID: Final = "hub_id"
CONF_PAIRING_ID: Final = "pairing_id"
CONF_PAIRING_SECRET: Final = "pairing_secret"
CONF_PORT: Final = "port"
CONF_TLS_PIN: Final = "tls_pin"
CONF_TOKEN_EXPIRES_AT_MS: Final = "token_expires_at_ms"
CONF_USE_TLS: Final = "use_tls"

CONFIG_ENTRY_MINOR_VERSION: Final = 2
CONFIG_ENTRY_VERSION: Final = 1
DEFAULT_PORT: Final = 443
