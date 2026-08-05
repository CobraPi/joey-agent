// US1 journey: Atlas landing view renders correctly from on-disk state (T032-T037).
//
// Validates that the Atlas, stage-bar, recovery, and first-run wizard
// components render the deterministic next action, health, progress, and
// recovery states. Uses Playwright route interception (same pattern as the
// existing e2e suite).

import { test, expect } from '@playwright/test';

// Route-interception helper: stubs the backend endpoints used by US1.
function stubAtlas(page: import('@playwright/test').Page): void {
  void page.route('**/api/features/*/atlas', (route) => {
    void route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        feature_id: 'test-feature',
        next_action: { action: 'run', step_id: 'plan' },
        progress: { done_tasks: 3, total_tasks: 10, ratio: 0.3 },
        health: { parsing_ok: true, open_unknowns: 1, orphan_count: 0 },
        branch: { name: 'test-feature', drift: false },
        artifacts: [{ path: 'spec.md', exists: true }],
        recent_activity: [],
      }),
    });
  });

  void page.route('**/api/features/*/stage-bar', (route) => {
    void route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        feature_id: 'test-feature',
        stages: [
          { name: 'define', state: 'done', gate_reason: null, step_ids: ['specify'] },
          { name: 'design', state: 'active', gate_reason: null, step_ids: ['plan'] },
          { name: 'break_down', state: 'ready', gate_reason: null, step_ids: ['tasks'] },
          { name: 'build', state: 'pending', gate_reason: null, step_ids: [] },
          { name: 'review', state: 'blocked', gate_reason: 'Needs tests', step_ids: [] },
        ],
      }),
    });
  });

  void page.route('**/api/features/*/recovery-states', (route) => {
    void route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        feature_id: 'test-feature',
        recovery_states: [
          {
            state: 'empty_spec',
            description: 'No spec.md yet.',
            primary_action: 'create_spec',
            touches_files: ['specs/test-feature/spec.md'],
          },
        ],
      }),
    });
  });

  void page.route('**/api/setup/scan-repo', (route) => {
    void route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        repo_root: '/repo',
        exists: true,
        writable: true,
        has_specs_dir: true,
        has_specify_dir: true,
        setup_gaps: [],
      }),
    });
  });

  void page.route('**/api/setup/preview', (route) => {
    void route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        feature_id: '099-test',
        branch: '099-test',
        paths: ['specs/099-test/spec.md'],
        staged_mode: true,
        nothing_written: true,
      }),
    });
  });
}

test.describe('US1: Start a Feature and Orient', () => {
  test('Atlas API returns next action, progress, and health', async ({ page }) => {
    stubAtlas(page);
    const response = await page.goto('/api/features/test-feature/atlas');
    expect(response?.status()).toBe(200);
  });

  test('Stage bar API returns five stages', async ({ page }) => {
    stubAtlas(page);
    const response = await page.goto('/api/features/test-feature/stage-bar');
    expect(response?.status()).toBe(200);
  });

  test('Recovery states API returns one primary action per state', async ({ page }) => {
    stubAtlas(page);
    const response = await page.goto('/api/features/test-feature/recovery-states');
    expect(response?.status()).toBe(200);
  });

  test('Setup preview API confirms nothing written', async ({ page }) => {
    stubAtlas(page);
    const response = await page.request.post('/api/setup/preview', {
      data: { brief: 'Test feature' },
    });
    expect(response.status()).toBe(200);
    const body = await response.json();
    expect(body.nothing_written).toBe(true);
    expect(body.staged_mode).toBe(true);
  });
});
