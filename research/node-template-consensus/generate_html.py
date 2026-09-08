#!/usr/bin/env python3
"""Render bip-node-template-attestation.md to a shareable index.html."""

from __future__ import annotations

import html
import pathlib
import re
import sys

import markdown

ROOT = pathlib.Path(__file__).resolve().parent
MD_PATH = ROOT / "bip-node-template-attestation.md"
HTML_PATH = ROOT / "index.html"

TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Node Template Attestation — soft-fork draft</title>
<meta name="description" content="Draft Bitcoin soft fork: a block may pay a coinbase address only if that same block carries a grind proving a node-template for that script, and it must include the short list of transactions that template named whenever they still fit and still spend.">
<link rel="canonical" href="https://github.com/AwokenLazarus/Bitcoin/blob/nta-soft-fork/research/node-template-consensus/bip-node-template-attestation.md">
<style>
  :root {
    --bg: #f6f1e8;
    --bg-2: #efe6d6;
    --ink: #1c1915;
    --muted: #5c564c;
    --rule: #c9bba3;
    --accent: #6b2d12;
    --code-bg: #1c1915;
    --code-ink: #f6f1e8;
    --link: #6b2d12;
    --card: #fffaf2;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #161410;
      --bg-2: #1e1a15;
      --ink: #f3eadc;
      --muted: #b7ad9c;
      --rule: #3d372e;
      --accent: #e0a070;
      --code-bg: #0e0c0a;
      --code-ink: #f3eadc;
      --link: #e0a070;
      --card: #1e1a15;
    }
  }
  * { box-sizing: border-box; }
  html { scroll-behavior: smooth; }
  body {
    margin: 0;
    background: var(--bg);
    color: var(--ink);
    font: 18px/1.55 "Iowan Old Style", "Palatino Linotype", Palatino, "Book Antiqua", Georgia, serif;
  }
  .wrap { max-width: 46rem; margin: 0 auto; padding: 2.2rem 1.25rem 4rem; }
  header.hero {
    border-bottom: 1px solid var(--rule);
    padding-bottom: 1.4rem;
    margin-bottom: 1.6rem;
  }
  p.kicker {
    font: 600 0.78rem/1.3 ui-sans-serif, system-ui, sans-serif;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--accent);
    margin: 0 0 0.6rem;
  }
  h1 { font-size: 2.1rem; line-height: 1.2; margin: 0 0 0.6rem; font-weight: 650; }
  .lede { font-size: 1.12rem; color: var(--muted); margin: 0; }
  .meta {
    display: grid;
    grid-template-columns: 9rem 1fr;
    gap: 0.2rem 0.8rem;
    background: var(--card);
    border: 1px solid var(--rule);
    padding: 0.9rem 1rem;
    margin: 1.3rem 0 1.6rem;
    font: 0.88rem/1.45 ui-sans-serif, system-ui, sans-serif;
  }
  .meta dt { color: var(--muted); }
  .meta dd { margin: 0; }
  nav.toc {
    background: var(--bg-2);
    border: 1px solid var(--rule);
    padding: 0.9rem 1rem 1rem;
    margin: 0 0 2rem;
    font: 0.92rem/1.4 ui-sans-serif, system-ui, sans-serif;
  }
  nav.toc .toctitle { font-weight: 650; margin-bottom: 0.4rem; }
  nav.toc ul { margin: 0; padding-left: 1.1rem; }
  nav.toc a { text-decoration: none; }
  h2, h3, h4 {
    font-family: ui-sans-serif, system-ui, sans-serif;
    line-height: 1.25;
    margin: 2rem 0 0.7rem;
  }
  h2 { font-size: 1.35rem; padding-top: 0.4rem; border-top: 1px solid var(--rule); }
  h3 { font-size: 1.12rem; }
  a { color: var(--link); }
  a.headerlink { color: var(--rule); text-decoration: none; margin-left: 0.25rem; font-weight: 400; }
  a.headerlink:hover { color: var(--accent); }
  p, li { hyphens: auto; }
  blockquote {
    margin: 1rem 0;
    padding: 0.15rem 0 0.15rem 1rem;
    border-left: 3px solid var(--accent);
    color: var(--muted);
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font: 0.88rem/1.4 ui-sans-serif, system-ui, sans-serif;
    margin: 1rem 0 1.4rem;
  }
  th, td {
    border: 1px solid var(--rule);
    padding: 0.4rem 0.55rem;
    text-align: left;
    vertical-align: top;
  }
  th { background: var(--bg-2); }
  pre, code { font-family: ui-monospace, "Cascadia Code", "SF Mono", Menlo, monospace; }
  code { font-size: 0.86em; }
  pre {
    background: var(--code-bg);
    color: var(--code-ink);
    padding: 0.9rem 1rem;
    overflow: auto;
    font-size: 0.78rem;
    line-height: 1.45;
  }
  pre code { font-size: inherit; }
  .mermaid {
    background: var(--card);
    border: 1px solid var(--rule);
    padding: 0.8rem;
    margin: 1rem 0 1.4rem;
    overflow-x: auto;
  }
  footer {
    margin-top: 3rem;
    padding-top: 1rem;
    border-top: 1px solid var(--rule);
    color: var(--muted);
    font: 0.88rem/1.45 ui-sans-serif, system-ui, sans-serif;
  }
  @media print {
    body { background: #fff; color: #000; }
    nav.toc { display: none; }
    a { color: inherit; text-decoration: none; }
  }
</style>
</head>
<body>
<div class="wrap">
<header class="hero">
  <p class="kicker">Draft · consensus (soft fork) · no BIP number</p>
  <h1>Node Template Attestation</h1>
  <p class="lede">A block may pay a coinbase address only if that same block carries a grind proving a node-template for that script, and it must include the short list of transactions that template named whenever they still fit and still spend.</p>
</header>
__BODY__
<footer>
  <p>Canonical markdown:
    <a href="https://github.com/AwokenLazarus/Bitcoin/blob/nta-soft-fork/research/node-template-consensus/bip-node-template-attestation.md">bip-node-template-attestation.md</a>
    · Reference model:
    <a href="https://github.com/AwokenLazarus/Bitcoin/tree/nta-soft-fork/research/node-template-consensus">node-template-consensus/</a>
  </p>
  <p>Not a Bitcoin Core or Knots patch. No activation bit. BSD-2-Clause.</p>
</footer>
</div>
<script type="module">
  import mermaid from "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs";
  mermaid.initialize({ startOnLoad: true, theme: window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "neutral" });
</script>
</body>
</html>
"""


def split_preamble(text: str) -> tuple[dict[str, str], str]:
    if not text.startswith("```"):
        return {}, text
    end = text.find("```", 3)
    if end < 0:
        return {}, text
    raw = text[3:end].strip()
    body = text[end + 3 :].lstrip()
    meta: dict[str, str] = {}
    for line in raw.splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip()
    return meta, body


def mermaid_placeholders(text: str) -> tuple[str, list[str]]:
    found: list[str] = []

    def repl(m: re.Match[str]) -> str:
        found.append(m.group(1).strip())
        return f"\n\nNTAMERMAID{len(found) - 1}NTA\n\n"

    out = re.sub(r"```mermaid\n(.*?)```", repl, text, flags=re.S)
    return out, found


def meta_dl(meta: dict[str, str]) -> str:
    order = [
        "BIP",
        "Title",
        "Author",
        "Status",
        "Type",
        "Layer",
        "Created",
        "License",
    ]
    rows = []
    for k in order:
        if k in meta:
            rows.append(f"<dt>{k}</dt><dd>{meta[k]}</dd>")
    return "<dl class='meta'>" + "".join(rows) + "</dl>" if rows else ""


def main() -> int:
    src = MD_PATH.read_text(encoding="utf-8")
    meta, body = split_preamble(src)
    body, diagrams = mermaid_placeholders(body)
    conv = markdown.Markdown(
        extensions=["extra", "toc", "sane_lists", "smarty"],
        extension_configs={"toc": {"permalink": True, "toc_depth": "2-3"}},
    )
    html_body = conv.convert(body)
    for i, src_diag in enumerate(diagrams):
        token = f"NTAMERMAID{i}NTA"
        figure = f'<div class="mermaid">{html.escape(src_diag)}</div>'
        html_body = html_body.replace(f"<p>{token}</p>", figure)
        html_body = html_body.replace(token, figure)
    # Drop the duplicate H1 from the markdown; the hero already has the title.
    html_body = re.sub(r"<h1[^>]*>.*?</h1>", "", html_body, count=1, flags=re.S)
    toc = getattr(conv, "toc", "")
    toc_html = f'<nav class="toc"><div class="toctitle">Contents</div>{toc}</nav>' if toc else ""
    page = TEMPLATE.replace("__BODY__", meta_dl(meta) + toc_html + html_body)
    HTML_PATH.write_text(page, encoding="utf-8")
    print(f"wrote {HTML_PATH}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
