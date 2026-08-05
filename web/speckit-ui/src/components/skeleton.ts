// Anti-freeze skeleton/shimmer affordances (T098, FR-027).
//
// Optimistic skeletons for boards/widgets about to populate so async content
// arrival never looks frozen. Uses a CSS shimmer animation (respects
// prefers-reduced-motion via the global guard).

/** Create a skeleton placeholder element that shimmers while content loads. */
export function createSkeleton(width: string = '100%', height: string = '20px'): HTMLElement {
  const skel = document.createElement('div');
  skel.className = 'speckit-skeleton';
  skel.style.cssText = `width:${width};height:${height};background:linear-gradient(90deg,#f0f0f0 25%,#e0e0e0 50%,#f0f0f0 75%);background-size:200% 100%;animation:speckit-shimmer 1.5s infinite;border-radius:4px;`;
  skel.setAttribute('aria-hidden', 'true');
  return skel;
}

/** Fill a container with N skeleton rows (for a board about to populate). */
export function showSkeletonRows(container: HTMLElement, count: number): void {
  container.innerHTML = '';
  container.setAttribute('aria-busy', 'true');
  for (let i = 0; i < count; i++) {
    const row = document.createElement('div');
    row.style.cssText = 'display:flex;gap:8px;padding:8px 0;border-bottom:1px solid #f5f5f5;';
    row.appendChild(createSkeleton('20px', '20px')); // checkbox
    row.appendChild(createSkeleton('60%', '16px')); // description
    row.appendChild(createSkeleton('40px', '16px')); // badge
    container.appendChild(row);
  }
}

/** Clear skeletons and mark the container as no longer busy. */
export function clearSkeletons(container: HTMLElement): void {
  container.innerHTML = '';
  container.removeAttribute('aria-busy');
}

/** Inject the shimmer keyframes + reduced-motion guard once. */
export function injectSkeletonStyles(): void {
  if (typeof document === 'undefined') return;
  if (document.getElementById('speckit-skeleton-styles')) return;
  const style = document.createElement('style');
  style.id = 'speckit-skeleton-styles';
  style.textContent = `
    @keyframes speckit-shimmer {
      0% { background-position: 200% 0; }
      100% { background-position: -200% 0; }
    }
    @media (prefers-reduced-motion: reduce) {
      .speckit-skeleton {
        animation: none !important;
        background: #e5e7eb !important;
      }
    }
  `;
  document.head.appendChild(style);
}
