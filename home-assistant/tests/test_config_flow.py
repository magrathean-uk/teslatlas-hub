"""Tests for Teslatlas Hub setup flows."""

from __future__ import annotations

from dataclasses import replace
from ipaddress import IPv4Address
from unittest.mock import patch

import pytest
from homeassistant.config_entries import (
    SOURCE_REAUTH,
    SOURCE_RECONFIGURE,
    SOURCE_USER,
    SOURCE_ZEROCONF,
)
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers.service_info.zeroconf import ZeroconfServiceInfo
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubConnectionError,
    HubPairingError,
    ProtocolContractUnavailable,
)
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from tests.helpers import FixtureHubClient


@pytest.fixture
def fixture_client() -> FixtureHubClient:
    """Return a complete fake of the public client boundary."""
    return FixtureHubClient()


@pytest.fixture
def client_factory(fixture_client: FixtureHubClient):
    """Route config flows to the fixture client."""
    with patch(
        "custom_components.teslatlas_hub.config_flow.create_client",
        return_value=fixture_client,
    ) as factory:
        yield factory


async def _start_user_flow(hass: HomeAssistant) -> dict:
    return await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_USER},
    )


async def _submit_endpoint(hass: HomeAssistant, flow_id: str) -> dict:
    return await hass.config_entries.flow.async_configure(
        flow_id,
        {
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
        },
    )


def _configured_entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "old-hub.local",
            CONF_PORT: 443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "old-device-bearer",
        },
    )
    entry.add_to_hass(hass)
    return entry


def _zeroconf_info() -> ZeroconfServiceInfo:
    address = IPv4Address("192.0.2.10")
    return ZeroconfServiceInfo(
        ip_address=address,
        ip_addresses=[address],
        port=7443,
        hostname="hub-fixture.local.",
        type="_teslatlas-hub._tcp.local.",
        name="Fixture Hub._teslatlas-hub._tcp.local.",
        properties={"ignored_unfrozen_txt_key": "ignored"},
    )


async def test_user_flow_claims_transient_secret(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch setup that stores a pairing secret or skips identity probing."""
    result = await _start_user_flow(hass)
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "user"

    result = await _submit_endpoint(hass, result["flow_id"])
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair"
    assert fixture_client.closed is True

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "fixture-pairing-secret"},
    )

    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["title"] == "Fixture Hub"
    assert result["data"] == {
        CONF_HOST: "hub-fixture.local",
        CONF_PORT: 7443,
        CONF_USE_TLS: True,
        CONF_HUB_ID: "hub-fixture",
        CONF_ACCESS_TOKEN: "fixture-device-bearer",
    }
    assert fixture_client.pairing_secrets == ["fixture-pairing-secret"]
    assert CONF_PAIRING_SECRET not in result["data"]
    assert client_factory.call_count == 2


@pytest.mark.parametrize(
    ("error", "expected_error"),
    [
        (HubConnectionError("offline"), "cannot_connect"),
        (HubAuthenticationError("expired"), "invalid_auth"),
        (
            ProtocolContractUnavailable("public protocol unavailable"),
            "protocol_not_ready",
        ),
    ],
)
async def test_user_flow_reports_probe_failure(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
    error: Exception,
    expected_error: str,
) -> None:
    """Catch endpoint failures that escape as exceptions or claim readiness."""
    fixture_client.probe_error = error
    result = await _start_user_flow(hass)

    result = await _submit_endpoint(hass, result["flow_id"])

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "user"
    assert result["errors"] == {"base": expected_error}


async def test_user_flow_rejects_invalid_pairing_secret(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch rejected pairing claims that create unusable entries."""
    fixture_client.pair_error = HubPairingError("rejected")
    result = await _start_user_flow(hass)
    result = await _submit_endpoint(hass, result["flow_id"])

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "wrong-secret"},
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair"
    assert result["errors"] == {"base": "invalid_pairing_secret"}


async def test_user_flow_prevents_duplicate_hub(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch multiple config entries for one stable Hub identity."""
    MockConfigEntry(
        domain=DOMAIN,
        unique_id="hub-fixture",
        data={CONF_HUB_ID: "hub-fixture"},
    ).add_to_hass(hass)
    result = await _start_user_flow(hass)

    result = await _submit_endpoint(hass, result["flow_id"])

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "already_configured"
    assert fixture_client.closed is True


@pytest.mark.parametrize(
    ("error", "expected_error"),
    [
        (HubConnectionError("offline"), "cannot_connect"),
        (HubAuthenticationError("expired"), "invalid_auth"),
        (
            ProtocolContractUnavailable("public protocol unavailable"),
            "protocol_not_ready",
        ),
    ],
)
async def test_pair_step_reports_transport_failures(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
    error: Exception,
    expected_error: str,
) -> None:
    """Catch pairing transport failures that escape the repairable form."""
    result = await _start_user_flow(hass)
    result = await _submit_endpoint(hass, result["flow_id"])
    fixture_client.pair_error = error

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "fixture-pairing-secret"},
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair"
    assert result["errors"] == {"base": expected_error}


async def test_pair_step_rejects_identity_change_and_closes_client(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch pairing against an identity different from the probed Hub."""
    result = await _start_user_flow(hass)
    result = await _submit_endpoint(hass, result["flow_id"])
    fixture_client.info = replace(fixture_client.info, hub_id="other-hub")

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "other-hub-secret"},
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert fixture_client.closed is True


