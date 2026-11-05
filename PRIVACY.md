# Privacy policy

Timbre is an open-source connector between Apple Mail and Claude, made by Alejandro San Martin Lamas ([github.com/alejandrosml](https://github.com/alejandrosml)). It runs entirely on your Mac. This page says what it reads, what it keeps, and what leaves your Mac. *[Version française plus bas.](#politique-de-confidentialité)*

## What Timbre reads

- **Mail's data on your Mac**: its message database (`~/Library/Mail`), opened read-only, and the message files, to search and read your mail when Claude asks.
- **Your account names** from macOS (`~/Library/Accounts`), read-only, to name accounts as Mail does.
- **Mail itself**, through Apple's scripting interface, only to do what you ask: open a draft, mark, move, make or delete a mailbox.

## What Timbre keeps, on your Mac only

- **A search index** of your messages in `~/Library/Application Support/Timbre`, so searches answer in milliseconds. Delete that folder to remove it; Timbre rebuilds it if it runs again.
- **An undo journal** of recent moves, in the same folder, so they can be undone.
- **Attachments you ask to save**, in the folder you choose in Timbre's settings.
- **A mail server password, only if you give one**: the first time you make or delete a mailbox in an account where Mail will not do it, a window asks for that account's password. It is kept in your login keychain as *Timbre IMAP (server)*, where only Timbre's signed app can read it. Delete it in Keychain Access and Timbre forgets it.

## What leaves your Mac

- **To Claude**: only what Claude reads from your mail to answer you, through the Claude app you use. Anthropic's privacy policy applies to it.
- **To GitHub**: once a day, a request for the latest version number of Timbre, with no information about you or your mail. Turn off *Check for updates* in Timbre's settings to stop it.
- **To your own mail server**: only when you make or delete a mailbox Mail will not, an encrypted connection (TLS, certificate checked by macOS) with the account's own user name and password.

Timbre collects nothing, has no analytics or telemetry, and sends nothing about your mail to its author or to anyone else. It has no server of its own.

## Contact

Questions or concerns: [open an issue](https://github.com/alejandrosml/timbre/issues) on GitHub.

---

## Politique de confidentialité

Timbre est un connecteur libre entre Mail et Claude, créé par Alejandro San Martin Lamas ([github.com/alejandrosml](https://github.com/alejandrosml)). Il fonctionne entièrement sur votre Mac.

**Ce qu'il lit.** Les données de Mail sur votre Mac (`~/Library/Mail`), en lecture seule, pour chercher et lire votre courrier quand Claude le demande ; le nom de vos comptes (`~/Library/Accounts`), en lecture seule ; et Mail lui-même, par l'interface de scripts d'Apple, seulement pour faire ce que vous demandez (brouillon, marquage, déplacement, création ou suppression d'une boîte).

**Ce qu'il garde, sur votre Mac seulement.** Un index de recherche et le journal des déplacements à annuler dans `~/Library/Application Support/Timbre` (supprimez ce dossier pour les effacer) ; les pièces jointes que vous demandez d'enregistrer, dans le dossier choisi ; et, seulement si vous le donnez, le mot de passe d'un compte, demandé la première fois que vous créez ou supprimez une boîte là où Mail ne le fait pas. Il est gardé dans votre trousseau de session sous le nom *Timbre IMAP (serveur)*, où seule l'application signée de Timbre peut le lire ; supprimez-le dans Trousseaux d'accès et Timbre l'oublie.

**Ce qui quitte votre Mac.** Vers Claude : seulement ce que Claude lit de votre courrier pour vous répondre, par l'app Claude que vous utilisez (la politique de confidentialité d'Anthropic s'y applique). Vers GitHub : une fois par jour, une demande du numéro de la dernière version, sans rien sur vous ni sur votre courrier (désactivez *Check for updates* pour l'arrêter). Vers votre propre serveur de messagerie : seulement quand vous créez ou supprimez une boîte que Mail ne gère pas, une connexion chiffrée avec l'identifiant et le mot de passe du compte.

Timbre ne collecte rien, n'a ni statistiques ni télémétrie, n'envoie rien de votre courrier à son auteur ni à personne, et n'a pas de serveur à lui.

**Contact.** [Ouvrez une issue](https://github.com/alejandrosml/timbre/issues) sur GitHub.
