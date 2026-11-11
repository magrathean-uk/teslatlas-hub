import { expect, test } from '@playwright/test';

test('fixture mode traverses every public reference view without API requests', async ({
  page,
}) => {
  const apiRequests: string[] = [];
  page.on('request', (request) => {
    const path = new URL(request.url()).pathname;
    if (path.includes('/.well-known/') || path.startsWith('/v1/')) {
      apiRequests.push(request.url());
    }
  });

  await page.goto('/?scenario=complete');

  const navigation = page.getByRole('navigation', { name: 'Viewer sections' });
  await expect(navigation.getByRole('button')).toHaveCount(7);
  await expect(page.getByRole('region', { name: 'Hub health' })).toHaveAttribute(
    'data-view-state',
    'complete',
  );

  const expectedHeadings = [
    'Vehicles',
    'Current state',
    'Recent sessions',
    'Data quality',
    'Collector freshness',
    'Paired devices',
  ];
  for (const heading of expectedHeadings) {
    await navigation.getByRole('button', { name: heading }).click();
    await expect(page.getByRole('region', { name: heading })).toBeVisible();
  }

  expect(apiRequests).toEqual([]);
});

test('home route exposes the editable live connection flow without pre-pair reads', async ({
  page,
}) => {
  const apiRequests: string[] = [];
  page.on('request', (request) => {
    const path = new URL(request.url()).pathname;
    if (path.includes('/.well-known/') || path.startsWith('/v1/')) {
      apiRequests.push(request.url());
    }
  });

  await page.goto('/');
  await page.getByRole('link', { name: 'Connect to my Hub' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
  await expect(page.getByLabel('Hub endpoint')).toBeVisible();

  await page.getByLabel('Hub endpoint').fill('http://hub.example.test');
  await page.getByLabel('Expected Hub UUID').fill('hub-id');
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
  await expect(page.getByRole('alert')).toContainText('HTTPS');
  await expect(page.getByLabel('Hub endpoint')).toBeFocused();
  expect(apiRequests).toEqual([]);
});

test('fixture history groups expose terminal state per vehicle', async ({ page }) => {
  await page.goto('/?scenario=complete');
  await page.getByRole('button', { name: 'Recent sessions' }).click();
  const groups = page.locator('.session-group');
  await expect(groups).toHaveCount(2);
  await expect(groups.nth(0)).toContainText('End of available history');
  await expect(groups.nth(1)).toContainText('End of available history');
  await expect(page.getByRole('button', { name: /Load more drives/ })).toHaveCount(0);
});

test('fixture state matrix keeps empty, stale, inferred, degraded, offline, error, and loading distinct', async ({
  page,
}) => {
  await page.goto('/?scenario=empty');
  await page.getByRole('button', { name: 'Vehicles' }).click();
  await expect(page.getByRole('region', { name: 'Vehicles' })).toHaveAttribute(
    'data-view-state',
    'empty',
  );
  await expect(page.getByText('No vehicles available')).toBeVisible();

  await page.goto('/?scenario=stale');
  await expect(page.getByRole('region', { name: 'Hub health' })).toHaveAttribute(
    'data-view-state',
    'stale',
  );

  await page.goto('/?scenario=inferred');
  await page.getByRole('button', { name: 'Current state' }).click();
  await expect(page.getByRole('region', { name: 'Current state' })).toHaveAttribute(
    'data-view-state',
    'inferred',
  );
  await expect(page.getByText('Estimated range is inferred')).toBeVisible();

  await page.goto('/?scenario=degraded');
  await page.getByRole('button', { name: 'Data quality' }).click();
  await expect(page.getByRole('region', { name: 'Data quality' })).toHaveAttribute(
    'data-view-state',
    'degraded',
  );

  await page.goto('/?scenario=offline');
  await expect(page.getByRole('region', { name: 'Hub health' })).toHaveAttribute(
    'data-view-state',
    'offline',
  );

  await page.goto('/?scenario=error');
  await expect(page.locator('main')).toHaveAttribute('data-view-state', 'error');
  await expect(page.getByRole('alert')).toContainText(
    'The fixture Hub could not be read.',
  );
  await page.getByRole('button', { name: 'Load complete fixture' }).click();
  await expect(page.getByRole('region', { name: 'Hub health' })).toHaveAttribute(
    'data-view-state',
    'complete',
  );

  await page.goto('/?scenario=loading');
  await expect(page.locator('main')).toHaveAttribute('data-view-state', 'loading');
  await expect(
    page.getByRole('status', { name: 'Loading Hub data' }),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Load complete fixture' }).click();
  await expect(page.getByRole('region', { name: 'Hub health' })).toHaveAttribute(
    'data-view-state',
    'complete',
  );
});

test('fixture discovery pairs in memory and device management updates the view', async ({
  page,
}) => {
  const externalRequests: string[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (url.hostname !== '127.0.0.1') externalRequests.push(url.href);
  });

  await page.goto('/?paired=false');
  await expect(page.getByRole('heading', { name: 'Hawthorn Hub' })).toBeVisible();
  await page.getByLabel('Invitation code').fill('482731');
  await page.getByRole('button', { name: 'Pair fixture Hub' }).click();

  await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
  await expect(page.getByText('SHA256:4A:89:71:03:DE:MO')).toBeVisible();

  await page.getByRole('button', { name: 'Paired devices' }).click();
  await page.getByRole('button', { name: 'Remove Home automation' }).click();
  await page
    .getByRole('button', { name: 'Confirm remove Home automation' })
    .click();
  await expect(page.getByText('Home automation removed.')).toBeVisible();
  await expect(
    page.getByRole('heading', { name: 'Home automation' }),
  ).toHaveCount(0);

  await page.getByRole('button', { name: 'Clear local session' }).click();
  await expect(
    page.getByRole('heading', { name: 'Pair with a Hub' }),
  ).toBeVisible();

  expect(externalRequests).toEqual([]);
});
