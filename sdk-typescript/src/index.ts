export type {
  AuthorizationContext,
  AuthorizationProvider,
  CredentialStore,
  MaybePromise,
} from "./auth/credential-store.js";
export type { IdempotencyKey } from "./commands/idempotency.js";
export { InvalidIdempotencyKeyError, asIdempotencyKey } from "./commands/idempotency.js";
export type { TeslatlasClient } from "./client/client.js";
export type { CreateClientOptions } from "./client/types.js";
export {
  InvalidReadOptionsError,
  type CommandCreateOptions,
  type ConditionalReadOptions,
  type DataQualityPageOptions,
  type HistoryPageOptions,
  type IfMatchOptions,
  type MetadataPageOptions,
  type PageReadOptions,
  type ReadResult,
  type RequestOptions,
  type ResponseMetadata,
  type WriteResult,
} from "./client/operations.js";
export {
  CommandUncertainError,
  IncompatibleProtocolError,
  MissingCapabilityError,
  ProtocolError,
  ProtocolHttpError,
  ProtocolValidationError,
  ReplayGapError,
  TeslatlasError,
  TransportError,
  type ProtocolErrorCode,
  type ProtocolErrorOptions,
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
  InvalidStreamEventsOptionsError,
  UnsupportedStreamEventTypeError,
  type StreamEventsOptions,
} from "./events/protocol-subscription.js";
export {
  InvalidSseCheckpointError,
  SseBodyError,
  SseContentTypeError,
  type SseCheckpointStore,
  type SseSleep,
} from "./events/sse-subscription.js";
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
export type {
  Charge,
  ChargePage,
  ChargeSamplePage,
  CommandJob,
  CommandRequest,
  CurrentState,
  DataQualityPage,
  Drive,
  DrivePage,
  HubDescriptor,
  MetadataCreate,
  MetadataPage,
  MetadataRecord,
  MetadataReplace,
  MetadataTombstone,
  PositionPage,
  ProtocolEvent,
  ProtocolProblem,
  StatePage,
  UpdatePage,
  VehiclePage,
} from "./protocol/models.js";
