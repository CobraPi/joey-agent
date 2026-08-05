// Semantic zoom — three altitudes (T100, FR-034).
//
// Whole-feature Atlas → single-artifact Board → single-node Focus.
// Zooming changes information density, not just scale.

export type Altitude = 'atlas' | 'board' | 'focus';

export interface ZoomState {
  altitude: Altitude;
  feature_id: string;
  artifact?: string;
  node_id?: string;
}

/** Manages the three-altitude semantic-zoom shell. */
export class SemanticZoom {
  private current: ZoomState;

  constructor(initial: ZoomState) {
    this.current = initial;
  }

  get altitude(): Altitude {
    return this.current.altitude;
  }

  get state(): ZoomState {
    return this.current;
  }

  /** Zoom in: atlas → board → focus. */
  zoomIn(artifact?: string, nodeId?: string): void {
    switch (this.current.altitude) {
      case 'atlas':
        this.current = { ...this.current, altitude: 'board', artifact };
        break;
      case 'board':
        this.current = { ...this.current, altitude: 'focus', node_id: nodeId };
        break;
      case 'focus':
        // Already at max zoom.
        break;
    }
  }

  /** Zoom out: focus → board → atlas. */
  zoomOut(): void {
    switch (this.current.altitude) {
      case 'focus':
        this.current = { ...this.current, altitude: 'board', node_id: undefined };
        break;
      case 'board':
        this.current = { ...this.current, altitude: 'atlas', artifact: undefined };
        break;
      case 'atlas':
        // Already at min zoom.
        break;
    }
  }

  /** Jump directly to an altitude. */
  goTo(altitude: Altitude, artifact?: string, nodeId?: string): void {
    this.current = { ...this.current, altitude, artifact, node_id: nodeId };
  }

  /** Render a zoom-level indicator showing the three altitudes. */
  renderIndicator(container: HTMLElement): void {
    container.innerHTML = '';
    container.setAttribute('role', 'navigation');
    container.setAttribute('aria-label', 'Semantic zoom level');

    const levels: Altitude[] = ['atlas', 'board', 'focus'];
    const labels: Record<Altitude, string> = {
      atlas: 'Atlas',
      board: 'Board',
      focus: 'Focus',
    };

    levels.forEach((alt) => {
      const btn = document.createElement('button');
      btn.textContent = labels[alt];
      const isActive = this.current.altitude === alt;
      btn.style.cssText = `padding:4px 12px;border:1px solid ${isActive ? '#2563eb' : '#ddd'};background:${isActive ? '#2563eb' : 'white'};color:${isActive ? 'white' : '#666'};cursor:pointer;font-size:12px;`;
      btn.setAttribute('aria-pressed', String(isActive));
      btn.setAttribute('aria-label', `Zoom to ${labels[alt]} level`);
      btn.addEventListener('click', () => this.goTo(alt));
      container.appendChild(btn);
    });
  }
}
