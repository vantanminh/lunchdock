import { Fragment, type ReactNode } from "react";

type Block =
  | { kind: "heading"; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "paragraph"; text: string };

const INLINE = /(\*\*[^*]+\*\*|`[^`]+`|\[[^\]]+\]\(https?:\/\/[^)\s]+\))/g;
const LINK = /^\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)$/;

/** Turn the Markdown of a release into headings, bullet lists and paragraphs. */
export function parseNotes(notes: string): Block[] {
  const blocks: Block[] = [];
  // Whether the last block is a paragraph that the next line continues.
  let open = false;
  for (const raw of notes.replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.trim();
    const last = blocks[blocks.length - 1];
    const heading = /^#{1,6}\s+(.*)$/.exec(line)?.[1];
    const bullet = /^[-*+]\s+(.*)$/.exec(line)?.[1];
    if (!line) {
      open = false;
    } else if (heading !== undefined) {
      blocks.push({ kind: "heading", text: heading });
      open = false;
    } else if (bullet !== undefined) {
      if (last?.kind === "list") {
        last.items.push(bullet);
      } else {
        blocks.push({ kind: "list", items: [bullet] });
      }
      open = false;
    } else if (last?.kind === "list" && /^\s/.test(raw)) {
      // A wrapped bullet continues the previous item.
      last.items[last.items.length - 1] += ` ${line}`;
    } else if (open && last?.kind === "paragraph") {
      last.text += ` ${line}`;
    } else {
      blocks.push({ kind: "paragraph", text: line });
      open = true;
    }
  }
  return blocks;
}

function inline(text: string): ReactNode[] {
  return text.split(INLINE).map((part, index) => {
    if (part.startsWith("**") && part.endsWith("**") && part.length > 4) {
      return <strong key={index}>{part.slice(2, -2)}</strong>;
    }
    if (part.startsWith("`") && part.endsWith("`") && part.length > 2) {
      return (
        <code key={index} className="rounded bg-sunken px-1 font-mono text-[12px]">
          {part.slice(1, -1)}
        </code>
      );
    }
    const link = LINK.exec(part);
    if (link) {
      return (
        <a key={index} className="underline" href={link[2]} target="_blank" rel="noreferrer">
          {link[1]}
        </a>
      );
    }
    return <Fragment key={index}>{part}</Fragment>;
  });
}

/** A release's notes as written in CHANGELOG.md: bold lead-ins, bullets, headings. */
export function ReleaseNotes({ notes }: { notes: string }) {
  return (
    <div className="space-y-2 text-sm text-ink-soft">
      {parseNotes(notes).map((block, index) => {
        if (block.kind === "heading") {
          return (
            <h4 key={index} className="pt-1 text-xs font-semibold uppercase tracking-wide text-muted">
              {block.text}
            </h4>
          );
        }
        if (block.kind === "list") {
          return (
            <ul key={index} className="list-disc space-y-1.5 pl-5">
              {block.items.map((item, itemIndex) => (
                <li key={itemIndex}>{inline(item)}</li>
              ))}
            </ul>
          );
        }
        return <p key={index}>{inline(block.text)}</p>;
      })}
    </div>
  );
}
