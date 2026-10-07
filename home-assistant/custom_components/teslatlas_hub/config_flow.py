"""Config flow for Teslatlas Hub."""

from __future__ import annotations

import re
from collections.abc import Mapping
from datetime import UTC, datetime
from typing import Any, override

import voluptuous as vol
from homeassistant.config_entries import SOURCE_REAUTH, ConfigFlow, ConfigFlowResult
from homeassistant.const import CONF_HOST
from homeassistant.core import callback
from homeassistant.helpers.selector import (
    BooleanSelector,
    NumberSelector,
    NumberSelectorConfig,
    NumberSelectorMode,
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    HubContractError,
    HubIdentityError,
    HubPairingError,
    ProtocolContractUnavailable,
    TeslatlasHubClient,
    create_client,
)
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_DEVICE_NAME,
    CONF_HUB_ID,
    CONF_PAIRING_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_TLS_PIN,
    CONF_TOKEN_EXPIRES_AT_MS,
    CONF_USE_TLS,
    CONFIG_ENTRY_MINOR_VERSION,
    CONFIG_ENTRY_VERSION,
    DEFAULT_PORT,
    DOMAIN,
)
from .credentials import (
    HubCredentialExpiredError,
    epoch_milliseconds,
    validate_current_hub_credential,
)
from .models import HubEndpoint, HubInfo, PairingResult


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
            vol.Optional(
                CONF_TLS_PIN,
                default=defaults.get(CONF_TLS_PIN, ""),
            ): TextSelector(TextSelectorConfig(type=TextSelectorType.PASSWORD)),
        }
    )


PAIRING_SCHEMA = vol.Schema(
    {
        vol.Required(CONF_PAIRING_ID): TextSelector(
            TextSelectorConfig(type=TextSelectorType.TEXT)
        ),
        vol.Required(CONF_PAIRING_SECRET): TextSelector(
            TextSelectorConfig(type=TextSelectorType.PASSWORD)
        ),
        vol.Required(CONF_DEVICE_NAME, default="Home Assistant"): TextSelector(
            TextSelectorConfig(type=TextSelectorType.TEXT)
        ),
    }
)
VALIDATE_SCHEMA = vol.Schema({})
APPROVE_NEW_TLS_PIN = "approve_new_tls_pin"
TLS_APPROVAL_SCHEMA = vol.Schema(
    {vol.Required(APPROVE_NEW_TLS_PIN, default=False): BooleanSelector()}
)


