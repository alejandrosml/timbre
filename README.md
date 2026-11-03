<p align="center"><img src="assets/icon.png" width="128" alt="Timbre icon"></p>

<h1 align="center">Timbre</h1>
<p align="center"><b>The connector for Apple Mail and Claude</b><br>
Instant full-text search of all your mail, safe reading, drafting and filing — on your Mac, nothing leaves it.</p>
<p align="center"><a href="README.fr.md">Français</a> · English</p>

---

Timbre lets Claude work with the mail you already have in Apple Mail, whatever the account (iCloud, Gmail, Exchange, IMAP…). It reads Mail's own data on disk, so a search across hundreds of thousands of messages — bodies included — answers in tens of milliseconds, and it acts in Mail through the official scripting interface when you ask it to draft, file or flag.

It is built for the reality of an AI assistant reading untrusted email: hidden instructions are stripped and reported, forged senders are flagged, nothing is ever deleted for good, and nothing is sent without you.

> **Status:** early development (v0.3), being tested on a real 250,000-message mailbox. Feedback welcome in [Issues](https://github.com/alejandrosml/timbre/issues).

## What it does

- **Search everything.** Full-text over subjects, people, message bodies and attachment names, ranked by relevance, recency and whether you correspond with the sender, one result per message even when Gmail files it under several labels, accent- and ligature-insensitive (`réunion` = `reunion`, `cœur` = `coeur`), with filters and Gmail-style operators: `from:`, `to:`, `subject:`, `in:`, `is:unread`, `is:flagged`, `has:attachment`, `after:`, `before:`, `"exact phrase"`, `prefix*` (3 letters or more), `-exclude`, `OR`.
- **Read safely.** The text a person sees in Mail, with hidden HTML content removed, sender-authenticity warnings and attachment risks.
- **Follow conversations.** A whole thread, each message reduced to its new text.
- **Read attachments.** PDF, Word, RTF and ODT (converted by macOS in a separate process), text, HTML, CSV, calendars, contact cards and attached emails.
- **Act, reversibly.** Drafts (new, reply, reply all, forward) open in Mail for you to review; mark read/unread, flag; move to archive, trash or any mailbox — with **undo**.
- **Send, only if you allow it.** Off by default. When enabled, every message takes two explicit steps, with an optional recipient allowlist and an hourly cap.

## How it compares

Timbre takes the best ideas of the existing open-source Apple Mail connectors and adds what none of them combined. Facts below come from each project's code or documentation (November 2026).

| | **Timbre** | [falconbradley](https://github.com/falconbradley/claude-connector-apple-mail) | [sweetrb](https://github.com/sweetrb/apple-mail-mcp) | [imdinu](https://github.com/imdinu/apple-mail-mcp) | [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp) |
|---|---|---|---|---|---|
| Runtime | Native binary (Rust), nothing to install | Python (uv) | Node.js | Python | Python |
| Reads Mail's database directly | ✅ | ✅ (AppleScript fallback) | opt-in | ✅ | ❌ AppleScript / IMAP |
| Full-text search in message bodies | ✅ ranked, incremental | ❌ subject and sender | opt-in index | ✅ | via IMAP server |
| Drafts, replies, forwards | ✅ | ✅ | ✅ | ❌ read-only | ✅ |
| Move, archive, trash | ✅ with undo | ❌ | ✅ | ❌ | ✅ |
| Sending | off by default, two-step | off by default | always available | — | with confirmation |
| macOS permission held by | Timbre's own signed binary | the `uv` launcher shared by Python extensions | Node.js | Python / Terminal | Python |
| Licence | MIT | MIT | MIT | GPL-3.0 | MIT |

**New in Timbre**

- **Hidden-text removal.** `display:none`, zero-size fonts, white-on-white, off-screen and transparent text, zero-width and bidi characters are removed from what Claude reads — and counted, and scanned for instructions aimed at an AI.
- **What you see is what Claude reads.** Timbre reads the HTML part Mail displays, not the text/plain alternative an attacker can fill with different content (and warns when they differ).
- **Sender checks.** SPF/DKIM/DMARC results, display names showing another address, reply-to on another domain, punycode domains, links whose text shows one domain but lead to another, and *first contact* (a sender you have never exchanged with).
- **Spotlighting.** Every untrusted body is fenced with a random marker the sender cannot predict.
- **Attachments.** Executables, double extensions (`invoice.pdf.app`) and macro files are labelled; saved files go to one folder you choose, are never overwritten and are marked for Gatekeeper.
- **Least privilege.** Read tools are marked read-only so Claude can use them without asking; tools that change anything are marked as such; sending tools do not even exist until you enable them.
- **One permission, for Timbre only.** Full Disk Access is granted to Timbre's own signed binary, not to a launcher shared with other extensions.

## Performance

A synthetic mailbox of 20,000 messages (788 MB: HTML newsletters, replies with quoted history, attachments, French, English and Spanish text with a realistic vocabulary), indexed on a small 2-core virtual machine:

| | v0.1.0 | v0.3.1 |
|---|---|---|
| Full index build | 28.6 s | 13.0 s |
| Index size on disk | 298 MB | 124 MB |
| Search, median | 42 ms | 17 ms |
| Search, 95th percentile | 62 ms | 30 ms |

What made the difference: messages are parsed on every core while a single writer commits large transactions, with FTS5's merging deferred and a 64 MiB pending-terms table; the text kept for snippets is stored once, compressed with zstd; there is no prefix index; snippets and details are computed for the returned page only, and very broad queries are ranked over their 10,000 most recent matches. Index-time work that only matters when reading a message is skipped. To reproduce: `timbre dev-corpus /tmp/corpus 20000` (debug build), then `timbre index --rebuild` and `timbre bench` from a release build with `TIMBRE_MAIL_ROOT`, `TIMBRE_ACCOUNTS_DB` and `TIMBRE_DATA_DIR` pointing at it. Figures from a real 250,000-message mailbox on a Mac will follow.

## Install

1. Download `timbre-<version>.mcpb` from [Releases](https://github.com/alejandrosml/timbre/releases) and double-click it; Claude Desktop installs it.
2. Give Timbre **Full Disk Access** (System Settings › Privacy & Security › Full Disk Access › +). Ask Claude for *Timbre status*: it tells you the exact file to add. Then turn the extension off and on in Claude's settings.
3. The first time you ask for a draft or a move, macOS asks whether Timbre may control Mail: click OK.

The full-text index builds in the background on first launch (a few minutes for a large mailbox); searches work meanwhile on subjects and senders.

## Tools

| Tool | What it does | Changes anything? |
|---|---|---|
| `search_mail` | Full-text search with filters | no |
| `read_mail` | One message, with warnings and attachments | no |
| `get_thread` | A whole conversation | no |
| `list_mailboxes` | Accounts and mailboxes with counts | no |
| `read_attachment` | Text of an attachment: PDF, Word, RTF, ODT, text, HTML, CSV, calendar, attached mail | no |
| `mail_status` | Permissions, index progress, settings | no |
| `selected_mail` | Messages selected in Mail | no |
| `open_in_mail` | Open a message in Mail | no |
| `save_attachment` | Save to your attachments folder | adds a file |
| `create_draft` | New, reply, reply all, forward — as a draft | adds a draft |
| `mark_mail` | Read/unread, flag/unflag | yes |
| `move_mail` / `undo_move` | Archive, trash, any mailbox — and back | yes, reversible |
| `prepare_send` / `confirm_send` | Two-step sending (only when enabled) | sends |

## Settings

In Claude › Settings › Extensions › Timbre: read-only mode, allow sending, allowed recipients (`alice@example.com, @mycompany.com`), maximum messages per hour, mailboxes left out of search (default: junk, trash) and the attachments folder.

## Privacy

Timbre makes no network connections and collects nothing. Its index (`~/Library/Application Support/Timbre`) contains the text of your messages for searching; delete that folder to remove it. Mail's own database is only ever opened read-only.

## Build from source

```sh
cargo test                      # unit and end-to-end tests on a synthetic mail store
cargo build --release           # on macOS
scripts/build-macos-from-linux.sh   # cross-compile from Linux (cargo-zigbuild)
scripts/package-mcpb.sh         # assemble the Claude Desktop extension
timbre index | timbre search "facture from:orange" | timbre doctor | timbre bench
```

## Credits

Created by **Alejandro San Martin Lamas** — [github.com/alejandrosml](https://github.com/alejandrosml).

Timbre stands on what others figured out first, with thanks: the Envelope Index read path and `flags` bitfield documented by [falconbradley/claude-connector-apple-mail](https://github.com/falconbradley/claude-connector-apple-mail), the Mail scripting patterns of [sweetrb/apple-mail-mcp](https://github.com/sweetrb/apple-mail-mcp), the FTS5 body index idea of [imdinu/apple-mail-mcp](https://github.com/imdinu/apple-mail-mcp) and the confirmation gates of [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp). Timbre's code is original and written in Rust.

## Licence

[MIT](LICENSE) © 2026 Alejandro San Martin Lamas
