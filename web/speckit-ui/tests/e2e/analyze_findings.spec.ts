import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('analyze findings anchor to specific file/section and drive the gauge', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.click('[data-view="workspace"]');
  await page.waitForSelector('[data-testid="assistant-panel"]');

  await page.click('[data-testid="run-analyze"]');

  const findingMsg = page.locator('[data-testid="assistant-msg-finding"]').first();
  await expect(findingMsg).toContainText('tasks.md');
  await expect(findingMsg).toContainText('T003');
  await expect(findingMsg).toContainText('nonexistent requirement');

  // constitution_compliance: "Fail" in the mock response must flip the gauge red.
  const gauge = page.locator('[data-testid="constitution-gauge"]');
  await expect(gauge).toHaveAttribute('data-status', 'Fail');
});
