// Playwright fixture: intercepts fetch/WS calls to a fake SpecKit UI backend
// so tests exercise real UI logic without a running Rust backend.

import type { Page, Route } from '@playwright/test';
import type { FeatureDetail } from '../../src/api-client';

export interface MockBackendState {
  feature: FeatureDetail;
  patchTaskShouldConflict?: boolean;
}

export function defaultFeature(): FeatureDetail {
  return {
    id: '001-speckit-visual-ui',
    spec: {
      title: 'SpecKit Visual UI',
      status: 'Draft',
      user_stories: [
        { id: 'US1', title: 'Visualize the Spec-to-Task Hierarchy', priority: 'P1', acceptance_scenarios: [] },
        { id: 'US2', title: 'Draft and Clarify Specs', priority: 'P2', acceptance_scenarios: [] },
        { id: 'US3', title: 'Track and Launch Execution', priority: 'P3', acceptance_scenarios: [] },
      ],
      functional_requirements: ['FR-001', 'FR-002'],
      clarifications: [],
      content_hash: 'sha256:spec-v1',
    },
    plan: {
      summary: 'Local UI plan',
      technical_context: 'Rust + TS',
      constitution_gates: [{ principle: 'I', result: 'Pass', notes: '' }],
      content_hash: 'sha256:plan-v1',
    },
    tasks: [
      {
        id: 'T001',
        description: 'Setup workspace',
        status: 'Completed',
        parallel_eligible: false,
        target_files: ['Cargo.toml'],
        user_story_ref: 'US1',
      },
      {
        id: 'T002',
        description: 'Implement canvas rendering',
        status: 'Todo',
        parallel_eligible: true,
        target_files: ['src/canvas/canvas.ts'],
        user_story_ref: 'US1',
      },
      {
        id: 'T003',
        description: 'Implement writer conflict detection',
        status: 'Todo',
        parallel_eligible: true,
        target_files: ['src/canvas/canvas.ts'],
        user_story_ref: 'US1',
      },
    ],
    tasks_content_hash: 'sha256:tasks-v1',
  };
}

/** Installs route interception for the REST endpoints. WebSocket
 * interception is handled separately via an injected fake WebSocket
 * (see installFakeWebSocket) since Playwright route() doesn't intercept ws://. */
export async function installMockApi(page: Page, state: MockBackendState): Promise<void> {
  await page.route('**/api/features', async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        features: [{ id: state.feature.id, title: 'SpecKit Visual UI', status: 'Draft' }],
      }),
    });
  });

  await page.route(`**/api/features/${state.feature.id}`, async (route: Route) => {
    if (route.request().method() !== 'GET') {
      await route.fallback();
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(state.feature),
    });
  });

  await page.route(`**/api/features/${state.feature.id}/tasks/*`, async (route: Route) => {
    if (route.request().method() !== 'PATCH') {
      await route.fallback();
      return;
    }
    if (state.patchTaskShouldConflict) {
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({
          error: 'conflict',
          current_hash: 'sha256:tasks-v2',
          message: 'tasks.md changed on disk. Reload and reapply your edit.',
        }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ content_hash: 'sha256:tasks-v2' }),
    });
  });

  await page.route(`**/api/features/${state.feature.id}/clarify`, async (route: Route) => {
    await route.fulfill({
      status: 202,
      contentType: 'application/json',
      body: JSON.stringify({ session_id: 'sess-1' }),
    });
  });

  await page.route(
    `**/api/features/${state.feature.id}/clarify/*/answer`,
    async (route: Route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          updated_line: 'FR-012',
          spec_content_hash: 'sha256:spec-v2',
        }),
      });
    },
  );

  await page.route(`**/api/features/${state.feature.id}/analyze`, async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        findings: [
          {
            target_file: 'tasks.md',
            target_line_or_section: 'T003',
            description: 'Task references nonexistent requirement FR-099',
            severity: 'Warning',
          },
        ],
        constitution_compliance: 'Fail',
      }),
    });
  });

  await page.route(
    `**/api/features/${state.feature.id}/tasks/*/execute`,
    async (route: Route) => {
      await route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify({ run_id: `run-${Date.now()}` }),
      });
    },
  );

  await page.route('**/api/init', async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ success: true, output: 'Initialized project.' }),
    });
  });
}

/** Mocks the Feature 010 IDE endpoints (artifacts, workflow, options,
 * preferences, history, health, run/interaction) so the new WorkspaceApp
 * views render without a live Rust backend. */
