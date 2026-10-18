/**
 * The terminal sockets main holds for its views, by `<window>:<connection>:<pty>`.
 * Each view of a terminal names itself (`view`), so a slow connect or a late
 * detach of a view that was replaced (terminal tabs stepped through quickly,
 * the panel hidden and shown) never closes or takes the place of the socket
 * of the view now shown, which would leave that view without output or keys
 * and with nothing to tell it to reconnect.
 */
export function terminalSockets<Socket extends { close(): void }>() {
  const sockets = new Map<string, { owner: number; socket: Socket; view: string }>();
  /** The view that last asked to connect to each terminal. */
  const views = new Map<string, string>();
  return {
    get: (key: string) => sockets.get(key),
    /** A view starts connecting: its terminal's current socket goes. */
    begin(key: string, view: string) {
      views.set(key, view);
      const previous = sockets.get(key);
      sockets.delete(key);
      previous?.socket.close();
    },
    /** A connect finished: false, and the socket is closed, when a newer
     * view asked since or this one let go meanwhile. Two connects of one
     * view that overlap leave the later socket: the earlier one closes, so
     * the view never gets the output twice. */
    accept(key: string, view: string, owner: number, socket: Socket) {
      if (views.get(key) !== view) {
        socket.close();
        return false;
      }
      const previous = sockets.get(key);
      if (previous && previous.socket !== socket) previous.socket.close();
      sockets.set(key, { owner, socket, view });
      return true;
    },
    /** A view lets go: only its own socket closes, without a "closed" event. */
    detach(key: string, view: string) {
      if (views.get(key) === view) views.delete(key);
      const current = sockets.get(key);
      if (current?.view !== view) return;
      sockets.delete(key);
      current.socket.close();
    },
    /** A socket ended by itself: true when it was the view's current one,
     * which then has to be told so it reconnects. */
    ended(key: string, socket: Socket) {
      if (sockets.get(key)?.socket !== socket) return false;
      sockets.delete(key);
      return true;
    },
    /** A window closed: all its sockets go. */
    close(owner: number) {
      for (const [key, item] of sockets)
        if (item.owner === owner) {
          sockets.delete(key);
          item.socket.close();
        }
      for (const key of views.keys())
        if (key.startsWith(owner + ":")) views.delete(key);
    },
    /** Every socket held now. */
    all: () => [...sockets.values()].map((item) => item.socket),
    /** The app quits. */
    closeAll() {
      for (const item of sockets.values()) item.socket.close();
      sockets.clear();
      views.clear();
    },
  };
}
