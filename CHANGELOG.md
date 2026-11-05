# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- **Making and deleting mailboxes.** `create_mailbox` makes a folder or a
  Gmail label, `delete_mailbox` deletes an empty one, without Mail's
  confirmation. Mail lets no script delete a mailbox, and on current macOS
  refuses to make one too at La Poste and elsewhere, so Timbre does what
  Mail's Mailbox menu does, on the mail server itself (IMAP), then asks Mail
  to synchronize; it still asks Mail first to make one. Nothing is asked at
  installation: the first time a mailbox is made or deleted in an account
  where Mail will not, a window of Timbre asks for the account's password
  in one sentence, the server checks it, and it stays in the login keychain,
  where only Timbre's signed app reads it. Claude never sees it. Google,
  Apple and Yahoo take only an app password there: the window's button opens
  the page that makes one. Where earlier versions of macOS kept Mail's own
  passwords, in the login keychain, Timbre looks too: macOS then asks for the
  Mac's password before letting it read one.
- **What is never deleted.** A mailbox is deleted only if the server says it
  holds no message and no other mailbox, and if neither the server nor Mail
  keeps it for itself (inbox, sent mail, drafts, trash, junk, mail being
  sent, Apple Notes, the provider's own folders, any the server marks as its
  own), by its flags or, where the server sets none, by its name; messages
  are never deleted with it. An empty archive may go: Mail makes it again
  when it next archives. Exchange,
  Outlook.com, POP accounts and On My Mac mailboxes are left to Mail's menu,
  and the result says so.
- **Mail catching up.** A mailbox made on the server shows in Mail within
  seconds; one deleted there stays in Mail's sidebar until Mail is next
  opened, which removes it by itself. The result says which happened.
- **Encrypted and checked.** The connection to the mail server is TLS from
  the first byte, or STARTTLS before anything is said; the certificate is
  checked by macOS, as Mail checks it; a password never travels unencrypted.

- **Mailboxes Mail no longer has.** Mail's database keeps a row for some
  mailboxes Mail deleted; Timbre knows them by their folder gone from the
  Mac (in an account whose other folders are where they should be, and only
  when nothing is in them), asks Mail when it is open, and leaves them out
  of `list_mailboxes` and of the roles' mailboxes.
- **A mailbox just made is there at once.** One `create_mailbox` made is
  listed, and mail is filed into it, before Mail's database lists it.
- **iCloud's own mailboxes.** Where another app made "Trash" or "Sent"
  beside iCloud's "Deleted Messages" and "Sent Messages", trash and sent
  mean iCloud's own, as in Mail.

### Fixed
- **Archives at La Poste.** Archiving the mail of an account with no archive
  mailbox, where Mail will not make one for a script (La Poste), makes it on
  the server when Timbre has the password already; otherwise the result
  says to call `create_mailbox` once.
- **Moves go exactly where asked.** Mail's mailbox is found at exactly its
  path for a move, an undo, or to know whether a mailbox exists: never
  another one sharing its last name, nor one whose name differs in case, nor
  a mailbox at the top for one inside an iCloud inbox. "archive", "trash",
  "junk" and "inbox" mean the mailbox Mail or the provider names so, never a
  folder of the user's that holds the word ("Archives clients 2019"); with
  no archive, one is made, as before.
- **Gmail labels named like a bin.** A label of the user's named "Spam
  reports" no longer makes the messages it holds count as junk: Gmail's own
  trash and spam alone do.
- **Accounts named alike.** "Gmail" means the account so named, not also
  "Gmail Pro", in searches, moves and deletions; an account's id works with
  or without a mailbox.
- **Searches.** Words with no letter or digit ("€", "🎉") are said to be
  left out, and alone find nothing instead of every message; `-subject:`
  leaves out what holds the word in its subject only, not in its text; a
  subject or a name that is not valid UTF-8 in Mail's database no longer
  fails the searches and threads it belongs to.
- **Late message files.** A message whose file Mail writes after Timbre
  looked for it is indexed within minutes, not at the next start.
- **Dates on a new Mac.** Started while Mail's database held no message
  yet, Timbre read later dates 31 years off.
