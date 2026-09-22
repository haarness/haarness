import { test } from "node:test";
import assert from "node:assert/strict";
import { classHighlighter, highlightTree } from "@lezer/highlight";
import { editorLanguage } from "../src/renderer/editor-language";

test("file types and extensionless configuration names select their languages", () => {
  const cases: [string, string][] = [
    ["deploy/build.yml", "YAML"],
    ["build.YAML", "YAML"],
    ["Cargo.toml", "TOML"],
    ["env.sh", "Shell"],
    ["env.zsh", "Shell"],
    ["/home/user/.zshrc", "Shell"],
    [".bash_profile", "Shell"],
    [".env.local", "Shell"],
    ["Dockerfile", "Dockerfile"],
    ["docker/Dockerfile.release", "Dockerfile"],
    ["Containerfile", "Dockerfile"],
    ["Jenkinsfile", "Groovy"],
    ["settings.jsonc", "JSON"],
    ["C:\\project\\app.tsx", "TSX"],
    ["module.mts", "TypeScript"],
    ["main.go", "Go"],
    ["main.java", "Java"],
    ["main.cpp", "C++"],
    ["main.cs", "C#"],
    ["query.sql", "SQL"],
    ["app.rb", "Ruby"],
    ["app.swift", "Swift"],
    ["app.kt", "Kotlin"],
    ["view.vue", "Vue"],
    ["app.php", "PHP"],
    ["document.xml", "XML"],
  ];
  for (const [path, expected] of cases)
    assert.equal(editorLanguage(path)?.name, expected, path);
  assert.equal(
    editorLanguage("script", "#!/usr/bin/env python3\nprint(1)")?.name,
    "Python",
  );
  assert.equal(
    editorLanguage("script", "#!/bin/bash\necho hello")?.name,
    "Shell",
  );
  assert.equal(editorLanguage("notes.txt", "ordinary prose"), null);
});

test("additional parsers produce syntax tokens, not plain text", async () => {
  for (const [path, text] of [
    ["build.yml", '---\nhosts: "localhost"\nretries: 3\n'],
    ["Cargo.toml", '[package]\nname = "demo"\nversion = 1\n'],
    ["env.sh", 'if true; then echo "hello"; fi\n'],
    ["Dockerfile", 'FROM alpine:3\nRUN echo "hello"\n'],
    ["main.go", 'package main\nfunc main() { println("hello") }'],
    ["main.java", "public class Main { int count = 3; }"],
    ["query.sql", "SELECT name FROM users WHERE id = 3;"],
    ["main.swift", 'let message = "hello"'],
    ["main.kt", 'val message = "hello"'],
  ]) {
    const support = await editorLanguage(path)!.load();
    const tokens: string[] = [];
    highlightTree(
      support.language.parser.parse(text),
      classHighlighter,
      (from, to) => tokens.push(text.slice(from, to)),
    );
    assert.ok(tokens.length >= 2, `${path} has no syntax tokens`);
  }
});
