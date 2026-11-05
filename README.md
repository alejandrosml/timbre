<p align="center"><img src="assets/icon.svg" width="128" height="128" alt="Timbre icon: a postage stamp with an envelope"></p>

<h1 align="center">Timbre</h1>
<p align="center"><b>The connector for Apple Mail and Claude</b><br>
Instant full-text search of all your mail accounts, safe reading, drafting and filing — searched on your Mac, not on Google's servers.</p>
<p align="center"><a href="README.fr.md">Français</a> · English</p>

---

Timbre connects Claude to Apple Mail on your Mac. It is an MCP server packaged as a one-click Claude Desktop extension, and it lets Claude work with the mail you already have in Mail, whatever the account (iCloud, Gmail, Outlook and Exchange, Orange, La Poste, any IMAP…). It reads Mail's own data on disk, so a search across hundreds of thousands of messages — bodies included — answers in tens of milliseconds, and it acts in Mail through the official scripting interface when you ask it to draft, file or flag.

It is built for the reality of an AI assistant reading untrusted email: hidden instructions are stripped and reported, forged senders are flagged, no message is ever deleted for good, and nothing is sent without you.

> **Status:** v0.6, in daily use on a real mailbox of 132,000 messages across six accounts. Feedback welcome in [Issues](https://github.com/alejandrosml/timbre/issues).

## Why Timbre

- **One search for all your mail.** iCloud, Gmail with its labels, Outlook, Orange, La Poste, any IMAP: message bodies and attachment names included, in 20 to 40 ms over 132,000 messages, with Gmail-style operators and folders named as Mail shows them.
- **Nothing missed, nothing hidden.** Shop and newsletter layouts are read in full, and so is text placed after an attachment; text hidden from you but aimed at an AI is removed and reported.
- **Phishing spotted.** Forged senders, links that lie about where they lead (behind Outlook's Safe Links too), first-time senders, risky attachments.
- **Attachments read.** PDF, Word, RTF, ODT, text, CSV, invitations, contact cards, forwarded mail.
- **Safe actions.** Drafts open in Mail and never leave on their own; archive and trash can be undone; folders are made, and empty ones deleted, even where Mail lets no app do it; Timbre says plainly what it cannot do instead of pretending.
- **Private and trusted.** Searched on your Mac, not on a server; one app, signed and notarized by Apple, holding its own permission; nothing else to install, nothing to restart.

## In practice

Ask Claude in your own words; Timbre does the searching and reading:

- *"Find September's electricity bill and tell me the amount."* Timbre finds it in whichever account it arrived, and reads the attached PDF.
- *"What did the notary send me about the sale?"* Bodies and attachment names are searched too, accents and plurals included (*facture* finds *factures*).
- *"Sum up my conversation with Sophie about the holidays."* The whole thread, each message reduced to what it adds.
- *"Is this email from my bank genuine?"* Sender authentication, look-alike addresses, links that lie about where they go, hidden text.
- *"Draft a reply saying Thursday works for me."* The draft opens in Mail for you to check; nothing is sent.
- *"Archive last month's newsletters."* Moves can be undone.
- *"Delete the empty folders in my La Poste account."* Each is checked empty on the server first; the password is asked once, in a window of Timbre, the first time.

## Searched on your Mac, not on Google's servers

Claude's Gmail connector sends each search to Google's servers through the Gmail API, and only reaches Gmail. Timbre searches an index kept on your Mac, built from the mail Apple Mail has already downloaded, for every account at once.

| | **Timbre** | Gmail connector |
|---|---|---|
| Where a search runs | On your Mac | On Google's servers (Gmail API) |
| Accounts | All of them: iCloud, Gmail, Outlook and Exchange, Orange, La Poste, any IMAP | Gmail only |
| What Google receives | Nothing: Mail syncs your mail as usual | Every search and every message read |
| Access you grant | Reading Mail's files on this Mac, to Timbre alone | Your Google account, through Google sign-in |
| Search time on 132,000 messages | 20 to 40 ms | A network round trip per search |

In both cases, what Claude reads goes to Claude to answer you.

**Also using the Gmail connector?** No need to remove it: Timbre asks Claude to go through Timbre first for anything in your mail, Gmail included. Keep the Gmail connector for what Timbre does not do, such as archiving Gmail mail or filing it under a label (Mail cannot do that for a script), or sending from Gmail while sending is off in Timbre. If Claude still reaches for Gmail, just tell it "search with Timbre".

## What it does

- **Search everything.** Full-text over subjects, people, message bodies and attachment names, ranked by relevance, recency and whether you correspond with the sender, one result per message even when Gmail files it under several labels, accent- and ligature-insensitive (`réunion` = `reunion`, `cœur` = `coeur`), with filters and Gmail-style operators: `from:`, `to:`, `cc:`, `subject:`, `in:`, `label:`, `is:unread`, `is:flagged`, `has:attachment`, `filename:`, `after:`, `before:`, `newer_than:7d`, `older_than:1y`, `"exact phrase"` (also after an operator: `from:"Jean Dupont"`), `prefix*` (3 letters or more), `-exclude` (also `-from:`, `-"a phrase"`), `OR`. Junk and trash are left out, and their words can still be searched when asked for.
- **Read safely.** The text a person sees in Mail: hidden HTML content removed, and nothing that is shown lost (shops' MJML layouts, text after an attachment placed inside the message, the text version of mail made of pictures), with warnings about sender authenticity, deceptive links and risky attachments (programs, macros, web pages).
- **Follow conversations.** A whole thread, each message reduced to its new text.
- **Read attachments.** PDF, Word, RTF and ODT (converted by macOS in a separate process), text, HTML, CSV, calendars, contact cards and attached emails.
- **Act, reversibly.** Drafts (new, reply, reply all, forward) open in Mail for you to review; mark read/unread, flag; move to archive, trash or any mailbox — with **undo**. An account with no archive mailbox is given one, as with Mail's Archive button. Gmail is the exception: Mail lets a script move Gmail mail to the trash or junk only, and Timbre says so rather than pretend.
- **Make and delete mailboxes.** Folders and Gmail labels are made, and empty ones deleted, even where Mail lets no app do it: Timbre then acts on the mail server itself, with a password asked once ([how and why](#making-and-deleting-mailboxes)).
- **Send, only if you allow it.** Off by default. When enabled, every message takes two explicit steps, with an optional recipient allowlist and an hourly cap.

## How it compares

Timbre takes the best ideas of the open-source Apple Mail connectors and adds what none of them combines. Facts below come from each project's documentation or code (November 2026); "—" means the project does not describe it.

| | **Timbre** | [sweetrb](https://github.com/sweetrb/apple-mail-mcp) | [patrickfreyer](https://github.com/patrickfreyer/apple-mail-mcp) | [imdinu](https://github.com/imdinu/apple-mail-mcp) | [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp) | [falconbradley](https://github.com/falconbradley/claude-connector-apple-mail) |
|---|---|---|---|---|---|---|
| Runs as | Native app (Rust), signed and notarized | Node.js 20+ | Python | Python 3.11+ | Python 3.10+ | Python 3.11+ (uv) |
| Reads mail through | Mail's database and files | AppleScript (IMAP optional) | AppleScript | Mail's database and files | AppleScript (IMAP optional) | Mail's database and files |
| Search in message bodies | ✅ ranked index, new mail within seconds | AppleScript (slow on big mailboxes, per its docs), or IMAP once set up | AppleScript | ✅ index, built by a command, kept fresh with `--watch` | AppleScript, or IMAP once set up | subject and sender |
| Hidden text and injected instructions | removed, reported, fenced | — | — | — | — | — |
| Sender and link checks | SPF/DKIM/DMARC, lying links, first contact | — | — | — | — | — |
| Text of attachments (PDF, Word…) | ✅ | saved to disk | saved to disk | ✅ | ✅ | raw content |
| Drafts, replies, forwards | ✅ | ✅ | ✅ | ❌ read-only | ✅ | ✅ |
| Move, archive, trash | ✅ with undo | ✅ | ✅ | ❌ | ✅ | ❌ |
| Create or delete mailboxes | ✅ on the server, password asked once in a window when first needed; empty mailboxes only | ✅ through IMAP, password stored beforehand in Terminal; deletes with what it holds | — | ❌ | ✅ deletes through IMAP, password stored beforehand with a setup command; a full one only when told | ❌ |
| Sending | off by default, two steps | on by default | on by default | — | with confirmation | none |
| macOS permission held by | Timbre's own app | Node.js | Python | Terminal and the launching app | Python | the `uv` launcher |
| Licence | MIT | MIT | MIT | GPL-3.0 | MIT | MIT |

To our knowledge, Timbre is the only Apple Mail connector that combines a ranked full-text index of message bodies, protection against hidden instructions, sender and link checks, undo, mailbox management with no setup beforehand, and a signed, notarized app that holds its own permission. sweetrb offers more tools for running Mail itself (rules, smart mailboxes, templates); Timbre keeps to what an assistant needs to find, read and file your mail safely.

**New in Timbre**

- **Every account, labels included.** Gmail keeps each message once, in All Mail: Timbre reads its labels, so Gmail's Inbox, Sent and your labels work like any mailbox. The sent mail of Orange, Wanadoo and La Poste (OUTBOX) and their spam (QUARANTAINE) are recognised too.
- **Sensible ranking.** A vague search (*invoice*) puts recent mail first; mail from people you write to ranks higher; mail you send yourself does not.
- **Nothing to restart.** Once you allow Full Disk Access, Timbre notices within seconds and starts working, in the same conversation. It also tells you when a new version is out.
- **Hidden-text removal.** `display:none`, zero-size fonts, white-on-white, off-screen, clipped, shrunk, blurred and transparent text, text that filters or blending make vanish, zero-width and bidi characters are removed from what Claude reads — and counted, and scanned for instructions aimed at an AI. Style sheets are read with the CSS parser and selector engine Firefox uses, and judged as Mail draws them on a Mac: cascade layers, nested rules, `@container`, `@scope`, `::first-line`, animations and shadow trees included. When what a message shows depends on the width of Mail's window, only what can be seen at any width is kept.
- **What you see is what Claude reads.** Timbre reads the HTML part Mail displays, not the text/plain alternative an attacker can fill with different content (and warns when they differ).
- **Sender checks.** SPF/DKIM/DMARC results, display names showing another address, reply-to on another domain, punycode domains, links whose text shows one domain but lead to another, and *first contact* (a sender you have never exchanged with).
- **Spotlighting.** Every untrusted body is fenced with a random marker the sender cannot predict.
- **Attachments.** Executables, double extensions (`invoice.pdf.app`) and macro files are labelled; saved files go to one folder you choose, are never overwritten and are marked for Gatekeeper.
- **Least privilege.** Read tools are marked read-only so Claude can use them without asking; tools that change anything are marked as such; sending tools do not even exist until you enable them.
- **One permission, for Timbre only.** Full Disk Access is granted to Timbre's own signed app, with its name and icon, not to Claude or a launcher shared with other extensions: Timbre tells macOS it answers for its own access.

## Making and deleting mailboxes

Mail lets no script delete a mailbox, and on current macOS it refuses to make one too, at La Poste for one: its scripting answers "AppleEvent handler failed" (sweetrb measured the same for every kind of account). So Timbre does what Mail's own Mailbox menu does: it acts on the mail server itself, over IMAP, then asks Mail to catch up. It still asks Mail first to make a mailbox, in case it will: no password is then needed.

- **What you do, once per account.** Nothing at installation. The first time you ask Claude to make or delete a folder in an account where Mail will not, a window of Timbre asks for that account's password and says why in one sentence. The server checks it; it then stays in your login keychain, where only Timbre's signed app may read it, and is never asked again. Claude never sees it. Google, Apple and Yahoo take only an app password here: the window's button opens the page that makes one.
- **What Timbre checks.** A mailbox is deleted only if the server says it holds no message and no other mailbox, and if neither the server nor Mail keeps it for itself (inbox, sent mail, drafts, trash, junk, mail waiting to be sent, Apple Notes, Gmail's own folders, and any the server marks as its own). An empty archive may go: Mail makes it again when it next archives. Messages are never deleted with it: move them first, which can be undone. The connection is encrypted, its certificate checked by macOS as Mail checks it.
- **Mail catching up.** A mailbox made on the server showed in Mail within seconds in our tests. One deleted there stays in Mail's sidebar until Mail is next opened, which removes it by itself: nothing to do, and quitting and reopening Mail shows it gone at once. Timbre says which happened.
- **Where it cannot.** Exchange and Outlook.com accounts (Microsoft takes only its own sign-in, which Mail keeps to itself), POP accounts and On My Mac mailboxes (kept by Mail alone): there, Mail's Mailbox menu does it.

Three other routes were weighed and left aside:

- *Editing Mail's database while Mail is closed.* It is Mail's copy of what the server holds: a mailbox deleted there comes back at Mail's next check.
- *Reusing the passwords Mail keeps, unlocked with your Mac's password.* Current macOS keeps them in the protected part of the keychain ("Local Items"), which only Apple's own apps may open: no window lets another app in. Timbre still looks where earlier versions of macOS kept them, the login keychain, where macOS asks for your Mac's password before letting it read one.
- *Driving Mail's menus.* It takes Accessibility, a permission to control your whole Mac, and breaks when Mail changes its menus or speaks another language.

sweetrb and apple-mail-fast-mcp also go through IMAP, with a password you make and store yourself beforehand, from Terminal or with a setup command. Both store it with macOS's `security` tool, which then hands it to any program that runs the same tool, without asking. sweetrb deletes a mailbox with whatever it holds; apple-mail-fast-mcp refuses one that is not empty unless told to delete its messages too; Timbre never deletes a message with a mailbox.

## Signed and notarized

Every release is signed with its author's Developer ID (Alejandro San Martin Lamas, team `4FQA525DB9`) and notarized by Apple. For you, that means:

- **It comes from its author, unaltered.** macOS checks the signature when Timbre starts; a copy changed by anyone else fails the check.
- **Checked by Apple.** Before publication, Apple's notary service scanned it for known malicious content. macOS opens it without the warning about an unidentified developer, and Apple can revoke it if a problem ever turns up.
- **Your permission survives updates.** macOS ties Full Disk Access to Timbre's signature, not to one exact file: a new version keeps it.
- **What it is not.** Notarization is an automated scan, not a review of what a program does. For that, Timbre's code is open.

To check it yourself:

```sh
APP="$HOME/Library/Application Support/Claude/Claude Extensions/local.mcpb.alejandro-san-martin-lamas.timbre/server/Timbre.app"
codesign -dv --verbose=2 "$APP" 2>&1 | grep Authority   # Developer ID Application: Alejandro San Martin Lamas (4FQA525DB9)
spctl --assess -vv "$APP"                                # accepted, source=Notarized Developer ID
```

## Performance

A synthetic mailbox of 20,000 messages (788 MB: HTML newsletters, replies with quoted history, attachments, French, English and Spanish text with a realistic vocabulary), indexed on a small 2-core virtual machine:

| | v0.1.0 | v0.4.0 | v0.5.7 |
|---|---|---|---|
| Full index build | 28.6 s | 13.0 s | 17.0 s |
| Index size on disk | 298 MB | 124 MB | 126 MB |
| Search, median | 42 ms | 18 ms | 28 ms |
| Search, 95th percentile | 62 ms | 30 ms | 45 ms |

0.5.7 is a little slower for being more exact: it reads every part of each message and works out which text can be seen the way Mail's WebKit does, indexes junk and trash too (search still leaves them out), and ranks a broad query over its most recent matches by date rather than by the order Mail stored them in, so the mail of an account added later cannot push this month's out.

What made the difference: messages are parsed on every core while a single writer commits large transactions, with FTS5's merging deferred and a 64 MiB pending-terms table; the text kept for snippets is stored once, compressed with zstd; there is no prefix index; snippets and details are computed for the returned page only, and very broad queries are ranked over their 10,000 most recent matches. Index-time work that only matters when reading a message is skipped. To reproduce: `timbre dev-corpus /tmp/corpus 20000` (debug build), then `timbre index --rebuild` and `timbre bench` from a release build with `TIMBRE_MAIL_ROOT`, `TIMBRE_ACCOUNTS_DB` and `TIMBRE_DATA_DIR` pointing at it.

0.6.5, which also makes and deletes mailboxes, is as fast as 0.5.7, measured side by side on the same machine: the index is built in 15 to 16 s and keeps the same size, and searches answer in 25 to 26 ms (median) and 43 to 45 ms (95th percentile). What Claude reads of Timbre's tools in every conversation is 1,600 characters shorter.

On a real Mac (Apple silicon, 132,000 messages across six accounts, Gmail included), the index takes about 500 MB and searches answer in 20 to 40 ms.

## Install

1. Download `timbre-<version>.mcpb` from [Releases](https://github.com/alejandrosml/timbre/releases) and double-click it; Claude Desktop installs it. Releases are signed with a Developer ID and notarized by Apple.
2. Give Timbre **Full Disk Access**. Ask Claude to *open Timbre's permission settings*: System Settings opens at Privacy & Security › Full Disk Access and Finder selects Timbre (the stamp icon). Drag it into the list and switch it on. Timbre starts working by itself a few seconds later: nothing to restart.
3. The first time you ask for a draft or a move, macOS asks whether Timbre may control Mail: click OK.

Nothing else is asked at installation. A password is asked only the first time you make or delete a folder in an account where Mail will not, and only once.

The full-text index builds in the background on first launch (about a minute per 100,000 messages on a recent Mac); searches work meanwhile on subjects and senders. Afterwards, new mail is searchable within seconds. After an update that reads messages better, Timbre reads them again in the background, newest first, while search keeps working.

## Tools

| Tool | What it does | Changes anything? |
|---|---|---|
| `search_mail` | Full-text search with filters | no |
| `read_mail` | One message, with warnings and attachments | no |
| `get_thread` | A whole conversation | no |
| `list_mailboxes` | Accounts and mailboxes with counts | no |
| `read_attachment` | Text of an attachment: PDF, Word, RTF, ODT, text, HTML, CSV, calendar, attached mail | no |
| `mail_status` | Permissions, index progress, settings, newer version | no |
| `open_permission_settings` | Open Full Disk Access (or Automation) settings and show Timbre | opens System Settings |
| `selected_mail` | Messages selected in Mail | no |
| `open_in_mail` | Open a message in Mail | no |
| `save_attachment` | Save to your attachments folder | adds a file |
| `create_draft` | New, reply, reply all, forward — as a draft | adds a draft |
| `mark_mail` | Read/unread, flag/unflag | yes |
| `move_mail` / `undo_move` | Archive, trash, any mailbox — and back (Gmail: trash and junk) | yes, reversible |
| `create_mailbox` | Make a mailbox or Gmail label: by Mail, or on the server | adds a mailbox |
| `delete_mailbox` | Delete an empty mailbox or Gmail label, on the server | yes, empty ones only |
| `prepare_send` / `confirm_send` | Two-step sending (only when enabled) | sends |

## Questions

**How do I connect Claude to Apple Mail?** Install Timbre in Claude Desktop (download the `.mcpb` from [Releases](https://github.com/alejandrosml/timbre/releases) and open it), give it Full Disk Access when Claude asks, and ask Claude about your mail in your own words.

**Which accounts work?** Every account set up in Apple Mail: iCloud, Gmail and Google Workspace (labels included), Outlook and Exchange, Yahoo, Orange, Free, SFR, La Poste and any IMAP or POP account, plus On My Mac mailboxes.

**Does my mail leave my Mac?** Searching and the index stay on your Mac. Only what Claude reads, to answer you, goes to Claude. Timbre itself collects nothing.

**Can Claude send email or delete my mail?** Sending is off until you turn it on, and then each message takes two explicit steps. No message is ever deleted for good: moves to the trash can be undone, and only empty mailboxes are deleted.

**Is it safe against phishing and prompt injection?** Text hidden from you but aimed at an AI is removed and reported, every email is fenced as untrusted, and forged senders and lying links are flagged.

**Does it need the Gmail connector, or work without Claude Desktop?** No connector is needed: Timbre reads every account through Mail. It is made for Claude Desktop on macOS 12 or later; other MCP clients on a Mac can run its program too.

## Settings

In Claude › Settings › Extensions › Timbre: read-only mode, allow sending, allowed recipients (`alice@example.com, @mycompany.com`), maximum messages per hour, mailboxes left out of search (default: junk and trash; leave the field empty to search everything, and write roles as you like: `spam`, `corbeille`, `archive`…), the attachments folder and the daily check for updates.

## Privacy

The full policy is in [PRIVACY.md](PRIVACY.md). Searching, reading and the index stay on your Mac: Timbre sends nothing about your mail anywhere and collects nothing. It goes on the network for two things only: a daily check of the latest version number on GitHub (turn off *Check for updates* to stop it), and, when you make or delete a mailbox Mail will not, an encrypted connection to your own mail server. The password for that stays in your login keychain as *Timbre IMAP (server)*; delete it there and Timbre forgets it. What Claude reads goes to Claude, to answer you. The index (`~/Library/Application Support/Timbre`) contains the text of your messages for searching; delete that folder to remove it. Mail's own database is only ever opened read-only.

## Build from source

```sh
cargo test                      # unit and end-to-end tests on a synthetic mail store
cargo build --release           # on macOS
scripts/build-macos-from-linux.sh   # cross-compile from Linux (cargo-zigbuild)
scripts/package-mcpb.sh         # assemble the Claude Desktop extension
timbre index | timbre search "facture from:orange" | timbre doctor | timbre bench
```

## Star history

<a href="https://www.star-history.com/#alejandrosml/timbre&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date" />
    <img alt="Star history of Timbre" src="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date" />
  </picture>
</a>

## Credits

Created by **Alejandro San Martin Lamas** — [github.com/alejandrosml](https://github.com/alejandrosml).

Timbre stands on what others figured out first, with thanks: the Envelope Index read path and `flags` bitfield documented by [falconbradley/claude-connector-apple-mail](https://github.com/falconbradley/claude-connector-apple-mail), the Mail scripting patterns of [sweetrb/apple-mail-mcp](https://github.com/sweetrb/apple-mail-mcp), the FTS5 body index idea of [imdinu/apple-mail-mcp](https://github.com/imdinu/apple-mail-mcp) and the confirmation gates of [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp). Timbre's code is original and written in Rust.

## Licence

[MIT](LICENSE) © 2026 Alejandro San Martin Lamas
