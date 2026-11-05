<p align="center"><img src="assets/icon.svg" width="128" height="128" alt="Icône de Timbre : un timbre-poste avec une enveloppe"></p>

<h1 align="center">Timbre</h1>
<p align="center"><b>Le connecteur pour Mail et Claude</b><br>
Recherche instantanée dans tous vos comptes mail, lecture, brouillons et rangement en toute sécurité — la recherche se fait sur votre Mac, pas sur les serveurs de Google.</p>
<p align="center">Français · <a href="README.md">English</a></p>

---

Timbre connecte Claude à Mail sur votre Mac. C'est un serveur MCP livré en extension Claude Desktop qui s'installe en un clic, et il permet à Claude de travailler avec le courrier que vous avez déjà dans Mail, quel que soit le compte (iCloud, Gmail, Outlook et Exchange, Orange, La Poste, tout compte IMAP…). Il lit directement les données de Mail sur le disque : une recherche parmi des centaines de milliers de messages, corps compris, répond en quelques dizaines de millisecondes. Et quand vous lui demandez un brouillon, un rangement ou un drapeau, il agit dans Mail par l'interface de scripts officielle.

Il est conçu pour la réalité d'un assistant qui lit des mails dont on ne connaît pas l'auteur : les instructions cachées sont retirées et signalées, les expéditeurs usurpés sont repérés, aucun message n'est jamais supprimé définitivement, et rien n'est envoyé sans vous.

> **État :** v0.6, utilisé chaque jour sur une vraie boîte de 132 000 messages répartis sur six comptes. Vos retours sont bienvenus dans les [Issues](https://github.com/alejandrosml/timbre/issues).

## Pourquoi Timbre

- **Une seule recherche pour tout votre courrier.** iCloud, Gmail et ses libellés, Outlook, Orange, La Poste, tout compte IMAP : corps des messages et noms des pièces jointes compris, en 20 à 40 ms sur 132 000 messages, avec les opérateurs de Gmail et les dossiers nommés comme Mail les affiche.
- **Rien de perdu, rien de caché.** Les mises en page des boutiques et des lettres d'information sont lues en entier, comme le texte placé après une pièce jointe ; le texte qu'on vous cache mais qui vise une IA est retiré et signalé.
- **L'hameçonnage repéré.** Expéditeurs usurpés, liens qui mentent sur leur destination (même derrière les liens protégés d'Outlook), premiers contacts, pièces jointes dangereuses.
- **Les pièces jointes lues.** PDF, Word, RTF, ODT, texte, CSV, invitations, fiches contact, mails transférés.
- **Des actions sans risque.** Les brouillons s'ouvrent dans Mail et ne partent jamais seuls ; archives et corbeille s'annulent ; les dossiers se créent, et les vides se suppriment, même là où Mail ne le permet à aucune app ; Timbre dit clairement ce qu'il ne peut pas faire, au lieu de faire semblant.
- **Privé et digne de confiance.** La recherche se fait sur votre Mac, pas sur un serveur ; une seule application, signée et notarisée par Apple, qui détient sa propre autorisation ; rien d'autre à installer, rien à redémarrer.

## Concrètement

Demandez à Claude avec vos mots ; Timbre se charge de chercher et de lire :

- *« Retrouve la facture d'électricité de septembre et donne-moi le montant. »* Timbre la trouve, quel que soit le compte où elle est arrivée, et lit le PDF joint.
- *« Qu'est-ce que le notaire m'a envoyé au sujet de la vente ? »* Le corps des messages et le nom des pièces jointes sont fouillés aussi, accents et pluriels compris (*facture* trouve *factures*).
- *« Résume ma conversation avec Sophie sur les vacances. »* Le fil entier, chaque message réduit à ce qu'il apporte.
- *« Ce mail de ma banque est-il authentique ? »* Authentification de l'expéditeur, adresses qui en imitent d'autres, liens qui mentent sur leur destination, texte caché.
- *« Prépare une réponse pour dire que jeudi me va. »* Le brouillon s'ouvre dans Mail pour que vous le relisiez ; rien n'est envoyé.
- *« Archive les lettres d'information du mois dernier. »* Les déplacements s'annulent.
- *« Supprime les dossiers vides de mon compte La Poste. »* Chacun est d'abord vérifié vide sur le serveur ; le mot de passe est demandé une seule fois, dans une fenêtre de Timbre, la première fois.

