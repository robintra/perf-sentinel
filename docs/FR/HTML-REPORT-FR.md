# Rapport HTML

`perf-sentinel report` produit un dashboard HTML self-contained pour
l'exploration post-mortem d'un trace set. Il fonctionne dans deux modes :

- **Statique** (par défaut, depuis 0.5.0) : le fichier HTML embarque
  tous les panels et tous les arbres de traces en JSON. Pas d'egress
  réseau, pas de connexion daemon. Adapté pour un upload comme
  artefact CI (GitLab Pages, GitHub Pages, Artifactory, S3 static
  hosting). La sortie est identique pour tous les utilisateurs.
- **Live** (depuis 0.5.23, opt-in via `--daemon-url`) : le fichier
  HTML contacte un daemon en runtime pour les interactions ack/revoke.
  Le dashboard ajoute des boutons `Ack`/`Revoke` par finding, un
  indicateur de statut de connexion, un panel Acknowledgments, un
  toggle `Show acknowledged`, et un bouton refresh manuel. Les panels
  statiques (Findings, Explain, pg_stat, Diff, Correlations, GreenOps)
  conservent leur comportement statique, le mode live est purement
  additif.

## Mode statique

```bash
perf-sentinel report --input traces.json --output report.html
open report.html
```

C'est l'artefact que toute pipeline CI peut produire. Sans
`--daemon-url`, le HTML généré est byte-équivalent à la sortie 0.5.22
pour la même entrée. La CSP reste stricte (`default-src 'none'`),
aucun `fetch()` n'est émis vers un host quelconque.

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
   host que le navigateur peut atteindre.
2. Avoir `[daemon.cors] allowed_origins` configuré pour inclure
   l'origine du document. Voir [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md)
   pour la référence de la section. Sans ça, le navigateur drop la
   réponse.
3. Avoir `[daemon.ack] enabled = true` (par défaut).

La première fois que l'utilisateur clique sur `Ack` ou `Revoke` sur un
daemon protégé par 401, le rapport ouvre une modale d'authentification
et demande la `X-API-Key`. La clé est stockée en `sessionStorage`,
scopée à l'onglet, et purgée à la fermeture de l'onglet.

### CSP en mode live

Le mode live réécrit la meta tag Content-Security-Policy rendue pour
ajouter `connect-src <daemon_url>`. Toutes les autres directives
gardent leur valeur statique. La URL du daemon est validée par le CLI
avant d'atteindre la meta tag (pas d'autre scheme que http/https, pas
de path, pas de userinfo, pas de query string), donc aucun byte qui
pourrait casser la CSP ne peut atterrir dans la directive.

```text
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline';
img-src data:; base-uri 'none'; form-action 'none';
connect-src http://localhost:4318
```

### Validation de la URL daemon

Le CLI rejette :

- Entrée vide
- Schemes autres que `http`/`https`
- Host manquant (par exemple `http://`, `http://:8080`)
- Userinfo (par exemple `http://alice@host`, la X-API-Key n'a pas sa
  place dans une URL)
- Path components (par exemple `https://example.com/v1/`, le rapport
  construit `/api/...` lui-même)
- Query strings et fragments

Un slash final sur l'authority est silencieusement trimmé pour
l'uniformité avec le flag existant `perf-sentinel ack --daemon`.

### Flow d'authentification

1. Boot : GET `/api/status` pour déterminer la connectivité.
   L'endpoint status n'est pas authentifié (read-only, pas de
   secrets), donc le badge de la top bar peut atteindre `Connected`
   sans clé.
2. Premier clic `Ack`/`Revoke` : POST ou DELETE sur
   `/api/findings/<sig>/ack`. Sur un 401, la modale d'auth s'ouvre
   avec un input password (sans echo). La clé est stockée en
   `sessionStorage` sous `perf-sentinel.daemon.api-key` et la requête
   échouée est retentée.
3. Appels suivants : chaque requête authentifiée lit la clé depuis
   `sessionStorage` et set `X-API-Key`.
4. Fermeture de l'onglet : `sessionStorage` est purgé, le prochain
   reload re-prompte au premier appel authentifié.

### Qui vit où

| Élément                              | Mode    | Détails                                                                                                            |
|--------------------------------------|---------|--------------------------------------------------------------------------------------------------------------------|
| Badge statut daemon dans la top bar  | Live    | Trois états : `Connected` (vert), `Authentication required` (orange), `Disconnected` / `Unreachable` (rouge)       |
| Bouton refresh dans la top bar       | Live    | Re-fetch `/api/status`, `/api/acks`, et re-render l'état live                                                      |
| Boutons par row `Ack` / `Revoke`     | Live    | Cachés en mode statique via CSS, révélés sous `body.ps-live`                                                       |
| Toggle `Show acknowledged`           | Live    | Filtre la liste statique des findings contre le set live `/api/acks`                                               |
| Panel Acknowledgments                | Live    | Nouvel onglet `Acks` listant les acks daemon (paginé à 1000, cap daemon)                                           |
| Modale d'authentification            | Live    | Déclenchée par le premier 401 sur un appel write, jamais sur `/api/status`                                         |
| Modale d'acknowledgment              | Live    | Déclenchée par `Ack`. Champs : reason (requis), expires (Never / 24h / 7d / 30d), by (optionnel)                   |