- **Moves and undo.** An id given twice is moved once; a message without a
  Message-ID is never journaled under an empty one (undo would have taken
  another), and the result says it cannot be undone; two Timbres (two
  Claude windows) no longer overwrite each other's undo journal; the
  database is read off the server's threads, which a long search no longer
  stalls.
- **Making and deleting mailboxes, reviewed.** A server's answer is read
  for what it is: a wrong password is asked again; the server's own message
  for the user (Gmail asking for an app password) is shown in the window;
  a server unable to sign in now is told as such, and the saved password is
  kept. Each call stays under the time Claude waits, says when the server is
  slow rather than blaming a window, and signs in once instead of twice. A
  mailbox Mail lists is matched exactly on the server ("Clients" is not
  "clients"); a deletion whose answer is lost says it may have happened.
  Making a mailbox while Mail is slow never makes it twice. Drafts, Notes or
  OUTBOX are kept on servers that flag nothing. The count of messages is
  read from the server's answer about that very mailbox, and an endless
  answer is refused. A keychain item already under Timbre's name is
  replaced, never updated.
- **Mailboxes, second review.** A role's mailbox is the account's own (its
  oldest row) over a folder of the user's named alike ("Trash" beside
  iCloud's "Deleted Messages"); "archive" with an account named is that
  account's archive; a mailbox at the top is named by its name ("Clients",
  not iCloud's "INBOX/Clients"); one made again under its old row is found
  at once. Gmail asking for an app password, met with a password kept in
  the keychain, opens the window with the server's message. Each call keeps
  to its time; an undo without a token takes this Claude window's moves.
- **Text Mail hides, read as Mail reads it.** An HTML page naming a charset
  browsers do not read (UTF-7), or UTF-16 in its own `<meta>`, is read as
  Mail's WebKit reads it: text Mail shows as a comment no longer reaches
  Claude. "base64," written in the visible text is kept; only pictures
  written in tags shrink.
- **Mail built to stall a reader.** End tags inside comments or scripts,
  and HTML elements inside svg, no longer pass the depth check: such a
  message is withheld at once, not after minutes.
- **Threads and attachments warn too.** `get_thread` gives each message the
  warnings `read_mail` gives (sender checks, first contact, hidden text,
  instructions aimed at an assistant); `read_attachment` says what it took
  out of HTML and mail attachments, and when it withholds one.
- **Sending.** A forward whose attachments would make Mail drop Claude's
  text is refused at `prepare_send` instead of going out without it; the
  files that go out are listed; the hourly limit holds with sends at once.
- **Mail rebuilding its database.** Timbre reads the new file at once,
  indexes it again, and acts on a message only if it is still the one it
  read (its Message-ID is checked).
- **Smaller things.** Reply-To domains under country suffixes
  ("barclays.co.uk") are compared whole; a damaged index found while running
  is set aside and rebuilt, and a first build that failed is tried again;
  display names are shown on one line; attachment names are never "." or
  ".."; `open_in_mail` no longer claims to change nothing (Mail marks the
  message read); the update check stays daily when its cache cannot be
  written.

## [0.5.7] - 2026-11-05

A review of every part of Timbre, by four independent reviewers and on a
real mailbox of 132,000 messages. After this update Timbre reads the
messages again in the background, newest first; search works meanwhile.

