export interface DrivePageRequest {
  readonly limit?: number;
  readonly cursor?: string;
}

export interface DrivePageLike {
  readonly kind: "page";
  readonly value: {
    readonly items: readonly unknown[];
    readonly nextCursor: string | null;
  };
}

export interface DrivePaginationClient {
  drives(vehicleId: string, options: DrivePageRequest): Promise<DrivePageLike>;
}

export interface DrivePaginationOptions {
  readonly limit?: number;
  readonly maxPages?: number;
}

export function collectDrivePages(
  client: DrivePaginationClient,
  vehicleId: string,
  options?: DrivePaginationOptions,
): Promise<DrivePageLike[]>;