### Limitations

- La liste des findings côté daemon n'est pas refetchée au toggle :
  le rapport statique est la source de vérité pour la liste des
  findings, et le toggle filtre seulement contre le set d'acks live.
  Pour voir les findings que le daemon a retenus au-delà du snapshot
  statique, utilisez `perf-sentinel query findings --include-acked`
  ou l'API HTTP daemon directement.
- Pas de timer auto-refresh. Le navigateur ne poll pas le daemon en
  permanence, utilisez le bouton refresh manuel. Le monitoring temps
  réel relève de Grafana, pas d'un artefact HTML par MR.
- Pas de cross-link `Explain` par row en mode live au-delà du
  comportement statique. Ack/Revoke ne déplace pas l'utilisateur de
  l'onglet Findings.
- Pas d'opérations en bulk. Un finding à la fois.
- `sessionStorage` est purgé à la fermeture de l'onglet, par design.
  Ne stockez pas de secrets de longue durée dans un artefact CI
  ouvert dans un profil de navigateur partagé.

### Caveat sécurité

La X-API-Key est stockée non chiffrée dans `sessionStorage`. C'est
acceptable pour un opérateur sur son poste personnel, où
`sessionStorage` est scopé à un seul onglet et purgé à la fermeture.
Ce n'est pas acceptable sur un host partagé, puisque tout autre code
qui tourne dans la même tab session peut lire `sessionStorage`. Le
rapport embarque une CSP stricte qui interdit le chargement de
scripts cross-origin et les handlers d'événements inline, ce qui
mitige le risque sans l'éliminer.

**Caveat `script-src 'unsafe-inline'`** : le dashboard embarque son
JavaScript dans le fichier HTML (le rapport est un artefact
self-contained, sans ressources externes). La CSP garde `script-src
'unsafe-inline'` pour cette raison. En mode live, `connect-src` est
limité à `'self'` plus la URL daemon passée par l'opérateur, donc même
si un changement futur du template introduisait un vecteur XSS, les
seules destinations sortantes disponibles sont l'origine du document
et le daemon lui-même, pas un host attaquant arbitraire. Un hardening
futur (hors scope pour 0.5.23) serait de livrer le JS dans un
`<script>` séparé hashé via `'sha256-...'` et de retirer
`'unsafe-inline'`.

**Surface de DoS via préflights CORS** : quand `[daemon.cors]
allowed_origins` est positionné, le daemon répond aux requêtes
`OPTIONS` préflight sur `/api/*` sans authentification (le check
X-API-Key passe après CORS). Une origine compromise dans la whitelist
(ou n'importe quelle origine en mode wildcard) peut envoyer des
préflights illimités qui contournent la barrière d'auth ack. Le
daemon n'embarque pas encore de rate limiter sur cette surface. Le
cache préflight `max_age=120s` mitige le volume des navigateurs
légitimes mais n'aide pas contre un script malveillant. Posture de
mitigation pour 0.5.23 : déployer le daemon derrière un reverse proxy
avec rate limiting par IP (nginx `limit_req`, Caddy `rate_limit`,
Cloudflare WAF) quand il est exposé cross-origin. Une intégration
native `tower-governor` est tracée pour une release future.

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

# 6. Redémarrer le daemon avec [daemon.ack] api_key positionné :
kill $DAEMON_PID
cat >> /tmp/daemon.toml <<EOF
api_key = "0123456789abcdef"
EOF
perf-sentinel watch --config /tmp/daemon.toml &
DAEMON_PID=$!
sleep 1
# Recharger /tmp/live.html, cliquer Ack : la modale d'auth s'ouvre,
# entrer la clé, submit. La requête ack se retente automatiquement.

# 7. Recharger l'onglet à nouveau. La clé persiste en sessionStorage,
# pas de re-prompt jusqu'à fermeture de l'onglet.

kill $DAEMON_PID
```

## Choisir entre statique et live

| Cas d'usage                                              | Mode      |
| -------------------------------------------------------- | --------- |
| Artefact CI uploadé sur chaque MR                        | Statique  |
| Revue de MR où le reviewer veut ack ou revoke            | Live      |
| Doc onboarding bundlée dans un tarball                   | Statique  |
| Dashboard ops live sur un poste personnel                | Live      |
| Profil de navigateur partagé (kiosk, machine de démo)    | Statique  |
| Analyse offline air-gapped                               | Statique  |

## Voir aussi

- [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md) pour la section de
  config `[daemon.cors]`.
- [`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md) pour la relation entre
  les acks TOML CI et les acks JSONL daemon.
- [`CLI-FR.md`](./CLI-FR.md) pour la référence de la sous-commande
  `perf-sentinel ack`.