async def test_zeroconf_uses_address_without_txt_assumptions(
    hass: HomeAssistant,
    client_factory,
) -> None:
    """Catch reliance on unfrozen TXT fields or silent TLS selection."""
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_ZEROCONF},
        data=_zeroconf_info(),
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "discovery"

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_USE_TLS: True},
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair"
    client_factory.assert_called_once()
    endpoint = client_factory.call_args.args[0]
    assert endpoint.host == "192.0.2.10"
    assert endpoint.port == 7443
    assert endpoint.use_tls is True


async def test_zeroconf_updates_existing_endpoint(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch discovery that creates duplicates or leaves a stale endpoint."""
    entry = _configured_entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_ZEROCONF},
        data=_zeroconf_info(),
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_USE_TLS: True},
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "already_configured"
    assert entry.data == {
        CONF_HOST: "192.0.2.10",
        CONF_PORT: 7443,
        CONF_USE_TLS: True,
        CONF_HUB_ID: "hub-fixture",
        CONF_ACCESS_TOKEN: "old-device-bearer",
    }
    assert fixture_client.closed is True


async def test_zeroconf_without_port_aborts(hass: HomeAssistant) -> None:
    """Catch discovery that fabricates a missing public service port."""
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_ZEROCONF},
        data=replace(_zeroconf_info(), port=None),
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "cannot_connect"


async def test_zeroconf_reports_probe_failure(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch a discovered endpoint failure that escapes the confirmation form."""
    fixture_client.probe_error = HubConnectionError("offline")
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_ZEROCONF},
        data=_zeroconf_info(),
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_USE_TLS: True},
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "discovery"
    assert result["errors"] == {"base": "cannot_connect"}


async def test_reauth_replaces_only_device_bearer(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch reauth that creates another entry or changes Hub identity."""
    entry = _configured_entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reauth_confirm"

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "fresh-pairing-secret"},
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reauth_successful"
    assert entry.data[CONF_ACCESS_TOKEN] == "fixture-device-bearer"
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert entry.unique_id == "hub-fixture"
    assert fixture_client.pairing_secrets == ["fresh-pairing-secret"]


async def test_reauth_rejects_different_hub_identity(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch bearer replacement from a different physical Hub."""
    entry = _configured_entry(hass)
    fixture_client.info = type(fixture_client.info)(
        hub_id="other-hub",
        name="Other Hub",
        protocol_version=fixture_client.info.protocol_version,
        capabilities=fixture_client.info.capabilities,
    )
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "other-hub-secret"},
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"
    assert fixture_client.closed is True


@pytest.mark.parametrize(
    ("error", "expected_error"),
    [
        (HubPairingError("rejected"), "invalid_pairing_secret"),
        (HubConnectionError("offline"), "cannot_connect"),
        (HubAuthenticationError("expired"), "invalid_auth"),
        (
            ProtocolContractUnavailable("public protocol unavailable"),
            "protocol_not_ready",
        ),
    ],
)
async def test_reauth_reports_failure_and_closes_attempt(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
    error: Exception,
    expected_error: str,
) -> None:
    """Catch leaked clients or hidden errors during bearer replacement."""
    entry = _configured_entry(hass)
    fixture_client.pair_error = error
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_PAIRING_SECRET: "failed-secret"},
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reauth_confirm"
    assert result["errors"] == {"base": expected_error}
    assert fixture_client.closed is True


async def test_reconfigure_updates_endpoint_for_same_hub(
    hass: HomeAssistant,
    client_factory,
) -> None:
    """Catch endpoint changes that replace credentials or create entries."""
    entry = _configured_entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reconfigure_confirm"

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "new-hub.local",
            CONF_PORT: 8443,
            CONF_USE_TLS: True,
        },
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reconfigure_successful"
    assert entry.data == {
        CONF_HOST: "new-hub.local",
        CONF_PORT: 8443,
        CONF_USE_TLS: True,
        CONF_HUB_ID: "hub-fixture",
        CONF_ACCESS_TOKEN: "old-device-bearer",
    }
    assert client_factory.call_args.kwargs["bearer_token"] == "old-device-bearer"


async def test_reconfigure_rejects_different_hub_identity(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch endpoint roaming across unverified Hub identities."""
    entry = _configured_entry(hass)
    fixture_client.info = type(fixture_client.info)(
        hub_id="other-hub",
        name="Other Hub",
        protocol_version=fixture_client.info.protocol_version,
        capabilities=fixture_client.info.capabilities,
    )
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "other-hub.local",
            CONF_PORT: 8443,
            CONF_USE_TLS: True,
        },
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert fixture_client.closed is True


async def test_reconfigure_reports_probe_failure(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch endpoint probe failures that mutate the working config entry."""
    entry = _configured_entry(hass)
    fixture_client.probe_error = HubConnectionError("offline")
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "offline-hub.local",
            CONF_PORT: 8443,
            CONF_USE_TLS: True,
        },
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reconfigure_confirm"
    assert result["errors"] == {"base": "cannot_connect"}
    assert entry.data[CONF_HOST] == "old-hub.local"
