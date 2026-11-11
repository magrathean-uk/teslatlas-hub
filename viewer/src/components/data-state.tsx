interface DataStateProps {
  title: string;
  detail: string;
  kind?: 'empty' | 'absent' | 'notice';
}

export function DataState({
  title,
  detail,
  kind = 'empty',
}: DataStateProps) {
  return (
    <div className="data-state" data-kind={kind} role="status">
      <span className="data-state-mark" aria-hidden="true">
        {kind === 'absent' ? '—' : '○'}
      </span>
      <div>
        <strong>{title}</strong>
        <p>{detail}</p>
      </div>
    </div>
  );
}
