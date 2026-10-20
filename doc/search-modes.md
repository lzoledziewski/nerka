# Search box modes: Auto, Text, NFD

Agreed with the owner on 2026-10-19 after a brainstorm.

## Problem

The Filter and Search boxes had two modes, Text and Query. Both are useful, but the mode is easy to overlook while typing: you only notice you were in the wrong one when the search fails (a query typed in Text mode finds nothing; text typed in Query mode is a syntax error).

## Decisions (owner, 2026-10-19)

- Each box (Filter and Search, the same in both) has a **mode list**: **Auto** (default), **Text**, **NFD**.
- **Auto** reads what was typed:
  - NFD syntax is a query (`level >= ERROR`, `apid == NAV`, `/re/`, `"text"`, `AND` / `OR` / `NOT`, parentheses);
  - a run of plain words is one **exact phrase** (`connection reset` = lines containing exactly that), and phrases mix with NFD: `connection reset AND level >= ERROR`;
  - the **Ignore case** checkbox applies to the phrases (not to a `/re/` or `"text"` written out: those say what they want);
  - the **Regex** checkbox (owner, 2026-10-20; shared with Text mode) makes the phrases and the plain-text fallback regexes: `fr.me AND level >= ERROR` → `/fr.me/ AND level >= ERROR`; regex syntax NFD doesn't know (`(`, `|`, `*`, `[`) makes the whole box one regex, so mix regexes with NFD as `/…/`;
  - if the box isn't a valid query, it is searched as **literal text** as a whole (a pasted log line with `(`, `[`, `:` or `>=` just works).
- **Text** is always literal text (or a regex with **Regex** ticked), never a query: for the rare text that Auto would read as a query.
- **NFD** is strict: an error is an error, shown with a **Search as text instead** button.
- A **preview line** under the box says how it will be read before Enter: `→ text: …`, `→ query: …`, or `→ text (not a query: …)`.
- A query that is nothing but a text search (one `"quoted string"` or `/regex/`, alone or as `raw ~ …`) is previewed as `→ text: …` / `→ regex: …`, not as a query (owner, 2026-10-19). It still runs as written: quoted text keeps its case.
- **The library isn't affected:** packs, library picks and expansions are plain NFD as before and put the box into NFD mode. Plain words are an Auto-only reading; the NFD language itself doesn't change.

## How Auto reads the box (core: `dsl::auto`)

Auto rewrites the box into plain NFD, so what runs (and goes into the history) is ordinary NFD:

1. Split it with the NFD lexer. If that fails (a character NFD doesn't know, such as `[` or `:`), it's text.
2. A word is part of a phrase unless it is a field (a word followed by an operator), a value (the word after an operator) or an operator keyword. Only **upper-case** `AND`, `OR`, `NOT` are operators in Auto, so `error and timeout` stays a phrase; `&&`, `||`, `!` are operators too.
3. Each run of phrase words becomes one phrase: the text from its first word to its last, exactly as typed (inner spacing kept), written as `"…"`, or as `/…/i` with Ignore case (`text_to_nfd`).
4. If the whole box is one phrase, it is text (the fast literal search). Otherwise the rewritten NFD must compile for the open file (fields included); if it doesn't, the whole box is text, and the preview names the query error.

Examples:

| Typed | Read as |
|---|---|
| `connection reset` | text `connection reset` |
| `level >= ERROR` | query `level >= ERROR` |
| `timeout AND level >= WARN` | query `"timeout" AND level >= WARN` |
| `NOT healthcheck` | query `NOT "healthcheck"` |
| `error and timeout` | text `error and timeout` |
| `[ERROR] disk full` | text (`[` isn't NFD) |
| `apid == NAV` on a text log | text (no field `apid`; the preview says so) |

## History and prefs

- History entries carry the mode tag (`auto`, `text`, `regex`, `query`); picking one restores the mode it ran in. Old entries keep working.
- The last mode and checkboxes of **both** boxes are remembered as the default for new tabs (`prefs`: `mode`, `regex`, `ignore_case`, `invert` for the Filter box; `search_mode`, `search_regex`, `search_ignore_case` for the Search box; fixed 2026-10-20: the Search box's weren't saved); new installs start in Auto.
