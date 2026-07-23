// Application entrypoint: wires nav + views together against a live backend.

import { SpeckitApiClient } from './api-client';
import { Board } from './board/board';
import { SpecTaskCanvas } from './canvas/canvas';
import { InitWizard } from './init-wizard';
import { Workspace } from './workspace/workspace';

const client = new SpeckitApiClient();

const DEFAULT_FEATURE_ID = '001-speckit-visual-ui';

function qs<T extends HTMLElement>(sel: string): T {
  const el = document.querySelector<T>(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el;
}

function setupNav(): void {
  const nav = qs<HTMLElement>('#view-nav');
  nav.addEventListener('click', (e) => {
    const target = e.target as HTMLElement;
    const view = target.dataset.view;
    if (!view) return;
    document.querySelectorAll('#view-nav button').forEach((b) => b.classList.remove('active'));
    target.classList.add('active');
    document.querySelectorAll('.view').forEach((v) => v.classList.remove('active'));
    qs<HTMLElement>(`#view-${view}`).classList.add('active');
  });
}

async function init(): Promise<void> {
  setupNav();

  const canvas = new SpecTaskCanvas({
    container: qs('#view-canvas'),
    client,
    featureId: DEFAULT_FEATURE_ID,
  });
  await canvas.load().catch((err) => {
    // Feature may not exist yet against a live backend during local dev;
    // surface the error rather than crash the whole app.
    console.error('canvas load failed', err);
  });

  const detail = await client.getFeature(DEFAULT_FEATURE_ID).catch(() => null);

  if (detail?.spec) {
    new Workspace({
      container: qs('#view-workspace'),
      client,
      featureId: DEFAULT_FEATURE_ID,
      specMarkdown: detail.spec.title ?? '',
    });
  }

  const board = new Board({
    container: qs('#view-board'),
    client,
    featureId: DEFAULT_FEATURE_ID,
  });
  if (detail?.tasks) board.setTasks(detail.tasks);

  new InitWizard({ container: qs('#view-init'), client });
}

void init();
