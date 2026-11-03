# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
