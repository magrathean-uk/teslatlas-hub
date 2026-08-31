import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

for (const target of [
  { name: 'complete viewer', url: '/?scenario=complete' },
  { name: 'pairing flow', url: '/?paired=false' },
  {
    name: 'degraded quality',
    url: '/?scenario=degraded',
    view: 'Data quality',
  },
]) {
  test(`${target.name} has no automatically detectable accessibility violations`, async ({
    page,
  }) => {
    await page.goto(target.url);
    await page.locator('main').waitFor();
    if (target.view) {
      await page.getByRole('button', { name: target.view }).click();
      await page.getByRole('region', { name: target.view }).waitFor();
    }
    const results = await new AxeBuilder({ page }).analyze();

    expect(results.violations).toEqual([]);
  });
}

test('keyboard entry exposes skip navigation and all section controls', async ({
  page,
}) => {
  await page.goto('/?scenario=complete');
  await page.getByRole('region', { name: 'Hub health' }).waitFor();

  await page.keyboard.press('Tab');
  await expect(page.getByRole('link', { name: 'Skip to content' })).toBeFocused();

  const healthButton = page.getByRole('button', { name: 'Hub health' });
  await healthButton.focus();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Vehicles' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('region', { name: 'Vehicles' })).toBeVisible();
});

test('device-removal confirmation is accessible and restores focus on cancel', async ({
  page,
}) => {
  await page.goto('/?scenario=complete');
  await page.getByRole('button', { name: 'Paired devices' }).click();

  const removeButton = page.getByRole('button', {
    name: 'Remove Home automation',
  });
  await removeButton.focus();
  await page.keyboard.press('Enter');

  await expect(
    page.getByRole('button', { name: 'Confirm remove Home automation' }),
  ).toBeFocused();
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);

  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(removeButton).toBeFocused();
});

test('400 percent equivalent reflow does not overflow the document', async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 800 });
  await page.goto('/?scenario=complete');
  await page.getByRole('region', { name: 'Hub health' }).waitFor();

  const sizes = await page.evaluate(() => ({
    clientWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }));

  expect(sizes.scrollWidth).toBeLessThanOrEqual(sizes.clientWidth);
});

test('reduced-motion preference is active in browser verification', async ({ page }) => {
  await page.goto('/?scenario=complete');

  await expect
    .poll(() =>
      page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches),
    )
    .toBe(true);
});
