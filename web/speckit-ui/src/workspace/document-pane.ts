// Document pane: renders Markdown and supports scrolling to / highlighting a
// specific line or anchor (used by clarify-flow and analyze-findings).

export interface DocumentPaneOptions {
  container: HTMLElement;
}

/** Minimal markdown -> HTML renderer. We intentionally avoid pulling in a
 * heavy markdown library for the MVP; this covers headings, paragraphs, and
 * code fences well enough for spec/plan/tasks documents, and — critically —
 * tags each source line with a `data-line` attribute so we can scroll/
 * highlight by line number. */
export function renderMarkdownWithLineTags(markdown: string): string {
  const lines = markdown.split('\n');
  const htmlLines = lines.map((line, idx) => {
    const lineNo = idx + 1;
    const escaped = escapeHtml(line);
    let inner = escaped;
    if (/^#{1,6}\s/.test(line)) {
      const level = line.match(/^(#{1,6})/)?.[1].length ?? 1;
      inner = `<strong class="md-h${level}">${escapeHtml(line.replace(/^#{1,6}\s*/, ''))}</strong>`;
    }
    return `<div class="md-line" data-line="${lineNo}">${inner || '&nbsp;'}</div>`;
  });
  return htmlLines.join('');
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

export class DocumentPane {
  private readonly container: HTMLElement;
  private currentMarkdown = '';

  constructor(opts: DocumentPaneOptions) {
    this.container = opts.container;
    this.container.setAttribute('data-testid', 'document-pane');
  }

  setContent(markdown: string): void {
    this.currentMarkdown = markdown;
    this.container.innerHTML = renderMarkdownWithLineTags(markdown);
  }

  getContent(): string {
    return this.currentMarkdown;
  }

  /** Scrolls to and highlights a specific 1-based line number. */
  scrollToLine(lineNo: number): void {
    this.clearHighlights();
    const el = this.container.querySelector<HTMLElement>(`[data-line="${lineNo}"]`);
    if (el) {
      el.classList.add('md-line--highlight');
      el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
  }

  /** Scrolls to and highlights the first line matching a text anchor
   * (e.g. a `[NEEDS CLARIFICATION: ...]` marker or a requirement ID). */
  scrollToAnchor(anchorText: string): void {
    this.clearHighlights();
    const lines = Array.from(this.container.querySelectorAll<HTMLElement>('.md-line'));
    const match = lines.find((el) => el.textContent?.includes(anchorText));
    if (match) {
      match.classList.add('md-line--highlight');
      match.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
  }

  clearHighlights(): void {
    this.container
      .querySelectorAll('.md-line--highlight')
      .forEach((el) => el.classList.remove('md-line--highlight'));
  }
}
