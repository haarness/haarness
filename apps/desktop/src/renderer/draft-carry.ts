/**
 * One part of a draft taken from one new chat to another (its text, its
 * attachments, its context chips): `write` puts it at the destination,
 * `clear` takes it from where it was.
 */
export interface DraftPart {
  write: () => Promise<unknown>;
  clear: () => Promise<unknown>;
}

/**
 * Takes a draft along one write at a time, as sending does: every part is
 * written at the destination before any is cleared at the source, so a write
 * that fails stops the move with the draft still where it was (and perhaps
 * also where it was going), never lost from both.
 */
export async function carryDraft(parts: DraftPart[]) {
  for (const part of parts) await part.write();
  for (const part of parts) await part.clear();
}