### Fixed
- **Emails that showed no text.** Shops and newsletters built with MJML
  (Mailjet, PrestaShop and many more) put `font-size:0` on every column and
  give the text inside its own size; Timbre hid all of it. Visibility now
  follows CSS inheritance, as WebKit draws it: styles for phones, dark mode
  or other mail clients (`[data-ogsc]`, `.ExternalClass`…) no longer hide
  what Mail shows on a Mac, `mso-hide:all` (Outlook only) no longer hides
  anything, and a link keeps its own colour on a white-on-white parent.
  Style sheets are read with the CSS parser and selector engine Firefox
  uses: rules apply exactly where their selectors hold (what is above or
  beside an element, attributes, `:not()`, `:nth-child()`, `:has()`), in
  CSS's order (specificity, `!important`, cascade layers, layers inside
  layers included); an invalid value leaves the one before it in place, and
  comments, escapes and broken strings are read as a browser reads them.
  Nested rules (CSS nesting), `@container`, `@scope`, `@namespace`,
  `var()`, `calc()` and the other math functions, every colour notation
  (`oklch()`, `color-mix()`, relative colours, system colours…), animations
  that end hidden, `::first-line` and `::first-letter` (on the first line
  only, handed down as WebKit hands them; where that line ends is taken on
  the side of hiding), style sheets written into
  the message as `data:` addresses, popovers and closed dialogs, and the
  style sheets of shadow trees (`<template shadowrootmode>`, with `:host`,
  `::slotted()` and `::part()`) are applied as WebKit applies them; names
  of animations, layers, containers and custom properties keep their case,
  and a value WebKit would reject is dropped as it drops it. `@supports`, `<style media>`
  and media queries are judged as on a Mac, so rules for phones, dark mode,
  Yahoo or Outlook are left out. `<noscript>` content, which Mail shows, is
  read; a closed `<details>` shows its summary only.
- **Attachment counts.** Mail writes 63 when it does not know, and counts
  inline images: Timbre now counts the attachments it reads in each message,
  for the results and for `has:attachment`.
- **Text after an attachment.** Apple Mail places an attachment where the
  cursor is; the text after it was missing from reading, threads and search.
  Every part of the body is now read, each by its type (HTML could
  previously pass as plain text and keep its hidden text), and photos placed
  in the text are listed as attachments.
- **Search.** `from:"Jean Dupont"`, `subject:"…"`, `in:"Sent Messages"`
  and `-"a phrase"` work; `from:a OR from:b` finds both; a query of words to
  leave out (`is:unread -newsletter`) no longer returns those words only;
  words in junk and trash are found when asked for; unknown operators are
  reported, and `label:`, `cc:`, `newer_than:`, `older_than:`, `filename:`
  and `-from:` are understood. Pages no longer repeat a conversation, a page
  is never empty while results remain, an offset past the end gives an empty
  page instead of an error, paging stops at the 50,000th result instead of
  repeating a page, and dates such as `2024/03/01` or
  `2024-03-01T10:00:00` are read.
- **Gmail moves.** Mail keeps each Gmail message in All Mail and, asked by
  a script, moves it from there, never from the inbox or label it is shown
  under: archiving or filing under a label did nothing (the message was back
  in the inbox seconds later) while Timbre said it was done, and moving a
  message from the trash to All Mail makes Gmail delete it for good. Timbre
  now asks Mail only for what it can do with Gmail: to the trash or junk,
  and back out of them. The other moves are refused, saying what can do
  them (Gmail itself, or Mail's Archive button). Undo takes a Gmail message
  out of the trash through the inbox, the one way Mail can: it keeps its
  labels, and one that was in the inbox lands in All Mail, as the result
  says. Trashed mail stays out of searches even when Gmail keeps its
  labels. Two accounts with the same name ("Google") are told apart. A
  trashed message that keeps its labels is shown, and acted on, in the
  trash.
- **Accounts without an archive.** Archiving the mail of an IMAP account
  that has no archive mailbox asks Mail to make one first, "Archive", as
  Mail's Archive button does. Where Mail will not make it for a script (La
  Poste), nothing is moved and the result says how to make it once, on the
  provider's website or in Mail; an Archive made on the website is used as
  soon as Mail lists it.
