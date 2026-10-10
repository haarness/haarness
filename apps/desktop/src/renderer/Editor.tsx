import {
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  drawSelection,
} from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import {
  defaultKeymap,
  history,
  historyField,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import {
  bracketMatching,
  foldGutter,
  syntaxHighlighting,
  HighlightStyle,
} from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { editorLanguage } from "./editor-language";
import { changedSpan } from "./diff";
/** Owned by an open document, so closing it releases history and scroll state. */
export interface EditorSession {
  state?: EditorState;
  scroll?: { top: number; left: number };
}
export function Editor(props: {
  value: string;
  session?: EditorSession;
  path: string;
  readonly?: boolean;
  wrap?: boolean;
  onChange?: (text: string) => void;
  onSave?: () => void;
  onSelection?: (selection: { from: number; to: number }) => void;
  onAddContext?: () => void;
}) {
  let host!: HTMLDivElement;
  let view: EditorView | undefined;
  const readonly = new Compartment(),
    wrap = new Compartment(),
    language = new Compartment();
  const [mounted, setMounted] = createSignal(false);
  let languageRequest = 0;
  const detectedLanguage = createMemo(() =>
    editorLanguage(props.path, props.value),
  );
  let updating = false;
  let activeSession: EditorSession | undefined;
  const remember = () => {
    if (!view || !activeSession) return;
    activeSession.state = view.state;
    activeSession.scroll = {
      top: view.scrollDOM.scrollTop,
      left: view.scrollDOM.scrollLeft,
    };
  };
  const restoreScroll = () => {
    const session = activeSession;
    const editor = view;
    editor?.requestMeasure({
      read: () => session?.scroll,
      write: (scroll) => {
        if (editor !== view || session !== activeSession) return;
        editor.scrollDOM.scrollTop = scroll?.top ?? 0;
        editor.scrollDOM.scrollLeft = scroll?.left ?? 0;
      },
    });
  };
  const makeState = () => EditorState.create({
    doc: activeSession?.state?.doc ?? props.value,
    selection: activeSession?.state?.selection,
    extensions: [
      lineNumbers(),
      history(),
      ...(activeSession?.state
        ? [historyField.init(() => activeSession!.state!.field(historyField))]
        : []),
      drawSelection(),
      highlightActiveLine(),
      foldGutter(),
      bracketMatching(),
      highlightSelectionMatches(),
      syntaxHighlighting(
        HighlightStyle.define([
          {
            tag: [tags.keyword, tags.modifier],
            color: "var(--syntax-keyword)",
          },
          {
            tag: [tags.string, tags.regexp],
            color: "var(--syntax-string)",
          },
          {
            tag: [tags.number, tags.bool, tags.null],
            color: "var(--syntax-number)",
          },
          {
            tag: [
              tags.propertyName,
              tags.typeName,
              tags.tagName,
              tags.attributeName,
              tags.definition(tags.variableName),
              tags.function(tags.variableName),
            ],
            color: "var(--syntax-property)",
          },
          { tag: [tags.comment, tags.meta], color: "var(--muted)" },
          {
            tag: tags.heading,
            color: "var(--primary)",
            fontWeight: "600",
          },
          {
            tag: tags.link,
            color: "var(--primary)",
            textDecoration: "underline",
          },
        ]),
      ),
      language.of([]),
      readonly.of(EditorState.readOnly.of(!!props.readonly)),
      wrap.of(props.wrap ? EditorView.lineWrapping : []),
      keymap.of([
        {
          key: "Mod-Shift-l",
          run: () => {
            if (!props.onAddContext) return false;
            props.onAddContext();
            return true;
          },
        },
        {
          key: "Mod-s",
          run: () => {
            props.onSave?.();
            return true;
          },
        },
        indentWithTab,
        ...defaultKeymap,
        ...historyKeymap,
        ...searchKeymap,
      ]),
      EditorView.theme({
        "&": {
          height: "100%",
          backgroundColor: "transparent",
          color: "var(--text)",
        },
        ".cm-scroller": { fontFamily: "var(--mono)", fontSize: "13px" },
        ".cm-gutters": {
          backgroundColor: "var(--page)",
          color: "var(--muted)",
          border: "none",
        },
        ".cm-activeLineGutter": { backgroundColor: "var(--surface)" },
        ".cm-activeLine": { backgroundColor: "var(--hover)" },
        ".cm-content": { caretColor: "var(--primary)" },
        "&.cm-focused": { outline: "none" },
        ".cm-cursor": { borderLeftColor: "var(--primary)" },
        ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
          backgroundColor: "var(--selected)",
        },
      }),
      EditorView.updateListener.of((update) => {
        if (update.selectionSet || update.docChanged)
          props.onSelection?.({
            from: update.state.selection.main.from,
            to: update.state.selection.main.to,
          });
        if (update.docChanged && !updating)
          props.onChange?.(update.state.doc.toString());
      }),
    ],
  });
  onMount(() => {
    activeSession = props.session;
    view = new EditorView({ parent: host, state: makeState() });
    setMounted(true);
    restoreScroll();
    const selection = view.state.selection.main;
    props.onSelection?.({ from: selection.from, to: selection.to });
  });
  // Reconcile each document in one effect: Solid does not guarantee ordering
  // between independent effects. Never apply the next file's controlled value
  // before remembering the outgoing document's history and selection.
  let configuredLanguage: ReturnType<typeof editorLanguage> | undefined;
  let configuredReadonly: boolean | undefined;
  let configuredWrap: boolean | undefined;
  createEffect(() => {
    if (!mounted()) return;
    const session = props.session;
    const value = props.value;
    const description = detectedLanguage();
    const readOnly = !!props.readonly;
    const wrapping = !!props.wrap;
    const editor = view!;
    const switched = session !== activeSession;
    if (switched) {
      remember();
      activeSession = session;
      editor.setState(untrack(makeState));
      restoreScroll();
      const selection = editor.state.selection.main;
      props.onSelection?.({ from: selection.from, to: selection.to });
    }
    if (value !== editor.state.doc.toString()) {
      updating = true;
      try {
        // Only the span that changed, so a file updated on disk elsewhere
        // keeps the cursor and scroll position.
        editor.dispatch({
          changes: changedSpan(editor.state.doc.toString(), value),
        });
      } finally {
        updating = false;
      }
    }
    if (
      switched ||
      readOnly !== configuredReadonly ||
      wrapping !== configuredWrap
    ) {
      configuredReadonly = readOnly;
      configuredWrap = wrapping;
      editor.dispatch({
        effects: [
          readonly.reconfigure(EditorState.readOnly.of(readOnly)),
          wrap.reconfigure(wrapping ? EditorView.lineWrapping : []),
        ],
      });
    }
    if (switched || description !== configuredLanguage) {
      configuredLanguage = description;
      const request = ++languageRequest;
      // Reconfiguration preserves text, selection and undo history. Ignore a
      // slow parser import after changing documents or closing the editor.
      editor.dispatch({ effects: language.reconfigure([]) });
      if (description)
        void description.load()
          .then((support) => {
            if (request === languageRequest && editor === view)
              editor.dispatch({ effects: language.reconfigure(support) });
          })
          .catch((error) => console.error("File highlighting could not load", error));
    }
  });
  onCleanup(() => {
    languageRequest++;
    remember();
    view?.destroy();
    view = undefined;
  });
  return <div class="editor" ref={host} />;
}