## La recherche se fait sur votre Mac, pas sur les serveurs de Google

Le connecteur Gmail de Claude envoie chaque recherche aux serveurs de Google par l'API Gmail, et n'atteint que Gmail. Timbre cherche dans un index gardé sur votre Mac, construit à partir du courrier que Mail a déjà téléchargé, pour tous vos comptes à la fois.

| | **Timbre** | Connecteur Gmail |
|---|---|---|
| Où se fait la recherche | Sur votre Mac | Sur les serveurs de Google (API Gmail) |
| Comptes | Tous : iCloud, Gmail, Outlook et Exchange, Orange, La Poste, tout IMAP | Gmail seulement |
| Ce que reçoit Google | Rien : Mail synchronise votre courrier comme d'habitude | Chaque recherche et chaque message lu |
| Accès que vous accordez | La lecture des fichiers de Mail sur ce Mac, à Timbre seul | Votre compte Google, par la connexion Google |
| Durée d'une recherche sur 132 000 messages | 20 à 40 ms | Un aller-retour réseau par recherche |

Dans les deux cas, ce que Claude lit part chez Claude pour vous répondre.

**Vous avez aussi le connecteur Gmail ?** Inutile de l'enlever : Timbre demande à Claude de passer par lui en premier pour tout ce qui touche à votre courrier, Gmail compris. Gardez le connecteur Gmail pour ce que Timbre ne fait pas, comme archiver le courrier Gmail ou le ranger sous un libellé (Mail ne le fait pas pour un script), ou envoyer depuis Gmail quand l'envoi est désactivé dans Timbre. Si Claude part quand même vers Gmail, dites-lui simplement « cherche avec Timbre ».

## Ce qu'il fait

