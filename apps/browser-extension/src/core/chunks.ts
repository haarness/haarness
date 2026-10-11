/**
 * Chrome kills a native port on any host → extension message over 1 MB, so the
 * bridge splits large messages into consecutive `chunk` frames, each carrying
 * base64 of at most 700 000 bytes of the message's UTF-8 JSON. Pieces are
 * decoded to bytes and joined before one TextDecoder pass, because a cut can
 * fall inside a multibyte character.
 */

export interface Chunk {
  type: "chunk";
  id: string;
  index: number;
  count: number;
  data: string;
}

/** The bridge never sends more than this in one reassembled message. */
const MAX_BYTES = 64 * 1024 * 1024;

export function isChunk(message: unknown): message is Chunk {
  const value = message as Partial<Chunk> | null;
  return (
    typeof value === "object" &&
    value !== null &&
    value.type === "chunk" &&
    typeof value.id === "string" &&
    Number.isSafeInteger(value.index) &&
    Number.isSafeInteger(value.count) &&
    typeof value.data === "string"
  );
}

export function base64Bytes(data: string): Uint8Array {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

export function createReassembler() {
  // The bridge writes one message's chunks contiguously, so one slot is enough;
  // a new id before the last one finished means that one was abandoned.
  let pending: { id: string; count: number; parts: Uint8Array[]; bytes: number } | undefined;
  return {
    /**
     * Feeds one chunk. Returns the parsed message when it was the last piece,
     * `undefined` while more are expected, and throws on a malformed sequence
     * (the caller drops the message; the bridge does not resend).
     */
    push(chunk: Chunk): unknown {
      if (chunk.count < 1 || chunk.count > 1_024 || chunk.index < 0 || chunk.index >= chunk.count)
        throw new Error("Malformed chunk");
      if (chunk.index === 0) pending = { id: chunk.id, count: chunk.count, parts: [], bytes: 0 };
      if (!pending || pending.id !== chunk.id || pending.count !== chunk.count || pending.parts.length !== chunk.index) {
        pending = undefined;
        throw new Error("Chunk out of sequence");
      }
      const bytes = base64Bytes(chunk.data);
      pending.bytes += bytes.length;
      if (pending.bytes > MAX_BYTES) {
        pending = undefined;
        throw new Error("Chunked message is too large");
      }
      pending.parts.push(bytes);
      if (pending.parts.length < pending.count) return undefined;
      const whole = new Uint8Array(pending.bytes);
      let offset = 0;
      for (const part of pending.parts) {
        whole.set(part, offset);
        offset += part.length;
      }
      pending = undefined;
      return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(whole));
    },
    reset() {
      pending = undefined;
    },
  };
}
