import { expect, test } from '@playwright/test';
import { defaultFeature, installFakeWebSocket, installIdeMocks, installMockApi } from '../mocks/mock-backend';

/**
 * Feature 010: Spec-Kit Development IDE — end-to-end journey tests.
 *
 * Covers quickstart scenarios 1–7 against the mocked backend, exercising the
 * new IDE views (explorer, editor, workflow, run-panel, search, status
 * badges) that WorkspaceApp renders inside #view-ide. These tests are
 * assertive — no skip-guards — so a regression that stops the IDE from
 * rendering surfaces as a failure.
 */
const FEATURE_ID = '001-speckit-visual-ui';

test.beforeEach(async ({ page }) => {
  const feature = defaultFeature();
  await installFakeWebSocket(page);
  await installMockApi(page, { feature });
  await installIdeMocks(page, FEATURE_ID);

  await page.goto('/');
  // Switch to the IDE view and wait for WorkspaceApp to mount.
  await page.click('[data-view="ide"]');
  await expect(page.locator('#view-ide')).toHaveClass(/active/);
  // Explorer + workflow fetch on init.
  await expect(page.locator('.explorer-tree')).toBeVisible({ timeout: 5000 });
});

test.describe('IDE Journeys (Feature 010)', () => {
  test('scenario 1: explorer lists artifacts by phase and opens one in the editor', async ({ page }) => {
    const items = page.locator('.artifact-item');
    await expect(items.first()).toBeVisible({ timeout: 5000 });
    const count = await items.count();
    expect(count).toBeGreaterThan(0);

    // Opening the spec artifact should render the editor source textarea.
    await page.click('.artifact-item[data-path="spec.md"]');
    await expect(page.locator('.editor-source')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('.editor-path')).toContainText('spec.md');
  });

  test('scenario 2: workflow steps render with states and run controls', async ({ page }) => {
    await expect(page.locator('.workflow-steps')).toBeVisible({ timeout: 5000 });
    const steps = page.locator('.workflow-step');
    const count = await steps.count();
    expect(count).toBeGreaterThan(0);

    // The ready 'specify' step must offer a Run button.
    const runBtn = page.locator('.workflow-step[data-step-id="specify"] .run-btn');
    await expect(runBtn).toBeVisible();

    // The blocked 'plan' step must show a blocking reason, no run button.
    await expect(page.locator('.workflow-step[data-step-id="plan"] .step-blocked')).toContainText('specify must complete first');
    await expect(page.locator('.workflow-step[data-step-id="plan"] .run-btn')).toHaveCount(0);
  });

  test('scenario 3: running a step streams a question that can be answered', async ({ page }) => {
    await page.click('.workflow-step[data-step-id="specify"] .run-btn');

    // Run panel should activate for the new attempt.
    await expect(page.locator('.run-panel .run-header')).toBeVisible({ timeout: 5000 });

    // Wait for the attempt stream WS to connect, then push a question event.
    await page.waitForFunction(() => {
      const instances = (
        window as unknown as { __FakeWebSocket?: { instances: Array<{ url: string }> } }
      ).__FakeWebSocket?.instances;
      return !!instances?.some((i) => i.url.includes('/attempts/'));
    });

    await page.evaluate(() => {
      interface FakeWs { url: string; __emit: (data: unknown) => void; }
      const instances = (
        window as unknown as { __FakeWebSocket: { instances: FakeWs[] } }
      ).__FakeWebSocket.instances;
      const socket = instances.find((i) => i.url.includes('/attempts/'));
      socket?.__emit({
        type: 'question',
        attempt_id: 'att-new',
        interaction_id: 'ix-1',
        prompt: 'Which model should the workflow use?',
      });
    });

    // The run panel must surface the question prompt with an answer control.
    await expect(page.locator('.interaction-prompt')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('.interaction-prompt')).toContainText('Which model');
    await page.fill('.interaction-answer', 'gpt-4o');
    await page.click('.interaction-submit');
  });

  test('scenario 4: options catalog and health advertise connectivity', async ({ page }) => {
    const health = await page.evaluate(async () => {
      const res = await fetch('/api/health');
      return res.json();
    });
    expect(health.backend_reachable).toBe(true);
    expect(health.repo_writable).toBe(true);

    const opts = await page.evaluate(async () => {
      const res = await fetch('/api/options');
      return res.json();
    });
    expect(opts.revision).toContain('sha256:');
    expect(Array.isArray(opts.models)).toBe(true);
    expect(opts.models.length).toBeGreaterThan(0);
  });

  test('scenario 5: keyboard shortcut opens cross-artifact search', async ({ page }) => {
    await page.keyboard.press('Control+K');
    await expect(page.locator('.search-overlay')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('.search-input')).toBeFocused();

    // The search index should include at least the artifacts and workflow steps.
    const results = page.locator('.search-result');
    await expect(results.first()).toBeVisible({ timeout: 5000 });
    expect(await results.count()).toBeGreaterThan(0);
  });

  test('scenario 6: status badges carry descriptive aria-labels', async ({ page }) => {
    const badges = page.locator('.status-badge');
    await expect(badges.first()).toBeVisible({ timeout: 5000 });
    const count = await badges.count();
    expect(count).toBeGreaterThan(0);
    for (let i = 0; i < count; i++) {
      const label = await badges.nth(i).getAttribute('aria-label');
      expect(label).toBeTruthy();
    }
  });

  test('scenario 7: IDE pane layout is keyboard-focusable', async ({ page }) => {
    // Explorer artifacts are keyboard-activatable (tabindex=0).
    const firstArtifact = page.locator('.artifact-item').first();
    await expect(firstArtifact).toHaveAttribute('tabindex', '0');

    // Focusing and pressing Enter opens the artifact in the editor.
    await firstArtifact.focus();
    await page.keyboard.press('Enter');
    await expect(page.locator('.editor-source')).toBeVisible({ timeout: 5000 });
  });
});
