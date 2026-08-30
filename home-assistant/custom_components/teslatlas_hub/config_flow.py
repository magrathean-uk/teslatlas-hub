"""Config flow for Teslatlas Hub."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any, override

import voluptuous as vol
from homeassistant.config_entries import ConfigFlow, ConfigFlowResult
from homeassistant.const import CONF_HOST
from homeassistant.data_entry_flow import AbortFlow
from homeassistant.helpers.selector import (
    BooleanSelector,
    NumberSelector,
    NumberSelectorConfig,
    NumberSelectorMode,
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)
from homeassistant.helpers.service_info.zeroconf import ZeroconfServiceInfo

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    HubPairingError,
    ProtocolContractUnavailable,
    TeslatlasHubClient,
    create_client,
)
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_USE_TLS,
    CONFIG_ENTRY_MINOR_VERSION,
    CONFIG_ENTRY_VERSION,
    DEFAULT_PORT,
    DOMAIN,
)
from .models import HubEndpoint, HubInfo


def _endpoint_schema(defaults: dict[str, Any] | None = None) -> vol.Schema:
    """Return the endpoint form schema."""
    defaults = defaults or {}
    return vol.Schema(
        {
            vol.Required(
                CONF_HOST,
                default=defaults.get(CONF_HOST, ""),
            ): TextSelector(TextSelectorConfig(type=TextSelectorType.TEXT)),
            vol.Required(
                CONF_PORT,
                default=defaults.get(CONF_PORT, DEFAULT_PORT),
            ): NumberSelector(
                NumberSelectorConfig(
                    min=1,
                    max=65535,
                    mode=NumberSelectorMode.BOX,
                )
            ),
            vol.Required(
                CONF_USE_TLS,
                default=defaults.get(CONF_USE_TLS, True),
            ): BooleanSelector(),
        }
    )


PAIRING_SCHEMA = vol.Schema(
    {
        vol.Required(CONF_PAIRING_SECRET): TextSelector(
            TextSelectorConfig(type=TextSelectorType.PASSWORD)
        )
    }
)

DISCOVERY_SCHEMA = vol.Schema(
    {vol.Required(CONF_USE_TLS, default=True): BooleanSelector()}
)


class TeslatlasHubConfigFlow(ConfigFlow, domain=DOMAIN):
    """Configure one public Teslatlas Hub identity."""

    VERSION = CONFIG_ENTRY_VERSION
    MINOR_VERSION = CONFIG_ENTRY_MINOR_VERSION

    def __init__(self) -> None:
        """Initialize transient flow state."""
        self._discovery_host: str | None = None
        self._discovery_port: int | None = None
        self._endpoint: HubEndpoint | None = None
        self._info: HubInfo | None = None

    async def _async_probe(
        self,
        endpoint: HubEndpoint,
        bearer_token: str | None = None,
    ) -> tuple[TeslatlasHubClient | None, HubInfo | None, str | None]:
        """Probe one endpoint and translate client failures for the flow."""
        client = create_client(endpoint, bearer_token=bearer_token)
        try:
            info = await client.async_probe()
        except HubConnectionError:
            error = "cannot_connect"
        except HubAuthenticationError:
            error = "invalid_auth"
        except ProtocolContractUnavailable:
            error = "protocol_not_ready"
        else:
            return client, info, None

        await client.async_close()
        return None, None, error

    async def _async_accept_endpoint(
        self,
        endpoint: HubEndpoint,
        *,
        update_existing: bool = False,
    ) -> ConfigFlowResult | str:
        """Probe, bind stable identity, and continue to pairing."""
        client, info, error = await self._async_probe(endpoint)
        if error is not None:
            return error
        assert client is not None
        assert info is not None

        try:
            self._endpoint = endpoint
            self._info = info
            await self.async_set_unique_id(info.hub_id)
            updates = None
            if update_existing:
                updates = {
                    CONF_HOST: endpoint.host,
                    CONF_PORT: endpoint.port,
                    CONF_USE_TLS: endpoint.use_tls,
                }
            self._abort_if_unique_id_configured(updates=updates)
        finally:
            await client.async_close()
        return await self.async_step_pair()

    @override
    async def async_step_user(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Collect and validate a public Hub endpoint."""
        errors: dict[str, str] = {}
        if user_input is not None:
            endpoint = HubEndpoint(
                host=user_input[CONF_HOST],
                port=int(user_input[CONF_PORT]),
                use_tls=user_input[CONF_USE_TLS],
            )
            result = await self._async_accept_endpoint(endpoint)
            if not isinstance(result, str):
                return result
            errors["base"] = result

        return self.async_show_form(
            step_id="user",
            data_schema=_endpoint_schema(user_input),
            errors=errors,
        )

    @override
    async def async_step_zeroconf(
        self,
        discovery_info: ZeroconfServiceInfo,
    ) -> ConfigFlowResult:
        """Collect a discovered address without trusting unfrozen TXT data."""
        if discovery_info.port is None:
            return self.async_abort(reason="cannot_connect")
        self._discovery_host = discovery_info.host
        self._discovery_port = discovery_info.port
        self.context["title_placeholders"] = {"name": discovery_info.name}
        return await self.async_step_discovery()

    async def async_step_discovery(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Confirm transport security before probing a discovered endpoint."""
        errors: dict[str, str] = {}
        if user_input is not None:
            assert self._discovery_host is not None
            assert self._discovery_port is not None
            endpoint = HubEndpoint(
                host=self._discovery_host,
                port=self._discovery_port,
                use_tls=user_input[CONF_USE_TLS],
            )
            result = await self._async_accept_endpoint(
                endpoint,
                update_existing=True,
            )
            if not isinstance(result, str):
                return result
            errors["base"] = result

        return self.async_show_form(
            step_id="discovery",
            data_schema=DISCOVERY_SCHEMA,
            errors=errors,
        )

    async def async_step_pair(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Claim a transient pairing secret for a scoped device bearer."""
        errors: dict[str, str] = {}
        if user_input is not None:
            assert self._endpoint is not None
            assert self._info is not None
            client = create_client(self._endpoint)
            try:
                result = await client.async_pair(user_input[CONF_PAIRING_SECRET])
            except HubPairingError:
                errors["base"] = "invalid_pairing_secret"
            except HubConnectionError:
                errors["base"] = "cannot_connect"
            except HubAuthenticationError:
                errors["base"] = "invalid_auth"
            except ProtocolContractUnavailable:
                errors["base"] = "protocol_not_ready"
            else:
                if result.info.hub_id != self._info.hub_id:
                    return self.async_abort(reason="wrong_hub")
                return self.async_create_entry(
                    title=result.info.name,
                    data={
                        CONF_HOST: self._endpoint.host,
                        CONF_PORT: self._endpoint.port,
                        CONF_USE_TLS: self._endpoint.use_tls,
                        CONF_HUB_ID: result.info.hub_id,
                        CONF_ACCESS_TOKEN: result.access_token,
                    },
                )
            finally:
                await client.async_close()

        return self.async_show_form(
            step_id="pair",
            data_schema=PAIRING_SCHEMA,
            errors=errors,
        )

    @override
    async def async_step_reauth(
        self,
        entry_data: Mapping[str, Any],
    ) -> ConfigFlowResult:
        """Start device-bearer replacement for an existing Hub."""
        return await self.async_step_reauth_confirm()

    async def async_step_reauth_confirm(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Claim a fresh device bearer and preserve stable Hub identity."""
        errors: dict[str, str] = {}
        if user_input is not None:
            entry = self._get_reauth_entry()
            endpoint = HubEndpoint(
                host=entry.data[CONF_HOST],
                port=entry.data[CONF_PORT],
                use_tls=entry.data[CONF_USE_TLS],
            )
            client = create_client(endpoint)
            try:
                result = await client.async_pair(user_input[CONF_PAIRING_SECRET])
            except HubPairingError:
                errors["base"] = "invalid_pairing_secret"
            except HubConnectionError:
                errors["base"] = "cannot_connect"
            except HubAuthenticationError:
                errors["base"] = "invalid_auth"
            except ProtocolContractUnavailable:
                errors["base"] = "protocol_not_ready"
            else:
                await self.async_set_unique_id(result.info.hub_id)
                try:
                    self._abort_if_unique_id_mismatch(reason="wrong_hub")
                except AbortFlow:
                    await client.async_close()
                    raise
                await client.async_close()
                return self.async_update_reload_and_abort(
                    entry,
                    data_updates={CONF_ACCESS_TOKEN: result.access_token},
                )
            await client.async_close()

        return self.async_show_form(
            step_id="reauth_confirm",
            data_schema=PAIRING_SCHEMA,
            errors=errors,
        )

    @override
    async def async_step_reconfigure(
        self,
        entry_data: Mapping[str, Any],
    ) -> ConfigFlowResult:
        """Start endpoint roaming for an existing Hub."""
        return await self.async_step_reconfigure_confirm()

    async def async_step_reconfigure_confirm(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Validate a replacement endpoint against stable Hub identity."""
        entry = self._get_reconfigure_entry()
        errors: dict[str, str] = {}
        if user_input is not None:
            endpoint = HubEndpoint(
                host=user_input[CONF_HOST],
                port=int(user_input[CONF_PORT]),
                use_tls=user_input[CONF_USE_TLS],
            )
            client, info, error = await self._async_probe(
                endpoint,
                bearer_token=entry.data[CONF_ACCESS_TOKEN],
            )
            if error is not None:
                errors["base"] = error
            else:
                assert client is not None
                assert info is not None
                await self.async_set_unique_id(info.hub_id)
                try:
                    self._abort_if_unique_id_mismatch(reason="wrong_hub")
                except AbortFlow:
                    await client.async_close()
                    raise
                await client.async_close()
                return self.async_update_reload_and_abort(
                    entry,
                    data_updates={
                        CONF_HOST: endpoint.host,
                        CONF_PORT: endpoint.port,
                        CONF_USE_TLS: endpoint.use_tls,
                    },
                )

        return self.async_show_form(
            step_id="reconfigure_confirm",
            data_schema=_endpoint_schema(dict(entry.data) | (user_input or {})),
            errors=errors,
        )
