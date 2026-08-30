import type { ReactNode } from 'react';

import type { QualityLevel } from '../data/types';
import { StatusPill, type ViewState } from '../components/status-pill';

export function formatDateTime(value: string | null): string {
  if (!value) return 'Not reported';
  return new Intl.DateTimeFormat('en-GB', {
    dateStyle: 'medium',
    timeStyle: 'short',
    timeZone: 'UTC',
  }).format(new Date(value));
}

export function formatDuration(seconds: number | null): string {
  if (seconds === null) return 'Not reported';
  if (seconds < 60) return `${seconds} seconds`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} minutes`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes === 0
    ? `${hours} hours`
    : `${hours} hours ${remainingMinutes} minutes`;
}

export function valueOrAbsent(
  value: string | number | null,
  suffix = '',
): ReactNode {
  if (value === null) {
    return <span className="absent-value">Not reported</span>;
  }
  return `${value}${suffix}`;
}

export function qualityState(level: QualityLevel): ViewState {
  if (level === 'degraded') return 'degraded';
  if (level === 'unknown') return 'empty';
  return 'complete';
}

export function QualityPill({ level }: { level: QualityLevel }) {
  const label = level.charAt(0).toUpperCase() + level.slice(1);
  return <StatusPill state={qualityState(level)} label={label} />;
}
