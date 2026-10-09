export type {
  AuthorizationContext,
  AuthorizationProvider,
  CredentialStore,
  MaybePromise,
} from "./auth/credential-store.js";
export {
  ProtocolValidationError,
  TeslatlasError,
  TransportError,
  type SafeRequestId,
} from "./core/errors.js";
export {
  InvalidEntityTagError,
  InvalidOpaqueCursorError,
  asEntityTag,
  asOpaqueCursor,
  type EntityTag,
  type OpaqueCursor,
} from "./core/opaque-values.js";
export {
  InvalidAuthorizationValueError,
  InvalidBaseUrlError,
  MissingFetchError,
} from "./http/fetch-transport.js";
export {
  InvalidStrongEntityTagError,
  asStrongEntityTag,
  type StrongEntityTag,
} from "./http/strong-etag.js";
export {
  HubClientDisposedError,
  HubHttpError,
  HubIdentityMismatchError,
  HubTlsPinMismatchError,
  HubTlsPinUnavailableError,
  UnsupportedHubMethodError,
  VehicleIdentityMismatchError,
} from "./hub/client.js";
export type {
  CreateHubClientOptions,
  HubCapability,
  HubClaim,
  HubClaimTransport,
  HubClaimTransportRequest,
  HubClient,
  HubCredential,
  HubCredentialStore,
  HubCurrent,
  HubDiscovery,
  HubDrive,
  HubDrives,
  HubDrivesOptions,
  HubDrivesResult,
  HubErrorCode,
  HubHealth,
  HubInvitation,
  HubReadiness,
  HubRequestOptions,
  HubResponse,
  HubResponseMetadata,
  HubVehicles,
} from "./hub/models.js";
