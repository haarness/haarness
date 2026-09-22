import type { AgentProfile } from "@haarness/sdk";

/** Internal and delegation-only agents remain in the catalog for management. */
export function conversationAgents(agents: AgentProfile[]) {
  return agents.filter((agent) => !agent.hidden && agent.mode !== "subagent");
}

export function selectConversationAgent(
  agents: AgentProfile[],
  requested?: string,
) {
  const available = conversationAgents(agents);
  return (
    available.find((agent) => agent.id === requested) ??
    available.find((agent) => agent.id === "build") ??
    available[0]
  )?.id;
}
