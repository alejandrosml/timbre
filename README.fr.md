<p align="center"><img src="assets/icon.png" width="128" alt="Icône de Timbre"></p>

<h1 align="center">Timbre</h1>
<p align="center"><b>Le connecteur pour Mail et Claude</b><br>
Recherche instantanée dans tout votre courrier, lecture, brouillons et rangement en toute sécurité — sur votre Mac, sans que rien n'en sorte.</p>
<p align="center">Français · <a href="README.md">English</a></p>

---

Timbre permet à Claude de travailler avec le courrier que vous avez déjà dans Mail, quel que soit le compte (iCloud, Gmail, Exchange, IMAP…). Il lit directement les données de Mail sur le disque : une recherche parmi des centaines de milliers de messages, corps compris, répond en quelques dizaines de millisecondes. Et quand vous lui demandez un brouillon, un rangement ou un drapeau, il agit dans Mail par l'interface de scripts officielle.

Il est conçu pour la réalité d'un assistant qui lit des mails dont on ne connaît pas l'auteur : les instructions cachées sont retirées et signalées, les expéditeurs usurpés sont repérés, rien n'est jamais supprimé définitivement, et rien n'est envoyé sans vous.

> **État :** début de développement (v0.2), en test sur une vraie boîte de 250 000 messages. Vos retours sont bienvenus dans les [Issues](https://github.com/alejandrosml/timbre/issues).

## Ce qu'il fait

- **Tout retrouver.** Recherche plein texte dans les sujets, les personnes, le corps des messages et le nom des pièces jointes, classée par pertinence et par date, insensible aux accents et aux ligatures (`réunion` = `reunion`, `cœur` = `coeur`), avec des filtres et les opérateurs à la Gmail : `from:`, `to:`, `subject:`, `in:`, `is:unread`, `is:flagged`, `has:attachment`, `after:`, `before:`, `"expression exacte"`, `préfixe*` (3 lettres minimum), `-exclure`, `OR`.
- **Lire sans risque.** Le texte que vous voyez dans Mail, sans le contenu HTML caché, avec des alertes sur l'authenticité de l'expéditeur et sur les pièces jointes dangereuses.
- **Suivre une conversation.** Le fil entier, chaque message réduit à ce qu'il apporte de nouveau.
- **Lire les pièces jointes.** Texte, HTML, CSV, invitations de calendrier, fiches contact et mails joints.
- **Agir, de façon réversible.** Les brouillons (nouveau, réponse, réponse à tous, transfert) s'ouvrent dans Mail pour que vous les relisiez ; marquer lu ou non lu, poser un drapeau ; déplacer vers les archives, la corbeille ou n'importe quelle boîte — avec **annulation**.
- **Envoyer, seulement si vous l'autorisez.** Désactivé par défaut. Une fois activé, chaque envoi demande deux étapes explicites, avec une liste facultative de destinataires autorisés et un plafond par heure.

## Ce qui change par rapport à l'existant

Timbre reprend les meilleures idées des connecteurs Apple Mail open source existants et ajoute ce qu'aucun ne réunissait. Les informations ci-dessous viennent du code ou de la documentation de chaque projet (novembre 2026).

| | **Timbre** | [falconbradley](https://github.com/falconbradley/claude-connector-apple-mail) | [sweetrb](https://github.com/sweetrb/apple-mail-mcp) | [imdinu](https://github.com/imdinu/apple-mail-mcp) | [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp) |
|---|---|---|---|---|---|
| Technologie | Binaire natif (Rust), rien à installer | Python (uv) | Node.js | Python | Python |
| Lit directement la base de Mail | ✅ | ✅ (repli sur AppleScript) | en option | ✅ | ❌ AppleScript / IMAP |
| Recherche dans le corps des messages | ✅ classée, mise à jour en continu | ❌ sujet et expéditeur | index en option | ✅ | via le serveur IMAP |
| Brouillons, réponses, transferts | ✅ | ✅ | ✅ | ❌ lecture seule | ✅ |
| Déplacer, archiver, corbeille | ✅ avec annulation | ❌ | ✅ | ❌ | ✅ |
| Envoi | désactivé par défaut, en deux étapes | désactivé par défaut | toujours disponible | — | avec confirmation |
| Permission macOS accordée à | son propre binaire signé | le lanceur `uv` partagé par les extensions Python | Node.js | Python / Terminal | Python |
| Licence | MIT | MIT | MIT | GPL-3.0 | MIT |

**Ce que Timbre apporte de nouveau**

- **Texte caché retiré.** `display:none`, polices de taille nulle, blanc sur blanc, texte hors écran ou transparent, caractères invisibles et de contrôle bidirectionnel : tout cela est retiré de ce que lit Claude, compté, et analysé pour y repérer des instructions destinées à une IA.
- **Claude lit ce que vous voyez.** Timbre lit la partie HTML que Mail affiche, et non la version texte brut qu'un attaquant peut remplir d'un autre contenu (et il prévient quand les deux diffèrent).
- **Contrôle de l'expéditeur.** Résultats SPF, DKIM et DMARC, nom affiché contenant une autre adresse, réponse redirigée vers un autre domaine, domaines en punycode, liens dont le texte affiche un domaine mais qui mènent ailleurs, et *premier contact* (un expéditeur avec qui vous n'avez jamais échangé).
- **Contenu balisé.** Chaque texte de mail est encadré par un marqueur aléatoire que l'expéditeur ne peut pas deviner.
- **Pièces jointes.** Les exécutables, les doubles extensions (`facture.pdf.app`) et les fichiers à macros sont signalés. Les fichiers enregistrés vont dans un seul dossier de votre choix, ne sont jamais écrasés et sont marqués pour que Gatekeeper les contrôle.
- **Le minimum de droits.** Les outils de lecture sont déclarés en lecture seule, Claude peut donc s'en servir sans vous demander ; ceux qui modifient quelque chose sont déclarés comme tels ; les outils d'envoi n'existent même pas tant que vous ne les avez pas activés.
- **Une permission, pour Timbre seul.** L'accès complet au disque est donné au binaire signé de Timbre, pas à un lanceur partagé avec d'autres extensions.

## Performances

Une boîte synthétique de 20 000 messages (788 Mo : lettres d'information en HTML, réponses avec historique cité, pièces jointes, textes en français, anglais et espagnol au vocabulaire réaliste), indexée sur une petite machine virtuelle à 2 cœurs :

| | v0.1.0 | v0.2.0 |
|---|---|---|
| Construction complète de l'index | 28,6 s | 13,0 s |
| Taille de l'index | 298 Mo | 124 Mo |
| Recherche, médiane | 42 ms | 16 ms |
| Recherche, 95ᵉ centile | 62 ms | 29 ms |

Ce qui a fait la différence : les messages sont analysés sur tous les cœurs pendant qu'un seul fil d'écriture enregistre de grosses transactions, avec les fusions de FTS5 repoussées à la fin et une table de termes en attente de 64 Mio ; le texte gardé pour les extraits n'est stocké qu'une fois, compressé en zstd ; il n'y a plus d'index de préfixes ; extraits et détails ne sont calculés que pour la page renvoyée, et les requêtes très larges sont classées parmi leurs 10 000 résultats les plus récents. Les vérifications utiles seulement à la lecture d'un message ne sont plus faites à l'indexation. Pour reproduire : `timbre dev-corpus /tmp/corpus 20000` (compilation de débogage), puis `timbre index --rebuild` et `timbre bench` en compilation optimisée, avec `TIMBRE_MAIL_ROOT`, `TIMBRE_ACCOUNTS_DB` et `TIMBRE_DATA_DIR` pointant dessus. Les chiffres d'une vraie boîte de 250 000 messages sur Mac suivront.

## Installation

1. Téléchargez `timbre-<version>.mcpb` dans les [Releases](https://github.com/alejandrosml/timbre/releases) et double-cliquez dessus : Claude Desktop l'installe.
2. Donnez à Timbre l'**accès complet au disque** (Réglages Système › Confidentialité et sécurité › Accès complet au disque › +). Demandez à Claude l'*état de Timbre* : il vous indique le fichier exact à ajouter. Désactivez puis réactivez ensuite l'extension dans les réglages de Claude.
3. La première fois que vous demandez un brouillon ou un déplacement, macOS demande si Timbre peut contrôler Mail : cliquez sur OK.

L'index plein texte se construit en arrière-plan au premier lancement (quelques minutes pour une grosse boîte) ; en attendant, la recherche porte sur les sujets et les expéditeurs.

## Outils

| Outil | Rôle | Modifie quelque chose ? |
|---|---|---|
| `search_mail` | Recherche plein texte avec filtres | non |
| `read_mail` | Un message, avec alertes et pièces jointes | non |
| `get_thread` | Une conversation entière | non |
| `list_mailboxes` | Comptes et boîtes, avec leurs compteurs | non |
| `read_attachment` | Texte d'une pièce jointe : PDF, Word, RTF, ODT, texte, HTML, CSV, agenda, mail joint | non |
| `mail_status` | Permissions, avancement de l'index, réglages | non |
| `selected_mail` | Messages sélectionnés dans Mail | non |
| `open_in_mail` | Ouvrir un message dans Mail | non |
| `save_attachment` | Enregistrer dans votre dossier de pièces jointes | ajoute un fichier |
| `create_draft` | Nouveau, réponse, réponse à tous, transfert — en brouillon | ajoute un brouillon |
| `mark_mail` | Lu ou non lu, drapeau | oui |
| `move_mail` / `undo_move` | Archives, corbeille, n'importe quelle boîte — et retour | oui, réversible |
| `prepare_send` / `confirm_send` | Envoi en deux étapes (seulement si activé) | envoie |

## Réglages

Dans Claude › Réglages › Extensions › Timbre : mode lecture seule, autorisation d'envoi, destinataires autorisés (`alice@example.com, @monentreprise.fr`), nombre maximal de messages par heure, boîtes exclues de la recherche (par défaut : indésirables et corbeille) et dossier des pièces jointes.

## Confidentialité

Timbre n'établit aucune connexion réseau et ne collecte rien. Son index (`~/Library/Application Support/Timbre`) contient le texte de vos messages pour la recherche ; supprimez ce dossier pour l'effacer. La base de Mail n'est jamais ouverte qu'en lecture seule.

## Compiler soi-même

```sh
cargo test                      # tests unitaires et de bout en bout sur une boîte de synthèse
cargo build --release           # sur macOS
scripts/build-macos-from-linux.sh   # compilation croisée depuis Linux (cargo-zigbuild)
scripts/package-mcpb.sh         # assembler l'extension Claude Desktop
timbre index | timbre search "facture from:orange" | timbre doctor | timbre bench
```

## Remerciements

Créé par **Alejandro San Martin Lamas** — [github.com/alejandrosml](https://github.com/alejandrosml).

Timbre s'appuie sur ce que d'autres ont découvert avant lui, avec nos remerciements : la lecture de l'Envelope Index et du champ `flags` documentées par [falconbradley/claude-connector-apple-mail](https://github.com/falconbradley/claude-connector-apple-mail), les techniques de pilotage de Mail de [sweetrb/apple-mail-mcp](https://github.com/sweetrb/apple-mail-mcp), l'idée d'index FTS5 des corps de message d'[imdinu/apple-mail-mcp](https://github.com/imdinu/apple-mail-mcp) et les confirmations d'[apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp). Le code de Timbre est original et écrit en Rust.

## Licence

[MIT](LICENSE) © 2026 Alejandro San Martin Lamas
