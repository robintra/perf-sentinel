# Vue d'ensemble sécurité

Cette page rassemble, pour une revue de sécurité, ce que Perf Sentinel et son service compagnon [PerfSentinelHub](https://github.com/robintra/PerfSentinelHub) exposent, envoient, stockent et signent. Chaque point donne le comportement par défaut et ce qui reste à la charge de l'exploitant, et renvoie au document qui le détaille. Les faits sont vérifiés sur le moteur en 0.25.4 et le Hub en 0.3.4.

Perf Sentinel est un binaire unique, auto-hébergé, qui analyse des traces OpenTelemetry, en CI ou en daemon (`watch`). Le Hub est un service optionnel, auto-hébergé lui aussi, qui garde l'historique des findings d'une flotte de daemons. Aucun des deux n'est un SaaS, et aucun n'envoie de télémétrie d'usage.

## En un coup d'œil

| Question | Perf Sentinel (moteur) | PerfSentinelHub |
|---|---|---|
| Hébergement | Votre infrastructure, binaire ou conteneur | Votre infrastructure, une seule réplique, SQLite |
| Ports entrants | OTLP gRPC `4317`, OTLP HTTP `4318`. L'API de requête, `/metrics` et `/health` partagent le `4318` | HTTP `8080` |
| Adresse d'écoute par défaut | `127.0.0.1` pour le binaire, `0.0.0.0` dans le chart Helm | Toutes les interfaces |
| TLS | Optionnel (`[daemon.tls]`), sans certificat client | Aucun dans le processus, à terminer sur l'ingress |
| Authentification | L'ingestion, `/metrics`, `/health` et les endpoints de lecture ne sont jamais authentifiés. Les écritures d'acquittement demandent une clé dès qu'elle est posée | L'envoi de findings demande une clé par source. L'interface est ouverte par défaut, avec une connexion OAuth2 optionnelle |
| Appels sortants par défaut | Aucun | Une vérification quotidienne des dernières versions sur l'API GitHub, qu'un réglage désactive |
| Données au repos | Le journal des acquittements, plus une archive NDJSON optionnelle. Jamais de spans bruts | Base SQLite, findings gardés 180 jours |
| Image | `FROM scratch`, UID 65534 | Ubuntu chiselée, sans shell, UID 1654 |
| Durcissement Helm | Non root, système de fichiers racine en lecture seule, toutes les capabilities retirées, seccomp `RuntimeDefault`, pas de jeton de service account. Modèle de NetworkPolicy désactivé par défaut | Non root, système de fichiers racine en lecture seule, toutes les capabilities retirées, seccomp `RuntimeDefault`. Pas de modèle de NetworkPolicy |
| Intégrité des releases | Tags signés, provenance de build SLSA (Build niveau 2) et SBOM SPDX pour les binaires, chart signé avec Cosign. Images non signées | Tags signés, signature Cosign sur chaque artefact de release, provenance GitHub, SBOM SPDX, reconstructions comparées octet par octet |
| Signalement de vulnérabilités | Signalement privé GitHub, accusé de réception sous 72 heures | Signalement privé GitHub, accusé de réception sous 3 jours ouvrés |
| Licence | AGPL-3.0-only | AGPL-3.0-only |

## Flux réseau

### Entrants

Le daemon du moteur écoute en OTLP gRPC sur `4317` et en OTLP HTTP sur `4318`. L'API de requête (`/api/*`), `/metrics` et `/health` sont servis par le même listener `4318`, donc une règle de pare-feu ou une NetworkPolicy ne peut pas laisser passer les traces tout en fermant l'API de requête. Un socket JSON local (`/tmp/perf-sentinel.sock` par défaut) est créé en mode `0600`. La commande `capture` reçoit de l'OTLP de la même façon, sur `127.0.0.1` par défaut. Les autres commandes n'écoutent rien.

Le binaire écoute sur `127.0.0.1` par défaut et journalise un avertissement pour toute autre adresse. Le chart Helm pose `0.0.0.0`, nécessaire à un pod derrière son Service, et reporte le contrôle d'accès sur le Service et sur la NetworkPolicy du chart, désactivée par défaut.

Le Hub écoute en HTTP simple sur `8080`. Le TLS est attendu sur l'ingress.

### Sortants

Le moteur ne fait aucun appel sortant tant que la configuration ou la ligne de commande ne nomme pas de destination :

| Destination | Ce qui le déclenche |
|---|---|
| Sources d'énergie : Alumet, Scaphandre, Kepler, BMC Redfish, énergie cloud via Prometheus | Une section `[green.*]` dans la configuration |
| Electricity Maps (`api.electricitymaps.com`) | Une section `[green.electricity_maps]` |
| Tempo, Jaeger, Victoria Traces | Les commandes `tempo` et `jaeger-query` |
| Prometheus | `pg-stat` et `mysql-stat` avec `--prometheus` |
| PerfSentinelHub | `[daemon.hub_export] enabled = true` |
| Un daemon en cours d'exécution | Les commandes `query`, `ack` et la TUI |
| Une URL HTTPS | `verify-hash --url` |
| Des processus locaux | `verify-hash` lance `cosign` et `gh`, `capture` lance la commande que vous lui donnez |

Les données de référence (intensités carbone, tables de puissance) sont compilées dans le binaire et jamais téléchargées à l'exécution.

Le client HTTPS du moteur ne fait confiance qu'aux certificats racines Mozilla embarqués. Il ne lit aucune variable de proxy et n'accepte pas de CA privée, donc un appel HTTPS sortant à travers un proxy qui inspecte le TLS échoue. Le HTTP simple vers un endpoint interne n'est pas concerné.

Le Hub appelle ses sources de daemons configurées (collecte, relais d'acquittement, vue en direct), et le sous-processus moteur qu'il lance joint le backend de traces fixé dans sa configuration. Quand la connexion est activée, il appelle les endpoints token et userinfo du fournisseur d'identité. Une fois par jour, il demande à l'API GitHub les dernières versions du moteur et du Hub. Cette vérification est active par défaut, et `hub.updateCheck.enabled: false` la désactive pour un cluster sans accès sortant.

## Données traitées et stockées

**Normalisation.** Les spans sont traités en mémoire, dans une fenêtre glissante (TTL de 30 s, 10 000 traces au plus par défaut). Les littéraux SQL deviennent `?`, les segments de chemin numériques et les UUID deviennent `{id}` et `{uuid}`, et la query string est retirée. Un finding porte le template obtenu et un décompte des valeurs distinctes, pas les valeurs. Quelques éléments restent tels quels, comme le texte entre guillemets doubles, les commentaires SQL et les segments de chemin non numériques : voir [Ce qui reste tel quel dans un template](LIMITATIONS-FR.md#tokenizer-sql). Un finding porte aussi le nom du service, les valeurs des attributs de regroupement, l'emplacement dans le code et l'identifiant de trace.

**Ce que le daemon écrit sur disque.**
- Le journal des acquittements, `acks.jsonl`, dans le répertoire de données local de l'utilisateur, en mode `0600`.
- L'archive NDJSON par fenêtre et l'archive des incidents, toutes deux optionnelles, en mode `0600`, ouvertes sans suivre les liens symboliques.
- Jamais de spans bruts.

La commande `capture` fait exception par conception : elle écrit les spans bruts qu'elle reçoit, littéraux compris, dans un fichier NDJSON. Traitez ce fichier comme sensible.

**Ce que le Hub stocke.** Le Hub garde chaque finding tel que le moteur l'a envoyé, dans une base SQLite (`/data/hub.db`) : template, identifiant de trace, valeurs des attributs de regroupement, emplacement dans le code, auteur et motif d'acquittement. Il ne masque rien lui-même. Par défaut, les findings sont gardés 180 jours, les rapports d'analyse 24 heures et les exécutions d'analyse 30 jours.

**Secrets.**
- Pour les clés d'API et les jetons du moteur, les variables d'environnement l'emportent sur le fichier de configuration.
- Les URL d'endpoint qui contiennent `user:pass@` sont refusées au chargement, et les identifiants de connexion sont masqués dans les journaux.
- Le Hub transmet l'identifiant d'une source au sous-processus moteur par une variable d'environnement, jamais sur la ligne de commande.

## Contrôle d'accès

### Moteur

Ne sont jamais authentifiés, quelle que soit la configuration :
- L'ingestion OTLP, gRPC et HTTP.
- `/metrics` et `/health`.
- Les endpoints de lecture `/api/findings`, `/api/findings/{trace_id}`, `/api/explain/{id}`, `/api/correlations`, `/api/status`, `/api/config`, `/api/energy` et `/api/export/report`.

Le daemon fait confiance à ses émetteurs de traces. C'est le modèle de menace annoncé : [Pas d'authentification](LIMITATIONS-FR.md#pas-dauthentification-tls-disponible-auth-non-intégrée).

Routes protégées par une clé :
- Les écritures d'acquittement (`POST` et `DELETE /api/findings/{signature}/ack`) restent ouvertes tant que `[daemon.ack] api_key` ou `PERF_SENTINEL_ACK_API_KEY` n'est pas posé.
- `GET /api/acks` et `GET /api/incidents` acceptent alors cette clé ou `[daemon] read_api_key`.
- `POST /api/incidents` exige une clé quand il est activé.

Les clés passent par `X-API-Key` ou `Authorization: Bearer` et sont comparées en temps constant. L'auteur d'un acquittement (`by`) est déclaré par l'appelant, pas authentifié.

Le CORS est désactivé par défaut, et une origine joker combinée à une clé d'écriture est refusée au démarrage. Le TLS couvre les deux listeners OTLP quand `[daemon.tls]` est posé. Les certificats clients (mTLS) ne sont pas pris en charge.

### Hub

- **Envoi de findings :** exige la clé `X-API-Key` de la source (au moins 32 caractères, liée à sa source, comparée en temps constant). Une source sans clé ne peut pas envoyer.
- **Interface et API :** ouvertes par défaut.
- **Connexion :** avec `hub.auth.enabled`, l'interface exige une session obtenue par OAuth2 authorization code avec PKCE auprès de votre fournisseur d'identité (Keycloak, Entra ID, Google, GitLab et d'autres). Le cookie de session est `Secure`, `SameSite=Lax`, glissant sur 8 heures. Ce n'est pas un OpenID Connect complet : l'identité vient de l'endpoint userinfo, l'ID token n'est pas validé et il n'y a pas de rôles.
- **Routes ouvertes même avec la connexion :** `/api/findings`, `/api/findings/{traceId}`, la route d'import (qui a sa propre clé), `/health` et `/metrics`. Gardez-les sur un réseau interne.
- **Lanceur :** il exécute le binaire moteur embarqué en sous-processus avec une liste d'arguments, sans shell. Le chemin du binaire et les endpoints de traces viennent de la configuration seulement. Les saisies de l'utilisateur sont validées, et une exécution s'arrête après 300 s par défaut.

Détails : [authentification du Hub](https://github.com/robintra/PerfSentinelHub/blob/main/docs/FR/AUTHENTICATION-FR.md), [limites du Hub](https://github.com/robintra/PerfSentinelHub/blob/main/docs/FR/LIMITATIONS-FR.md).

## Durcissement

**Code du moteur.**
- La bibliothèque cœur porte `#![forbid(unsafe_code)]`. La CLI contient quatre blocs `unsafe`, tous des appels `libc` (`killpg` pour arrêter un groupe de processus capturé, `getrusage` pour `bench`).
- Sous musl, l'allocateur est `mimalloc`, et la pile TLS (`ring`) contient du C et de l'assembleur.
- Clippy tourne en mode pedantic avec les avertissements traités comme des erreurs, et CodeQL analyse le code Rust.
- Le normaliseur SQL et l'ingestion JSON ont des cibles de fuzzing dans `fuzz/`, lancées à la main, pas en CI.

**Limites du moteur.**
- Les payloads sont plafonnés à 16 Mio, compressés comme décompressés (`max_payload_size`).
- La concurrence est plafonnée : 32 requêtes OTLP HTTP, 32 requêtes gRPC, 256 flux gRPC et 128 connexions socket.
- Une requête expire au bout d'une minute.
- Une réponse contient au plus 1 000 lignes.
- Les handshakes TLS expirent au bout de 10 s, 128 au plus en parallèle.
- Le contrôle d'admission mémoire (cgroup v2) est optionnel.
- Il n'y a pas de limite de débit par client.

**Rapport HTML du moteur.** Il pose une Content Security Policy et affiche chaque valeur avec `textContent`, et un test fait échouer le build si le gabarit utilise une API DOM dangereuse.

**Code du Hub.**
- .NET 10 compilé en NativeAOT, références nullables, avertissements traités comme des erreurs.
- Deux paquets à l'exécution (`Microsoft.Data.Sqlite`, `SQLitePCLRaw`) et du SQL paramétré.
- Les corps de requête sont plafonnés à 2 Mio (import, analyses) et 8 Kio (acquittement), et des verrous de concurrence répondent `503` avec `Retry-After`.
- Le Hub ne pose aucun en-tête de sécurité (CSP, HSTS, `X-Frame-Options`) : ajoutez-les sur l'ingress.

**Conteneurs et Helm.** Les deux charts :
- tournent sans root, avec un système de fichiers racine en lecture seule
- retirent toutes les capabilities et interdisent l'élévation de privilèges
- appliquent le profil seccomp `RuntimeDefault`

Le chart du moteur désactive aussi le jeton de service account. Son modèle de NetworkPolicy est désactivé par défaut et, une fois activé, ouvre `4317` et `4318` ensemble, donc tout pod autorisé à envoyer des traces peut aussi lire l'API de requête. Le chart du Hub ne fournit ni NetworkPolicy ni Ingress.

## Chaîne d'approvisionnement

**Moteur.**
- Chaque GitHub Action est épinglée à un SHA de commit.
- `Cargo.lock` est commité, et `cargo audit` et `cargo deny` tournent chaque jour.
- Trivy scanne l'image avant chaque release et bloque sur les vulnérabilités `HIGH` ou `CRITICAL` qui ont un correctif.
- Gitleaks scanne tout l'historique.
- Les tags de release sont signés, et GitHub les affiche comme vérifiés.

Les binaires de release portent :
- une attestation de provenance de build SLSA, Build niveau 2 selon la documentation de GitHub
- les données de dépendances `cargo-auditable` embarquées
- un SBOM SPDX attesté

Le chart Helm est signé avec Cosign. Les images de conteneur ne portent ni signature ni attestation. Voir la [politique de pinning supply chain](SUPPLY-CHAIN-FR.md) pour le détail et la [chaîne d'approvisionnement logicielle](HELM-DEPLOYMENT-FR.md#chaîne-dapprovisionnement-logicielle) pour le chart.

```bash
gh attestation verify perf-sentinel-linux-amd64 --repo robintra/perf-sentinel
gh attestation verify perf-sentinel-linux-amd64 --repo robintra/perf-sentinel \
  --predicate-type https://spdx.dev/Document/v2.3
cargo audit bin perf-sentinel-linux-amd64
```

**Hub.**
- Chaque artefact de release, archive OCI de l'image et chart compris, est signé avec Cosign (`sign-blob`) et porte une provenance de build GitHub et une attestation SBOM SPDX.
- La release construit l'image deux fois et compare les résultats octet par octet.
- Les restaurations NuGet sont verrouillées.
- Les actions sont épinglées par SHA.
- CodeQL, SonarCloud, Trivy, OSV-Scanner, Gitleaks, TruffleHog et OpenSSF Scorecard tournent en CI.

L'image poussée sur GHCR n'est pas signée dans le registre : vérifiez les artefacts de release comme le décrit la [procédure de release du Hub](https://github.com/robintra/PerfSentinelHub/blob/main/RELEASING.md).

Aucun des deux dépôts n'exige aujourd'hui de commits signés sur sa branche principale. Les tags de release sont signés sur les deux.

## Signaler une vulnérabilité

Les deux projets reçoivent les signalements par le signalement privé de vulnérabilités de GitHub.
- **Moteur :** accusé de réception sous 72 heures, au mieux puisque le projet a un seul mainteneur. Première évaluation sous 7 jours. Un CVE est demandé à partir de la sévérité Medium. Seule la dernière version mineure reçoit les correctifs. Voir [SECURITY.md](https://github.com/robintra/perf-sentinel/blob/main/SECURITY.md).
- **Hub :** accusé de réception sous 3 jours ouvrés, évaluation sous 7 jours ouvrés. Seule la dernière version `0.x` publiée reçoit les correctifs. Voir la [politique de sécurité du Hub](https://github.com/robintra/PerfSentinelHub/blob/main/SECURITY.md).

## Recommandations de déploiement

1. Gardez l'ingestion OTLP sur un réseau de confiance. Le daemon fait confiance à ses émetteurs.
2. Activez la NetworkPolicy du chart du moteur avec des sélecteurs de namespace ou de pod. Pour soustraire l'API de requête à tout ce qui envoie des traces, placez devant le `4318` un reverse proxy qui filtre `/api/*`.
3. Si vous utilisez les acquittements, posez `PERF_SENTINEL_ACK_API_KEY` depuis un Secret. Sans elle, quiconque atteint le port peut acquitter un finding.
4. Chiffrez le trafic avec `[daemon.tls]`, un service mesh ou l'ingress.
5. Pour le Hub, activez `hub.auth`, terminez le TLS et ajoutez les en-têtes de sécurité sur l'ingress, et écrivez une NetworkPolicy, puisque le chart n'en fournit pas.
6. Dans un cluster sans accès sortant, posez `hub.updateCheck.enabled: false`.
7. Posez `http.route` dans votre instrumentation, et gardez les données personnelles hors des segments de chemin d'URL et des chaînes MySQL entre guillemets doubles.
8. Traitez les fichiers écrits par `capture` comme sensibles. Ils contiennent des spans bruts.
9. Si votre politique exige des images signées, vérifiez le binaire de release et construisez ou signez l'image dans votre propre registre.