export async function installIdeMocks(page: Page, featureId: string): Promise<void> {
  const artifacts = [
    { path: 'spec.md', kind: 'spec', exists: true, content_hash: 'sha256:spec-v1', dirty: false, save_state: 'clean', validity: [], workflow_phase: 'specify', stale: false },
    { path: 'plan.md', kind: 'plan', exists: true, content_hash: 'sha256:plan-v1', dirty: false, save_state: 'clean', validity: [], workflow_phase: 'plan', stale: false },
    { path: 'tasks.md', kind: 'tasks', exists: true, content_hash: 'sha256:tasks-v1', dirty: false, save_state: 'clean', validity: [], workflow_phase: 'tasks', stale: false },
    { path: 'research.md', kind: 'research', exists: false, content_hash: null, dirty: false, save_state: 'clean', validity: [], workflow_phase: 'plan', stale: false },
  ];

  await page.route(`**/api/features/${featureId}/artifacts`, async (route: Route) => {
    if (route.request().method() !== 'GET') { await route.fallback(); return; }
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ artifacts }) });
  });

  await page.route(`**/api/features/${featureId}/artifacts/*`, async (route: Route) => {
    if (route.request().method() !== 'GET') { await route.fallback(); return; }
    const path = route.request().url().split('/artifacts/')[1];
    const art = artifacts.find(a => a.path === decodeURIComponent(path));
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        path: art?.path ?? 'spec.md',
        kind: art?.kind ?? 'spec',
        text: `# ${art?.path ?? 'spec.md'}\n\nSample content for the IDE test.`,
        content_hash: art?.content_hash ?? 'sha256:spec-v1',
        outline: [{ title: 'Spec', line: 1, level: 1 }],
        save_state: 'clean',
        validity: [],
      }),
    });
  });

  await page.route(`**/api/features/${featureId}/workflow`, async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        steps: [
          { id: 'specify', order: 1, purpose: 'Create the feature specification', inputs: [], outputs: [{ path: 'spec.md' }], prerequisites: [], available: true, state: 'ready', blocking_reason: null, latest_attempt_id: null, installed_definition_ref: 'speckit-specify' },
          { id: 'plan', order: 3, purpose: 'Generate the implementation plan', inputs: [{ path: 'spec.md' }], outputs: [{ path: 'plan.md' }], prerequisites: ['specify'], available: true, state: 'blocked', blocking_reason: 'specify must complete first', latest_attempt_id: null, installed_definition_ref: 'speckit-plan' },
          { id: 'implement', order: 7, purpose: 'Execute the task list', inputs: [], outputs: [], prerequisites: ['tasks'], available: false, state: 'unavailable', blocking_reason: null, latest_attempt_id: null, installed_definition_ref: 'speckit-implement' },
        ],
      }),
    });
  });

  await page.route('**/api/options', async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        revision: 'sha256:opts-v1',
        models: ['gpt-4o', 'claude-sonnet-4'],
        reasoning_efforts: ['low', 'medium', 'high'],
        max_iterations: { min: 1, max: 50, default: 10 },
      }),
    });
  });

  await page.route(`**/api/features/${featureId}/preferences`, async (route: Route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ last_feature_id: featureId }) });
    } else {
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ last_feature_id: featureId }) });
    }
  });

  await page.route(`**/api/features/${featureId}/history`, async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        attempts: [
          { attempt_id: 'att-1', step_id: 'specify', status: 'succeeded', started_at: '2026-08-01T10:00:00Z', ended_at: '2026-08-01T10:05:00Z', prior_attempt_id: null, changes_count: 3 },
        ],
        next_cursor: null,
      }),
    });
  });

  await page.route('**/api/health', async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ backend_reachable: true, agent_binary_discovered: true, credentials_present: true, repo_writable: true, read_only: false }),
    });
  });

  await page.route(`**/api/features/${featureId}/workflow/*/run`, async (route: Route) => {
    await route.fulfill({ status: 202, contentType: 'application/json', body: JSON.stringify({ attempt_id: 'att-new', ws: `/api/attempts/att-new/stream` }) });
  });

  await page.route('**/api/attempts/*/answer', async (route: Route) => {
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ confirmed: true }) });
  });

  await page.route('**/api/attempts/*/cancel', async (route: Route) => {
    await route.fulfill({ status: 202, contentType: 'application/json', body: JSON.stringify({ cancelled: true }) });
  });
}

/** Installs a fake global WebSocket in the page that never actually connects
 * over the network; tests can trigger fake incoming messages via
 * `window.__fakeWs.emit(url, data)`. */
export async function installFakeWebSocket(page: Page): Promise<void> {
  await page.addInitScript(() => {
    class FakeWebSocket {
      static instances: FakeWebSocket[] = [];
      url: string;
      listeners: Record<string, Array<(ev: unknown) => void>> = {};
      readyState = 1;

      constructor(url: string) {
        this.url = url;
        FakeWebSocket.instances.push(this);
      }

      addEventListener(type: string, cb: (ev: unknown) => void): void {
        this.listeners[type] = this.listeners[type] || [];
        this.listeners[type].push(cb);
      }

      removeEventListener(): void {
        // no-op for tests
      }

      close(): void {
        this.readyState = 3;
      }

      send(): void {
        // no-op
      }

      __emit(data: unknown): void {
        for (const cb of this.listeners['message'] || []) {
          cb({ data: JSON.stringify(data) });
        }
      }
    }

    (window as unknown as { __FakeWebSocket: typeof FakeWebSocket }).__FakeWebSocket =
      FakeWebSocket;
    (window as unknown as { WebSocket: unknown }).WebSocket = FakeWebSocket;
  });
}
