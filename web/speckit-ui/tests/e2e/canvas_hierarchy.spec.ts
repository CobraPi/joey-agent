import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('canvas renders full hierarchy with zero drops/duplicates', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.waitForSelector('[data-testid="canvas-svg"]');

  // 1 spec node
  await expect(page.locator('[data-node-type="Specification"]')).toHaveCount(1);
  // 3 user story nodes
  await expect(page.locator('[data-node-type="UserStory"]')).toHaveCount(3);
  // 1 plan node
  await expect(page.locator('[data-node-type="Plan"]')).toHaveCount(1);
  // 3 task nodes (matches tasks.md length exactly, no drops/dupes)
  await expect(page.locator('[data-node-type="Task"]')).toHaveCount(feature.tasks.length);

  // Status color coding: Completed task should render distinctly from Todo.
  const completedNode = page.locator('[data-testid="node-task:T001"]');
  await expect(completedNode).toHaveAttribute('data-node-status', 'Completed');
  const todoNode = page.locator('[data-testid="node-task:T002"]');
  await expect(todoNode).toHaveAttribute('data-node-status', 'Draft');
});
