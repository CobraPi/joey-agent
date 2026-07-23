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
