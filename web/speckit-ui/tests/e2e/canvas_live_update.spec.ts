import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('canvas auto-refreshes and updates node color on external file change', async ({
  page,
}) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.waitForSelector('[data-testid="canvas-svg"]');

  await expect(page.locator('[data-testid="node-task:T002"]')).toHaveAttribute(
    'data-node-status',
    'Draft',
  );

  // Simulate the task being completed on disk out-of-band, then the backend
  // reflecting the change on the next GET (as the real backend would).
  feature.tasks[1].status = 'Completed';

  // Trigger the watch WebSocket's file_changed event, which the canvas
  // subscribes to and reacts to by refetching.
  await page.evaluate(() => {
    interface FakeWs {
      url: string;
      __emit: (data: unknown) => void;
    }
    const instances = (
      window as unknown as { __FakeWebSocket: { instances: FakeWs[] } }
    ).__FakeWebSocket.instances;
    const watchSocket = instances.find((i) => i.url.includes('/watch'));
    watchSocket?.__emit({ type: 'file_changed', file: 'tasks.md', content_hash: 'sha256:tasks-v2' });
  });

  await expect(page.locator('[data-testid="node-task:T002"]')).toHaveAttribute(
    'data-node-status',
    'Completed',
    { timeout: 5000 },
  );
});
