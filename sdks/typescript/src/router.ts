import { HttpTransport, type ClientOptions, type RequestOptions, TransportError } from "./transport.js";
import type { JsonValue } from "./types.js";
export type RouterObject = Record<string, JsonValue>;
export interface RouterModels { object: string; data: Array<{id: string; [field: string]: unknown}>; }
export interface RouterCompletion {
  id?: string;
  model?: string;
  choices: Array<{index?: number; message?: {role?: string; content?: string | null; [field: string]: unknown}; delta?: {content?: string | null; [field: string]: unknown}; finish_reason?: string | null; [field: string]: unknown}>;
  usage?: {prompt_tokens?: number; completion_tokens?: number; total_tokens?: number; [field: string]: unknown};
  [field: string]: unknown;
}
/** Forward-compatible OpenAI wire request, including tools, response_format and provider options. */
export type RouterChatRequest = RouterObject & {model: string; messages: JsonValue[]};
/** Inference client. baseUrl is the default or named router URL ending in /v1. */
export class HaarnessRouter extends HttpTransport {
  constructor(options: ClientOptions) {
    const url = new URL(options.baseUrl);
    if (url.username || url.password || url.search || url.hash) throw new TypeError("Router URL cannot contain credentials, query or fragment");
    if (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost","127.0.0.1","[::1]"].includes(url.hostname))) throw new TypeError("Router requires HTTPS or loopback HTTP");
    super({...options, token: options.token ?? process.env.HAARNESS_API_KEY ?? ""});
  }
  models(options: RequestOptions = {}): Promise<RouterModels> {return this.request("GET", "/models", options);}
  chatCompletions(body: RouterChatRequest, options: RequestOptions = {}): Promise<RouterCompletion> {return this.request("POST", "/chat/completions", {...options, body:{...body, stream:false}});}
  /** No automatic retries: repeating inference can spend credits twice. */
  async *streamChatCompletions(body: RouterChatRequest, options: RequestOptions = {}): AsyncGenerator<RouterCompletion> {
    const signal = AbortSignal.any([this.signal(options.signal), AbortSignal.timeout(options.timeoutMs ?? this.timeoutMs)]);
    const response = await this.fetcher(this.url("/chat/completions"), {method:"POST", headers:{...await this.headers(),...options.headers,"Content-Type":"application/json", Accept:"text/event-stream"}, body:JSON.stringify({...body,stream:true}), redirect:"error", signal});
    if (!response.ok) await this.failure(response);
    const reader=response.body?.getReader();
    if (!reader) throw new TransportError("Router returned no event stream");
    let buffer="", data:string[]=[], eventSize=0;const decoder=new TextDecoder();
    try {
      while (true) {
        const chunk=await reader.read();buffer+=decoder.decode(chunk.value,{stream:!chunk.done});
        if (buffer.length>8*1024*1024) throw new TransportError("Router event exceeds 8 MiB");
        let index:number;
        while ((index=buffer.indexOf("\n"))>=0) {
          const line=buffer.slice(0,index).replace(/\r$/,"");buffer=buffer.slice(index+1);
          eventSize += line.length;
          if (eventSize > 8*1024*1024) throw new TransportError("Router event exceeds 8 MiB");
          if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
          else if (!line && data.length) {
            const value=data.join("\n");data=[];eventSize=0;
            if (value==="[DONE]") return;
            let parsed:RouterCompletion;
            try {parsed=JSON.parse(value);} catch {throw new TransportError("Invalid router event JSON");}
            if (parsed.error) throw new TransportError("Router streaming error: "+JSON.stringify(parsed.error));
            yield parsed;
          }
        }
        if (chunk.done) throw new TransportError("Router stream ended before [DONE]");
      }
    } finally {await reader.cancel();reader.releaseLock();}
  }
}
