import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('dependency view toggles and links tasks sharing a target file', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.click('[data-view="board"]');
  await page.waitForSelector('[data-testid="board"]');

  const depView = page.locator('[data-testid="dependency-view"]');
  await expect(depView).toBeHidden();

  await page.click('[data-testid="toggle-dependency-view"]');
  await expect(depView).toBeVisible();

  // T002 and T003 both target src/canvas/canvas.ts.
  await expect(page.locator('[data-testid="dependency-link-T002-T003"]')).toContainText(
    'src/canvas/canvas.ts',
  );

  await page.click('[data-testid="toggle-dependency-view"]');
  await expect(depView).toBeHidden();
});
