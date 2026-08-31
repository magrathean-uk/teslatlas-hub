import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

const outputDirectory = resolve('output/playwright/screenshots');

test('captures deterministic reference screenshots', async ({ page }) => {
  mkdirSync(outputDirectory, { recursive: true });

  async function capture(
    name: string,
    url: string,
    viewport: { width: number; height: number },
    readySelector: string,
  ) {
    await page.setViewportSize(viewport);
    await page.goto(url);
    await page.locator(readySelector).waitFor();
    await page.addStyleTag({
      content:
        '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}',
    });
    await page.screenshot({
      path: resolve(outputDirectory, name),
      fullPage: true,
    });
  }

  await capture(
    'complete-desktop.png',
    '/?scenario=complete',
    { width: 1280, height: 800 },
    '[data-view-state="complete"]',
  );
  await capture(
    'complete-mobile.png',
    '/?scenario=complete',
    { width: 375, height: 812 },
    '[data-view-state="complete"]',
  );
  await capture(
    'stale-desktop.png',
    '/?scenario=stale',
    { width: 1280, height: 800 },
    '[data-view-state="stale"]',
  );
  await capture(
    'error-mobile.png',
    '/?scenario=error',
    { width: 375, height: 812 },
    '[data-view-state="error"]',
  );
  await capture(
    'pairing-mobile.png',
    '/?paired=false',
    { width: 375, height: 812 },
    '.discovered-hub',
  );

  await page.setViewportSize({ width: 375, height: 812 });
  await page.goto('/?scenario=complete');
  await page.getByRole('button', { name: 'Paired devices' }).click();
  await page.getByRole('button', { name: 'Remove Home automation' }).click();
  await page.evaluate(() => {
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
    window.scrollTo(0, 0);
  });
  await page.addStyleTag({
    content:
      '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}',
  });
  await page.screenshot({
    path: resolve(outputDirectory, 'removal-confirmation-mobile.png'),
    fullPage: true,
  });

  await expect(page.locator('body')).toBeVisible();
});
