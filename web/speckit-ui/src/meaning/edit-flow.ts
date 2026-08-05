// Edit-flow controller (T048, FR-016).
//
// Every widget routes edits through this controller, which:
// 1. Compiles the edit to PatchOp(s).
// 2. POSTs to /api/features/{id}/patch (contracts/patch-engine.md).
// 3. On Applied → re-renders from the refreshed semantic stream.
// 4. On Conflict → surfaces the three-way merge card.
// 5. On AnchorUnresolved → degrades to read-only with a reopen prompt.
// 6. Offers undo from Applied as an explicit action.

import type { SpeckitApiClient, PatchOp } from '../api-client';

export interface PatchResponse {
  result: 'applied' | 'conflict' | 'anchor_unresolved' | 'validation_failed';
  new_revision_hash?: string;
  undo?: PatchOp[];
  conflicts?: Array<{
    node_fingerprint: string;
    base_bytes: string;
    current_bytes: string;
    proposed_bytes: string;
    resolution: string | null;
  }>;
  proposed_bytes?: string;
  diagnostics?: string[];
}

export type EditFlowHandler =
  | ((response: PatchResponse) => void)
  | ((response: PatchResponse) => Promise<void>);

/** Coordinates the edit flow for all meaning widgets. */
export class EditFlowController {
  private lastUndo: PatchOp[] | null = null;

  constructor(
    private api: SpeckitApiClient,
    private featureId: string,
  ) {}

  /** Submit a patch through the byte-anchor engine. */
  async applyPatch(artifact: string, ops: PatchOp[]): Promise<PatchResponse> {
    const res = await fetch(`${this.api.getBase_url()}/api/features/${this.featureId}/patch`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ artifact, ops }),
    });
    const response = (await res.json()) as PatchResponse;

    // Track undo for the explicit undo action (FR-014).
    if (response.result === 'applied' && response.undo) {
      this.lastUndo = response.undo;
    }

    return response;
  }

  /** Undo the last applied patch (FR-014 — undo is always safe to offer). */
  async undoLast(artifact: string): Promise<PatchResponse | null> {
    if (!this.lastUndo) return null;
    const undo = this.lastUndo;
    this.lastUndo = null;
    return this.applyPatch(artifact, undo);
  }

  /** Whether an undo is available. */
  get canUndo(): boolean {
    return this.lastUndo !== null;
  }

  /** Handle a patch response — routes to the appropriate UI surface. */
  async handleResponse(
    response: PatchResponse,
    callbacks: {
      onApplied?: () => void | Promise<void>;
      onConflict?: (conflicts: NonNullable<PatchResponse['conflicts']>) => void | Promise<void>;
      onAnchorUnresolved?: () => void | Promise<void>;
      onValidationFailed?: (diagnostics: string[]) => void | Promise<void>;
    },
  ): Promise<void> {
    switch (response.result) {
      case 'applied':
        await callbacks.onApplied?.();
        break;
      case 'conflict':
        if (response.conflicts) {
          await callbacks.onConflict?.(response.conflicts);
        }
        break;
      case 'anchor_unresolved':
        // Degrade to read-only with a reopen prompt (FR-016).
        await callbacks.onAnchorUnresolved?.();
        break;
      case 'validation_failed':
        if (response.diagnostics) {
          await callbacks.onValidationFailed?.(response.diagnostics);
        }
        break;
    }
  }
}
