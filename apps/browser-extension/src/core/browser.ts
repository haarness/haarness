/**
 * The browser tool wire types and codecs, from the shared driver
 * (`packages/browser-driver/src/rpc.ts`).
 *
 * The driver's schemas belong to the driver's own `effect`: this package only
 * ever calls the driver's codecs (`decodeHostCommand`, `encodeHostOutcome`),
 * never `Schema.*` on a driver schema (see tests/driver-boundary.test.ts).
 */
import * as Browser from "@haarness/browser-driver/rpc";

export type TabID = Browser.TabID;
export type FileID = Browser.FileID;
export type BrowserName = Browser.BrowserName;
export type Tab = Browser.Tab;
export type State = Browser.State;
export type Target = Browser.Target;
export type Method = Browser.Method;
export type Action = Browser.Action;
export type File = Browser.File;
export type FileRef = Browser.FileRef;
export type HostFile = Browser.HostFile;
export type HostCommand = Browser.HostCommand;
export type HostResult = Browser.HostResult;
/** A command's outcome as the extension returns it: downloads may be `FileRef`s the bridge reads. */
export type Outcome = Browser.HostOutcome;
/** The site rules a command carries (browsing, downloads and uploads). */
export type Policy = NonNullable<HostCommand["policy"]>;
export type Access = Policy["defaults"];
export type Decision = Access["browsing"];

/** The server refuses longer URLs in a published state. */
export const MAX_URL = 16_384;
export const MAX_TITLE = 2_048;
/** Tabs one chat may hold in one browser, as on Desktop. */
export const MAX_TABS = 32;
/** Chats one browser may publish to one server. */
export const MAX_THREADS = 256;

/** Every browser tool operation (`browser_<name>` with dots as underscores). */
export const methods: readonly Method[] = Browser.Operations.map((operation) => operation.name);

/** A new tab or file id in the wire format. */
export const tabID = (uuid: string) => `tab_${uuid}` as TabID;
export const fileID = (uuid: string) => `file_${uuid}` as FileID;

/** Bridge → extension: one command, validated; throws on a malformed one. */
export const decodeHostCommand: (input: unknown) => HostCommand = Browser.decodeHostCommand;
/** Extension → bridge: the outcome as JSON (capture bytes as base64). */
export const encodeHostOutcome: (outcome: Outcome) => unknown = Browser.encodeHostOutcome;