class TeslatlasHubConfigFlow(ConfigFlow, domain=DOMAIN):
    """Configure one public Teslatlas Hub identity."""

    VERSION = CONFIG_ENTRY_VERSION
    MINOR_VERSION = CONFIG_ENTRY_MINOR_VERSION

    def __init__(self) -> None:
        """Initialize transient flow state."""
        self._endpoint: HubEndpoint | None = None
        self._info: HubInfo | None = None
        self._pending_pairing_result: PairingResult | None = None
        self._replacement_endpoint: HubEndpoint | None = None
        self._replacement_hub_id: str | None = None
        self._replacement_approved = False
        self._flow_removed = False
        self._reauth_generation = 0

    @override
    @callback
    def async_remove(self) -> None:
        """Invalidate pending mutations when HA removes this flow."""
        self._flow_removed = True
        self._reauth_generation += 1
        if self.context.get("source") == SOURCE_REAUTH:
            self._pending_pairing_result = None
        super().async_remove()

    def _reauth_is_current(self, generation: int) -> bool:
        return not self._flow_removed and generation == self._reauth_generation

    def _superseded_reauth_result(self) -> ConfigFlowResult:
        """Leave a newer active operation's invitation or candidate intact."""
        if self._flow_removed:
            return self.async_abort(reason="reauth_cancelled")
        step = (
            "reauth_validate"
            if self._pending_pairing_result is not None
            else "reauth_confirm"
        )
        if (
            self.cur_step is not None
            and self.cur_step.get("type") == "form"
            and self.cur_step.get("step_id") == step
        ):
            return self.cur_step
        if self._pending_pairing_result is not None:
            return self.async_show_form(
                step_id="reauth_validate", data_schema=VALIDATE_SCHEMA
            )
        return self.async_show_form(
            step_id="reauth_confirm", data_schema=PAIRING_SCHEMA
        )

    async def _async_probe(
        self,
        endpoint: HubEndpoint,
        bearer_token: str | None = None,
    ) -> tuple[TeslatlasHubClient | None, HubInfo | None, str | None]:
        """Probe one endpoint and translate client failures for the flow."""
        try:
            client = create_client(endpoint, bearer_token=bearer_token, hass=self.hass)
        except HubContractError:
            return None, None, "invalid_contract"
        keep_client = False
        try:
            info = await client.async_probe()
        except HubConnectionError:
            error = "cannot_connect"
        except HubAuthenticationError:
            error = "invalid_auth"
        except ProtocolContractUnavailable:
            error = "protocol_not_ready"
        except HubContractError:
            error = "invalid_contract"
        else:
            keep_client = True
            return client, info, None
        finally:
            if not keep_client:
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

    async def _async_validate_access(
        self,
        endpoint: HubEndpoint,
        *,
        hub_id: str,
        access_token: str,
        expires_at_ms: int | None,
    ) -> str | None:
        """Prove a candidate bearer can read the expected Hub before storing it."""
        try:
            client = create_client(
                endpoint,
                bearer_token=access_token,
                expected_hub_id=hub_id,
                bearer_expires_at_ms=expires_at_ms,
                hass=self.hass,
            )
        except HubContractError:
            return "invalid_contract"
        try:
            snapshot = await client.async_snapshot()
        except HubIdentityError:
            return "wrong_hub"
        except HubConnectionError:
            return "cannot_connect"
        except HubAuthenticationError:
            return "invalid_auth"
        except ProtocolContractUnavailable:
            return "protocol_not_ready"
        except HubContractError:
            return "invalid_contract"
        finally:
            await client.async_close()
        if snapshot.info.hub_id != hub_id:
            return "wrong_hub"
        return None

    @staticmethod
    def _issued_credential_error(result: PairingResult) -> str | None:
        """Check issued metadata and freshness at the point of use."""
        try:
            validate_current_hub_credential(
                result.access_token,
                result.device_id,
                result.expires_at_ms,
                now_ms=epoch_milliseconds(datetime.now(UTC)),
            )
        except HubCredentialExpiredError:
            return "invalid_auth"
        except HubContractError:
            return "invalid_contract"
        return None

    async def _async_validate_issued_access(
        self, endpoint: HubEndpoint, result: PairingResult, hub_id: str
    ) -> str | None:
        """Admit the complete credential before dispatch and entry mutation."""
        if error := self._issued_credential_error(result):
            return error
        if result.info.hub_id != hub_id:
            return "wrong_hub"
        error = await self._async_validate_access(
            endpoint,
            hub_id=hub_id,
            access_token=result.access_token,
            expires_at_ms=result.expires_at_ms,
        )
        return error if error is not None else self._issued_credential_error(result)

    def _clear_replacement(self) -> None:
        self._replacement_endpoint = None
        self._replacement_hub_id = None
        self._replacement_approved = False
        self._pending_pairing_result = None

    @staticmethod
    def _has_saved_origin_binding(
        entry_data: Mapping[str, Any], endpoint: HubEndpoint
    ) -> bool:
        """Require the candidate TLS channel to present the saved leaf pin."""
        saved_pin = entry_data.get(CONF_TLS_PIN)
        return (
            entry_data[CONF_USE_TLS] is True
            and isinstance(saved_pin, str)
            and bool(saved_pin)
            and endpoint.use_tls
            and endpoint.tls_pin == saved_pin
        )

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
                tls_pin=user_input.get(CONF_TLS_PIN) or None,
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

    async def async_step_pair(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Claim a transient pairing secret for a scoped device bearer."""
        errors: dict[str, str] = {}
        if user_input is not None:
            assert self._endpoint is not None
            assert self._info is not None
            client = create_client(
                self._endpoint,
                expected_hub_id=self._info.hub_id,
                hass=self.hass,
            )
            try:
                result = await client.async_pair(
                    user_input[CONF_PAIRING_ID],
                    user_input[CONF_PAIRING_SECRET],
                    user_input[CONF_DEVICE_NAME],
                )
            except HubIdentityError:
                return self.async_abort(reason="wrong_hub")
            except HubPairingError:
                errors["base"] = "invalid_pairing_secret"
            except HubConnectionError:
                errors["base"] = "cannot_connect"
            except HubAuthenticationError:
                errors["base"] = "invalid_auth"
            except ProtocolContractUnavailable:
                errors["base"] = "protocol_not_ready"
            except HubContractError:
                errors["base"] = "invalid_contract"
            else:
                if result.info.hub_id != self._info.hub_id:
                    return self.async_abort(reason="wrong_hub")
                self._pending_pairing_result = result
                return await self.async_step_pair_validate({})
            finally:
                await client.async_close()

        return self.async_show_form(
            step_id="pair",
            data_schema=PAIRING_SCHEMA,
            errors=errors,
        )

    async def async_step_pair_validate(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Retry validation of one already-issued bearer without another claim."""
        assert self._endpoint is not None
        assert self._pending_pairing_result is not None
        errors: dict[str, str] = {}
        if user_input is not None:
            result = self._pending_pairing_result
            error = await self._async_validate_issued_access(
                self._endpoint, result, result.info.hub_id
            )
            if error == "wrong_hub":
                self._pending_pairing_result = None
                return self.async_abort(reason="wrong_hub")
            if error == "invalid_auth":
                self._pending_pairing_result = None
                return self.async_show_form(
                    step_id="pair",
                    data_schema=PAIRING_SCHEMA,
                    errors={"base": "invalid_auth"},
                )
            if error is not None:
                errors["base"] = error
            else:
                self._pending_pairing_result = None
                return self.async_create_entry(
                    title=result.info.name,
                    data={
                        CONF_HOST: self._endpoint.host,
                        CONF_PORT: self._endpoint.port,
                        CONF_USE_TLS: self._endpoint.use_tls,
                        CONF_TLS_PIN: self._endpoint.tls_pin,
                        CONF_HUB_ID: result.info.hub_id,
                        CONF_ACCESS_TOKEN: result.access_token,
                        CONF_DEVICE_ID: result.device_id,
                        CONF_TOKEN_EXPIRES_AT_MS: result.expires_at_ms,
                    },
                )

        return self.async_show_form(
            step_id="pair_validate",
            data_schema=VALIDATE_SCHEMA,
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
            self._reauth_generation += 1
            generation = self._reauth_generation
            entry = self._get_reauth_entry()
            endpoint = HubEndpoint(
                host=entry.data[CONF_HOST],
                port=entry.data[CONF_PORT],
                use_tls=entry.data[CONF_USE_TLS],
                tls_pin=entry.data.get(CONF_TLS_PIN),
            )
            client = create_client(
                endpoint,
                expected_hub_id=entry.data[CONF_HUB_ID],
                hass=self.hass,
            )
            result: PairingResult | None = None
            try:
                result = await client.async_pair(
                    user_input[CONF_PAIRING_ID],
                    user_input[CONF_PAIRING_SECRET],
                    user_input[CONF_DEVICE_NAME],
                )
            except HubIdentityError:
                errors["base"] = "wrong_hub"
            except HubPairingError:
                errors["base"] = "invalid_pairing_secret"
            except HubConnectionError:
                errors["base"] = "cannot_connect"
            except HubAuthenticationError:
                errors["base"] = "invalid_auth"
            except ProtocolContractUnavailable:
                errors["base"] = "protocol_not_ready"
            except HubContractError:
                errors["base"] = "invalid_contract"
            finally:
                await client.async_close()

            if not self._reauth_is_current(generation):
                return self._superseded_reauth_result()
            if errors.get("base") == "wrong_hub":
                return self.async_abort(reason="wrong_hub")
            if result is not None:
                await self.async_set_unique_id(result.info.hub_id)
                if not self._reauth_is_current(generation):
                    return self._superseded_reauth_result()
                self._abort_if_unique_id_mismatch(reason="wrong_hub")
                self._pending_pairing_result = result
                return await self.async_step_reauth_validate({})

        return self.async_show_form(
            step_id="reauth_confirm",
            data_schema=PAIRING_SCHEMA,
            errors=errors,
        )

    async def async_step_reauth_validate(
        self,
        user_input: dict[str, Any] | None = None,
    ) -> ConfigFlowResult:
        """Retry validation of a replacement bearer without another claim."""
        entry = self._get_reauth_entry()
        result = self._pending_pairing_result
        assert result is not None
        endpoint = HubEndpoint(
            host=entry.data[CONF_HOST],
            port=entry.data[CONF_PORT],
            use_tls=entry.data[CONF_USE_TLS],
            tls_pin=entry.data.get(CONF_TLS_PIN),
        )
        errors: dict[str, str] = {}
        if user_input is not None:
            generation = self._reauth_generation
            error = await self._async_validate_issued_access(
                endpoint, result, entry.data[CONF_HUB_ID]
            )
            if (
                not self._reauth_is_current(generation)
                or self._pending_pairing_result is not result
            ):
                return self._superseded_reauth_result()
            if error == "wrong_hub":
                self._pending_pairing_result = None
                return self.async_abort(reason="wrong_hub")
            if error == "invalid_auth":
                self._pending_pairing_result = None
                return self.async_show_form(
                    step_id="reauth_confirm",
                    data_schema=PAIRING_SCHEMA,
                    errors={"base": "invalid_auth"},
                )
            if error is not None:
                errors["base"] = error
            else:
                self._pending_pairing_result = None
                return self.async_update_reload_and_abort(
                    entry,
                    data_updates={
                        CONF_ACCESS_TOKEN: result.access_token,
                        CONF_DEVICE_ID: result.device_id,
                        CONF_TOKEN_EXPIRES_AT_MS: result.expires_at_ms,
                    },
                )

        return self.async_show_form(
            step_id="reauth_validate",
            data_schema=VALIDATE_SCHEMA,
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
            self._clear_replacement()
            endpoint = HubEndpoint(
                host=user_input[CONF_HOST],
                port=int(user_input[CONF_PORT]),
                use_tls=user_input[CONF_USE_TLS],
                tls_pin=user_input.get(CONF_TLS_PIN) or None,
            )
            client, info, error = await self._async_probe(endpoint)
            if error is not None:
                errors["base"] = error
            else:
                assert client is not None
                assert info is not None
                try:
                    await self.async_set_unique_id(info.hub_id)
                    self._abort_if_unique_id_mismatch(reason="wrong_hub")
                finally:
                    await client.async_close()
                if not self._has_saved_origin_binding(entry.data, endpoint):
                    if (
                        not endpoint.use_tls
                        or endpoint.tls_pin is None
                        or re.fullmatch(r"[0-9a-f]{64}", endpoint.tls_pin) is None
                    ):
                        errors["base"] = "invalid_contract"
                    else:
                        self._replacement_endpoint = endpoint
                        self._replacement_hub_id = entry.data[CONF_HUB_ID]
                        return await self.async_step_reconfigure_tls()
                else:
                    error = await self._async_validate_access(
                        endpoint,
                        hub_id=entry.data[CONF_HUB_ID],
                        access_token=entry.data[CONF_ACCESS_TOKEN],
                        expires_at_ms=entry.data.get(CONF_TOKEN_EXPIRES_AT_MS),
                    )
                    if error == "wrong_hub":
                        return self.async_abort(reason="wrong_hub")
                    if error is not None:
                        errors["base"] = error
                    else:
                        return self.async_update_reload_and_abort(
                            entry,
                            data_updates={
                                CONF_HOST: endpoint.host,
                                CONF_PORT: endpoint.port,
                                CONF_USE_TLS: endpoint.use_tls,
                                CONF_TLS_PIN: endpoint.tls_pin,
                            },
                        )

        return self.async_show_form(
            step_id="reconfigure_confirm",
            data_schema=_endpoint_schema(dict(entry.data) | (user_input or {})),
            errors=errors,
        )

    async def async_step_reconfigure_tls(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Require explicit approval of the frozen replacement TLS channel."""
        endpoint = self._replacement_endpoint
        if endpoint is None or self._replacement_hub_id is None:
            self._clear_replacement()
            return self.async_abort(reason="replacement_not_approved")
        if user_input is not None:
            if user_input.get(APPROVE_NEW_TLS_PIN) is not True:
                self._clear_replacement()
                return self.async_abort(reason="replacement_not_approved")
            self._replacement_approved = True
            return await self.async_step_reconfigure_pair()
        return self.async_show_form(
            step_id="reconfigure_tls",
            data_schema=TLS_APPROVAL_SCHEMA,
            description_placeholders={
                "host": endpoint.host,
                "port": str(endpoint.port),
                "tls_pin": endpoint.tls_pin or "",
            },
        )

    async def async_step_reconfigure_pair(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Claim a fresh invitation without the saved bearer on the new channel."""
        endpoint = self._replacement_endpoint
        hub_id = self._replacement_hub_id
        if not self._replacement_approved or endpoint is None or hub_id is None:
            self._clear_replacement()
            return self.async_abort(reason="replacement_not_approved")
        errors: dict[str, str] = {}
        if user_input is not None:
            client = create_client(endpoint, expected_hub_id=hub_id, hass=self.hass)
            result: PairingResult | None = None
            try:
                result = await client.async_pair(
                    user_input[CONF_PAIRING_ID],
                    user_input[CONF_PAIRING_SECRET],
                    user_input[CONF_DEVICE_NAME],
                )
            except HubIdentityError:
                errors["base"] = "wrong_hub"
            except HubPairingError:
                errors["base"] = "invalid_pairing_secret"
            except HubAuthenticationError:
                errors["base"] = "invalid_auth"
            except HubConnectionError:
                errors["base"] = "cannot_connect"
            except ProtocolContractUnavailable:
                errors["base"] = "protocol_not_ready"
            except HubContractError:
                errors["base"] = "invalid_contract"
            finally:
                await client.async_close()
            if (
                not self._replacement_approved
                or self._replacement_endpoint is not endpoint
                or self._replacement_hub_id != hub_id
            ):
                return self.async_abort(reason="replacement_not_approved")
            if errors.get("base") == "wrong_hub" or (
                result is not None and result.info.hub_id != hub_id
            ):
                self._clear_replacement()
                return self.async_abort(reason="wrong_hub")
            if result is not None:
                self._pending_pairing_result = result
                return await self.async_step_reconfigure_validate({})
        return self.async_show_form(
            step_id="reconfigure_pair", data_schema=PAIRING_SCHEMA, errors=errors
        )

    async def async_step_reconfigure_validate(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Retry one issued bearer before replacing all endpoint credentials."""
        endpoint = self._replacement_endpoint
        hub_id = self._replacement_hub_id
        result = self._pending_pairing_result
        if (
            not self._replacement_approved
            or endpoint is None
            or hub_id is None
            or result is None
        ):
            self._clear_replacement()
            return self.async_abort(reason="replacement_not_approved")
        errors: dict[str, str] = {}
        if user_input is not None:
            entry = self._get_reconfigure_entry()
            if entry.data[CONF_HUB_ID] != hub_id:
                self._clear_replacement()
                return self.async_abort(reason="wrong_hub")
            error = await self._async_validate_issued_access(endpoint, result, hub_id)
            if entry.data[CONF_HUB_ID] != hub_id:
                self._clear_replacement()
                return self.async_abort(reason="wrong_hub")
            if (
                not self._replacement_approved
                or self._replacement_endpoint is not endpoint
                or self._replacement_hub_id != hub_id
                or self._pending_pairing_result is not result
            ):
                return self.async_abort(reason="replacement_not_approved")
            if error == "wrong_hub":
                self._clear_replacement()
                return self.async_abort(reason="wrong_hub")
            if error == "invalid_auth":
                self._pending_pairing_result = None
                return self.async_show_form(
                    step_id="reconfigure_pair",
                    data_schema=PAIRING_SCHEMA,
                    errors={"base": "invalid_auth"},
                )
            if error is not None:
                errors["base"] = error
            else:
                updates = {
                    CONF_HOST: endpoint.host,
                    CONF_PORT: endpoint.port,
                    CONF_USE_TLS: endpoint.use_tls,
                    CONF_TLS_PIN: endpoint.tls_pin,
                    CONF_ACCESS_TOKEN: result.access_token,
                    CONF_DEVICE_ID: result.device_id,
                    CONF_TOKEN_EXPIRES_AT_MS: result.expires_at_ms,
                }
                self._clear_replacement()
                return self.async_update_reload_and_abort(entry, data_updates=updates)
        return self.async_show_form(
            step_id="reconfigure_validate", data_schema=VALIDATE_SCHEMA, errors=errors
        )
