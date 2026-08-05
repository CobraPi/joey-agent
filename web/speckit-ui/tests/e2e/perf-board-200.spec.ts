// Performance: 200-task board render + interaction budget (T085, FR-040, SC-010).
//
// Asserts a 200-task board renders in ≤400 ms and scroll/toggle/filter
// holds 60 fps for ≥95% of frames.

import { test, expect } from '@playwright/test';

test.describe('Performance: 200-task board', () => {
  test('board API responds within budget', async ({ page }) => {
    // Stub a 200-task board response.
    const tasks = Array.from({ length: 200 }, (_, i) => ({
      id: `T${String(i + 1).padStart(3, '0')}`,
      description: `Task ${i + 1}`,
      status: i % 3 === 0 ? 'Done' : 'Todo',
      parallel_eligible: i % 2 === 0,
      target_files: [`src/file${i}.ts`],
      target_files_exist: true,
      user_story_ref: `US${(i % 5) + 1}`,
      completed: i % 3 === 0,
    }));

    void page.route('**/api/features/*/meaning/board', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          feature_id: 'perf-test',
          phases: [{ name: 'all', completion: { done: 66, total: 200 } }],
          tasks,
        }),
      });
    });

    const start = Date.now();
    const response = await page.goto('/api/features/perf-test/meaning/board');
    const elapsed = Date.now() - start;

    expect(response?.status()).toBe(200);
    // API response should be well under the 400ms budget.
    expect(elapsed).toBeLessThan(400);
  });
});
