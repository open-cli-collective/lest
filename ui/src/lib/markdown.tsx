// A small Markdown subset for flow descriptions: paragraphs, bullet and
// numbered lists, inline code, bold, italic and links. Everything else is
// rendered as text (React escapes it).

import { Fragment, type ReactNode } from "react";

export type Block =
  | { type: "p"; text: string }
  | { type: "ul" | "ol"; items: string[] };

export function parseBlocks(src: string): Block[] {
  const blocks: Block[] = [];
  let para: string[] = [];
  let list: { type: "ul" | "ol"; items: string[] } | null = null;
  const flushPara = () => {
    if (para.length) blocks.push({ type: "p", text: para.join(" ") });
    para = [];
  };
  const flushList = () => {
    if (list) blocks.push(list);
    list = null;
  };
  for (const raw of src.replace(/\r\n/g, "\n").split("\n")) {
    const line = raw.trimEnd();
    const ul = /^\s*[-*+]\s+(.*)$/.exec(line);
    const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (ul || ol) {
      flushPara();
      const type = ul ? "ul" : "ol";
      if (!list || list.type !== type) {
        flushList();
        list = { type, items: [] };
      }
      list.items.push(((ul ?? ol) as RegExpExecArray)[1] ?? "");
    } else if (!line.trim()) {
      flushPara();
      flushList();
    } else if (list && /^\s+\S/.test(line)) {
      list.items[list.items.length - 1] += ` ${line.trim()}`;
    } else {
      flushList();
      para.push(line.trim());
    }
  }
  flushPara();
  flushList();
  return blocks;
}

export type Inline =
  | { type: "text"; text: string }
  | { type: "code"; text: string }
  | { type: "strong" | "em"; children: Inline[] }
  | { type: "link"; href: string; children: Inline[] };

// Absolute web and mail links, same-site paths and anchors; never a
// protocol-relative `//host` link.
const SAFE_HREF = /^(https?:|mailto:|\/(?!\/)|#)/i;

export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const pushText = () => {
    if (text) out.push({ type: "text", text });
    text = "";
  };
  let i = 0;
  while (i < src.length) {
    const rest = src.slice(i);
    let m: RegExpExecArray | null;
    if ((m = /^`([^`]+)`/.exec(rest))) {
      pushText();
      out.push({ type: "code", text: m[1] ?? "" });
      i += m[0].length;
    } else if ((m = /^\*\*([^*]+)\*\*/.exec(rest)) || (m = /^__([^_]+)__/.exec(rest))) {
      pushText();
      out.push({ type: "strong", children: parseInline(m[1] ?? "") });
      i += m[0].length;
    } else if ((m = /^\*([^*\s][^*]*)\*/.exec(rest))) {
      pushText();
      out.push({ type: "em", children: parseInline(m[1] ?? "") });
      i += m[0].length;
    } else if ((m = /^\[([^\]]+)\]\(([^)\s]+)\)/.exec(rest)) && SAFE_HREF.test(m[2] ?? "")) {
      pushText();
      out.push({ type: "link", href: m[2] ?? "", children: parseInline(m[1] ?? "") });
      i += m[0].length;
    } else {
      text += src[i];
      i++;
    }
  }
  pushText();
  return out;
}

function renderInline(nodes: Inline[]): ReactNode {
  return nodes.map((n, i) => {
    switch (n.type) {
      case "text":
        return <Fragment key={i}>{n.text}</Fragment>;
      case "code":
        return <code key={i}>{n.text}</code>;
      case "strong":
        return <strong key={i}>{renderInline(n.children)}</strong>;
      case "em":
        return <em key={i}>{renderInline(n.children)}</em>;
      case "link":
        return (
          <a key={i} href={n.href} target="_blank" rel="noreferrer noopener">
            {renderInline(n.children)}
          </a>
        );
    }
  });
}

export function Markdown({ text, className }: { text: string; className?: string }) {
  return (
    <div className={className ? `md ${className}` : "md"}>
      {parseBlocks(text).map((b, i) =>
        b.type === "p" ? (
          <p key={i}>{renderInline(parseInline(b.text))}</p>
        ) : b.type === "ul" ? (
          <ul key={i}>
            {b.items.map((it, j) => (
              <li key={j}>{renderInline(parseInline(it))}</li>
            ))}
          </ul>
        ) : (
          <ol key={i}>
            {b.items.map((it, j) => (
              <li key={j}>{renderInline(parseInline(it))}</li>
            ))}
          </ol>
        ),
      )}
    </div>
  );
}

/** Plain text of the first paragraph, with inline markup removed. */
export function plainFirstLine(src: string | null | undefined): string {
  if (!src) return "";
  const first = parseBlocks(src)[0];
  if (!first) return "";
  const text = first.type === "p" ? first.text : (first.items[0] ?? "");
  const flat = (nodes: Inline[]): string =>
    nodes.map((n) => (n.type === "text" || n.type === "code" ? n.text : flat(n.children))).join("");
  return flat(parseInline(text));
}
