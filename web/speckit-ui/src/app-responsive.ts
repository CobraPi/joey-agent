// Responsive mode controller (T081, FR-039).
// Purpose-built responsive modes: desktop (graph authoring + multi-panel),
// tablet (structured forms + board review), mobile (status/questions/
// approvals/diffs, not precision graph manipulation).

export type ResponsiveMode = 'desktop' | 'tablet' | 'mobile';

export interface ModeCapabilities {
  mode: ResponsiveMode;
  graph_authoring: boolean;
  multi_panel: boolean;
  structured_forms: boolean;
  board_review: boolean;
  status_questions: boolean;
  approvals: boolean;
  diffs: boolean;
  min_touch_target_px: number;
}

const MODE_CAPABILITIES: Record<ResponsiveMode, ModeCapabilities> = {
  desktop: {
    mode: 'desktop',
    graph_authoring: true,
    multi_panel: true,
    structured_forms: true,
    board_review: true,
    status_questions: true,
    approvals: true,
    diffs: true,
    min_touch_target_px: 32,
  },
  tablet: {
    mode: 'tablet',
    graph_authoring: false, // no precision graph on tablet
    multi_panel: false,
    structured_forms: true,
    board_review: true,
    status_questions: true,
    approvals: true,
    diffs: true,
    min_touch_target_px: 44, // WCAG 2.5.5
  },
  mobile: {
    mode: 'mobile',
    graph_authoring: false,
    multi_panel: false,
    structured_forms: false, // reduced authoring surface
    board_review: false,
    status_questions: true,
    approvals: true,
    diffs: true,
    min_touch_target_px: 44,
  },
};

/** Detect the responsive mode from the viewport width. */
export class ResponsiveController {
  private current: ResponsiveMode = 'desktop';
  private listeners: Array<(mode: ResponsiveMode) => void> = [];

  constructor() {
    if (typeof window !== 'undefined') {
      this.current = this.detect();
      window.addEventListener('resize', () => {
        const next = this.detect();
        if (next !== this.current) {
          this.current = next;
          this.listeners.forEach((fn) => fn(next));
        }
      });
    }
  }

  detect(): ResponsiveMode {
    if (typeof window === 'undefined') return 'desktop';
    const width = window.innerWidth;
    if (width < 640) return 'mobile';
    if (width < 1024) return 'tablet';
    return 'desktop';
  }

  get mode(): ResponsiveMode {
    return this.current;
  }

  get capabilities(): ModeCapabilities {
    return MODE_CAPABILITIES[this.current];
  }

  onModeChange(callback: (mode: ResponsiveMode) => void): () => void {
    this.listeners.push(callback);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== callback);
    };
  }
}
