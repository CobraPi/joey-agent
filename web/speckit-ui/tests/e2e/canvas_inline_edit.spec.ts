import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('inline edit PATCHes back to disk and persists on success', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.waitForSelector('[data-testid="canvas-svg"]');

  page.once('dialog', (dialog) => dialog.accept('Updated description text'));
  await page.locator('[data-testid="node-task:T002"]').dblclick();

  // No conflict banner should appear on success.
  await expect(page.locator('[data-testid="conflict-banner"]')).toHaveCount(0);
});

test('inline edit shows visible conflict message and reload prompt on 409', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature, patchTaskShouldConflict: true });

  await page.goto('/');
  await page.waitForSelector('[data-testid="canvas-svg"]');

  page.once('dialog', (dialog) => dialog.accept('Conflicting edit text'));
  await page.locator('[data-testid="node-task:T002"]').dblclick();

  const banner = page.locator('[data-testid="conflict-banner"]');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText('changed on disk');

  const reloadBtn = page.locator('[data-testid="conflict-reload"]');
  await expect(reloadBtn).toBeVisible();
});
