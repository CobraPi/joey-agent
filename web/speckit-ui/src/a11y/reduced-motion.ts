// Global reduced-motion guard (T102, FR-038).
//
// Injects a global CSS rule that disables all animations and transitions when
// the user has `prefers-reduced-motion: reduce` set. Motion MUST be optional.

/** Inject the global reduced-motion guard into the document head. */
export function injectReducedMotionGuard(): void {
  if (typeof document === 'undefined') return;
  if (document.getElementById('speckit-reduced-motion-guard')) return;

  const style = document.createElement('style');
  style.id = 'speckit-reduced-motion-guard';
  style.textContent = `
    @media (prefers-reduced-motion: reduce) {
      *,
      *::before,
      *::after {
        animation-duration: 0.01ms !important;
        animation-iteration-count: 1 !important;
        transition-duration: 0.01ms !important;
        scroll-behavior: auto !important;
      }
    }
  `;
  document.head.appendChild(style);
}

/** Check if the user prefers reduced motion. */
export function prefersReducedMotion(): boolean {
  if (typeof window === 'undefined') return false;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}
