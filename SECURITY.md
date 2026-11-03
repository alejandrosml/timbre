# Security policy

Timbre reads your mail and can act on it, so security reports are welcome and
taken seriously.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting:
**Security → Report a vulnerability** on
https://github.com/alejandrosml/timbre. Do not open a public issue for a
vulnerability.

Useful details: the Timbre version (`timbre --version` or `mail_status`), the
macOS version, and the smallest message or steps that reproduce the problem.
Never send real mail content that you would not want shared; a synthetic
message that triggers the issue is best.

## Scope

In scope: anything that lets mail content, an attachment, or another program
make Timbre act without the user's request, send mail, write outside its
folders, leak mail to a third party, or crash it repeatedly. Prompt-injection
defences (hidden text removal, warnings, fences around untrusted content) are
in scope too.

## Design notes

- Timbre opens Mail's database read-only and never deletes messages.
- Drafts are only saved, never sent. Sending is off by default and needs two
  steps with the recipients repeated.
- Mail content is returned between random fences and labelled as untrusted.
