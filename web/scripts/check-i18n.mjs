#!/usr/bin/env node
// Checks the Vietnamese dictionary against the source.
//
//   node scripts/check-i18n.mjs                 fail when a t("…")/msg("…") key has no translation
//   node scripts/check-i18n.mjs --hardcoded     also list user-facing text that bypasses t()
//   node scripts/check-i18n.mjs --unused        also list dictionary entries no t() call uses
//                                               (status and role words are looked up dynamically)
//
// English text is the dictionary key, so a missing entry falls back to English at runtime
// instead of breaking; this script keeps that from happening unnoticed.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = fileURLToPath(new URL("../src", import.meta.url));
const showHardcoded = process.argv.includes("--hardcoded");
const showUnused = process.argv.includes("--unused");

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      yield* files(path);
    } else if (/\.tsx?$/.test(name) && !name.endsWith(".d.ts") && name !== "api-types.ts") {
      yield path;
    }
  }
}

async function loadDictionary() {
  const source = readFileSync(join(root, "i18n/vi.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  const module = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
  return module.vi;
}

const placeholders = (text) => [...text.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort().join(",");
const literal = (node) =>
  node && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) ? node.text : null;

const dictionary = await loadDictionary();
const used = new Map();
const dynamic = [];
const hardcoded = [];

const TEXT_ATTRIBUTES = new Set(["title", "placeholder", "aria-label", "label", "description", "alt", "hint"]);
const TOAST_METHODS = new Set(["success", "error", "message", "info", "warning"]);

for (const path of files(root)) {
  if (path.endsWith("i18n/vi.ts")) continue;
  const text = readFileSync(path, "utf8");
  const sf = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, path.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  const where = (node) => `${relative(root, path)}:${sf.getLineAndCharacterOfPosition(node.getStart()).line + 1}`;
  const remember = (key, node) => {
    if (!used.has(key)) used.set(key, where(node));
  };
  const flag = (value, node) => {
    const trimmed = value.replace(/\s+/g, " ").trim();
    if (/[A-Za-z]{2}/.test(trimmed)) hardcoded.push(`${where(node)}  ${trimmed.slice(0, 90)}`);
  };

  const visit = (node) => {
    if (ts.isCallExpression(node)) {
      const callee = node.expression;
      const name = ts.isIdentifier(callee) ? callee.text : null;
      if (name === "t" || name === "msg") {
        const key = literal(node.arguments[0]);
        if (key === null) {
          dynamic.push(where(node));
        } else {
          remember(key, node);
        }
      } else if (name === "translate") {
        const key = literal(node.arguments[1]);
        if (key !== null) remember(key, node);
      } else if (
        ts.isPropertyAccessExpression(callee) &&
        ((ts.isIdentifier(callee.expression) && callee.expression.text === "toast" && TOAST_METHODS.has(callee.name.text)) ||
          (ts.isIdentifier(callee.expression) && callee.expression.text === "window" && callee.name.text === "confirm"))
      ) {
        const value = literal(node.arguments[0]);
        if (value !== null) flag(value, node);
        else if (node.arguments[0] && ts.isTemplateExpression(node.arguments[0])) flag(node.arguments[0].getText(), node);
      }
    }
    if (ts.isJsxText(node)) flag(node.text, node);
    if (ts.isJsxAttribute(node) && TEXT_ATTRIBUTES.has(node.name.text) && node.initializer && ts.isStringLiteral(node.initializer)) {
      flag(node.initializer.text, node);
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
}

let failed = false;
for (const [key, location] of used) {
  const translation = dictionary[key];
  if (translation === undefined) {
    console.error(`missing  ${location}  ${JSON.stringify(key)}`);
    failed = true;
  } else if (placeholders(translation) !== placeholders(key)) {
    console.error(`placeholders differ  ${location}  ${JSON.stringify(key)}`);
    failed = true;
  }
}
for (const key of Object.keys(dictionary)) {
  if (showUnused && !used.has(key)) console.warn(`unused   ${JSON.stringify(key)}`);
}
if (showHardcoded) {
  console.log(`\n${hardcoded.length} hard-coded strings outside t():`);
  for (const line of hardcoded) console.log("  " + line);
}
console.log(
  `${used.size} keys used, ${Object.keys(dictionary).length} translated, ${dynamic.length} dynamic t() calls`,
);
process.exit(failed ? 1 : 0);
