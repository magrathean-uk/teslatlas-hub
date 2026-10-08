import type { components } from "../generated/protocol.js";

type WithoutSchemaDefs<T> = Omit<T, "$defs">;

export type HubDescriptor = WithoutSchemaDefs<components["schemas"]["Discovery"]>;
export type VehiclePage = components["schemas"]["Resources"]["$defs"]["vehicle_page"];
export type CurrentState = components["schemas"]["Resources"]["$defs"]["current_state"];
export type DrivePage = components["schemas"]["Resources"]["$defs"]["drive_page"];
export type Drive = components["schemas"]["Resources"]["$defs"]["drive"];
export type PositionPage = components["schemas"]["Resources"]["$defs"]["position_page"];
export type ChargePage = components["schemas"]["Resources"]["$defs"]["charge_page"];
export type Charge = components["schemas"]["Resources"]["$defs"]["charge"];
export type ChargeSamplePage = components["schemas"]["Resources"]["$defs"]["charge_sample_page"];
export type StatePage = components["schemas"]["Resources"]["$defs"]["state_page"];
export type UpdatePage = components["schemas"]["Resources"]["$defs"]["update_page"];
export type DataQualityPage = components["schemas"]["Resources"]["$defs"]["data_quality_page"];
export type MetadataPage = components["schemas"]["Resources"]["$defs"]["metadata_page"];
export type MetadataCreate = components["schemas"]["Metadata"]["$defs"]["metadata_create"];
export type MetadataReplace = components["schemas"]["Metadata"]["$defs"]["metadata_replace"];
export type MetadataRecord = components["schemas"]["Metadata"]["$defs"]["metadata_record"];
export type MetadataTombstone = components["schemas"]["Metadata"]["$defs"]["metadata_tombstone"];
export type CommandRequest = components["schemas"]["Command"]["$defs"]["command_request"];
export type CommandJob = components["schemas"]["Command"]["$defs"]["command_job"];
export type ProtocolEvent = components["schemas"]["Event"];
export type ProtocolProblem = WithoutSchemaDefs<components["schemas"]["Problem"]>;
