import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('clicking Execute on one card runs only that task, never cascades', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.click('[data-view="board"]');
  await page.waitForSelector('[data-testid="board"]');

  // T002 shows a Parallel badge and its target files.
  await expect(page.locator('[data-testid="badge-parallel-T002"]')).toBeVisible();
  await expect(page.locator('[data-testid="target-files-T002"]')).toContainText(
    'src/canvas/canvas.ts',
  );

  await page.click('[data-testid="execute-task-T002"]');

  // Only T002's button should be in the running state; T003 (which shares a
  // target file and is also parallel-eligible) must remain untouched.
  await expect(page.locator('[data-testid="execute-task-T002"]')).toHaveText('Running…');
  await expect(page.locator('[data-testid="execute-task-T003"]')).toHaveText('Execute Task');
  await expect(page.locator('[data-testid="execute-task-T003"]')).toBeEnabled();

  await page.waitForFunction(() => {
    const instances = (
      window as unknown as { __FakeWebSocket?: { instances: Array<{ url: string }> } }
    ).__FakeWebSocket?.instances;
    return !!instances?.some((i) => i.url.includes('/api/runs/'));
  });

  // Simulate live output + terminal success over the run WebSocket.
  await page.evaluate(() => {
    interface FakeWs {
      url: string;
      __emit: (data: unknown) => void;
    }
    const instances = (
      window as unknown as { __FakeWebSocket: { instances: FakeWs[] } }
    ).__FakeWebSocket.instances;
    const runSocket = instances.find((i) => i.url.includes('/api/runs/'));
    runSocket?.__emit({ type: 'output', text: 'Running task T002...' });
    runSocket?.__emit({ type: 'status', status: 'succeeded' });
  });

  await expect(page.locator('[data-testid="task-output-T002"]')).toContainText(
    'Running task T002',
  );
  await expect(page.locator('[data-testid="column-Completed"]')).toContainText('T002');
  await expect(page.locator('[data-testid="execute-task-T003"]')).toHaveText('Execute Task');
});
