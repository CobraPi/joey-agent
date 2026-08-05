declare module 'split.js' {
  interface SplitOptions {
    sizes?: number[];
    minSize?: number | number[];
    gutterSize?: number;
    gutterAlign?: 'center' | 'start' | 'end';
    snapOffset?: number;
    dragInterval?: number;
    direction?: 'horizontal' | 'vertical';
    cursor?: string;
    elementStyle?: (dimension: number, elementSize: number, gutterSize: number) => Record<string, string>;
    gutterStyle?: (dimension: number, gutterSize: number) => Record<string, string>;
    onDragStart?: (sizes: number[]) => void;
    onDrag?: (sizes: number[]) => void;
    onDragEnd?: (sizes: number[]) => void;
  }

  interface SplitInstance {
    getSizes(): number[];
    setSizes(sizes: number[]): void;
    collapse(index: number): void;
    destroy(): void;
  }

  function Split(elements: (HTMLElement | string)[], options?: SplitOptions): SplitInstance;

  export default Split;
}

declare module 'diff' {
  export interface Change {
    count?: number;
    value: string;
    added?: boolean;
    removed?: boolean;
  }

  export function diffLines(oldStr: string, newStr: string, options?: any): Change[];
  export function diffWords(oldStr: string, newStr: string, options?: any): Change[];
  export function diffChars(oldStr: string, newStr: string, options?: any): Change[];
  export function createPatch(
    fileName: string,
    oldStr: string,
    newStr: string,
    oldHeader?: string,
    newHeader?: string,
    options?: any,
  ): string;

  export default diffLines;
}
