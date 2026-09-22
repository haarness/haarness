import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";

/** Detect common config filenames as well as the catalog's file extensions. */
export function editorLanguage(path: string, content = "") {
  const filename = path.replaceAll("\\", "/").split("/").pop() ?? path;
  const lower = filename.toLowerCase();
  let name: string | undefined;
  if (/^(dockerfile|containerfile)(\..+)?$/.test(lower)) name = "Dockerfile";
  else if (
    /^\.(bashrc|bash_profile|bash_login|bash_logout|zshrc|zshenv|zprofile|zlogin|profile)$/.test(
      lower,
    ) ||
    /\.zsh$/.test(lower) ||
    /^\.env(?:\..+)?$/.test(lower)
  )
    name = "Shell";
  else if (/^jenkinsfile(?:\..+)?$/.test(lower)) name = "Groovy";
  else if (/\.(jsonc|jsonl)$/.test(lower)) name = "JSON";
  const detected = name
    ? LanguageDescription.matchLanguageName(languages, name, false)
    : (LanguageDescription.matchFilename(languages, filename) ??
      LanguageDescription.matchFilename(languages, lower));
  if (detected) return detected;
  // Extensionless executable scripts often identify their language in a shebang.
  const interpreter = content
    .split("\n", 1)[0]
    .match(/^#!.*\b(bash|sh|zsh|ksh|python[\d.]*|node|ruby|perl)\b/);
  if (!interpreter) return null;
  const command = interpreter[1];
  return LanguageDescription.matchLanguageName(
    languages,
    command.startsWith("python")
      ? "Python"
      : command === "node"
        ? "JavaScript"
        : ["bash", "sh", "zsh", "ksh"].includes(command)
          ? "Shell"
          : command,
    false,
  );
}