- **New mailboxes.** A mailbox Mail creates while Timbre runs ("Recovered
  Messages", a new archive) is named in results instead of left blank.
- **Mailbox names with accents.** Mail's database may write "é" as an "e"
  followed by a combining accent: Gmail's "Messages envoyés" was then not
  taken for the sent mailbox (searching sent mail failed, and people written
  to from Gmail neither ranked first nor counted when telling a first
  contact), and no mailbox with an accent could be named in a search or a
  move. Accents now count however they are written, and a name is found
  with or without them ("Envoyes"). An inbox is the one at the top of an
  account: a copy kept in a folder (".../Boîte de réception") is not
  searched as one.
- **Folders as Mail shows them.** Where an account keeps every mailbox
  inside INBOX (La Poste, Orange), a folder named as Mail shows it
  ("Archives/2009") was not found; it is now, in searches and in moves, and
  Mail is asked for it by the name it uses. A folder is searched with the
  folders inside it, an inbox excepted: an Orange account's "Archives", the
  folder that holds "Archives/2009" and the like, no longer gives nothing,
  and `list_mailboxes` lists it as the account's archive.
- **Mailbox roles.** Whole words only: "Présentations" or "Essentiel" are
  no longer taken for the sent mailbox; each account's usual mailbox is
  chosen, the same every time (Gmail's own before a label of the same
  name, and a label called "Todos" is not taken for All Mail). Mailbox
  names in Dutch, Portuguese, the Nordic languages, Polish and Czech are
  recognised.
- **Undo.** An undo that partly fails can be retried; without a token, only
  the latest move is undone; moves are sent to Mail in small batches, so a
  slow Mail never leaves moves without an undo: each batch is recorded
  before Mail is asked, so moves Mail made before it stopped answering can
  be undone too.
- **Permissions in Mail.** A refused Automation permission is reported
  instead of looking like "nothing selected", and `open_permission_settings`
  opens the Automation settings when asked.
- **Waiting for access.** The message follows the actual cause (no access,
  Mail never set up, another problem) and changes when it does. A damaged
  index is set aside and rebuilt instead of stopping Timbre.
- **Reading.** Outlook forwards keep what they forward; a booking's own
  "From:/To:/Date:" lines, or "Le client nous a écrit :", no longer cut the
  message; answers written between quoted lines are kept; quoted history in
  HTML is marked `> ` (lists, rules and links in it too), but only history
  as mail programs mark it: a newsletter's quotation stays part of the
  message. Forwards written in German, Italian, Portuguese or Dutch are
  recognised. Mail without a charset reads as Windows-1252 rather than with
  replacement marks; text attachments use their declared charset.
- **Warnings.** All of iCloud's authentication results are read (it adds
  one header per check), and only those of the receiving server: one forged
  below them, or below Gmail's single header, is ignored. Links wrapped by Safe Links, Proofpoint or Google
  are compared with their real destination (only the services' own domains
  are unwrapped: `evil-cudasvc.com` is not Barracuda), and links written
  `HTTPS:…`, with backslashes or with a name before the host
  (`paypal.com@evil.example`) are read as a browser follows them; an image
  link in a zero-size cell keeps its address. White text on the page's
  white is hidden text, and so is text hidden with
  `-webkit-text-fill-color`, `-webkit-text-security`, `zoom`, transforms,
  filters (`invert()`, `brightness()`, blur, SVG filters), blending
  (`mix-blend-mode`, `backdrop-filter`), transparency adding up, clip
  paths, masks (mask pictures included), placement off the page on any
  side, an element drawn under its parent's background, or black text left
  on a dark background (white, on a page made for a dark colour scheme
  alone); transparent text that a sharp `text-shadow` or `drop-shadow()`,
  or a thin outline (`-webkit-text-stroke`), draws close by is seen. A
  container query whose answer depends on a width the page leaves open is
  read at each width its rules compare with, and what any of them hides is
  hidden. The width of Mail's window
  is not known: when a message's styles depend on it, the message is read
  at a narrower and a wider width, and what either hides is left out. A
  message that loads a style
  sheet from the web says so: what it hides could not be checked. A sender
  who proved its domain
  may count clicks on its own links; "Sarah @ Notion" is a name, not an
  address; links keep their full address (meeting passwords included).
  HTML, SVG and configuration-profile attachments are flagged.
- **Chinese, Japanese and Korean.** Words written without spaces (会議 in
  会議の議事録) are found.
- **Attachments of partly downloaded messages** kept by Mail as
  `.emlxpart` files are read; a partly downloaded message never returns
  another attachment's bytes.
- **Forwards with attachments.** Mail drops a forward's attachments when a
  script replaces its text: Timbre then leaves the text for the user to
  type and says so. Undo moves back the copy that was moved, when a mailbox
  holds two with the same Message-ID. Messages deleted in Mail are not
  acted on.
