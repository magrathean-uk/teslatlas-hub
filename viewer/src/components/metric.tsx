import type { ReactNode } from 'react';

interface MetricProps {
  label: string;
  value: ReactNode;
  detail?: ReactNode;
}

export function Metric({ label, value, detail }: MetricProps) {
  return (
    <div className="metric">
      <dt>{label}</dt>
      <dd>
        {value}
        {detail && <span className="metric-detail">{detail}</span>}
      </dd>
    </div>
  );
}
