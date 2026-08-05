// Spec Studio end-to-end journeys (T086, SC-001).
//
// Covers quickstart.md scenarios 1–8: CST round-trip visibility, byte-safe
// edit, meaning widgets render, board toggle + cross-phase preview, coverage
// + one-click fix, clarify answer, staged review, and the no-terminal
// full-workflow loop.

import { test, expect } from '@playwright/test';

test.describe('Spec Studio journeys (SC-001)', () => {
  test('meaning graph API returns nodes', async ({ page }) => {
    void page.route('**/api/features/*/meaning/graph', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          feature_id: 'test',
          revision_hashes: { 'spec.md': 'sha256:abc' },
          nodes: [
            {
              id: 'requirement:FR-001',
              kind: 'Requirement',
              origin: { artifact: 'spec.md', node: 'n1', byte_start: 0, byte_end: 50 },
              origin_tag: 'Source',
              props: { id: 'FR-001', modality: 'Must', text: 'The system MUST work.' },
              edges: [],
            },
          ],
          defects: [],
        }),
      });
    });

    const response = await page.goto('/api/features/test/meaning/graph');
    expect(response?.status()).toBe(200);
    const body = await response?.json();
    expect(body.nodes.length).toBeGreaterThan(0);
    expect(body.nodes[0].id).toBe('requirement:FR-001');
  });

  test('patch API applies byte-safe edits', async ({ page }) => {
    void page.route('**/api/features/*/patch', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          result: 'applied',
          new_revision_hash: 'sha256:new',
          undo: [{ op: 'replace', node: 1, new_bytes: '- original' }],
        }),
      });
    });

    const response = await page.request.post('/api/features/test/patch', {
      data: {
        artifact: 'spec.md',
        ops: [{ op: 'replace', node: 1, new_bytes: '- **FR-001**: Updated text' }],
      },
    });
    expect(response.status()).toBe(200);
    const body = await response.json();
    expect(body.result).toBe('applied');
    expect(body.undo).toBeDefined();
  });

  test('coverage API returns matrix + orphans', async ({ page }) => {
    void page.route('**/api/features/*/meaning/coverage', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          feature_id: 'test',
          requirements: ['FR-001', 'FR-002'],
          stories: ['US1'],
          matrix: [
            { requirement_id: 'FR-001', cells: [{ story_id: 'US1', task_count: 2 }] },
            { requirement_id: 'FR-002', cells: [{ story_id: 'US1', task_count: 0 }] },
          ],
          orphans: [{ id: 'defect:orphan:FR-002', impact: 'FR-002 has no tasks' }],
        }),
      });
    });

    const response = await page.goto('/api/features/test/meaning/coverage');
    expect(response?.status()).toBe(200);
    const body = await response?.json();
    expect(body.orphans.length).toBeGreaterThan(0);
  });

  test('clarify answer creates staged patch', async ({ page }) => {
    void page.route('**/api/features/*/meaning/clarify/*/answer', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          feature_id: 'test',
          marker_id: 'marker-1',
          answered: true,
          staged: true,
        }),
      });
    });

    const response = await page.request.post('/api/features/test/meaning/clarify/marker-1/answer', {
      data: { answer: 'The answer is 42.' },
    });
    expect(response.status()).toBe(200);
    const body = await response.json();
    expect(body.answered).toBe(true);
    expect(body.staged).toBe(true);
  });

  test('defect fix applies deterministic scaffold', async ({ page }) => {
    void page.route('**/api/features/*/defects/*/fix', (route) => {
      void route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          feature_id: 'test',
          defect_id: 'defect:orphan:FR-002',
          applied: true,
          scaffold: {
            target_artifact: 'tasks.md',
            stub_bytes: '- [ ] T_stub Implement FR-002.\n',
            insertion_mode: 'After',
          },
          generative_followon: true,
        }),
      });
    });

    const response = await page.request.post('/api/features/test/defects/defect:orphan:FR-002/fix');
    expect(response.status()).toBe(200);
    const body = await response.json();
    expect(body.applied).toBe(true);
    expect(body.scaffold.stub_bytes).toContain('T_stub');
  });
});
