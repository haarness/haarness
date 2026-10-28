/** The bodies Desktop sends for a message and a slash command. A person is
 * at the window and can stop what they start, so each says `attended`: a
 * command's or skill's `!` snippet in that turn then runs until it ends
 * instead of stopping at the shell deadline. */

type Mentioned = { resources?: unknown[] };

/** `startTurn` and `queueInput` take the same body. */
export function messageBody(message: {
  delivery: string;
  text: string;
  attachments: string[];
  mentioned: Mentioned;
  requestId: string;
  effort?: string;
}) {
  return {
    delivery: message.delivery,
    preview: true,
    tools: { preview: true },
    text: message.text,
    attachments: message.attachments,
    ...message.mentioned,
    requestId: message.requestId,
    ...(message.effort ? { reasoningEffort: message.effort } : {}),
    attended: true,
  };
}

export function commandBody(command: {
  name: string;
  arguments: string;
  mentioned: Mentioned;
  requestId: string;
}) {
  return {
    name: command.name,
    tools: { preview: true },
    arguments: command.arguments,
    ...command.mentioned,
    requestId: command.requestId,
    attended: true,
  };
}
