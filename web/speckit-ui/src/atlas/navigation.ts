// Intent-based navigation (T036, FR-003).
//
// Extends the app shell with stage-based navigation:
// Overview → Define → Design → Break down → Build → Review.
// `spec.md`/`plan.md`/`tasks.md` appear inside stages as source indicators
// and escape hatches, not as upfront knowledge.
//
// This module is additive — it does not modify the existing WorkspaceApp.
// The app composes it as an optional navigation layer.

export type Stage =
  | 'overview'
  | 'define'
  | 'design'
  | 'break_down'
  | 'build'
  | 'review';

export const STAGE_ORDER: Stage[] = [
  'overview',
  'define',
  'design',
  'break_down',
  'build',
  'review',
];

export const STAGE_LABELS: Record<Stage, string> = {
  overview: 'Overview',
  define: 'Define',
  design: 'Design',
  break_down: 'Break down',
  build: 'Build',
  review: 'Review',
};

/** Which artifact each stage surfaces as a source indicator. */
export const STAGE_ARTIFACTS: Record<Stage, string[]> = {
  overview: [],
  define: ['spec.md'],
  design: ['plan.md'],
  break_down: ['tasks.md'],
  build: ['tasks.md'],
  review: ['spec.md', 'plan.md', 'tasks.md'],
};

/** Intent-based navigation controller. Manages stage transitions and deep
 * links without losing context. */
export class StageNavigator {
  private current: Stage = 'overview';
  private listeners: Array<(stage: Stage) => void> = [];

  constructor(initial: Stage = 'overview') {
    this.current = initial;
    // Restore from URL hash if present (deep links, FR-038).
    if (typeof window !== 'undefined') {
      const hash = window.location.hash.replace('#', '');
      if (STAGE_ORDER.includes(hash as Stage)) {
        this.current = hash as Stage;
      }
      window.addEventListener('hashchange', () => {
        const h = window.location.hash.replace('#', '');
        if (STAGE_ORDER.includes(h as Stage)) {
          this.go(h as Stage);
        }
      });
    }
  }

  /** The current stage. */
  get stage(): Stage {
    return this.current;
  }

  /** Navigate to a stage. Updates the URL hash (deep link) and notifies listeners. */
  go(stage: Stage): void {
    if (stage === this.current) return;
    this.current = stage;
    if (typeof window !== 'undefined') {
      window.location.hash = stage;
    }
    this.listeners.forEach((fn) => fn(stage));
  }

  /** Register a listener for stage changes. */
  onChange(fn: (stage: Stage) => void): () => void {
    this.listeners.push(fn);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== fn);
    };
  }

  /** Render the navigation tabs into a container element. */
  renderTabs(container: HTMLElement, onArtifactClick?: (path: string) => void): void {
    container.innerHTML = '';
    container.setAttribute('role', 'tablist');
    container.setAttribute('aria-label', 'Stage navigation');
    container.style.cssText = 'display:flex;gap:0;border-bottom:1px solid #ddd;padding:0 16px;';

    STAGE_ORDER.forEach((stage) => {
      const tab = document.createElement('button');
      tab.textContent = STAGE_LABELS[stage];
      tab.setAttribute('role', 'tab');
      tab.setAttribute('aria-selected', stage === this.current ? 'true' : 'false');
      const isActive = stage === this.current;
      tab.style.cssText = `padding:12px 16px;border:none;background:none;cursor:pointer;border-bottom:2px solid ${
        isActive ? '#2563eb' : 'transparent'
      };color:${isActive ? '#2563eb' : '#666'};font-weight:${isActive ? '600' : '400'};`;
      tab.addEventListener('click', () => this.go(stage));
      container.appendChild(tab);
    });

    // Source-indicator artifacts for the current stage (escape hatches).
    const artifacts = STAGE_ARTIFACTS[this.current];
    if (artifacts.length > 0) {
      const arts = document.createElement('span');
      arts.style.cssText = 'margin-left:auto;align-self:center;font-size:12px;color:#999;';
      arts.textContent = 'sources: ';
      artifacts.forEach((a, i) => {
        if (i > 0) arts.textContent += ', ';
        const link = document.createElement('button');
        link.textContent = a;
        link.style.cssText = 'background:none;border:none;color:#2563eb;cursor:pointer;text-decoration:underline;font-size:12px;font-family:monospace;';
        link.setAttribute('aria-label', `Open ${a} (escape hatch to raw source)`);
        link.addEventListener('click', () => onArtifactClick?.(a));
        arts.appendChild(link);
      });
      container.appendChild(arts);
    }
  }
}
