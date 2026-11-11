import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './app/App';
import { createDataSource } from './data/create-data-source';
import type { AppMode, FixtureScenario } from './data/types';
import './styles.css';

const scenarios: FixtureScenario[] = [
  'complete',
  'empty',
  'stale',
  'inferred',
  'degraded',
  'offline',
  'error',
  'loading',
];

const search = new URLSearchParams(window.location.search);
const mode: AppMode = search.get('mode') === 'live' ? 'live' : 'fixture';
const scenarioValue = search.get('scenario');
const scenario = scenarios.includes(scenarioValue as FixtureScenario)
  ? (scenarioValue as FixtureScenario)
  : 'complete';
const initialPaired = mode === 'fixture' && search.get('paired') !== 'false';
const dataSource = createDataSource(mode);

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App
      dataSource={dataSource}
      mode={mode}
      initialScenario={scenario}
      initialPaired={initialPaired}
    />
  </StrictMode>,
);
