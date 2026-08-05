import { SpeckitApiClient } from './api-client';
import { ExplorerView } from './views/explorer';
import { EditorView } from './views/editor';
import { WorkflowView } from './views/workflow';
import { RunPanelView } from './views/run-panel';
import { ReviewView } from './views/review';
import { ReadinessView } from './views/readiness';
import { SearchView } from './views/search';
import { PaneLayout } from './components/pane-layout';
import { installKeyboardShortcuts, announce, focusFirst } from './a11y/keyboard';

/** Unified resizable workspace shell composing explorer + editor + workflow + run panel (FR-002). */
export class WorkspaceApp {
  private api: SpeckitApiClient;
  private featureId: string;
  private root: HTMLElement;
  private layout: PaneLayout;

  private explorer: ExplorerView;
  private editor: EditorView;
  private workflow: WorkflowView;
  private runPanel: RunPanelView;
  private readiness: ReadinessView;

  constructor(api: SpeckitApiClient, featureId: string, root: HTMLElement) {
    this.api = api;
    this.featureId = featureId;
    this.root = root;
    this.layout = new PaneLayout();

    this.editor = new EditorView(api, featureId);
    this.runPanel = new RunPanelView(api);
    this.readiness = new ReadinessView(api, featureId);
    this.explorer = new ExplorerView(api, featureId, (path) => this.onArtifactSelect(path));
    this.workflow = new WorkflowView(api, featureId, (step) => this.onRunStep(step));

    this.layout.element.className = 'workspace-app';
  }

  async init(): Promise<void> {
    this.root.innerHTML = '';
    this.root.appendChild(this.layout.element);

    this.layout.addPane('explorer', 'Artifacts', this.explorer.element);
    this.layout.addPane('editor', 'Editor', this.editor.element);
    this.layout.addPane('workflow', 'Workflow', this.workflow.element);
    this.layout.addPane('run-panel', 'Run', this.runPanel.element);

    await Promise.all([
      this.explorer.load(),
      this.workflow.load(),
    ]);

    // Load preferences (FR-026).
    try {
      const prefs = await this.api.getPreferences(this.featureId);
      if (prefs.last_feature_id) announce(`Restored workspace for ${prefs.last_feature_id}`);
    } catch {
      // preferences may not exist yet — fine.
    }

    this.installShortcuts();
    announce(`Workspace loaded for feature ${this.featureId}`);
  }

  private async onArtifactSelect(path: string): Promise<void> {
    const opened = await this.editor.open(path);
    if (opened) {
      announce(`Opened ${path}`);
    }
  }

  private async onRunStep(step: { id: string }): Promise<void> {
    const opts = await this.api.getOptions();
    try {
      const result = await this.api.runWorkflowStep(this.featureId, step.id, {
        option_catalog_rev: opts.revision,
        change_mode: 'staged',
      });
      this.runPanel.watch(result.attempt_id);
      announce(`Started ${step.id} run`);
    } catch (e) {
      announce(`Failed to start run: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  private installShortcuts(): void {
    installKeyboardShortcuts({
      onSearch: () => this.showSearch(),
      onSave: () => this.editor.element.querySelector<HTMLButtonElement>('.save-btn')?.click(),
      onToggleExplorer: () => this.togglePane('explorer'),
      onToggleWorkflow: () => this.togglePane('workflow'),
      onToggleReview: () => this.showReview(),
      onToggleReadiness: () => this.showReadiness(),
      onCancel: () => {
        if (this.runPanel) {
          // Cancel is handled by the run panel's cancel button.
        }
      },
    });
  }

  private togglePane(id: string): void {
    const pane = this.layout.element.querySelector(`[data-pane-id="${id}"]`);
    if (pane) {
      const content = pane.querySelector('.pane > *:last-child') as HTMLElement;
      if (content) content.hidden = !content.hidden;
    }
  }

  private async showSearch(): Promise<void> {
    const search = new SearchView(this.api, this.featureId, (result) => {
      if (result.type === 'artifact' && result.path) {
        this.editor.open(result.path);
      }
    });
    await search.load();
    // Show as a modal overlay.
    const overlay = document.createElement('div');
    overlay.className = 'search-overlay';
    overlay.setAttribute('role', 'dialog');
    overlay.setAttribute('aria-modal', 'true');
    overlay.appendChild(search.element);
    this.root.appendChild(overlay);
    // Move focus into the search input so keyboard users can type immediately (FR-027).
    focusFirst(overlay);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) overlay.remove();
    });
  }

  private async showReview(): Promise<void> {
    // Opens the review view for the latest attempt.
    const history = await this.api.getHistory(this.featureId, 1);
    if (history.attempts.length > 0) {
      const review = new ReviewView(this.api, history.attempts[0].attempt_id);
      await review.load();
      // Show in the run panel area.
    }
  }

  private async showReadiness(): Promise<void> {
    await this.readiness.load();
    // Toggle readiness panel visibility.
    const existing = this.layout.element.querySelector('[data-pane-id="readiness"]');
    if (existing) {
      this.layout.removePane('readiness');
    } else {
      this.layout.addPane('readiness', 'Readiness', this.readiness.element);
    }
  }
}
