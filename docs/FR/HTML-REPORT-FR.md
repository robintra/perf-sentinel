# Rapport HTML

`perf-sentinel report` produit un dashboard HTML autonome pour
l'exploration post-mortem d'un ensemble de traces. Il fonctionne dans deux modes :

- **Statique** (par défaut, depuis 0.5.0) : le fichier HTML embarque
  tous les panneaux et tous les arbres de traces en JSON. Pas de trafic
  réseau sortant, pas de connexion daemon. Adapté à un envoi comme
  artefact CI (GitLab Pages, GitHub Pages, Artifactory, S3 static
  hosting). La sortie est identique pour tous les utilisateurs.
- **Live** (depuis 0.5.23, opt-in via `--daemon-url`) : le fichier
  HTML contacte un daemon en runtime pour les interactions ack/revoke.
  Le dashboard ajoute des boutons `Ack`/`Revoke` par finding, un
  indicateur de statut de connexion, un panneau Acknowledgments, une
  bascule `Show acknowledged`, et un bouton de rafraîchissement manuel.
  Les panneaux statiques (Findings, Explain, pg_stat, mysql_stat, Diff,
  Correlations, Carbon) conservent leur comportement statique. Le mode
  live est purement additif.

## Mode statique

```bash
perf-sentinel report --input traces.json --output report.html
open report.html
```

C'est l'artefact que toute pipeline CI peut produire. `--sort <CLE>`
prend `impact` (le défaut) ou `severity`, mêmes clés que
`analyze --sort`. Il ordonne la liste de findings sur laquelle la page
s'ouvre. Avec `--max-traces-embedded <N>`, il décide aussi quels arbres
de spans le rapport embarque sous le plafond, le générateur gardant les
arbres des findings de tête. Sans `--daemon-url`, le HTML généré est
entièrement statique et déterministe pour la même entrée. La CSP
(Content-Security-Policy, l'en-tête navigateur qui déclare quels scripts
et ressources la page a le droit de charger) reste stricte
(`default-src 'none'`), et aucun `fetch()` n'est émis vers un hôte
quelconque.