- **Faster sender, recipient and subject filters**: matched in Mail's
  tables of addresses and subjects first, instead of message by message.
- **Robustness.** One Timbre keeps the index when two run; a stalled index
  says so in `mail_status`; messages missed while Mail was busy are picked
  up; Mail rebuilding its own database (renumbering every message) is
  noticed by comparing subjects, and deleting many recent messages is no
  longer taken for it; a hostile style sheet cannot make reading a message
  take minutes, and one built to wear the checks out (rules nested too
  deep, a parent selector repeated at every level, thousands of classes on
  one element) has its text withheld;
  subjects are shown on one
  line; long file names fit macOS's limit; the update check retries after a
  failure and counts sleep; and searches no longer fall back to subjects
  and senders for a few seconds after each start.

### Changed
- Claude is told to use Timbre first for anything in the user's mail, before
  another mail connector such as Gmail's. There is no need to remove the
  Gmail connector.
- Junk and trash are now indexed too (and still left out of searches unless
  asked for). Clearing "Mailboxes skipped by search" searches everything.
  Only the mailboxes Mail or the provider names so are skipped ("Junk",
  "[Gmail]/Corbeille", "QUARANTAINE"), never a folder of the user's that
  merely holds the word ("Old Spam"); with "sent" skipped, Gmail's sent
  mail is skipped too.
- `mail_status` counts unread mail outside junk and trash, reports the live
  open mode, and when new mail was last indexed; `list_mailboxes` lists empty
  folders of the user's.

## [0.5.1] - 2026-11-04

### Fixed
- Timbre restarts by itself once Full Disk Access is granted, also when
  Claude starts Timbre.app's executable as a process that already answers
  for its own permissions: on macOS the launcher now always runs, since it
  is what makes the restart possible. The message about missing access names
  Timbre.app rather than the executable inside it.

## [0.5.0] - 2026-11-04

### Added
- **Timbre.app.** The server ships as a small app, so System Settings
  (Full Disk Access) and Finder show Timbre's name and stamp icon instead
  of a bare executable. Full Disk Access has to be given once more, to
  Timbre.app; it then survives updates.
- **Nothing to restart after granting access.** Under the launcher, a
  server that cannot read Mail yet checks every few seconds from a fresh
  process and asks for a restart once it can; the launcher replays the MCP
  handshake and the unanswered requests, so Claude keeps its session. In
  the meantime the server offers the same tools as the full one, each
  explaining what to do, and `open_permission_settings` exists in both.
- **New version notice.** Once a day Timbre asks GitHub for the number of
  the latest release, through macOS's curl, and nothing else. `mail_status`
  reports a newer version and the next search mentions it once. The
  extension's *Check for updates* setting (`TIMBRE_CHECK_UPDATES`) turns it
  off.
- Icons drawn from one description (`scripts/make-icons.py`): a portable
  `assets/icon.svg` (SVG 1.1 filters), `assets/icon-macos.svg` at Apple's
  icon proportions, and `assets/Timbre.icns`.
- Timbre Signer 0.2.0 signs, notarizes and staples the app.

### Changed
- **Gmail labels.** Gmail keeps each message once, in All Mail, and lists
  its Inbox, Sent and labels in the Envelope Index's `labels` table: mailbox
  filters, sent mail (correspondents, first-contact warnings) and mailbox
  counts now include them, and results show a Gmail message's inbox or label
  rather than All Mail.
- OUTBOX (Orange, Wanadoo, La Poste) is recognised as sent mail, QUARANTAINE
  as junk.
- **Ranking.** The user's own addresses no longer count as correspondents,
  so mail people send themselves stops ranking first; recency weighs more,
  and more for broad queries (up to 16 times for today's mail when a query
  matches a hundred messages or more).

## [0.4.3] - 2026-11-04

### Fixed
- Claude Desktop rejected most results ("Structured content does not match
  the tool's output schema"). Fields left out when empty (a message without
  attachments, already read, without warnings…) were listed as required in
  the output schemas; they are optional now. The end-to-end test checks
  every result against the schema the server advertises, and a unit test
  keeps such fields optional.

