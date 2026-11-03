# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed
- A message stored in several mailboxes (Gmail labels and All Mail) appears
  once in search results, preferably from the inbox.
- Messages from people you have written to rank higher (1.5x), so a
  colleague's mail outranks a newsletter with the same words.

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
