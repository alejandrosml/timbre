# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