## [0.4.2] - 2026-11-04

### Fixed
- Full Disk Access given to `timbre` now takes effect. macOS checks the
  permissions of the *responsible* process, which for a program started by
  Claude Desktop is Claude itself. The server now relaunches itself once
  with its responsibility disclaimed, as Chromium and Qt Creator do for the
  programs they start, so macOS checks Timbre's own access and Claude needs
  none. `mail_status` and `timbre doctor` say whose access macOS checks.
  Set `TIMBRE_DISCLAIMED=1` to keep the old behaviour.

### Changed
- `from:`, `to:` and `subject:` filters ignore accents, ligatures and case
  (`from:helene` finds Hélène), like the full-text search.

## [0.4.1] - 2026-11-03

### Added
- Before Full Disk Access is granted, `open_permission_settings` opens
  System Settings at the right pane and selects Timbre's program in
  Finder: the user only drags it into the list.

## [0.4.0] - 2026-11-03

### Added
- Searches also find the other grammatical number of each word in French,
  English and Spanish (`facture` finds `factures`, `reuniones` finds
  `reunión`), with light rules instead of a stemmer for mixed-language
  mailboxes. Exact phrases in quotes stay exact.
- New mail is searchable about 2 s after Mail stores it (Timbre watches
  Mail's database instead of checking every 15 s).
- Search results come one per conversation by default, with
  `thread_matches` counting the other matching messages of the thread
  (`group_threads: false` lists every message).
- Timbre Signer (tools/signer): signs and notarizes releases with the
  maintainer's Developer ID on his Mac; releases are now notarized.

### Changed
- Starting Timbre no longer walks Mail's whole store when the index is
  complete (seconds of disk activity at every Claude launch).

## [0.3.1] - 2026-11-03

### Fixed
- Found by an independent review, each covered by a regression test:
  a forced rebuild now empties the index in one transaction, and tables
  left disagreeing by an interrupted run are detected and rebuilt; the
  index file is never deleted (outdated tables are dropped in place, and
  a database that is not Timbre's is refused); paging stays stable when
  filters thin a broad query; RTF typed `text/rtf` goes to the converter;
  documents with more than 8 MB of text are truncated instead of failing;
  `percent_decode` no longer panics on non-ASCII mailbox names; the HTML
  nesting guard cannot be bypassed with stray end tags or `<div/>`, and
  its own scans are linear. Long words, file names and scheme-less links
  are indexed; only machine strings (runs of 41+ letters and digits) are
  dropped.
- HTML nested more than 512 levels deep no longer stalls the parser (tree
  builders are quadratic in depth): a linear pre-scan withholds its text
  and warns, as no real mail is built that way.

## [0.3.0] - 2026-11-03

### Added
- `read_attachment` reads PDF (PDFKit) and Word, RTF and ODT files
  (textutil). macOS converts them in a separate process with a 30 s timeout.

### Changed
- A message stored in several mailboxes (Gmail labels and All Mail) appears
  once in search results, preferably from the inbox.
- Messages from people you have written to rank higher (1.5x), so a
  colleague's mail outranks a newsletter with the same words.
- zstd 0.14.

## [0.2.0] - 2026-11-03

### Changed
- Index format 3: the indexed text is stored once, zstd-compressed, and the
  full-text index points at it (index about half the size).
- Text is folded before indexing and searching (NFKC, œ/æ/ß), so `coeur`
  finds `cœur`. Links are indexed by host name; long machine tokens are
  skipped. Prefix search needs three letters.
- Ranking: subject 4, people 2.5, attachment names 2, body 1, with a recency
  boost of up to 1.75x over about six weeks.
- Search ranks ids first and reads details for the returned page only;
  broad queries are ranked over their 10,000 most recent matches.
- Faster full builds: parsing runs on all cores while one thread writes in
  large transactions, with FTS5 merging deferred to the end.

### Fixed
- A message that makes the parser panic is skipped instead of stopping the
  server. Deeply nested HTML no longer risks a stack overflow.
- Chinese, Japanese and Korean legacy charsets are decoded.

## [0.1.0] - 2026-11-03

First public version.
