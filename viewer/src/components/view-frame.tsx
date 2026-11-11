import type { ReactNode } from 'react';

import { StatusPill, type ViewState } from './status-pill';

interface ViewFrameProps {
  id: string;
  eyebrow: string;
  title: string;
  description: string;
  state: ViewState;
  stateLabel?: string;
  children: ReactNode;
}

export function ViewFrame({
  id,
  eyebrow,
  title,
  description,
  state,
  stateLabel,
  children,
}: ViewFrameProps) {
  const titleId = `${id}-title`;

  return (
    <section
      aria-labelledby={titleId}
      className="view-frame"
      data-view-state={state}
    >
      <header className="view-heading">
        <div>
          <p className="eyebrow">{eyebrow}</p>
          <h1 id={titleId}>{title}</h1>
          <p className="view-description">{description}</p>
        </div>
        <StatusPill state={state} label={stateLabel} />
      </header>
      {children}
    </section>
  );
}
