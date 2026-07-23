// Split-screen co-pilot workspace wrapper (Pillar 2): wires document-pane,
// assistant-panel, and constitution-gauge together for a feature.

import type { SpeckitApiClient } from '../api-client';
import { AssistantPanel } from './assistant-panel';
import { ConstitutionGauge } from './constitution-gauge';
import { DocumentPane } from './document-pane';

export interface WorkspaceOptions {
  container: HTMLElement;
  client: SpeckitApiClient;
  featureId: string;
  specMarkdown: string;
}

export class Workspace {
  readonly documentPane: DocumentPane;
  readonly assistantPanel: AssistantPanel;
  readonly gauge: ConstitutionGauge;

  constructor(opts: WorkspaceOptions) {
    opts.container.innerHTML = '';
    const layout = document.createElement('div');
    layout.style.display = 'grid';
    layout.style.gridTemplateColumns = '1fr 1fr';
    layout.style.height = '100%';

    const docCol = document.createElement('div');
    docCol.style.overflow = 'auto';
    docCol.style.padding = '8px';

    const asideCol = document.createElement('div');
    asideCol.style.display = 'flex';
    asideCol.style.flexDirection = 'column';
    asideCol.style.borderLeft = '1px solid #3c3c3c';

    const gaugeEl = document.createElement('div');
    gaugeEl.style.padding = '8px';
    const panelEl = document.createElement('div');
    panelEl.style.flex = '1';
    panelEl.style.overflow = 'auto';
    panelEl.style.padding = '8px';

    asideCol.appendChild(gaugeEl);
    asideCol.appendChild(panelEl);
    layout.appendChild(docCol);
    layout.appendChild(asideCol);
    opts.container.appendChild(layout);

    this.documentPane = new DocumentPane({ container: docCol });
    this.documentPane.setContent(opts.specMarkdown);

    this.gauge = new ConstitutionGauge({ container: gaugeEl });

    this.assistantPanel = new AssistantPanel({
      container: panelEl,
      client: opts.client,
      featureId: opts.featureId,
      documentPane: this.documentPane,
      onFindings: (_findings, constitutionCompliance) => {
        this.gauge.setFromAnalyze(constitutionCompliance);
      },
    });
  }

  destroy(): void {
    this.assistantPanel.destroy();
  }
}
