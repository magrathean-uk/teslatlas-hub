export function synchronizeProtocol(options: {
  root?: string;
  checkout: string;
  candidateMode?: boolean;
  authorityCommit: string;
  committedInputs?: Map<string, Uint8Array> | undefined;
  generate?: (stageRoot: string) => void | Promise<void>;
  failpoint?: (stage: string, stageRoot: string) => void | Promise<void>;
}): Promise<Record<string, unknown>>;
export function recoverProtocolSync(
  root: string,
  activeOwnerPid?: number,
  failpoint?: (stage: string, stageRoot: string) => void | Promise<void>,
): Promise<void>;