Les heures affichées sur la page (fenêtre d'un finding, heure des spans
dans l'arbre Explain, expiration des acks en mode live) sont dans le
fuseau horaire local du lecteur, et l'infobulle d'un span garde son
horodatage exact. Le JSON embarqué et les exports CSV restent en UTC.

### Onglets de statistiques base de données

- `--pg-stat <FICHIER>` embarque un export `pg_stat_statements` CSV ou
  JSON : le dashboard gagne un onglet `pg_stat` plus la navigation
  croisée Explain vers `pg_stat` sur les spans SQL dont le template
  normalisé correspond à une ligne. `--pg-stat-prometheus <URL>`
  scrape une seule fois un `postgres_exporter` à la place d'un fichier
  (mutuellement exclusif, `--pg-stat-auth-header` optionnel), et
  `--pg-stat-top <N>` dimensionne les classements (défaut 10). Le scrape
  suppose la requête intégrée de `postgres_exporter`, qui publie
  `pg_stat_statements_seconds_total` avec un label `query`. Un exporter
  qui exécute une requête écrite à la main nomme ses propres colonnes :
  `--pg-stat-metric <SERIE>` et `--pg-stat-query-label <LABEL>` pointent
  alors le scrape vers ces noms. Sans label correspondant, l'onglet
  se rabat sur `queryid` et affiche des identifiants opaques au lieu des
  requêtes, et la navigation croisée ne peut plus rien apparier.
  Deux autres options couvrent ce qu'une requête écrite à la main change
  au-delà des noms. `--pg-stat-calls-metric <SERIE>` nomme le compteur
  d'appels, récupéré par une seconde requête et joint sur `queryid`, car
  tous les exporters publient les appels comme une série à part et non
  comme un label. Une valeur vide saute cette requête : le classement par
  appels reste alors à zéro et le classement par moyenne répète le total. `--pg-stat-unit
  seconds|milliseconds` déclare ce que compte la série de temps :
  `pg_stat_statements` compte en millisecondes, la requête intégrée de
  l'exporter convertit en secondes, et lire l'une pour l'autre se trompe
  d'un facteur mille.
- `--mysql-stat <FICHIER>` embarque un export
  `events_statements_summary_by_digest` CSV ou JSON (MySQL Performance
  Schema) : le dashboard gagne un onglet `mysql_stat` avec le même
  sous-sélecteur de classements (quatrième classement : lignes examinées).
  `--mysql-stat-top <N>` dimensionne les classements (défaut 10).
- `--mysql-stat-prometheus <URL>` récupère les mêmes digests depuis un
  `mysqld_exporter` au lieu d'un fichier, avec `--mysql-stat-auth-header`
  pour un endpoint authentifié. Il faut
  `--collect.perf_schema.eventsstatements` sur l'exporteur, désactivé par
  défaut. Le scrape suppose
  `mysql_perf_schema_events_statements_seconds_total` avec un label
  `digest_text`. Une recording rule nomme sa propre série, donc
  `--mysql-stat-metric <SERIE>` et `--mysql-stat-query-label <LABEL>`
  pointent le scrape vers ces noms. Sans label correspondant, l'onglet
  se rabat sur `digest` et affiche des hachages opaques au lieu des
  requêtes. Le collecteur publie `COUNT_STAR`, `SUM_ROWS_SENT` et
  `SUM_ROWS_EXAMINED` comme des séries à part et non comme des labels :
  une requête chacune les récupère et les joint sur l'identité du digest.
  `--mysql-stat-calls-metric <SERIE>`, `--mysql-stat-rows-sent-metric
  <SERIE>` et `--mysql-stat-rows-examined-metric <SERIE>` les nomment, et une
  valeur vide saute la requête correspondante : le classement par appels reste alors à
  zéro et le classement par moyenne répète le total. Cette identité est
  `digest` plus le label de schéma, donc
  `--mysql-stat-schema-label <LABEL>` le nomme quand une recording rule le
  renomme, faute de quoi deux schémas fusionnent en une ligne.
  `--mysql-stat-unit seconds|milliseconds|picoseconds` déclare ce que
  compte la série de temps : Performance Schema compte `SUM_TIMER_WAIT` en
  picosecondes, le collecteur convertit en secondes, et une recording rule
  transmet en général la colonne telle quelle. Un export fichier n'exige
  aucun collecteur activé sur l'exporteur, ce qui explique qu'il reste
  l'entrée recommandée.

## Fonctions interactives

Le dashboard est entièrement côté client et fonctionne hors ligne. Les
préférences d'interface (densité, tri des tableaux) persistent par
navigateur dans `localStorage`, jamais dans le fichier du rapport.

### Tri des tableaux

Chaque en-tête de tableau est cliquable. Le premier clic trie la
colonne (les colonnes numériques commencent en décroissant, le texte
en croissant), le deuxième clic inverse l'ordre, le troisième revient
à l'ordre par défaut du rapport. Shift+clic ajoute une autre colonne
comme critère de départage des ex æquo, les flèches affichent alors
leur rang (↓1, ↓2). Les pastilles de sévérité se trient par rang de
sévérité, pas par ordre alphabétique, et une ligne `pg_stat` en
surbrillance reste épinglée en tête. Le tri actif persiste par
tableau, et `Copy link` l'ajoute à l'URL partagée via la clé de hash
`tsort` pour que le destinataire retrouve le même ordre.

### Réglages d'affichage

L'engrenage à droite de la barre du haut regroupe les deux réglages
d'affichage, pour que la barre porte l'état du rapport plutôt que les
préférences. Il se déroule vers le bas, se ferme sur `Esc` ou sur un clic
à l'extérieur, et ni l'un ni l'autre ne touche aux filtres en dessous.

Le rapport s'ouvre en densité confortable. Le bouton `Comfort`/`Compact`
bascule vers une mise en page plus serrée qui affiche davantage de lignes
par écran, et le choix persiste dans le navigateur. Survoler le bouton
prévisualise le mode vers lequel il va basculer. À côté, le bouton de
thème fait défiler System, Light et Dark.

### Trier les findings

La liste s'ouvre sur `impact` décroissant, la somme des opérations d'I/O
évitables de toutes les détections partageant une signature, le problème le
plus coûteux vient donc en tête. `severity` est à un clic et classe la pire
détection unitaire. Chaque clé départage avec l'autre, et recliquer la clé
active inverse le sens. Cet ordre décide aussi quels arbres de spans le
rapport embarque quand `--max-traces-embedded` les plafonne, le générateur gardant
les arbres des findings de tête.

### Filtrer les findings

La rangée de sévérité porte une pastille par sévérité présente, chacune
avec son compte. Elles se combinent : activez `critical` et `warnings`
pour voir les deux, recliquez une pastille active pour la retirer. Aucune
pastille active affiche toutes les sévérités, il n'y a donc pas de
pastille `All`. `Clear filters`, en bout de rangée, vide toutes les
familles d'un coup et n'apparaît qu'une fois quelque chose de filtré.
`Échap` fait la même chose au clavier.

Les trois autres familles se replient chacune dans un menu, dans l'ordre
`Type`, `Service`, puis l'attribut de regroupement. Ce dernier menu prend
le nom de la clé d'attribut quand le rapport n'en contient qu'une, et
chaque option ne porte alors que la valeur. Un rapport qui mélange les
clés (par exemple `k8s.namespace.name` sur certains findings et
`service.namespace` sur d'autres) garde le nom générique `Grouping` et
écrit `clé=valeur` sur chaque option, puisque c'est la clé qui distingue
les valeurs. Chaque menu accepte plusieurs valeurs. Les valeurs d'un
même menu sont combinées par OU, et les menus entre eux par ET :
`Type : N+1 SQL, Slow SQL` avec `Service : order-svc` se lit donc
"l'un ou l'autre de ces deux problèmes, sur ce seul service". Un menu qui
filtre l'annonce une fois replié, sous la forme `Type · 2`.

Le tri occupe la rangée suivante, aligné sur la colonne des findings
qu'il ordonne, puisqu'il classe la liste au lieu de la restreindre.

Les filtres et la recherche passent tous deux par l'URL, donc
`Copy link` reproduit la vue exacte. Une valeur que le rapport ne
contient plus est écartée à la restauration plutôt qu'appliquée, ce qui
explique qu'un lien périmé s'ouvre sur une liste visible et non vide.

### Recherche

Le champ de la barre du haut est le seul champ de recherche, centré dans
la barre, et une requête unique filtre tous les onglets filtrables à la
fois : Findings, pg_stat, mysql_stat, Diff et Correlations. Chacun de
ces onglets affiche son propre nombre de correspondances dans sa
pastille de la barre latérale, si bien que vous pouvez taper depuis
n'importe quel onglet, y compris Overview et Carbon, et voir où sont les
correspondances avant de basculer. La requête est conservée au
changement d'onglet, et les correspondances de deux caractères ou plus
sont surlignées dans le panneau que vous regardez.

Les findings sont mis en correspondance sur leur sévérité, leur type
(à la fois l'identifiant brut `n_plus_one_sql` et le libellé `N+1 SQL`
affiché sur la ligne), leur service, leur endpoint et leur template SQL.
Les autres onglets sont mis en correspondance sur le texte de leurs
lignes. Le bouton `Export CSV` d'un onglet exporte ce que la requête y a
laissé visible.

`⌘K` (macOS) ou `Ctrl+K` place le focus dans le champ, `/` également.
Le champ ayant le focus, `Esc` vide la requête et restaure les pastilles.
Ouvrir un finding précis (une carte KPI de l'Overview, un top offender,
un span SQL de l'arbre de trace) vide d'abord la requête, qui masquerait
sinon la ligne même que vous ouvrez. `?` ouvre la liste complète des raccourcis.

### Cartes KPI de l'Overview

La carte `Findings` est un aplat de couleur sémantique : vert quand le
rapport est propre, bleu quand il n'y a que des findings info, orange
pour des warnings, rouge dès qu'un critique est présent. La carte
voisine promeut la sévérité la plus haute présente : son libellé, son
compte et sa teinte pastel suivent cette sévérité, et la sous-ligne ne
liste que les sévérités inférieures. La carte `Δ Baseline` passe au
rouge sur une régression nette et au vert sur une amélioration nette.
Chaque carte KPI est cliquable et mène à l'onglet correspondant,
préfiltré quand c'est pertinent (la carte de sévérité dominante ouvre
Findings filtré sur cette sévérité).

## Mode live

```bash
perf-sentinel report --input traces.json --output report.html \
  --daemon-url http://localhost:4318
open report.html
```

Le daemon doit :

1. Être joignable depuis le navigateur qui ouvre le HTML. Pour un
   poste de dev, c'est `localhost:4318`. Pour un rapport partagé via
   GitLab Pages ou GitHub Pages, le daemon doit exposer son API à un
   hôte que le navigateur peut atteindre.
2. Avoir `[daemon.cors] allowed_origins` configuré pour inclure
   l'origine du document. Voir [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md)
   pour la référence de la section. Sans ça, le navigateur rejette la
   réponse.
3. Avoir `[daemon.ack] enabled = true` (par défaut).

La première fois que l'utilisateur clique sur `Ack` ou `Revoke` sur un
daemon protégé par 401, le rapport ouvre une modale d'authentification
et demande la `X-API-Key`. La clé est stockée en `sessionStorage`
(une API navigateur qui stocke des paires clé-valeur limitées à
l'onglet courant et purgées à sa fermeture), donc elle ne persiste
jamais sur disque et ne fuit jamais vers un autre onglet.

Un `Ack` réussi affiche un toast avec un bouton `Undo` pendant huit
secondes : un clic supprime l'acquittement directement, sans la
boîte de confirmation. Le bouton `Revoke` d'une ligne garde sa
confirmation.

### CSP en mode live

Le mode live réécrit la balise meta Content-Security-Policy rendue pour
ajouter `connect-src <daemon_url>`. Toutes les autres directives
gardent leur valeur statique. L'URL du daemon est validée par le CLI
avant d'atteindre la balise meta (pas d'autre schéma que http/https, pas
de chemin, pas de userinfo, pas de query string), donc aucun octet qui
pourrait casser la CSP ne peut atterrir dans la directive.

```text
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline';
img-src data:; base-uri 'none'; form-action 'none';
connect-src http://localhost:4318
```

### Validation de la URL daemon

Le CLI rejette :

- Entrée vide
- Schémas autres que `http`/`https`
- Hôte manquant (par exemple `http://`, `http://:8080`)
- Userinfo (par exemple `http://alice@host`, la X-API-Key n'a pas sa
  place dans une URL)
- Composants de chemin (par exemple `https://example.com/v1/`, le rapport
  construit `/api/...` lui-même)
- Query strings et fragments

Une barre oblique finale sur l'autorité est silencieusement retirée par
souci d'uniformité avec le flag existant `perf-sentinel ack --daemon`.

### Avertissement mixed-content

Depuis 0.5.27, appeler `perf-sentinel
report --daemon-url http://...` avec un hôte non-loopback émet un
événement de niveau `WARN` au moment du rendu. Héberger ensuite le
HTML sur une origine HTTPS (GitLab Pages, GitHub Pages, un reverse
proxy interne en HTTPS) fait bloquer par le navigateur chaque appel
ack/revoke en mixed content, transformant silencieusement le panneau
Acks en cul-de-sac. L'avertissement détecte l'incohérence avant que
l'opérateur n'ouvre le rapport. Les URL loopback (`localhost`,
`127.0.0.1`, `[::1]`) sont exemptées car les environnements de dev font
tourner le daemon en HTTP en clair.

### Flow d'authentification

1. Démarrage : GET `/api/status` pour déterminer la connectivité.
   L'endpoint status n'est pas authentifié (lecture seule, pas de
   secrets), donc le badge de la barre du haut peut atteindre `Connected`
   sans clé.
2. Premier clic `Ack`/`Revoke` : POST ou DELETE sur
   `/api/findings/<sig>/ack`. Sur un 401, la modale d'auth s'ouvre
   avec un champ mot de passe (sans écho). La clé est stockée en
   `sessionStorage` sous `perf-sentinel.daemon.api-key` et la requête
   échouée est retentée.
3. Appels suivants : chaque requête authentifiée lit la clé depuis
   `sessionStorage` et fixe l'en-tête `X-API-Key`.
4. Fermeture de l'onglet : `sessionStorage` est purgé, le prochain
   rechargement redemande la clé au premier appel authentifié.

### Qui vit où

| Élément                                          | Mode | Détails                                                                                                      |
|--------------------------------------------------|------|--------------------------------------------------------------------------------------------------------------|
| Badge statut daemon dans la barre du haut        | Live | Trois états : `Connected` (vert), `Authentication required` (orange), `Disconnected` / `Unreachable` (rouge) |
| Bouton de rafraîchissement dans la barre du haut | Live | Récupère à nouveau `/api/status`, `/api/acks`, et réaffiche l'état live                                      |
| Boutons par ligne `Ack` / `Revoke`               | Live | Cachés en mode statique via CSS, révélés sous `body.ps-live`                                                 |
| Bascule `Show acknowledged`                      | Live | Filtre la liste statique des findings contre l'ensemble live `/api/acks`                                     |
| Panneau Acknowledgments                          | Live | Nouvel onglet `Acks` listant les acks daemon (paginé à 1000, plafond du daemon)                              |
| Modale d'authentification                        | Live | Déclenchée par le premier 401 sur un appel en écriture, jamais sur `/api/status`                             |
| Modale d'acquittement                            | Live | Déclenchée par `Ack`. Champs : reason (requis), expires (Never / 24h / 7d / 30d), by (optionnel)             |

### Limitations

- La liste des findings côté daemon n'est pas récupérée à nouveau à la
  bascule : le rapport statique est la source de vérité pour la liste des
  findings, et la bascule filtre seulement contre l'ensemble d'acks live.
  Pour voir les findings que le daemon a retenus au-delà de l'instantané
  statique, utilisez `perf-sentinel query findings --include-acked`
  ou l'API HTTP daemon directement.
- Pas de rafraîchissement automatique. Le navigateur n'interroge pas le
  daemon en permanence. Utilisez le bouton de rafraîchissement manuel. La
  supervision temps réel relève de Grafana, pas d'un artefact HTML par MR.
- Pas de lien croisé `Explain` par ligne en mode live au-delà du
  comportement statique. Ack/Revoke ne déplace pas l'utilisateur de
  l'onglet Findings.
- Pas d'opérations en masse. Un finding à la fois.
- `sessionStorage` est purgé à la fermeture de l'onglet.
  Ne stockez pas de secrets de longue durée dans un artefact CI
  ouvert dans un profil de navigateur partagé.

### Caveat sécurité

La X-API-Key est stockée non chiffrée dans `sessionStorage`. C'est
acceptable pour un opérateur sur son poste personnel, où
`sessionStorage` est limité à un seul onglet et purgé à la fermeture.
Ce n'est pas acceptable sur un hôte partagé, puisque tout autre code
qui tourne dans la même session d'onglet peut lire `sessionStorage`. Le
rapport embarque une CSP stricte qui interdit le chargement de
scripts cross-origin et les gestionnaires d'événements inline, ce qui
atténue le risque sans l'éliminer.

**Réserve sur `script-src 'unsafe-inline'`** : le dashboard embarque son
JavaScript dans le fichier HTML (le rapport est un artefact
autonome, sans ressources externes). La CSP garde `script-src
'unsafe-inline'` pour cette raison. En mode live, `connect-src` est
limité à `'self'` plus l'URL daemon passée par l'opérateur, donc même
si un changement futur du template introduisait un vecteur XSS, les
seules destinations sortantes disponibles sont l'origine du document
et le daemon lui-même, pas un hôte attaquant arbitraire. Un durcissement
futur (hors périmètre pour 0.5.23) serait de livrer le JS dans un
`<script>` séparé hashé via `'sha256-...'` et de retirer
`'unsafe-inline'`. À suivre dans [`LIMITATIONS-FR.md`](./LIMITATIONS-FR.md)
quand ce travail aboutira.

**Surface de DoS via préflights CORS** : quand `[daemon.cors]
allowed_origins` est positionné, le daemon répond aux requêtes
`OPTIONS` préflight sur `/api/*` sans authentification (la vérification
X-API-Key passe après CORS). Une origine compromise dans la liste d'autorisation
(ou n'importe quelle origine en mode wildcard) peut envoyer des
préflights illimités qui contournent la barrière d'auth ack. Le
daemon n'embarque pas encore de limiteur de débit sur cette surface. Le
cache préflight `max_age=120s` atténue le volume des navigateurs
légitimes mais n'aide pas contre un script malveillant. Posture
d'atténuation pour 0.5.23 : déployer le daemon derrière un reverse proxy
avec limitation de débit par IP (nginx `limit_req`, Caddy `rate_limit`,
Cloudflare WAF) quand il est exposé cross-origin. Une intégration
native `tower-governor` fait l'objet d'un suivi pour une version future.

Si votre modèle de menace inclut un profil de navigateur partagé,
générez le HTML en mode statique et utilisez le CLI (`perf-sentinel
ack`) pour les opérations ack.

## Smoke test (manuel)

La procédure d'acceptation pour `--daemon-url` :

```bash
# 1. Baseline statique
perf-sentinel report --input traces.json --output /tmp/static.html
open /tmp/static.html
# Vérifier : pas de badge daemon, pas de boutons Ack, pas d'onglet
# Acknowledgments.

# 2. Daemon avec CORS ouvert
cat > /tmp/daemon.toml <<EOF
[daemon.cors]
allowed_origins = ["*"]

[daemon.ack]
enabled = true
EOF
perf-sentinel watch --config /tmp/daemon.toml &
DAEMON_PID=$!
sleep 1

# 3. Rapport live
perf-sentinel report --input traces.json --output /tmp/live.html \
  --daemon-url http://localhost:4318
open /tmp/live.html
# Vérifier : badge Connected vert, boutons Ack présents sur chaque
# row, onglet Acks visible, bouton refresh visible.

# 4. Cliquer Ack sur n'importe quel finding, remplir la modale,
# submit. Le badge sur la row passe à Revoke.

# 5. Cliquer Revoke, confirmer. Le badge repasse à Ack.

# 6. Redémarrer le daemon avec [daemon.ack] api_key positionné.
# Générez un secret frais à chaque run, ne jamais coller une valeur
# littérale en production :
kill $DAEMON_PID
SMOKE_KEY=$(openssl rand -hex 16)
cat >> /tmp/daemon.toml <<EOF
api_key = "${SMOKE_KEY}"
EOF
perf-sentinel watch --config /tmp/daemon.toml &
DAEMON_PID=$!
sleep 1
# Recharger /tmp/live.html, cliquer Ack : la modale d'auth s'ouvre,
# entrer $SMOKE_KEY, submit. La requête ack se retente automatiquement.

# 7. Recharger l'onglet à nouveau. La clé persiste en sessionStorage,
# pas de re-prompt jusqu'à fermeture de l'onglet.

kill $DAEMON_PID
```

## Choisir entre statique et live

| Cas d'usage                                           | Mode     |
|-------------------------------------------------------|----------|
| Artefact CI envoyé sur chaque MR                      | Statique |
| Revue de MR où le relecteur veut ack ou revoke        | Live     |
| Doc de prise en main empaquetée dans un tarball       | Statique |
| Dashboard ops live sur un poste personnel             | Live     |
| Profil de navigateur partagé (kiosk, machine de démo) | Statique |
| Analyse hors ligne air-gapped                         | Statique |

## Voir aussi

- [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md) pour la section de
  config `[daemon.cors]`.
- [`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md) pour la relation entre
  les acks TOML CI et les acks JSONL daemon.
- [`CLI-FR.md`](./CLI-FR.md) pour la référence de la sous-commande
  `perf-sentinel ack`.
