import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installMockApi } from '../mocks/mock-backend';

test('clarify flow shows question in chat and highlights the right line', async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });

  await page.goto('/');
  await page.click('[data-view="workspace"]');
  await page.waitForSelector('[data-testid="assistant-panel"]');

  await page.click('[data-testid="run-clarify"]');
  await page.waitForFunction(() => {
    const instances = (
      window as unknown as { __FakeWebSocket?: { instances: Array<{ url: string }> } }
    ).__FakeWebSocket?.instances;
    return !!instances?.some((i) => i.url.includes('/session/'));
  });

  // Simulate the backend pushing a question over the clarify session WS.
  await page.evaluate(() => {
    interface FakeWs {
      url: string;
      __emit: (data: unknown) => void;
    }
    const instances = (
      window as unknown as { __FakeWebSocket: { instances: FakeWs[] } }
    ).__FakeWebSocket.instances;
    const sessionSocket = instances.find((i) => i.url.includes('/session/'));
    sessionSocket?.__emit({
      type: 'question',
      question: 'Which rendering approach should the Visual UI use?',
      target_line: 'NEEDS CLARIFICATION',
    });
  });

  await expect(page.locator('[data-testid="assistant-msg-question"]')).toContainText(
    'Which rendering approach',
  );

  await page.fill('[data-testid="answer-input"]', 'Separate local web frontend');
  await page.click('[data-testid="submit-answer"]');

  await expect(page.locator('[data-testid="assistant-msg-answer"]')).toContainText(
    'Separate local web frontend',
  );
});
