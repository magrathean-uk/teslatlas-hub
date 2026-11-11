export type ViewState =
  | 'complete'
  | 'empty'
  | 'stale'
  | 'inferred'
  | 'degraded'
  | 'offline';

const defaultLabels: Record<ViewState, string> = {
  complete: 'Complete',
  empty: 'Empty',
  stale: 'Stale',
  inferred: 'Inferred',
  degraded: 'Degraded',
  offline: 'Offline',
};

interface StatusPillProps {
  state: ViewState;
  label?: string;
}

export function StatusPill({ state, label }: StatusPillProps) {
  return (
    <span className="status-pill" data-status={state}>
      <span aria-hidden="true" className="status-dot" />
      {label ?? defaultLabels[state]}
    </span>
  );
}