- **Tout retrouver.** Recherche plein texte dans les sujets, les personnes, le corps des messages et le nom des pièces jointes, classée par pertinence, par date et selon que vous échangez avec l'expéditeur, un seul résultat par message même quand Gmail le range sous plusieurs libellés, insensible aux accents et aux ligatures (`réunion` = `reunion`, `cœur` = `coeur`), avec des filtres et les opérateurs à la Gmail : `from:`, `to:`, `cc:`, `subject:`, `in:`, `label:`, `is:unread`, `is:flagged`, `has:attachment`, `filename:`, `after:`, `before:`, `newer_than:7d`, `older_than:1y`, `"expression exacte"` (aussi après un opérateur : `from:"Jean Dupont"`), `préfixe*` (3 lettres minimum), `-exclure` (aussi `-from:`, `-"une expression"`), `OR`. Les indésirables et la corbeille sont laissés de côté, mais on peut y chercher des mots quand on le demande.
- **Lire sans risque.** Le texte que vous voyez dans Mail, sans le contenu HTML caché mais sans rien perdre de ce qui s'affiche (mises en page MJML des boutiques en ligne, texte après une pièce jointe insérée dans le message, version texte des mails faits d'images), avec des alertes sur l'authenticité de l'expéditeur, sur les liens trompeurs et sur les pièces jointes dangereuses (programmes, macros, pages web).
- **Suivre une conversation.** Le fil entier, chaque message réduit à ce qu'il apporte de nouveau.
- **Lire les pièces jointes.** PDF, Word, RTF et ODT (convertis par macOS dans un processus séparé), texte, HTML, CSV, invitations de calendrier, fiches contact et mails joints.
- **Agir, de façon réversible.** Les brouillons (nouveau, réponse, réponse à tous, transfert) s'ouvrent dans Mail pour que vous les relisiez ; marquer lu ou non lu, poser un drapeau ; déplacer vers les archives, la corbeille ou n'importe quelle boîte — avec **annulation**. Un compte sans boîte d'archives en reçoit une, comme avec le bouton Archiver de Mail. Gmail fait exception : Mail ne laisse un script déplacer le courrier Gmail que vers la corbeille ou les indésirables, et Timbre le dit au lieu de faire semblant.
- **Créer et supprimer des boîtes.** Dossiers et libellés Gmail se créent, et les vides se suppriment, même là où Mail ne le permet à aucune app : Timbre agit alors sur le serveur de messagerie lui-même, avec un mot de passe demandé une seule fois ([comment et pourquoi](#créer-et-supprimer-des-boîtes-aux-lettres)).
- **Envoyer, seulement si vous l'autorisez.** Désactivé par défaut. Une fois activé, chaque envoi demande deux étapes explicites, avec une liste facultative de destinataires autorisés et un plafond par heure.

## Ce qui change par rapport à l'existant

Timbre reprend les meilleures idées des connecteurs Apple Mail open source et ajoute ce qu'aucun ne réunit. Les informations ci-dessous viennent de la documentation ou du code de chaque projet (novembre 2026) ; « — » signifie que le projet ne le décrit pas.

| | **Timbre** | [sweetrb](https://github.com/sweetrb/apple-mail-mcp) | [patrickfreyer](https://github.com/patrickfreyer/apple-mail-mcp) | [imdinu](https://github.com/imdinu/apple-mail-mcp) | [apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp) | [falconbradley](https://github.com/falconbradley/claude-connector-apple-mail) |
|---|---|---|---|---|---|---|
| Fonctionne comme | Application native (Rust), signée et notarisée | Node.js 20+ | Python | Python 3.11+ | Python 3.10+ | Python 3.11+ (uv) |
| Lit le courrier par | La base et les fichiers de Mail | AppleScript (IMAP en option) | AppleScript | La base et les fichiers de Mail | AppleScript (IMAP en option) | La base et les fichiers de Mail |
| Recherche dans le corps des messages | ✅ index classé, nouveau courrier en quelques secondes | AppleScript (lent sur les grosses boîtes, selon sa doc), ou IMAP une fois configuré | AppleScript | ✅ index, construit par une commande, tenu à jour avec `--watch` | AppleScript, ou IMAP une fois configuré | sujet et expéditeur |
| Texte caché et instructions piégées | retirés, signalés, balisés | — | — | — | — | — |
| Contrôle de l'expéditeur et des liens | SPF/DKIM/DMARC, liens trompeurs, premier contact | — | — | — | — | — |
| Texte des pièces jointes (PDF, Word…) | ✅ | enregistrées sur le disque | enregistrées sur le disque | ✅ | ✅ | contenu brut |
| Brouillons, réponses, transferts | ✅ | ✅ | ✅ | ❌ lecture seule | ✅ | ✅ |
| Déplacer, archiver, corbeille | ✅ avec annulation | ✅ | ✅ | ❌ | ✅ | ❌ |
| Créer ou supprimer des boîtes | ✅ sur le serveur, mot de passe demandé une fois dans une fenêtre, au premier besoin ; boîtes vides seulement | ✅ par IMAP, mot de passe rangé à l'avance dans le Terminal ; supprime avec ce qu'elle contient | — | ❌ | ✅ suppression par IMAP, mot de passe rangé à l'avance par une commande d'installation ; une boîte pleine seulement sur demande | ❌ |
| Envoi | désactivé par défaut, en deux étapes | activé par défaut | activé par défaut | — | avec confirmation | aucun |
| Permission macOS accordée à | l'application de Timbre | Node.js | Python | Terminal et l'app qui le lance | Python | le lanceur `uv` |
| Licence | MIT | MIT | MIT | GPL-3.0 | MIT | MIT |

À notre connaissance, Timbre est le seul connecteur pour Mail qui réunisse un index plein texte classé du corps des messages, la protection contre les instructions cachées, la vérification des expéditeurs et des liens, l'annulation, la gestion des boîtes sans rien préparer à l'avance, et une application signée et notarisée qui détient sa propre autorisation. sweetrb propose plus d'outils pour piloter Mail lui-même (règles, boîtes intelligentes, modèles) ; Timbre s'en tient à ce dont un assistant a besoin pour trouver, lire et ranger votre courrier en toute sécurité.

**Ce que Timbre apporte de nouveau**

- **Tous les comptes, libellés compris.** Gmail ne garde chaque message qu'une fois, dans « Tous les messages » : Timbre lit ses libellés, si bien que la boîte de réception, les envoyés et vos libellés Gmail fonctionnent comme n'importe quelle boîte. Les envoyés d'Orange, Wanadoo et La Poste (OUTBOX) et leurs indésirables (QUARANTAINE) sont reconnus aussi.
- **Un classement sensé.** Une recherche vague (*facture*) met le courrier récent en tête ; les messages des personnes à qui vous écrivez remontent ; ceux que vous vous envoyez à vous-même, non.
- **Rien à redémarrer.** Dès que vous accordez l'accès complet au disque, Timbre s'en aperçoit en quelques secondes et se met au travail, dans la même conversation. Il vous prévient aussi quand une nouvelle version sort.
- **Texte caché retiré.** `display:none`, polices de taille nulle, blanc sur blanc, texte hors écran, rogné, rétréci, flouté ou transparent, texte qu'un filtre ou un mode de fusion fait disparaître, caractères invisibles et de contrôle bidirectionnel : tout cela est retiré de ce que lit Claude, compté, et analysé pour y repérer des instructions destinées à une IA. Les feuilles de style sont lues avec l'analyseur CSS et le moteur de sélecteurs de Firefox, et jugées comme Mail les affiche sur un Mac : couches (`@layer`), règles imbriquées, `@container`, `@scope`, `::first-line`, animations et arbres fantômes (shadow DOM) compris. Quand ce qu'un message montre dépend de la largeur de la fenêtre de Mail, seul ce qui se voit quelle que soit la largeur est gardé.
- **Claude lit ce que vous voyez.** Timbre lit la partie HTML que Mail affiche, et non la version texte brut qu'un attaquant peut remplir d'un autre contenu (et il prévient quand les deux diffèrent).
- **Contrôle de l'expéditeur.** Résultats SPF, DKIM et DMARC, nom affiché contenant une autre adresse, réponse redirigée vers un autre domaine, domaines en punycode, liens dont le texte affiche un domaine mais qui mènent ailleurs, et *premier contact* (un expéditeur avec qui vous n'avez jamais échangé).
- **Contenu balisé.** Chaque texte de mail est encadré par un marqueur aléatoire que l'expéditeur ne peut pas deviner.
- **Pièces jointes.** Les exécutables, les doubles extensions (`facture.pdf.app`) et les fichiers à macros sont signalés. Les fichiers enregistrés vont dans un seul dossier de votre choix, ne sont jamais écrasés et sont marqués pour que Gatekeeper les contrôle.
- **Le minimum de droits.** Les outils de lecture sont déclarés en lecture seule, Claude peut donc s'en servir sans vous demander ; ceux qui modifient quelque chose sont déclarés comme tels ; les outils d'envoi n'existent même pas tant que vous ne les avez pas activés.
- **Une permission, pour Timbre seul.** L'accès complet au disque est donné à l'application signée de Timbre, avec son nom et son icône, ni à Claude ni à un lanceur partagé avec d'autres extensions : Timbre indique à macOS qu'il répond lui-même de ses accès.

## Créer et supprimer des boîtes aux lettres

Mail ne laisse aucun script supprimer une boîte aux lettres, et sur les versions actuelles de macOS il refuse aussi d'en créer, à La Poste par exemple : ses scripts répondent « Le gestionnaire AppleEvent a échoué » (sweetrb a mesuré la même chose pour tous les types de comptes). Timbre fait donc ce que fait le menu Boîte aux lettres de Mail : il agit sur le serveur de messagerie lui-même, par IMAP, puis demande à Mail de se mettre à jour. Pour créer une boîte, il demande quand même d'abord à Mail, au cas où il accepterait : aucun mot de passe n'est alors nécessaire.

- **Ce que vous faites, une fois par compte.** Rien à l'installation. La première fois que vous demandez à Claude de créer ou de supprimer un dossier dans un compte où Mail ne le fait pas, une fenêtre de Timbre demande le mot de passe de ce compte et explique pourquoi en une phrase. Le serveur le vérifie ; il reste ensuite dans votre trousseau de session, où seule l'application signée de Timbre peut le lire, et ne vous est plus jamais demandé. Claude ne le voit jamais. Google, Apple et Yahoo n'acceptent ici qu'un mot de passe d'application : le bouton de la fenêtre ouvre la page qui le crée.
- **Ce que Timbre vérifie.** Une boîte n'est supprimée que si le serveur dit qu'elle ne contient ni message ni autre boîte, et si ni le serveur ni Mail ne la gardent pour eux (réception, envoyés, brouillons, corbeille, indésirables, courrier en attente d'envoi, Notes d'Apple, dossiers propres à Gmail, et toute boîte que le serveur marque comme sienne). Une boîte d'archives vide peut partir : Mail la recrée au prochain archivage. Aucun message n'est supprimé avec elle : déplacez-les d'abord, ce qui s'annule. La connexion est chiffrée, et son certificat vérifié par macOS comme Mail le vérifie.
- **Mail qui se met à jour.** Une boîte créée sur le serveur est apparue dans Mail en quelques secondes dans nos essais. Une boîte supprimée sur le serveur reste dans la barre latérale de Mail jusqu'à sa prochaine ouverture, qui la retire d'elle-même : rien à faire, et quitter puis rouvrir Mail la fait disparaître tout de suite. Timbre dit ce qu'il en est.
- **Là où ce n'est pas possible.** Comptes Exchange et Outlook.com (Microsoft n'accepte que sa propre connexion, que Mail garde pour lui), comptes POP et boîtes « Sur mon Mac » (gardées par Mail seul) : là, le menu Boîte aux lettres de Mail s'en charge.

Trois autres voies ont été étudiées puis écartées :

- *Modifier la base de Mail pendant que Mail est fermé.* C'est la copie, par Mail, de ce que contient le serveur : une boîte supprimée là revient à la vérification suivante de Mail.
- *Réutiliser les mots de passe que garde Mail, déverrouillés avec le mot de passe de votre Mac.* Les versions actuelles de macOS les rangent dans la partie protégée du trousseau (« Éléments locaux »), que seules les apps d'Apple peuvent ouvrir : aucune fenêtre ne laisse une autre app y entrer. Timbre regarde quand même là où les anciennes versions de macOS les rangeaient, le trousseau de session, où macOS demande le mot de passe de votre Mac avant de le laisser en lire un.
- *Piloter les menus de Mail.* Cela demande l'autorisation Accessibilité, qui permet de contrôler tout le Mac, et se casse dès que Mail change ses menus ou parle une autre langue.

sweetrb et apple-mail-fast-mcp passent aussi par IMAP, avec un mot de passe que vous créez et rangez vous-même à l'avance, dans le Terminal ou par une commande d'installation. Tous deux le rangent avec l'outil `security` de macOS, qui le donne ensuite sans rien demander à tout programme qui utilise ce même outil. sweetrb supprime une boîte avec tout ce qu'elle contient ; apple-mail-fast-mcp refuse une boîte qui n'est pas vide, sauf si on lui demande d'en supprimer aussi les messages ; Timbre ne supprime jamais un message avec une boîte.

## Signé et notarisé

Chaque version est signée avec le Developer ID de son auteur (Alejandro San Martin Lamas, équipe `4FQA525DB9`) et notarisée par Apple. Pour vous, cela signifie :

- **Il vient bien de son auteur, sans modification.** macOS vérifie la signature au lancement ; une copie modifiée par quelqu'un d'autre échoue à ce contrôle.
- **Contrôlé par Apple.** Avant publication, le service de notarisation d'Apple l'a analysé à la recherche de contenus malveillants connus. macOS l'ouvre sans l'avertissement sur les développeurs non identifiés, et Apple peut le révoquer si un problème apparaissait.
- **Votre autorisation survit aux mises à jour.** macOS rattache l'accès complet au disque à la signature de Timbre, pas à un fichier précis : une nouvelle version le conserve.
- **Ce que ce n'est pas.** La notarisation est une analyse automatique, pas un examen de ce que fait le programme. Pour cela, le code de Timbre est ouvert.

Pour le vérifier vous-même :

```sh
APP="$HOME/Library/Application Support/Claude/Claude Extensions/local.mcpb.alejandro-san-martin-lamas.timbre/server/Timbre.app"
codesign -dv --verbose=2 "$APP" 2>&1 | grep Authority   # Developer ID Application: Alejandro San Martin Lamas (4FQA525DB9)
spctl --assess -vv "$APP"                                # accepted, source=Notarized Developer ID
```

## Performances

Une boîte synthétique de 20 000 messages (788 Mo : lettres d'information en HTML, réponses avec historique cité, pièces jointes, textes en français, anglais et espagnol au vocabulaire réaliste), indexée sur une petite machine virtuelle à 2 cœurs :

| | v0.1.0 | v0.4.0 | v0.5.7 |
|---|---|---|---|
| Construction complète de l'index | 28,6 s | 13,0 s | 17,0 s |
| Taille de l'index | 298 Mo | 124 Mo | 126 Mo |
| Recherche, médiane | 42 ms | 18 ms | 28 ms |
| Recherche, 95ᵉ centile | 62 ms | 30 ms | 45 ms |

La 0.5.7 est un peu plus lente parce qu'elle est plus exacte : elle lit chaque partie de chaque message et détermine le texte visible comme le fait le moteur WebKit de Mail, elle indexe aussi les indésirables et la corbeille (que la recherche laisse toujours de côté), et elle classe une recherche large parmi ses résultats les plus récents selon leur date plutôt que selon l'ordre où Mail les a enregistrés : le courrier d'un compte ajouté après coup ne peut plus évincer celui du mois.

Ce qui a fait la différence : les messages sont analysés sur tous les cœurs pendant qu'un seul fil d'écriture enregistre de grosses transactions, avec les fusions de FTS5 repoussées à la fin et une table de termes en attente de 64 Mio ; le texte gardé pour les extraits n'est stocké qu'une fois, compressé en zstd ; il n'y a plus d'index de préfixes ; extraits et détails ne sont calculés que pour la page renvoyée, et les requêtes très larges sont classées parmi leurs 10 000 résultats les plus récents. Les vérifications utiles seulement à la lecture d'un message ne sont plus faites à l'indexation. Pour reproduire : `timbre dev-corpus /tmp/corpus 20000` (compilation de débogage), puis `timbre index --rebuild` et `timbre bench` en compilation optimisée, avec `TIMBRE_MAIL_ROOT`, `TIMBRE_ACCOUNTS_DB` et `TIMBRE_DATA_DIR` pointant dessus.

Sur un vrai Mac (Apple silicon, 132 000 messages répartis sur six comptes, dont Gmail), l'index occupe environ 500 Mo et les recherches répondent en 20 à 40 ms.

## Installation

1. Téléchargez `timbre-<version>.mcpb` dans les [Releases](https://github.com/alejandrosml/timbre/releases) et double-cliquez dessus : Claude Desktop l'installe. Les versions publiées sont signées avec un Developer ID et notarisées par Apple.
2. Donnez à Timbre l'**accès complet au disque**. Demandez à Claude d'*ouvrir les réglages d'autorisation de Timbre* : Réglages Système s'ouvre sur Confidentialité et sécurité › Accès complet au disque et le Finder sélectionne Timbre (l'icône du timbre-poste). Glissez-le dans la liste et activez-le. Timbre se met au travail tout seul quelques secondes plus tard : rien à redémarrer.
3. La première fois que vous demandez un brouillon ou un déplacement, macOS demande si Timbre peut contrôler Mail : cliquez sur OK.

Rien d'autre n'est demandé à l'installation. Un mot de passe n'est demandé que la première fois que vous créez ou supprimez un dossier dans un compte où Mail ne le fait pas, et une seule fois.

L'index plein texte se construit en arrière-plan au premier lancement (environ une minute pour 100 000 messages sur un Mac récent) ; en attendant, la recherche porte sur les sujets et les expéditeurs. Ensuite, un nouveau mail est cherchable en quelques secondes. Après une mise à jour qui lit mieux les messages, Timbre les relit en arrière-plan, les plus récents d'abord, sans interrompre la recherche.

## Outils

| Outil | Rôle | Modifie quelque chose ? |
|---|---|---|
| `search_mail` | Recherche plein texte avec filtres | non |
| `read_mail` | Un message, avec alertes et pièces jointes | non |
| `get_thread` | Une conversation entière | non |
| `list_mailboxes` | Comptes et boîtes, avec leurs compteurs | non |
| `read_attachment` | Texte d'une pièce jointe : PDF, Word, RTF, ODT, texte, HTML, CSV, agenda, mail joint | non |
| `mail_status` | Permissions, avancement de l'index, réglages, nouvelle version | non |
| `open_permission_settings` | Ouvrir les réglages d'accès complet au disque (ou d'automatisation) et montrer Timbre | ouvre Réglages Système |
| `selected_mail` | Messages sélectionnés dans Mail | non |
| `open_in_mail` | Ouvrir un message dans Mail | non |
| `save_attachment` | Enregistrer dans votre dossier de pièces jointes | ajoute un fichier |
| `create_draft` | Nouveau, réponse, réponse à tous, transfert — en brouillon | ajoute un brouillon |
| `mark_mail` | Lu ou non lu, drapeau | oui |
| `move_mail` / `undo_move` | Archives, corbeille, n'importe quelle boîte — et retour (Gmail : corbeille et indésirables) | oui, réversible |
| `create_mailbox` | Créer une boîte ou un libellé Gmail : par Mail, ou sur le serveur | ajoute une boîte |
| `delete_mailbox` | Supprimer une boîte ou un libellé Gmail vide, sur le serveur | oui, boîtes vides seulement |
| `prepare_send` / `confirm_send` | Envoi en deux étapes (seulement si activé) | envoie |

## Questions

**Comment connecter Claude à Mail ?** Installez Timbre dans Claude Desktop (téléchargez le `.mcpb` dans les [Releases](https://github.com/alejandrosml/timbre/releases) et ouvrez-le), donnez-lui l'accès complet au disque quand Claude vous le propose, puis parlez à Claude de votre courrier avec vos mots.

**Quels comptes fonctionnent ?** Tous ceux configurés dans Mail : iCloud, Gmail et Google Workspace (libellés compris), Outlook et Exchange, Yahoo, Orange, Free, SFR, La Poste et tout compte IMAP ou POP, ainsi que les boîtes « Sur mon Mac ».

**Mon courrier quitte-t-il mon Mac ?** La recherche et l'index restent sur votre Mac. Seul ce que Claude lit pour vous répondre part chez Claude. Timbre lui-même ne collecte rien.

**Claude peut-il envoyer des mails ou supprimer mon courrier ?** L'envoi est désactivé tant que vous ne l'activez pas, et chaque message demande alors deux étapes explicites. Aucun message n'est jamais supprimé définitivement : un passage à la corbeille s'annule, et seules les boîtes vides sont supprimées.

**Est-il protégé contre l'hameçonnage et l'injection d'instructions ?** Le texte qu'on vous cache mais qui vise une IA est retiré et signalé, chaque mail est encadré comme non fiable, et les expéditeurs usurpés et les liens trompeurs sont signalés.

**Faut-il le connecteur Gmail, ou peut-on s'en servir sans Claude Desktop ?** Aucun connecteur n'est nécessaire : Timbre lit tous les comptes à travers Mail. Il est fait pour Claude Desktop sur macOS 12 ou plus récent ; d'autres clients MCP sur Mac peuvent aussi lancer son programme.

## Réglages

Dans Claude › Réglages › Extensions › Timbre : mode lecture seule, autorisation d'envoi, destinataires autorisés (`alice@example.com, @monentreprise.fr`), nombre maximal de messages par heure, boîtes exclues de la recherche (par défaut : indésirables et corbeille ; laissez le champ vide pour tout chercher, et écrivez les rôles comme vous voulez : `spam`, `corbeille`, `archive`…), dossier des pièces jointes et vérification quotidienne des mises à jour.

## Confidentialité

La politique complète est dans [PRIVACY.md](PRIVACY.md#politique-de-confidentialité). La recherche, la lecture et l'index restent sur votre Mac : Timbre n'envoie rien de votre courrier nulle part et ne collecte rien. Il ne va sur le réseau que pour deux choses : une vérification quotidienne du numéro de la dernière version sur GitHub (désactivez *Check for updates* pour l'arrêter) et, quand vous créez ou supprimez une boîte que Mail ne gère pas, une connexion chiffrée à votre propre serveur de messagerie. Le mot de passe nécessaire reste dans votre trousseau de session sous le nom *Timbre IMAP (serveur)* ; supprimez-le là et Timbre l'oublie. Ce que Claude lit part chez Claude, pour vous répondre. L'index (`~/Library/Application Support/Timbre`) contient le texte de vos messages pour la recherche ; supprimez ce dossier pour l'effacer. La base de Mail n'est jamais ouverte qu'en lecture seule.

## Compiler soi-même

```sh
cargo test                      # tests unitaires et de bout en bout sur une boîte de synthèse
cargo build --release           # sur macOS
scripts/build-macos-from-linux.sh   # compilation croisée depuis Linux (cargo-zigbuild)
scripts/package-mcpb.sh         # assembler l'extension Claude Desktop
timbre index | timbre search "facture from:orange" | timbre doctor | timbre bench
```

## Historique des étoiles

<a href="https://www.star-history.com/#alejandrosml/timbre&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date" />
    <img alt="Historique des étoiles de Timbre" src="https://api.star-history.com/svg?repos=alejandrosml/timbre&type=Date" />
  </picture>
</a>

## Remerciements

Créé par **Alejandro San Martin Lamas** — [github.com/alejandrosml](https://github.com/alejandrosml).

Timbre s'appuie sur ce que d'autres ont découvert avant lui, avec nos remerciements : la lecture de l'Envelope Index et du champ `flags` documentées par [falconbradley/claude-connector-apple-mail](https://github.com/falconbradley/claude-connector-apple-mail), les techniques de pilotage de Mail de [sweetrb/apple-mail-mcp](https://github.com/sweetrb/apple-mail-mcp), l'idée d'index FTS5 des corps de message d'[imdinu/apple-mail-mcp](https://github.com/imdinu/apple-mail-mcp) et les confirmations d'[apple-mail-fast-mcp](https://github.com/s-morgan-jeffries/apple-mail-fast-mcp). Le code de Timbre est original et écrit en Rust.

## Licence

[MIT](LICENSE) © 2026 Alejandro San Martin Lamas
